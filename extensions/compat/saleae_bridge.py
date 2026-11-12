"""Run a Saleae Logic 2 High Level Analyzer (HLA) as a libsigrokdecode
protocol decoder.

Each installed HLA gets one decoder package per input protocol (see hla.ps1).
That package calls make_decoder(), which loads the HLA and builds a Decoder
class that is stacked on a sigrok decoder (see INPUTS: uart, i2c, spi, can,
uart for LIN, onewire_link, manchester, i2s, parallel):

- the lower decoder's Python output is turned into the frames Logic 2's
  analyzers give an HLA (types and data keys as in Saleae's frame format
  reference),
- HLA settings become decoder options,
- frames the HLA returns become annotations, one class per result type,
  with text rendered from the result_types 'format' strings.

HLA print() output goes to hla.log next to this file. With the environment
variable SALEAE_HLA_TRACE=1 every frame an HLA returns is logged there too.

Python packages added with "hla.ps1 package add" live in site-packages next
to this file and their DLLs in bin; both are put on the search paths here.
"""

import contextlib
import importlib
import json
import os
import re
import sys
import threading
import traceback
import types
from datetime import datetime, timezone

import sigrokdecode as srd

import saleae.data as _sdata
from saleae.analyzers import AnalyzerFrame, HighLevelAnalyzer
from saleae.analyzers.settings import (StringSetting, NumberSetting,
                                       ChoicesSetting, ValidationError)

_HERE = os.path.dirname(os.path.abspath(__file__))
LOG_PATH = os.path.join(_HERE, 'hla.log')
TRACE = os.environ.get('SALEAE_HLA_TRACE', '') not in ('', '0')


def _add_package_paths():
    packages = os.path.join(_HERE, 'site-packages')
    if os.path.isdir(packages) and packages not in sys.path:
        sys.path.append(packages)
    dlls = os.path.join(_HERE, 'bin')
    if os.path.isdir(dlls):
        # Extension modules (numpy's .pyd) load their DLLs from here.
        if hasattr(os, 'add_dll_directory'):
            os.add_dll_directory(dlls)
        path = os.environ.get('PATH', '')
        if dlls not in path.split(os.pathsep):
            os.environ['PATH'] = dlls + os.pathsep + path


_add_package_paths()


def _value_bytes(value, nbits):
    nbytes = max(1, (nbits + 7) // 8, (int(value).bit_length() + 7) // 8)
    return int(value).to_bytes(nbytes, 'big')


class _ByteInt(int):
    """CAN data_field value. Saleae documents it as an int; HLAs written
    against bytes-like data (frame.data['data'][0]) work too."""

    def __getitem__(self, index):
        return bytes([int(self) & 0xff])[index]

    def __len__(self):
        return 1

    def __iter__(self):
        return iter(bytes([int(self) & 0xff]))

    def __bytes__(self):
        return bytes([int(self) & 0xff])


# --- Input adapters: sigrok Python output -> Saleae frames ------------------
# feed(ss, es, data) yields (ss, es, frame_type, frame_data) tuples.

class UartInput:
    """sigrok uart -> Async Serial 'data' frames (start bit to stop bit)."""

    options = (
        {'id': 'hla_uart_line', 'desc': 'UART line fed to the HLA',
         'default': 'both', 'values': ('both', 'RX', 'TX')},
    )

    def __init__(self, options):
        line = options.get('hla_uart_line', 'both')
        self.lines = {'both': (0, 1), 'RX': (0,), 'TX': (1,)}.get(line, (0, 1))
        self.error = {0: None, 1: None}
        self.nbits = {0: 8, 1: 8}
        self.have_data = {0: False, 1: False}

    def feed(self, ss, es, data):
        cmd, rxtx, pdata = data
        if rxtx not in self.lines:
            return
        if cmd == 'STARTBIT':
            self.error[rxtx] = None
            self.have_data[rxtx] = False
        elif cmd == 'DATA':
            self.nbits[rxtx] = len(pdata[1]) or 8
            self.have_data[rxtx] = True
        elif cmd == 'PARITY ERROR':
            self.error[rxtx] = 'Parity'
        elif cmd == 'INVALID STOPBIT':
            self.error[rxtx] = self.error[rxtx] or 'Framing'
        elif cmd == 'FRAME':
            # A FRAME without data follows an invalid start bit: skip it.
            if not self.have_data[rxtx]:
                return
            self.have_data[rxtx] = False
            value, valid = pdata
            frame = {'data': _value_bytes(value, self.nbits[rxtx])}
            error = self.error[rxtx] or (None if valid else 'Framing')
            if error:
                frame['error'] = error
            yield ss, es, 'data', frame


class I2cInput:
    """sigrok i2c -> 'start', 'address', 'data', 'stop' frames. Address and
    data frames span the byte and its ACK/NACK bit."""

    options = ()

    def __init__(self, options):
        self.pending = None

    def _flush(self):
        if self.pending:
            ss, es, ftype, frame = self.pending
            frame.setdefault('ack', False)
            self.pending = None
            yield ss, es, ftype, frame

    def feed(self, ss, es, data):
        cmd, value = data
        if cmd in ('START', 'START REPEAT'):
            yield from self._flush()
            yield ss, es, 'start', {}
        elif cmd in ('ADDRESS READ', 'ADDRESS WRITE'):
            yield from self._flush()
            self.pending = [ss, es, 'address',
                            {'address': _value_bytes(value, 7),
                             'read': cmd == 'ADDRESS READ'}]
        elif cmd in ('DATA READ', 'DATA WRITE'):
            yield from self._flush()
            self.pending = [ss, es, 'data', {'data': _value_bytes(value, 8)}]
        elif cmd in ('ACK', 'NACK'):
            if self.pending:
                self.pending[1] = es
                self.pending[3]['ack'] = cmd == 'ACK'
                yield from self._flush()
        elif cmd == 'STOP':
            yield from self._flush()
            yield ss, es, 'stop', {}


class SpiInput:
    """sigrok spi -> 'enable', 'result', 'disable' frames."""

    options = (
        {'id': 'hla_spi_cs_polarity',
         'desc': 'CS# polarity (same as the SPI decoder)',
         'default': 'active-low', 'values': ('active-low', 'active-high')},
    )

    def __init__(self, options):
        polarity = options.get('hla_spi_cs_polarity', 'active-low')
        self.active = 1 if polarity == 'active-high' else 0
        self.mosi_bits = self.miso_bits = 8

    def feed(self, ss, es, data):
        cmd, d1, d2 = data
        if cmd == 'BITS':
            self.mosi_bits = len(d1) if d1 else self.mosi_bits
            self.miso_bits = len(d2) if d2 else self.miso_bits
        elif cmd == 'CS-CHANGE':
            # The first CS-CHANGE (old value None) only reports the start
            # state; Logic 2 sends no frame for it either.
            if d1 is None or d2 is None or d1 == d2:
                return
            yield ss, es, ('enable' if d2 == self.active else 'disable'), {}
        elif cmd == 'DATA':
            yield ss, es, 'result', {
                'mosi': _value_bytes(d1 or 0, self.mosi_bits),
                'miso': _value_bytes(d2 or 0, self.miso_bits),
            }


def _can_crc15(bits):
    crc = 0
    for bit in bits:
        top = ((crc >> 14) & 1) ^ bit
        crc = (crc << 1) & 0x7fff
        if top:
            crc ^= 0x4599
    return crc


def _bits(value, width):
    return [(value >> i) & 1 for i in range(width - 1, -1, -1)]


class CanInput:
    """sigrok can -> identifier/control/data/crc/ack field frames.

    The sigrok decoder reports a whole frame (SOF to EOF) at once, so field
    boundaries are placed by bit count (stuff bits ignored). The CRC is
    computed from the decoded fields for classic CAN frames (reserved bits
    taken as dominant); CAN FD crc_field frames carry no 'crc' key. ACK is
    reported as present, since the sigrok decoder only outputs frames that
    reached EOF.
    """

    options = ()

    def __init__(self, options):
        pass

    def feed(self, ss, es, data):
        frame_type, ident, rtr_type, dlc, payload = data
        extended = frame_type == 'extended'
        remote = rtr_type == 'remote'
        payload = list(payload or [])
        fd = (dlc or 0) > 8 or len(payload) > 8
        id_bits = 32 if extended else 12          # SOF .. RTR
        total = id_bits + 6 + 8 * len(payload) + 16 + 2 + 7
        step = (es - ss) / float(total)
        pos = [float(ss)]

        def span(nbits):
            begin = pos[0]
            pos[0] += nbits * step
            return int(round(begin)), int(round(pos[0]))

        a, b = span(id_bits)
        frame = {'identifier': ident}
        if extended:
            frame['extended'] = True
        if remote:
            frame['remote_frame'] = True
        yield a, b, 'identifier_field', frame

        a, b = span(6)
        yield a, b, 'control_field', {
            'num_data_bytes': dlc if not fd else len(payload)}

        for byte in payload:
            a, b = span(8)
            yield a, b, 'data_field', {'data': _ByteInt(byte)}

        a, b = span(16)
        crc = {}
        if not fd and dlc is not None:
            if extended:
                head = ([0] + _bits(ident >> 18, 11) + [1, 1] +
                        _bits(ident & 0x3ffff, 18) + [int(remote), 0, 0])
            else:
                head = [0] + _bits(ident, 11) + [int(remote), 0, 0]
            body = [] if remote else [bit for byte in payload
                                      for bit in _bits(byte, 8)]
            crc['crc'] = _can_crc15(head + _bits(dlc, 4) + body)
        yield a, b, 'crc_field', crc

        a, b = span(2)
        yield a, b, 'ack_field', {'ack': True}


class LinInput:
    """sigrok uart (one line) -> LIN 'header_break', 'header_sync',
    'header_pid', 'data' (data, index) and 'checksum' frames.

    The header frames are sent as they arrive. The response bytes are sent
    when the transaction ends (the next break, two idle frame times, or eight
    data bytes and a checksum), with the last byte as the checksum, so
    'data_or_checksum' is never sent. protected_id is the 6-bit identifier.
    The 0x00 byte that a break reads as a framing error is dropped."""

    options = (
        {'id': 'hla_lin_line', 'desc': 'UART line carrying LIN',
         'default': 'RX', 'values': ('RX', 'TX')},
    )

    def __init__(self, options):
        self.line = 1 if options.get('hla_lin_line') == 'TX' else 0
        self.state = 'break'
        self.response = []
        self.idle = 0
        self.null = None

    def _flush(self):
        response, self.response = self.response, []
        if not response:
            return
        *data, last = response
        for i, (ss, es, value) in enumerate(data):
            yield ss, es, 'data', {'data': value, 'index': i}
        yield last[0], last[1], 'checksum', {'checksum': last[2]}

    def _byte(self, ss, es, value):
        if self.state == 'sync':
            self.state = 'pid' if value == 0x55 else 'break'
            if value == 0x55:
                yield ss, es, 'header_sync', {}
        elif self.state == 'pid':
            self.state = 'response'
            yield ss, es, 'header_pid', {'protected_id': value & 0x3f}
        elif self.state == 'response':
            self.idle = 0
            self.response.append((ss, es, value))
            if len(self.response) == 9:
                self.state = 'break'
                yield from self._flush()

    def feed(self, ss, es, data):
        cmd, rxtx, pdata = data
        if rxtx != self.line:
            return
        if cmd == 'BREAK':
            self.null = None
            yield from self._flush()
            self.state = 'sync'
            yield ss, es, 'header_break', {}
            return
        if cmd not in ('FRAME', 'IDLE'):
            return
        if self.null:
            null, self.null = self.null, None
            yield from self._byte(*null)
        if cmd == 'FRAME':
            value, valid = pdata
            if value == 0 and not valid:
                self.null = (ss, es, value)
            else:
                yield from self._byte(ss, es, value)
        elif self.state == 'response' and self.response:
            # Idle before the first response byte is the response space.
            self.idle += 1
            if self.idle >= 2:
                self.state = 'break'
                yield from self._flush()


# ROM command -> (frame type, description, what follows).
_ONEWIRE_COMMANDS = {
    0x33: ('rom_command', 'read', 'rom'),
    0x0f: ('rom_command', 'read', 'rom'),
    0x55: ('rom_command', 'match', 'rom'),
    0x69: ('rom_command', 'match', 'rom'),
    0xcc: ('rom_command', 'skip', 'data'),
    0x3c: ('rom_command', 'skip', 'data'),
    0xa5: ('rom_command', 'skip', 'data'),
    0xf0: ('rom_command', 'search', 'search'),
    0xec: ('alarm', None, 'search'),
}


class OneWireInput:
    """sigrok onewire_link -> 1-Wire 'reset', 'presence', 'rom_command',
    'family_code', 'id', 'crc', 'data', 'invalid_rom_command' and 'alarm'
    frames. 'reset' and 'presence' both span the presence detect window
    (onewire_link reports them as one packet). Resume (0xA5) and overdrive
    skip (0x3C) are reported as 'skip', overdrive match (0x69) as 'match'."""

    options = ()

    def __init__(self, options):
        self.state = None
        self.bits = []
        self.slots = []

    def feed(self, ss, es, data):
        cmd, value = data
        if cmd == 'RESET/PRESENCE':
            self.state, self.bits, self.slots = 'command', [], []
            yield ss, es, 'reset', {}
            if value:
                yield ss, es, 'presence', {}
            return
        if cmd != 'BIT' or self.state is None:
            return
        if self.state == 'search':
            # Each ROM bit: device bit, its complement, the master's choice.
            self.slots.append((ss, es, value))
            if len(self.slots) < 3:
                return
            (first, _, _), _, (_, end, chosen) = self.slots
            self.slots = []
            self.bits.append((first, end, chosen))
        else:
            self.bits.append((ss, es, value))
        need = 64 if self.state in ('rom', 'search') else 8
        if len(self.bits) < need:
            return
        bits, self.bits = self.bits, []

        def field(lo, hi):
            value = sum(b[2] << i for i, b in enumerate(bits[lo:hi]))
            return bits[lo][0], bits[hi - 1][1], value

        if self.state == 'command':
            a, b, command = field(0, 8)
            ftype, desc, self.state = _ONEWIRE_COMMANDS.get(
                command, ('invalid_rom_command', None, 'data'))
            frame = {'rom_command': bytes([command])}
            if desc:
                frame['description'] = desc
            yield a, b, ftype, frame
        elif self.state in ('rom', 'search'):
            a, b, family = field(0, 8)
            yield a, b, 'family_code', {'family': bytes([family])}
            a, b, ident = field(8, 56)
            yield a, b, 'id', {'id': ident}
            a, b, crc = field(56, 64)
            yield a, b, 'crc', {'crc': bytes([crc])}
            self.state = 'data'
        else:
            a, b, byte = field(0, 8)
            yield a, b, 'data', {'data': bytes([byte])}


class ManchesterInput:
    """manchester (the decoder installed with this bridge) -> 'data' frames,
    one per word."""

    options = ()

    def __init__(self, options):
        pass

    def feed(self, ss, es, data):
        if data[0] == 'WORD':
            yield ss, es, 'data', {'data': data[1]}


class I2sInput:
    """sigrok i2s -> 'data' frames (channel 0 left, 1 right). sigrok gives
    the raw word, so the sample format is an option here."""

    options = (
        {'id': 'hla_i2s_format',
         'desc': 'I2S samples for the HLA (signed-N: N-bit two\'s complement)',
         'default': 'unsigned',
         'values': ('unsigned', 'signed-8', 'signed-16', 'signed-24',
                    'signed-32')},
    )

    def __init__(self, options):
        fmt = options.get('hla_i2s_format', 'unsigned')
        self.bits = int(fmt.split('-')[1]) if fmt.startswith('signed-') else 0

    def feed(self, ss, es, data):
        if data[0] != 'DATA':
            return
        channel, value = data[1]
        if self.bits:
            value &= (1 << self.bits) - 1
            if value >> (self.bits - 1):
                value -= 1 << self.bits
        yield ss, es, 'data', {'channel': 0 if channel == 'L' else 1,
                               'data': value}


class ParallelInput:
    """sigrok parallel -> Simple Parallel 'data' frames, one per clocked
    item (the bus width is the number of assigned data lines)."""

    options = ()

    def __init__(self, options):
        pass

    def feed(self, ss, es, data):
        if data[0] == 'ITEM':
            yield ss, es, 'data', {'data': data[1][0]}


# Input protocol -> (sigrok decoder the HLA stacks on, adapter, Logic 2 name).
INPUTS = {
    'uart': ('uart', UartInput, 'Async Serial'),
    'i2c': ('i2c', I2cInput, 'I2C'),
    'spi': ('spi', SpiInput, 'SPI'),
    'can': ('can', CanInput, 'CAN'),
    'lin': ('uart', LinInput, 'LIN'),
    'onewire': ('onewire_link', OneWireInput, '1-Wire'),
    'manchester': ('manchester', ManchesterInput, 'Manchester'),
    'i2s': ('i2s', I2sInput, 'I2S / PCM'),
    'parallel': ('parallel', ParallelInput, 'Simple Parallel'),
}


# --- Rendering ---------------------------------------------------------------

_PLACEHOLDER = re.compile(r'\{\{\{?\s*([A-Za-z_][\w.]*)\s*\}?\}\}')


def _text(value):
    if value is None:
        return ''
    if isinstance(value, bool):
        return 'true' if value else 'false'
    if isinstance(value, (bytes, bytearray)):
        return ' '.join('0x{:02X}'.format(b) for b in value)
    if isinstance(value, float):
        return '{:g}'.format(value)
    if isinstance(value, (list, tuple)):
        return ', '.join(_text(v) for v in value)
    return str(value)


def _lookup(frame, path):
    parts = path.split('.')
    if parts == ['type']:
        return frame.type
    if parts[0] != 'data':
        return None
    value = frame.data or {}
    for key in parts[1:]:
        if isinstance(value, dict):
            value = value.get(key)
        else:
            value = getattr(value, key, None)
    return value


def render(frame, result_types):
    spec = result_types.get(frame.type) if result_types else None
    fmt = spec.get('format') if isinstance(spec, dict) else None
    if fmt:
        return _PLACEHOLDER.sub(lambda m: _text(_lookup(frame, m.group(1))),
                                fmt)
    fields = ', '.join('{}={}'.format(k, _text(v))
                       for k, v in (frame.data or {}).items())
    return '{}: {}'.format(frame.type, fields) if fields else str(frame.type)


# --- Loading -----------------------------------------------------------------

# Decode signals run in separate threads; appends on Windows are not atomic.
_LOG_LOCK = threading.Lock()


class _Log:
    """File-like target for HLA print() output."""

    def __init__(self, tag):
        self.tag = tag
        self.buf = ''

    def write(self, text):
        self.buf += text
        if '\n' in self.buf:
            lines, self.buf = self.buf.rsplit('\n', 1)
            out = ''.join('[{}] {}\n'.format(self.tag, line)
                          for line in lines.split('\n'))
            try:
                with _LOG_LOCK, open(LOG_PATH, 'a', encoding='utf-8') as f:
                    f.write(out)
            except OSError:
                pass
        return len(text)

    def flush(self):
        pass


def _load_hla_class(manifest, package_dir):
    ext_dir = os.path.join(package_dir, 'ext')
    module_name, class_name = manifest['entry'].rsplit('.', 1)
    # Import the extension as its own package so that several extensions
    # with the same file names (HighLevelAnalyzer.py) do not collide.
    root = '_hla_' + manifest['id']
    if root not in sys.modules:
        pkg = types.ModuleType(root)
        pkg.__path__ = [ext_dir]
        pkg.__package__ = root
        sys.modules[root] = pkg
    if ext_dir not in sys.path:
        sys.path.append(ext_dir)
    module = importlib.import_module(root + '.' + module_name)
    hla_class = getattr(module, class_name)
    if not (isinstance(hla_class, type) and
            issubclass(hla_class, HighLevelAnalyzer)):
        raise TypeError(
            '{} is not a HighLevelAnalyzer subclass (the pre-1.0 HLA API '
            'with get_capabilities() is not supported, as in Logic 2)'.format(
                manifest['entry']))
    return hla_class


def _option_for(setting):
    desc = setting.label or setting.name
    if isinstance(setting, ChoicesSetting):
        return {'id': setting.name, 'desc': desc,
                'default': setting.choices[0] if setting.choices else '',
                'values': tuple(setting.choices)}
    if isinstance(setting, NumberSetting):
        default = 0.0
        if setting.min_value is not None:
            default = max(default, float(setting.min_value))
        if setting.max_value is not None:
            default = min(default, float(setting.max_value))
        return {'id': setting.name, 'desc': desc, 'default': default}
    return {'id': setting.name, 'desc': desc, 'default': ''}


def _setting_value(setting, value):
    if isinstance(setting, NumberSetting):
        value = float(value)
        return int(value) if value.is_integer() else value
    return value if isinstance(value, str) else str(value)


def _doc(manifest, settings, result_types):
    lines = [manifest.get('desc') or '', '',
             'Saleae Logic 2 HLA extension "{}" {} by {}.'.format(
                 manifest.get('extension_name', ''),
                 manifest.get('version', ''), manifest.get('author', '')),
             'Input: {} decoder ({} frames).'.format(
                 INPUTS[manifest['input']][0], INPUTS[manifest['input']][2])]
    if manifest.get('source'):
        lines.append('Source: ' + manifest['source'])
    if settings:
        lines.append('Settings: ' + ', '.join(s.label or s.name
                                               for s in settings))
    if result_types:
        lines.append('Result types: ' + ', '.join(result_types))
    return '\n'.join(lines).strip()


def make_decoder(package_dir):
    """Return (Decoder class, docstring) for the HLA package in package_dir."""
    with open(os.path.join(package_dir, 'hla.json'), encoding='utf-8') as f:
        manifest = json.load(f)
    hla_class = _load_hla_class(manifest, package_dir)
    lower, adapter_class, _ = INPUTS[manifest['input']]
    settings = hla_class._get_settings()
    result_types = dict(getattr(hla_class, 'result_types', None) or {})
    # Some HLAs add result types in __init__: probe once with default settings.
    try:
        defaults = {s.name: _setting_value(s, _option_for(s)['default'])
                    for s in settings}
        with contextlib.redirect_stdout(_Log(manifest['id'])):
            probe = hla_class(defaults)
        for key, spec in (getattr(probe, 'result_types', None) or {}).items():
            result_types.setdefault(key, spec)
    except Exception:
        pass

    class_ids, used = [], set()
    for key in result_types:
        cid = 'rt-' + re.sub(r'[^A-Za-z0-9_-]', '_', str(key))
        while cid in used:
            cid += '_'
        used.add(cid)
        class_ids.append((cid, str(key)))
    type_index = {key: i for i, key in enumerate(result_types)}
    other_index = len(class_ids)
    error_index = other_index + 1
    annotations = tuple(class_ids) + (('other', 'Other frames'),
                                      ('error', 'HLA errors'))
    annotation_rows = (('frames', 'Frames', tuple(range(other_index + 1))),
                       ('errors', 'Errors', (error_index,)))
    options = tuple(_option_for(s) for s in settings) + adapter_class.options

    class Decoder(srd.Decoder):
        api_version = 3
        id = manifest['id']
        name = manifest['name']
        longname = manifest['longname']
        desc = manifest.get('desc') or manifest['longname']
        license = manifest.get('license') or 'unknown'
        inputs = [lower]
        outputs = [manifest['id']]
        tags = ['Saleae HLA']

        def __init__(self):
            self.reset()

        def reset(self):
            self.hla = None
            self.error = None
            self.error_shown = False
            self.samplerate = None

        def metadata(self, key, value):
            if key == srd.SRD_CONF_SAMPLERATE:
                self.samplerate = value

        def start(self):
            self.out_ann = self.register(srd.OUTPUT_ANN)
            self.out_python = self.register(srd.OUTPUT_PYTHON)
            self.clock = _sdata._Clock(self.samplerate,
                                       datetime.now(timezone.utc))
            self.adapter = adapter_class(self.options)
            self.log = _Log(manifest['id'])
            values = {}
            try:
                for s in settings:
                    values[s.name] = _setting_value(s, self.options[s.name])
                with self._running():
                    self.hla = hla_class(values)
            except ValidationError as e:
                self.error = 'Setting: {}'.format(e)
            except Exception as e:
                self.error = 'HLA init failed: {}: {}'.format(
                    type(e).__name__, e)
                self._log_traceback()

        @contextlib.contextmanager
        def _running(self):
            previous = _sdata._current_clock
            _sdata._current_clock = self.clock
            try:
                with contextlib.redirect_stdout(self.log):
                    yield
            finally:
                _sdata._current_clock = previous

        def _log_traceback(self):
            self.log.write(traceback.format_exc())
            self.log.write('\n')

        def _put_error(self, ss, es, text):
            self.put(ss, es, self.out_ann, [error_index, [text, 'Error', 'E']])
            if TRACE:
                self.log.write('{}-{} ERROR {}\n'.format(ss, es, text))

        def decode(self, ss, es, data):
            if self.hla is None:
                if self.error and not self.error_shown:
                    self._put_error(ss, es, self.error)
                    self.error_shown = True
                return
            for fs, fe, ftype, fdata in self.adapter.feed(ss, es, data):
                frame = AnalyzerFrame(ftype, self.clock.at(fs),
                                      self.clock.at(fe), fdata)
                try:
                    with self._running():
                        result = self.hla.decode(frame)
                except Exception as e:
                    self._put_error(fs, fe, '{}: {}'.format(
                        type(e).__name__, e))
                    self._log_traceback()
                    continue
                self._emit(result, fs, fe)

        def _sample(self, t, fallback):
            if isinstance(t, _sdata.GraphTime):
                return max(0, t._sample())
            return fallback

        def _emit(self, result, fs, fe):
            if result is None:
                return
            frames = result if isinstance(result, (list, tuple)) else [result]
            # Some HLAs add result_types in __init__, so read them now.
            current_types = getattr(self.hla, 'result_types', None)
            if not isinstance(current_types, dict):
                current_types = result_types
            for f in frames:
                if not isinstance(f, AnalyzerFrame):
                    continue
                s = self._sample(f.start_time, fs)
                e = self._sample(f.end_time, fe)
                if e < s:
                    s, e = e, s
                text = render(f, current_types)
                texts = [text] if text == str(f.type) else [text, str(f.type)]
                self.put(s, e, self.out_ann,
                         [type_index.get(f.type, other_index), texts])
                self.put(s, e, self.out_python,
                         {'type': f.type, 'data': f.data})
                if TRACE:
                    self.log.write('{}-{} {} {}\n'.format(
                        s, e, f.type,
                        text.replace('\r', '\\r').replace('\n', '\\n')))

    # Set after the class body: a class body cannot read these function
    # locals under the same names it would define.
    Decoder.annotations = annotations
    Decoder.annotation_rows = annotation_rows
    Decoder.options = options
    return Decoder, _doc(manifest, settings, result_types)
