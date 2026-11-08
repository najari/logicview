#!/usr/bin/env python3
"""logicview MCP server.

The tools of Saleae Logic 2's MCP server (same names, arguments and result
shapes), for the FX2-based analyzers that SLogicView drives. Captures and
decoding run through sigrok-cli built from the same sources as the app, so
the app's decoders and the Saleae HLA bridge (extensions/) apply.

Transports: Streamable HTTP (JSON responses, default http://127.0.0.1:10530,
like Logic 2) or stdio (--stdio). Standard library only.

Differences from Logic 2:
- Captures are sigrok session files (.sr), not .sal.
- Analyzers: "Async Serial", "I2C", "SPI", "CAN" with Logic 2 style settings,
  or any sigrok decoder id with its own channel/option names.
- Data table / legacy exports contain annotation text (one value column).
- No analog channels, threshold voltage, glitch filter or pulse triggers
  (the hardware has none of these).
- Extra tool read_analyzer_frames returns decoded frames directly.
"""

import argparse
import configparser
import csv
import json
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import zipfile
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SERVER_NAME = 'logicview'
SERVER_VERSION = '0.1.0'
PROTOCOL_VERSION = '2025-03-26'
HERE = os.path.dirname(os.path.abspath(__file__))
NO_WINDOW = getattr(subprocess, 'CREATE_NO_WINDOW', 0)


class McpError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


def need(condition, code, message):
    if not condition:
        raise McpError(code, message)


def log(*parts):
    print('[logicview-mcp]', *parts, file=sys.stderr, flush=True)


# --- sigrok session files ------------------------------------------------------

_RATE = re.compile(r'([\d.]+)\s*([kMG]?)Hz', re.I)


def parse_rate(text):
    m = _RATE.search(text or '')
    need(m, 'INVALID_REQUEST', 'Capture file has no samplerate')
    return float(m.group(1)) * {'': 1, 'k': 1e3, 'm': 1e6, 'g': 1e9}[m.group(2).lower()]


def rate_text(rate):
    for div, unit in ((1e9, 'GHz'), (1e6, 'MHz'), (1e3, 'kHz')):
        if rate >= div and rate % div == 0:
            return '{:g} {}'.format(rate / div, unit)
    return '{:g} Hz'.format(rate)


class SrData:
    """Logic data of a sigrok session file (version 2, one logic device)."""

    def __init__(self, path):
        try:
            with zipfile.ZipFile(path) as z:
                meta = z.read('metadata').decode('utf-8', 'replace')
                parser = configparser.ConfigParser(interpolation=None)
                parser.read_string(meta)
                dev = parser['device 1']
                self.metadata = meta
                self.samplerate = parse_rate(dev.get('samplerate'))
                self.unitsize = int(dev.get('unitsize', '1'))
                base = dev.get('capturefile', 'logic-1')
                self.probes = {}
                for key, name in dev.items():
                    m = re.fullmatch(r'probe(\d+)', key)
                    if m:
                        self.probes[int(m.group(1)) - 1] = name
                names = [n for n in z.namelist() if n == base or n.startswith(base + '-')]
                names.sort(key=lambda n: int(n.rsplit('-', 1)[1]) if n != base else 0)
                self.data = b''.join(z.read(n) for n in names)
        except (KeyError, zipfile.BadZipFile, configparser.Error) as e:
            raise McpError('LOAD_CAPTURE_FAILED', 'Not a sigrok session file ({}): {}'.format(e, path))
        self.nsamples = len(self.data) // self.unitsize
        # Channel index n is probe "D<n>" (fx2lafw names) or bit position n.
        self.channel_bits = {}
        for bit, name in self.probes.items():
            m = re.fullmatch(r'D(\d+)', name)
            self.channel_bits[int(m.group(1)) if m else bit] = bit

    @property
    def channels(self):
        return sorted(self.channel_bits)

    def probe_name(self, channel):
        return self.probes[self.channel_bits[channel]]

    def duration(self):
        return self.nsamples / self.samplerate

    def runs(self, mask):
        """Yield (sample, value & mask) at the start of every run."""
        if self.unitsize == 1:
            table = bytes(i & mask for i in range(256))
            data = self.data.translate(table)
            for m in re.finditer(rb'(.)\1*', data, re.S):
                yield m.start(), data[m.start()]
            return
        last = None
        step = self.unitsize
        for i in range(self.nsamples):
            v = int.from_bytes(self.data[i * step:(i + 1) * step], 'little') & mask
            if v != last:
                last = v
                yield i, v

    def write_slice(self, path, first, count):
        step = self.unitsize
        with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
            z.writestr('version', '2')
            z.writestr('metadata', self.metadata)
            z.writestr('logic-1-1', self.data[first * step:(first + count) * step])


# --- Analyzers -------------------------------------------------------------------

class SettingDef:
    def __init__(self, title, kind, target, default=None, choices=None, required=False):
        self.title, self.kind, self.target = title, kind, target
        self.default, self.choices, self.required = default, choices or [], required

    def describe(self, channels):
        if self.kind == 'channel':
            return '  "{}" (channel, {}, enabled channels: {})'.format(
                self.title, 'required' if self.required else 'optional', ', '.join(map(str, channels)))
        if self.kind == 'choices':
            return '  "{}" (choices: {})'.format(self.title, ', '.join(
                '"{}"{}'.format(text, ' [default]' if value == self.default else '')
                for text, value in self.choices))
        if self.kind == 'integer':
            return '  "{}" (integer, default: {})'.format(self.title, self.default)
        return '  "{}"'.format(self.title)


def _n_bits(lo, hi):
    return [('{} Bits per Transfer{}'.format(n, ' (Standard)' if n == 8 else ''), n) for n in range(lo, hi + 1)]


# Logic 2 analyzer names and settings, mapped onto sigrok decoders and options.
SALEAE_ANALYZERS = {
    'Async Serial': {
        'decoder': 'uart',
        'classes': ['rx-data', 'tx-data', 'rx-warning', 'tx-warning', 'rx-break', 'tx-break'],
        'settings': [
            SettingDef('Input Channel', 'channel', 'rx', required=True),
            SettingDef('Bit Rate (Bits/s)', 'integer', 'baudrate', 9600),
            SettingDef('Bits per Frame', 'choices', 'data_bits', 8, _n_bits(5, 9)),
            SettingDef('Stop Bits', 'choices', 'stop_bits', 1.0,
                       [('1 Stop Bit (Standard)', 1.0), ('1.5 Stop Bits', 1.5), ('2 Stop Bits', 2.0)]),
            SettingDef('Parity Bit', 'choices', 'parity', 'none',
                       [('No Parity Bit (Standard)', 'none'), ('Even Parity Bit', 'even'),
                        ('Odd Parity Bit', 'odd')]),
            SettingDef('Significant Bit', 'choices', 'bit_order', 'lsb-first',
                       [('Least Significant Bit Sent First (Standard)', 'lsb-first'),
                        ('Most Significant Bit Sent First', 'msb-first')]),
            SettingDef('Signal inversion', 'choices', 'invert_rx', 'no',
                       [('Non Inverted (Standard)', 'no'), ('Inverted', 'yes')]),
        ],
    },
    'I2C': {
        'decoder': 'i2c',
        'classes': ['start', 'repeat-start', 'stop', 'address-read', 'address-write',
                    'data-read', 'data-write', 'warning'],
        'settings': [
            SettingDef('SDA', 'channel', 'sda', required=True),
            SettingDef('SCL', 'channel', 'scl', required=True),
        ],
    },
    'SPI': {
        'decoder': 'spi',
        'classes': ['mosi-data', 'miso-data', 'warning'],
        'settings': [
            SettingDef('MOSI', 'channel', 'mosi'),
            SettingDef('MISO', 'channel', 'miso'),
            SettingDef('Clock', 'channel', 'clk', required=True),
            SettingDef('Enable', 'channel', 'cs'),
            SettingDef('Significant Bit', 'choices', 'bitorder', 'msb-first',
                       [('Most Significant Bit First (Standard)', 'msb-first'),
                        ('Least Significant Bit First', 'lsb-first')]),
            SettingDef('Bits per Transfer', 'choices', 'wordsize', 8, _n_bits(4, 32)),
            SettingDef('Clock State', 'choices', 'cpol', 0,
                       [('Clock is Low when inactive (CPOL = 0)', 0),
                        ('Clock is High when inactive (CPOL = 1)', 1)]),
            SettingDef('Clock Phase', 'choices', 'cpha', 0,
                       [('Data is Valid on Clock Leading Edge (CPHA = 0)', 0),
                        ('Data is Valid on Clock Trailing Edge (CPHA = 1)', 1)]),
            SettingDef('Enable Line', 'choices', 'cs_polarity', 'active-low',
                       [('Enable line is Active Low (Standard)', 'active-low'),
                        ('Enable line is Active High', 'active-high')]),
        ],
    },
    'CAN': {
        'decoder': 'can',
        'classes': ['sof', 'id', 'ext-id', 'full-id', 'rtr', 'dlc', 'data', 'crc-sequence',
                    'ack-slot', 'eof', 'warning'],
        'settings': [
            SettingDef('CAN', 'channel', 'can_rx', required=True),
            SettingDef('Bit Rate (Bits/s)', 'integer', 'nominal_bitrate', 1000000),
        ],
    },
}

HLA_INPUTS = ('uart', 'i2c', 'spi', 'can')
RADIX_FORMATS = {1: 'bin', 2: 'dec', 3: 'hex', 4: 'ascii'}
_ANN = re.compile(r'^(\d+)-(\d+) ([^:]+): ([^:]+): ?(.*)$')


class Analyzer:
    def __init__(self, aid, kind, name, label, specs, decoder, classes=None):
        self.id, self.kind, self.name, self.label = aid, kind, name, label
        self.specs = specs            # sigrok-cli -P stack, bottom first
        self.decoder = decoder        # decoder id of the top of the stack
        self.classes = classes        # annotation classes for tables (None = all)
        self.frames = []              # (ss, es, class, text)
        self.input_id = None


class Capture:
    def __init__(self, cid, path, owned):
        self.id, self.path, self.owned = cid, path, owned
        self.proc = None
        self.mode = 'loaded'
        self.done = threading.Event()
        self.error = None
        self.start_wall = datetime.now(timezone.utc)
        self.limit_samples = None
        self.trim_seconds = 0
        self.analyzers = {}
        self._data = None

    def data(self):
        if self._data is None:
            self._data = SrData(self.path)
        return self._data


# --- Server state and tools ----------------------------------------------------------

class Logicview:
    def __init__(self, sigrok_cli, hla_script, data_dir):
        self.cli = sigrok_cli
        self.hla_script = hla_script
        self.data_dir = data_dir
        self.hla_root = os.path.join(data_dir, 'hla')
        self.capture_dir = os.path.join(data_dir, 'captures')
        os.makedirs(self.capture_dir, exist_ok=True)
        self.lock = threading.Lock()
        self.captures = {}
        self.next_capture = 1
        self.next_analyzer = 1
        self._decoder_info = {}

    # -- helpers --
    def run_cli(self, args, timeout=600):
        env = dict(os.environ)
        decoders = os.path.join(self.hla_root, 'decoders')
        if os.path.isdir(decoders):
            env['SIGROKDECODE_DIR'] = decoders
        try:
            r = subprocess.run([self.cli] + args, capture_output=True, timeout=timeout,
                               env=env, creationflags=NO_WINDOW)
        except FileNotFoundError:
            raise McpError('INTERNAL_ERROR', 'sigrok-cli not found: {}'.format(self.cli))
        return r.returncode, r.stdout.decode('utf-8', 'replace'), r.stderr.decode('utf-8', 'replace')

    def capture(self, cid):
        cap = self.captures.get(int(cid))
        need(cap is not None, 'INVALID_REQUEST', 'Capture Id "{}" does not exist.'.format(cid))
        return cap

    def finished_capture(self, cid):
        cap = self.capture(cid)
        need(cap.done.is_set(), 'INVALID_REQUEST',
             'Capture {} is still running. Call stop_capture or wait_capture first.'.format(cid))
        if cap.error:
            raise cap.error
        return cap

    def scan(self):
        rc, out, err = self.run_cli(['--scan'], timeout=60)
        devices = []
        for line in out.splitlines():
            m = re.match(r'^(\S+?:\S*) - (.*?)(?: \[S/N: [^\]]*\])? with (\d+) channels?: (.*)$', line.strip())
            if m:
                devices.append({'deviceId': m.group(1), 'deviceType': m.group(2),
                                'isSimulation': m.group(1).startswith('demo'),
                                'channels': m.group(4).split()})
        return devices

    def decoder_info(self, decoder):
        if decoder in self._decoder_info:
            return self._decoder_info[decoder]
        rc, out, err = self.run_cli(['-P', decoder, '--show'], timeout=60)
        need('ID: ' + decoder in out, 'INVALID_REQUEST', 'Unknown decoder "{}"'.format(decoder))
        info = {'required': [], 'optional': [], 'options': [], 'inputs': [], 'classes': []}
        section = None
        headers = {'Required channels:': 'required', 'Optional channels:': 'optional',
                   'Options:': 'options', 'Possible decoder input IDs:': 'inputs',
                   'Annotation classes:': 'classes'}
        for line in out.splitlines():
            if line in headers:
                section = headers[line]
            elif not line.startswith('- '):
                section = None if line.endswith(':') else section
            elif section:
                info[section].append(re.match(r'- ([^ :]+)', line).group(1))
        self._decoder_info[decoder] = info
        return info

    def decode(self, cap, specs, top):
        args = ['-i', cap.path, '-P', ','.join(specs), '-A', top,
                '--protocol-decoder-samplenum', '--protocol-decoder-ann-class']
        rc, out, err = self.run_cli(args)
        frames = []
        for line in out.splitlines():
            m = _ANN.match(line)
            if m:
                frames.append((int(m.group(1)), int(m.group(2)), m.group(4), m.group(5)))
        problems = [l for l in err.splitlines() if re.search(r'rror|Traceback|Failed|nvalid', l)]
        need(rc == 0 and not problems, 'INVALID_REQUEST',
             'Decoding failed: ' + ('\n'.join(problems[-8:]) or err.strip()[-800:] or 'exit {}'.format(rc)))
        frames.sort(key=lambda f: (f[0], f[1]))
        return frames

    @staticmethod
    def spec_value(key, value):
        text = ('yes' if value else 'no') if isinstance(value, bool) else (
            '{:g}'.format(value) if isinstance(value, float) else str(value))
        need(':' not in text and ',' not in text and '=' not in text, 'INVALID_REQUEST',
             'Setting "{}": values cannot contain ":", "," or "=" ({!r}).'.format(key, text))
        return text

    # -- tools --
    def get_devices(self, args):
        return {'devices': [{k: d[k] for k in ('deviceId', 'deviceType', 'isSimulation')}
                            for d in self.scan()
                            if args.get('includeSimulationDevices') or not d['isSimulation']]}

    def start_capture(self, args):
        dev_cfg = args.get('logicDeviceConfiguration')
        need(dev_cfg is not None, 'INVALID_REQUEST', 'logicDeviceConfiguration must be provided.')
        devices = self.scan()
        device_id = args.get('deviceId') or ''
        if device_id:
            match = [d for d in devices if d['deviceId'] == device_id]
            need(match, 'MISSING_DEVICE', 'No device with the given id is connected. Connected devices: {}.'.format(
                ', '.join(d['deviceId'] for d in devices) or 'none'))
            device = match[0]
        else:
            physical = [d for d in devices if not d['isSimulation']]
            need(physical, 'MISSING_DEVICE', 'No physical devices found. Is the analyzer plugged in, and '
                 'not opened by SLogicView/PulseView?')
            device = physical[0]

        channels = (dev_cfg.get('logicChannels') or {})
        digital = [int(c) for c in channels.get('digitalChannels') or []]
        need(not channels.get('analogChannels'), 'INVALID_REQUEST',
             'Requested analog channels are unavailable: this device has digital channels only.')
        need(digital, 'INVALID_REQUEST', 'At least 1 digital channel must be enabled')
        names = device['channels']
        bad = [c for c in digital if not 0 <= c < len(names)]
        need(not bad, 'INVALID_REQUEST', 'Requested digital channels are unavailable: {}. Device channels: 0-{}.'.format(
            ', '.join(map(str, bad)), len(names) - 1))
        need(not dev_cfg.get('digitalThresholdVolts'), 'INVALID_REQUEST',
             'Digital threshold "{}" requested, but the requested device does not support a configurable '
             'threshold voltage.'.format(dev_cfg.get('digitalThresholdVolts')))
        need(not dev_cfg.get('glitchFilters'), 'INVALID_REQUEST',
             'Glitch filters are not supported by this device.')
        rate = float(dev_cfg.get('digitalSampleRate') or 0)
        need(rate > 0, 'INVALID_REQUEST', 'digitalSampleRate must be provided (samples/sec).')
        rates = self.samplerates(device['deviceId'])
        need(not rates or rate in rates, 'INVALID_REQUEST',
             'Invalid supplied sample rate. Allowed sample rates: {}'.format(', '.join('{:.0f}'.format(r) for r in rates)))

        cfg = args.get('captureConfiguration') or {}
        config = 'samplerate={}'.format(int(rate))
        extra, mode, limit, trim = [], 'manual', None, 0
        if cfg.get('timedCaptureMode'):
            timed = cfg['timedCaptureMode']
            mode = 'timed'
            seconds = float(timed.get('durationSeconds') or 1.0)
            need(seconds > 0, 'INVALID_REQUEST', 'durationSeconds must be greater than 0')
            limit = max(1, int(round(seconds * rate)))
            trim = float(timed.get('trimDataSeconds') or 0)
        elif cfg.get('digitalCaptureMode'):
            trig = cfg['digitalCaptureMode']
            mode = 'trigger'
            ch = int(trig.get('triggerChannelIndex') or 0)
            need(ch in digital, 'INVALID_REQUEST',
                 'Digital channel "{}" must be enabled to be used as a digital trigger'.format(ch))
            kind = int(trig.get('triggerType') or 0)
            need(kind in (1, 2, 3, 4), 'INVALID_REQUEST', 'Unknown digital trigger type')
            need(kind in (1, 2), 'INVALID_REQUEST',
                 'Pulse triggers (3, 4) are not supported by this device; use 1=rising or 2=falling.')
            triggers = ['{}={}'.format(names[ch], 'r' if kind == 1 else 'f')]
            for linked in trig.get('linkedChannels') or []:
                lc, state = int(linked['channelIndex']), int(linked['state'])
                need(lc in digital, 'INVALID_REQUEST', 'Digital channel "{}" must be enabled to be used as a '
                     'linked channel in the digital trigger'.format(lc))
                need(state in (1, 2), 'INVALID_REQUEST', 'Unknown digital trigger linked channel state')
                triggers.append('{}={}'.format(names[lc], '0' if state == 1 else '1'))
            after = float(trig.get('afterTriggerSeconds') or 0.1)
            # 10 % of the samples come from before the trigger.
            limit = max(10, int(math.ceil(after * rate / 0.9)))
            config += ':captureratio=10'
            extra = ['--triggers', ','.join(triggers)]
            trim = float(trig.get('trimDataSeconds') or 0)
        elif cfg.get('manualCaptureMode'):
            trim = float(cfg['manualCaptureMode'].get('trimDataSeconds') or 0)

        with self.lock:
            cid = self.next_capture
            self.next_capture += 1
        path = os.path.join(self.capture_dir, 'capture-{}.sr'.format(cid))
        if os.path.exists(path):
            os.remove(path)
        cmd = [self.cli, '-d', device['deviceId'], '--config', config,
               '--channels', ','.join(names[c] for c in sorted(set(digital))),
               '--continuous', '-o', path] + extra
        if limit:
            cmd += ['--samples', str(limit)]
        cap = Capture(cid, path, owned=True)
        cap.mode, cap.limit_samples, cap.trim_seconds = mode, limit, trim
        log('capture', cid, ' '.join(cmd[1:]))
        cap.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.PIPE, creationflags=NO_WINDOW)
        with self.lock:
            self.captures[cid] = cap
        threading.Thread(target=self._watch, args=(cap,), daemon=True).start()
        return {'captureId': cid}

    def samplerates(self, device_id):
        rc, out, err = self.run_cli(['-d', device_id, '--show'], timeout=60)
        rates, in_list = [], False
        for line in out.splitlines():
            if 'supported samplerates' in line:
                in_list = True
            elif in_list:
                m = re.match(r'\s+([\d.]+ [kMG]?Hz)', line)
                if not m:
                    break
                rates.append(parse_rate(m.group(1)))
        return rates

    def _watch(self, cap):
        err = cap.proc.stderr.read().decode('utf-8', 'replace')
        rc = cap.proc.wait()
        try:
            ok = rc == 0 and os.path.exists(cap.path) and os.path.getsize(cap.path) > 0
            if not ok:
                lines = [l for l in err.splitlines() if l.strip()]
                cap.error = McpError('DEVICE_ERROR', 'Capture failed (sigrok-cli exit {}): {}'.format(
                    rc, ' / '.join(lines[-4:]) or 'no data written'))
                return
            data = cap.data()
            if data.nsamples == 0:
                cap.error = McpError('DEVICE_ERROR', 'Capture stopped before any data was recorded'
                                     + (' (trigger not reached).' if cap.mode == 'trigger' else '.'))
                return
            if cap.trim_seconds and cap.trim_seconds > 0:
                keep = int(round(cap.trim_seconds * data.samplerate))
                if keep < data.nsamples:
                    tmp = cap.path + '.tmp'
                    data.write_slice(tmp, data.nsamples - keep, keep)
                    os.replace(tmp, cap.path)
                    cap._data = None
        except McpError as e:
            cap.error = e
        except Exception as e:
            cap.error = McpError('INTERNAL_ERROR', 'Capture post-processing failed: {}'.format(e))
        finally:
            try:
                cap.proc.stdin.close()
            except OSError:
                pass
            cap.done.set()

    def stop_capture(self, args):
        cap = self.capture(args['captureId'])
        if not cap.done.is_set() and cap.proc and cap.proc.poll() is None:
            try:
                cap.proc.stdin.write(b'\n')
                cap.proc.stdin.flush()
            except OSError:
                pass
            if not cap.done.wait(20):
                cap.proc.kill()
                cap.done.wait(10)
        cap.done.wait()
        if cap.error:
            raise cap.error
        return {}

    def wait_capture(self, args):
        cap = self.capture(args['captureId'])
        cap.done.wait()
        if cap.error:
            raise cap.error
        return {}

    def load_capture(self, args):
        path = args['filepath']
        need(len(path) > 0, 'INVALID_REQUEST', 'Filepath must not be empty')
        need(not path.lower().endswith('.sal'), 'LOAD_CAPTURE_FAILED',
             'Saleae .sal files cannot be read; logicview uses sigrok session files (.sr).')
        need(os.path.isfile(path), 'LOAD_CAPTURE_FAILED', 'File not found: {}'.format(path))
        SrData(path)
        with self.lock:
            cid = self.next_capture
            self.next_capture += 1
            cap = Capture(cid, os.path.abspath(path), owned=False)
            cap.start_wall = datetime.fromtimestamp(os.path.getmtime(path), timezone.utc)
            cap.done.set()
            self.captures[cid] = cap
        return {'captureId': cid}

    def save_capture(self, args):
        cap = self.finished_capture(args['captureId'])
        path = args['filepath']
        need(len(path) > 0, 'INVALID_REQUEST', 'Filepath must not be empty')
        need(not path.lower().endswith('.sal'), 'INVALID_REQUEST',
             'Captures are saved as sigrok session files; use a .sr file name.')
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        if os.path.abspath(path) != os.path.abspath(cap.path):
            shutil.copyfile(cap.path, path)
        return {}

    def close_capture(self, args):
        cap = self.capture(args['captureId'])
        if not cap.done.is_set() and cap.proc:
            cap.proc.kill()
            cap.done.wait(10)
        with self.lock:
            self.captures.pop(cap.id, None)
        if cap.owned and os.path.exists(cap.path):
            os.remove(cap.path)
        return {}

    def _new_analyzer_id(self):
        with self.lock:
            aid = self.next_analyzer
            self.next_analyzer += 1
        return aid

    def add_analyzer(self, args):
        cap = self.finished_capture(args['captureId'])
        data = cap.data()
        name = args['analyzerName']
        settings = args.get('settings') or {}
        enabled = data.channels

        def channel_name(key, value):
            need(value is not None, 'INVALID_REQUEST',
                 'Invalid value type for analyzer setting "{}", expected number'.format(key))
            ch = int(value)
            need(ch in enabled, 'INVALID_REQUEST',
                 'Invalid value type for analyzer setting "{}", channel "{}" is not enabled'.format(key, ch))
            return data.probe_name(ch)

        if name in SALEAE_ANALYZERS:
            spec = SALEAE_ANALYZERS[name]
            decoder, defs = spec['decoder'], spec['settings']
            values = {d.target: d.default for d in defs if d.kind != 'channel'}
            chans = {}
            describe = 'Settings:\n' + '\n'.join(d.describe(enabled) for d in defs)
            for key, entry in settings.items():
                d = next((d for d in defs if d.title == key), None)
                need(d, 'INVALID_REQUEST', 'Invalid setting name: {}, available settings: {}\n{}'.format(
                    key, ', '.join('"{}"'.format(x.title) for x in defs), describe))
                entry = entry or {}
                if d.kind == 'channel':
                    chans[d.target] = channel_name(key, entry.get('numberValue'))
                elif d.kind == 'integer':
                    need(entry.get('numberValue') is not None, 'INVALID_REQUEST',
                         'Invalid value type for analyzer setting "{}", expected number\n{}'.format(key, describe))
                    values[d.target] = int(entry['numberValue'])
                else:
                    opt = None
                    if entry.get('stringValue') is not None:
                        opt = next((v for t, v in d.choices if t == entry['stringValue'] or str(v) == entry['stringValue']), None)
                    elif entry.get('numberValue') is not None:
                        opt = next((v for t, v in d.choices if v == entry['numberValue']), None)
                    need(opt is not None, 'INVALID_REQUEST', 'Invalid value type for analyzer setting "{}", '
                         'expected one of {}\n{}'.format(key, ', '.join('"{}"'.format(t) for t, _ in d.choices), describe))
                    values[d.target] = opt
            missing = [d.title for d in defs if d.kind == 'channel' and d.required and d.target not in chans]
            need(not missing, 'INVALID_REQUEST', 'Analyzer settings errors: "Please select a channel for {}"\n{}'.format(
                ', '.join(missing), describe))
            parts = ['{}={}'.format(k, v) for k, v in chans.items()]
            parts += ['{}={}'.format(k, self.spec_value(k, v)) for k, v in values.items()]
            classes = spec['classes']
        else:
            info = self.decoder_info(name)
            need('logic' in info['inputs'], 'INVALID_REQUEST',
                 'Analyzer "{}" is not a first-level decoder (inputs: {}). Available Logic 2 analyzers: {}. '
                 'Any sigrok decoder id with logic input also works.'.format(
                     name, ', '.join(info['inputs']), ', '.join(SALEAE_ANALYZERS)))
            decoder, parts = name, []
            chan_ids = info['required'] + info['optional']
            for key, entry in settings.items():
                entry = entry or {}
                if key in chan_ids:
                    parts.append('{}={}'.format(key, channel_name(key, entry.get('numberValue'))))
                else:
                    need(key in info['options'], 'INVALID_REQUEST',
                         'Invalid setting name: {}. Channels: {}. Options: {}'.format(
                             key, ', '.join(chan_ids), ', '.join(info['options'])))
                    value = next((entry[k] for k in ('stringValue', 'numberValue', 'boolValue') if entry.get(k) is not None), None)
                    need(value is not None, 'INVALID_REQUEST', 'Setting "{}" has no value'.format(key))
                    parts.append('{}={}'.format(key, self.spec_value(key, value)))
            missing = [c for c in info['required'] if not any(p.startswith(c + '=') for p in parts)]
            need(not missing, 'INVALID_REQUEST', 'Missing channel setting(s): {}'.format(', '.join(missing)))
            classes = None
        specs = [':'.join([decoder] + parts)]
        aid = self._new_analyzer_id()
        an = Analyzer(aid, 'lla', name, args.get('analyzerLabel') or name, specs, decoder, classes)
        an.frames = self.decode(cap, specs, decoder)
        cap.analyzers[aid] = an
        return {'analyzerId': aid}

    def remove_analyzer(self, args):
        cap = self.capture(args['captureId'])
        aid = int(args['analyzerId'])
        an = cap.analyzers.get(aid)
        need(an is not None and an.kind == 'lla', 'INVALID_REQUEST', 'Analyzer "{}" does not exist.'.format(aid))
        for other in list(cap.analyzers.values()):
            if other.input_id == aid:
                del cap.analyzers[other.id]
        del cap.analyzers[aid]
        return {}

    def add_high_level_analyzer(self, args):
        cap = self.finished_capture(args['captureId'])
        ext_dir = os.path.abspath(args['extensionDirectory'])
        json_path = os.path.join(ext_dir, 'extension.json')
        need(os.path.isfile(json_path), 'INVALID_REQUEST', 'Failed to load extension at {}'.format(json_path))
        with open(json_path, encoding='utf-8-sig') as f:
            ext = json.load(f)
        hlas = {k: v for k, v in (ext.get('extensions') or {}).items() if v.get('type') == 'HighLevelAnalyzer'}
        hla_name = args['hlaName']
        need(hla_name in hlas, 'INVALID_REQUEST', 'HLA "{}" not found in {}. High level analyzers: {}'.format(
            hla_name, json_path, ', '.join('"{}"'.format(k) for k in hlas) or 'none'))
        source = cap.analyzers.get(int(args['inputAnalyzerId']))
        need(source is not None and source.kind == 'lla', 'INVALID_REQUEST',
             'Analyzer Id "{}" does not exist.'.format(args['inputAnalyzerId']))
        need(source.decoder in HLA_INPUTS, 'INVALID_REQUEST',
             'High level analyzers take Async Serial, I2C, SPI or CAN input, not "{}".'.format(source.name))
        decoder = self.install_hla(ext_dir, hla_name, source.decoder)

        info = self.decoder_info(decoder)
        option_ids = [o for o in info['options'] if not o.startswith('hla_')]
        labels = self.hla_labels(decoder)
        parts = []
        for key, entry in (args.get('settings') or {}).items():
            entry = entry or {}
            opt = key if key in option_ids else labels.get(key)
            need(opt, 'INVALID_REQUEST', 'Unknown High Level Analyzer setting(s): "{}".\nSettings: {}'.format(
                key, ', '.join('"{}"'.format(l) for l in labels) or 'none'))
            value = entry.get('numberValue') if entry.get('numberValue') is not None else entry.get('stringValue')
            need(value is not None, 'INVALID_REQUEST',
                 'Encountered unknown High Level Analyzer setting type for key "{}"'.format(key))
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                value = float(value)
            parts.append('{}={}'.format(opt, self.spec_value(key, value)))
        if source.decoder == 'spi' and 'cs_polarity=active-high' in source.specs[0]:
            parts.append('hla_spi_cs_polarity=active-high')
        specs = source.specs + [':'.join([decoder] + parts)]
        frames = self.decode(cap, specs, decoder)
        init_error = next((f for f in frames if f[2] == 'error' and
                           (f[3].startswith('Setting:') or f[3].startswith('HLA init failed'))), None)
        need(init_error is None, 'INVALID_REQUEST', 'High Level Analyzer settings errors:\n  {}'.format(
            init_error[3] if init_error else ''))
        aid = self._new_analyzer_id()
        an = Analyzer(aid, 'hla', hla_name, args.get('hlaLabel') or hla_name, specs, decoder)
        an.input_id = source.id
        an.frames = frames
        cap.analyzers[aid] = an
        return {'analyzerId': aid}

    def install_hla(self, ext_dir, hla_name, protocol):
        need(self.hla_script and os.path.isfile(self.hla_script), 'INTERNAL_ERROR',
             'extensions/hla.ps1 not found ({})'.format(self.hla_script))
        shell = shutil.which('pwsh') or shutil.which('powershell')
        need(shell, 'INTERNAL_ERROR', 'PowerShell is needed to install HLA extensions.')
        r = subprocess.run([shell, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', self.hla_script,
                            'install', ext_dir, '-Protocol', protocol, '-Root', self.hla_root],
                           capture_output=True, timeout=300, creationflags=NO_WINDOW)
        out = (r.stdout + r.stderr).decode('utf-8', 'replace')
        need(r.returncode == 0, 'INVALID_REQUEST', 'Installing the extension failed: {}'.format(out.strip()[-800:]))
        decoders = os.path.join(self.hla_root, 'decoders')
        for name in os.listdir(decoders):
            manifest = os.path.join(decoders, name, 'hla.json')
            if not os.path.isfile(manifest):
                continue
            with open(manifest, encoding='utf-8-sig') as f:
                m = json.load(f)
            if (os.path.normcase(m.get('source', '')) == os.path.normcase(ext_dir) and
                    m.get('hla_name') == hla_name and m.get('input') == protocol):
                self._decoder_info.pop(m['id'], None)
                return m['id']
        raise McpError('INTERNAL_ERROR', 'Installed extension not found under {}'.format(decoders))

    def hla_labels(self, decoder):
        rc, out, err = self.run_cli(['-P', decoder, '--show'], timeout=60)
        labels, in_opts = {}, False
        for line in out.splitlines():
            if line == 'Options:':
                in_opts = True
            elif in_opts and line.startswith('- '):
                m = re.match(r'- ([^:]+): (.*) \(', line)
                if m and not m.group(1).startswith('hla_'):
                    labels[m.group(2)] = m.group(1)
            elif in_opts:
                break
        return labels

    def remove_high_level_analyzer(self, args):
        cap = self.capture(args['captureId'])
        aid = int(args['analyzerId'])
        an = cap.analyzers.get(aid)
        need(an is not None and an.kind == 'hla', 'INVALID_REQUEST',
             'High Level Analyzer "{}" does not exist.'.format(aid))
        del cap.analyzers[aid]
        return {}

    # -- exports --
    def _timefmt(self, cap, iso):
        rate = cap.data().samplerate
        if iso:
            return lambda s: (cap.start_wall + timedelta(seconds=s / rate)).isoformat().replace('+00:00', 'Z')
        return lambda s: '{:.9f}'.format(s / rate)

    def _export_channels(self, cap, args):
        data = cap.data()
        sel = args.get('logicChannels') or {}
        need(not sel.get('analogChannels'), 'INVALID_REQUEST', 'Analog channel(s) {} are not enabled in the capture'.format(
            ', '.join(map(str, sel.get('analogChannels') or []))))
        chans = [int(c) for c in sel.get('digitalChannels') or []] or data.channels
        missing = [c for c in chans if c not in data.channels]
        need(not missing, 'INVALID_REQUEST', 'Digital channel(s) {} are not enabled in the capture'.format(
            ', '.join(map(str, missing))))
        ratio = args.get('analogDownsampleRatio')
        need(ratio is None or 1 <= ratio <= 1000000, 'INVALID_REQUEST',
             'Analog Downsample Rate must be between 1 and 1000000')
        need(args.get('directory'), 'INVALID_REQUEST', 'Export directory is required')
        os.makedirs(args['directory'], exist_ok=True)
        return data, chans

    def export_raw_data_csv(self, args):
        cap = self.finished_capture(args['captureId'])
        data, chans = self._export_channels(cap, args)
        bits = [data.channel_bits[c] for c in chans]
        mask = sum(1 << b for b in bits)
        when = self._timefmt(cap, args.get('iso8601Timestamp'))
        path = os.path.join(args['directory'], 'digital.csv')
        with open(path, 'w', newline='', encoding='utf-8') as f:
            w = csv.writer(f)
            w.writerow(['Time [s]'] + ['Channel {}'.format(c) for c in chans])
            for sample, value in data.runs(mask):
                w.writerow([when(sample)] + [(value >> b) & 1 for b in bits])
        return {}

    def export_raw_data_binary(self, args):
        cap = self.finished_capture(args['captureId'])
        data, chans = self._export_channels(cap, args)
        rate, end = data.samplerate, data.duration()
        for c in chans:
            bit = data.channel_bits[c]
            runs = list(data.runs(1 << bit))
            initial = (runs[0][1] >> bit) & 1 if runs else 0
            times = [s / rate for s, _ in runs[1:]]
            with open(os.path.join(args['directory'], 'digital_{}.bin'.format(c)), 'wb') as f:
                f.write(b'<SALEAE>' + struct.pack('<iiIddQ', 0, 0, initial, 0.0, end, len(times)))
                f.write(struct.pack('<{}d'.format(len(times)), *times))
        return {}

    def _frames(self, cap, an, radix):
        """Frames of an analyzer as table rows (re-decoded for a uart radix)."""
        frames = an.frames
        fmt = RADIX_FORMATS.get(int(radix or 0))
        if fmt and an.kind == 'lla' and an.decoder == 'uart' and fmt != 'hex':
            frames = self.decode(cap, [an.specs[0] + ':format=' + fmt], 'uart')
        rows = []
        for ss, es, cls, text in frames:
            if an.classes is not None and cls not in an.classes:
                continue
            if an.kind == 'hla' and cls.startswith('rt-'):
                cls = cls[3:]
            rows.append((ss, es, an.label, cls, text))
        return rows

    def export_data_table_csv(self, args):
        cap = self.finished_capture(args['captureId'])
        configs = args.get('analyzers') or []
        selected = []
        if configs:
            for c in configs:
                an = cap.analyzers.get(int(c['analyzerId']))
                need(an is not None, 'INVALID_REQUEST', 'Analyzer Id "{}" does not exist.'.format(c['analyzerId']))
                selected.append((an, c.get('radixType') or 4))
        else:
            selected = [(an, 4) for an in cap.analyzers.values()]
            need(selected, 'INVALID_REQUEST',
                 'Exporting the data table requires at least one analyzer already present on the capture.')
        rows = []
        for an, radix in selected:
            rows += self._frames(cap, an, radix)
        rows.sort(key=lambda r: (r[0], r[1]))
        rate = cap.data().samplerate
        when = self._timefmt(cap, args.get('iso8601Timestamp'))
        columns = ['name', 'type', 'start_time', 'duration', 'value']
        wanted = args.get('exportColumns') or columns
        unknown = [c for c in wanted if c not in columns]
        need(not unknown, 'INVALID_REQUEST', 'Unknown column(s) {}. Columns: {}'.format(
            ', '.join(unknown), ', '.join(columns)))
        flt = args.get('filter') or {}
        query = (flt.get('query') or '').lower()
        search = flt.get('columns') or columns
        path = args['filepath']
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        with open(path, 'w', newline='', encoding='utf-8') as f:
            w = csv.writer(f)
            w.writerow(wanted)
            for ss, es, label, cls, text in rows:
                record = {'name': label, 'type': cls, 'start_time': when(ss),
                          'duration': '{:.9f}'.format((es - ss) / rate), 'value': text}
                if query and not any(query in str(record.get(c, '')).lower() for c in search):
                    continue
                w.writerow([record[c] for c in wanted])
        return {}

    def legacy_export_analyzer(self, args):
        cap = self.finished_capture(args['captureId'])
        an = cap.analyzers.get(int(args['analyzerId']))
        need(an is not None, 'INVALID_REQUEST', 'Analyzer Id "{}" does not exist.'.format(args['analyzerId']))
        need(args['filepath'], 'INVALID_REQUEST', 'Filepath cannot be empty.')
        need(int(args['radixType']) in RADIX_FORMATS, 'INVALID_REQUEST', 'Unknown radix type')
        rate = cap.data().samplerate
        with open(args['filepath'], 'w', newline='', encoding='utf-8') as f:
            w = csv.writer(f)
            w.writerow(['Time [s]', 'Type', 'Value'])
            for ss, es, label, cls, text in self._frames(cap, an, args['radixType']):
                w.writerow(['{:.9f}'.format(ss / rate), cls, text])
        return {}

    def read_analyzer_frames(self, args):
        cap = self.finished_capture(args['captureId'])
        an = cap.analyzers.get(int(args['analyzerId']))
        need(an is not None, 'INVALID_REQUEST', 'Analyzer Id "{}" does not exist.'.format(args['analyzerId']))
        rows = self._frames(cap, an, 0)
        offset = int(args.get('offset') or 0)
        limit = int(args.get('limit') or 200)
        rate = cap.data().samplerate
        page = rows[offset:offset + limit]
        return {'total': len(rows), 'offset': offset, 'frames': [
            {'type': cls, 'start_time': ss / rate, 'end_time': es / rate, 'value': text}
            for ss, es, label, cls, text in page]}


# --- Tool table and input schemas (mirroring Logic 2's) ------------------------------

def _num(desc):
    return {'description': desc, 'type': 'number'}


_CHANNELS = {'type': 'object', 'properties': {
    'digitalChannels': {'description': 'Digital channel indices (e.g. [0, 1, 2])', 'type': 'array',
                        'items': {'type': 'number'}},
    'analogChannels': {'description': 'Analog channel indices (not available on this device)', 'type': 'array',
                       'items': {'type': 'number'}}}}
_SETTING_VALUE = {'type': 'object', 'properties': {
    'stringValue': {'type': 'string'}, 'numberValue': {'type': 'number'}, 'boolValue': {'type': 'boolean'}}}

DEFINITIONS = {
    'GlitchFilter': {'type': 'object', 'properties': {'channelIndex': _num('Digital channel index'),
                                                      'pulseWidthSeconds': _num('Minimum pulse width in seconds')},
                     'required': ['channelIndex', 'pulseWidthSeconds']},
    'LinkedChannel': {'type': 'object', 'properties': {'channelIndex': _num('Digital channel index'),
                                                       'state': _num('Required state: 1=low, 2=high')},
                      'required': ['channelIndex', 'state']},
    'AnalyzerSettingValue': _SETTING_VALUE,
    'AnalyzerSettings': {'type': 'object', 'additionalProperties': {'$ref': '#/definitions/AnalyzerSettingValue'}},
    'AnalyzerExportConfig': {'type': 'object', 'properties': {'analyzerId': {'type': 'number'},
                                                              'radixType': {'type': 'number'}},
                             'required': ['analyzerId']},
}
CAPTURE_ID = _num('Capture ID')
TOOLS = [
    ('get_devices', 'List connected logic analyzers (sigrok scan)', {
        'includeSimulationDevices': {'description': 'Include simulation devices in the response', 'type': 'boolean'}}, []),
    ('start_capture', 'Start a new capture. Digital channels only; samplerates as listed in errors', {
        'deviceId': {'description': 'Device id from get_devices (e.g. "fx2lafw:conn=2.1"). If omitted, '
                                    'the first device is used', 'type': 'string'},
        'logicDeviceConfiguration': {'description': 'Hardware configuration', 'type': 'object', 'properties': {
            'logicChannels': dict(_CHANNELS, description='Channels to enable'),
            'digitalSampleRate': _num('Digital sample rate in samples/sec (e.g. 24000000)'),
            'analogSampleRate': _num('Not supported'),
            'digitalThresholdVolts': _num('Not supported (fixed threshold)'),
            'glitchFilters': {'description': 'Not supported', 'type': 'array',
                              'items': {'$ref': '#/definitions/GlitchFilter'}}}},
        'captureConfiguration': {'description': 'Capture mode: one of manualCaptureMode, timedCaptureMode, '
                                                'digitalCaptureMode. Default: manual', 'type': 'object', 'properties': {
            'bufferSizeMegabytes': _num('Ignored'),
            'manualCaptureMode': {'description': 'Capture until stop_capture', 'type': 'object', 'properties': {
                'trimDataSeconds': _num('Keep only the last N seconds of data')}},
            'timedCaptureMode': {'description': 'Capture for a fixed duration', 'type': 'object', 'properties': {
                'durationSeconds': _num('Capture duration in seconds'),
                'trimDataSeconds': _num('Keep only the last N seconds of data')}},
            'digitalCaptureMode': {'description': 'Capture on a digital edge trigger (10 % pre-trigger data)',
                                   'type': 'object', 'properties': {
                'triggerType': _num('1=rising, 2=falling (3/4 pulse triggers are not supported)'),
                'afterTriggerSeconds': _num('Seconds to capture after the trigger'),
                'trimDataSeconds': _num('Keep only the last N seconds of data'),
                'triggerChannelIndex': _num('Digital channel index to trigger on'),
                'minPulseWidthSeconds': _num('Not supported'),
                'maxPulseWidthSeconds': _num('Not supported'),
                'linkedChannels': {'description': 'Other channel levels required at the trigger', 'type': 'array',
                                   'items': {'$ref': '#/definitions/LinkedChannel'}}}}}}}, []),
    ('stop_capture', 'Stop an active capture', {'captureId': _num('Capture ID to stop')}, ['captureId']),
    ('wait_capture', 'Wait for a capture to complete. Blocks until the capture finishes; a manual capture '
                     'only finishes through stop_capture', {'captureId': _num('Capture ID to wait for')}, ['captureId']),
    ('load_capture', 'Load a capture from a sigrok session file (.sr, as saved by SLogicView/PulseView '
                     'or save_capture)', {'filepath': {'description': 'Absolute path to the .sr file',
                                                       'type': 'string'}}, ['filepath']),
    ('save_capture', 'Save a capture to a sigrok session file (.sr)', {
        'captureId': _num('Capture ID to save'),
        'filepath': {'description': 'Absolute path of the .sr file', 'type': 'string'}}, ['captureId', 'filepath']),
    ('close_capture', 'Close a capture and free resources', {'captureId': _num('Capture ID to close')}, ['captureId']),
    ('add_analyzer', 'Add a protocol analyzer to a capture and decode it', {
        'captureId': _num('Capture ID to add analyzer to'),
        'analyzerName': {'description': 'Logic 2 analyzer name ("Async Serial", "I2C", "SPI", "CAN") or any '
                                        'sigrok decoder id with logic input (e.g. "onewire_link", "ir_nec")',
                         'type': 'string'},
        'analyzerLabel': {'description': 'User-facing label for the analyzer', 'type': 'string'},
        'settings': {'description': 'Settings keyed by setting name (Logic 2 names; for sigrok decoders the '
                                    'channel/option ids). Channels take numberValue = channel index',
                     '$ref': '#/definitions/AnalyzerSettings'}}, ['analyzerName', 'captureId']),
    ('remove_analyzer', 'Remove a protocol analyzer from a capture', {
        'captureId': CAPTURE_ID, 'analyzerId': _num('Analyzer ID to remove')}, ['analyzerId', 'captureId']),
    ('add_high_level_analyzer', 'Add a Saleae Logic 2 high-level analyzer extension (HLA) on top of an '
                                'Async Serial, I2C, SPI or CAN analyzer', {
        'captureId': CAPTURE_ID,
        'extensionDirectory': {'description': 'Path to the extension directory (with extension.json)',
                               'type': 'string'},
        'hlaName': {'description': 'Name of the HLA as listed in extension.json', 'type': 'string'},
        'hlaLabel': {'description': 'User-facing label for the HLA', 'type': 'string'},
        'inputAnalyzerId': _num('Analyzer ID to use as input'),
        'settings': {'description': 'HLA settings keyed by setting label or attribute name',
                     '$ref': '#/definitions/AnalyzerSettings'}},
     ['captureId', 'extensionDirectory', 'hlaName', 'inputAnalyzerId']),
    ('remove_high_level_analyzer', 'Remove a high-level analyzer from a capture', {
        'captureId': CAPTURE_ID, 'analyzerId': _num('HLA ID to remove')}, ['analyzerId', 'captureId']),
    ('export_raw_data_csv', 'Export raw digital data as CSV (digital.csv: one row per transition)', {
        'captureId': CAPTURE_ID,
        'directory': {'description': 'Directory to create exported CSV files in', 'type': 'string'},
        'logicChannels': dict(_CHANNELS, description='Channels to export. If omitted, all are exported.'),
        'analogDownsampleRatio': _num('Downsample ratio (1-1000000), kept for Logic 2 compatibility'),
        'iso8601Timestamp': {'description': 'Use ISO8601 timestamps', 'type': 'boolean'}},
     ['analogDownsampleRatio', 'captureId', 'directory']),
    ('export_raw_data_binary', 'Export raw digital data in the Logic 2 binary format (digital_N.bin)', {
        'captureId': CAPTURE_ID,
        'directory': {'description': 'Directory to create exported binary files in', 'type': 'string'},
        'logicChannels': dict(_CHANNELS, description='Channels to export. If omitted, all are exported.'),
        'analogDownsampleRatio': _num('Downsample ratio (1-1000000), kept for Logic 2 compatibility')},
     ['analogDownsampleRatio', 'captureId', 'directory']),
    ('export_data_table_csv', 'Export analyzer results as a CSV data table (name, type, start_time, '
                              'duration, value)', {
        'captureId': CAPTURE_ID,
        'filepath': {'description': 'Path to export the CSV to', 'type': 'string'},
        'analyzers': {'description': 'Analyzers to include. If omitted, all are included', 'type': 'array',
                      'items': {'$ref': '#/definitions/AnalyzerExportConfig'}},
        'iso8601Timestamp': {'description': 'Use ISO8601 timestamps', 'type': 'boolean'},
        'exportColumns': {'description': 'Columns to export. If omitted, all are exported', 'type': 'array',
                          'items': {'type': 'string'}},
        'filter': {'description': 'Keep rows containing the query text (case-insensitive)', 'type': 'object',
                   'properties': {'query': {'type': 'string'},
                                  'columns': {'type': 'array', 'items': {'type': 'string'}}},
                   'required': ['query']}}, ['captureId', 'filepath']),
    ('legacy_export_analyzer', 'Export one analyzer as CSV (Time [s], Type, Value)', {
        'captureId': CAPTURE_ID,
        'filepath': {'description': 'Path to export data to', 'type': 'string'},
        'analyzerId': _num('Analyzer ID to export'),
        'radixType': _num('Radix type (1=binary, 2=decimal, 3=hex, 4=ascii); applies to Async Serial')},
     ['analyzerId', 'captureId', 'filepath', 'radixType']),
    ('read_analyzer_frames', 'logicview addition: return decoded frames of an analyzer or HLA directly', {
        'captureId': CAPTURE_ID, 'analyzerId': _num('Analyzer or HLA ID'),
        'offset': _num('First frame (default 0)'), 'limit': _num('Number of frames (default 200)')},
     ['analyzerId', 'captureId']),
]


def tool_list():
    tools = []
    for name, desc, props, required in TOOLS:
        schema = {'type': 'object', 'properties': props, 'definitions': DEFINITIONS}
        if required:
            schema['required'] = required
        tools.append({'name': name, 'description': desc, 'inputSchema': schema})
    return tools


# --- Argument validation (subset of JSON Schema, with Logic 2's coercions) -------------

def _resolve(schema):
    ref = schema.get('$ref')
    return DEFINITIONS[ref.rsplit('/', 1)[1]] if ref else schema


def validate(value, schema, path='.'):
    schema = _resolve(schema)
    kind = schema.get('type')
    if kind in ('object', 'array') and isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            pass
    if kind == 'number':
        if isinstance(value, str):
            try:
                value = float(value)
            except ValueError:
                raise ValueError('{}: should be number'.format(path))
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError('{}: should be number'.format(path))
        return int(value) if float(value).is_integer() else value
    if kind == 'string':
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            return str(value)
        if not isinstance(value, str):
            raise ValueError('{}: should be string'.format(path))
        return value
    if kind == 'boolean':
        if isinstance(value, str) and value.lower() in ('true', 'false'):
            return value.lower() == 'true'
        if not isinstance(value, bool):
            raise ValueError('{}: should be boolean'.format(path))
        return value
    if kind == 'array':
        if not isinstance(value, list):
            raise ValueError('{}: should be array'.format(path))
        return [validate(v, schema.get('items', {}), '{}[{}]'.format(path, i)) for i, v in enumerate(value)]
    if kind == 'object':
        if not isinstance(value, dict):
            raise ValueError('{}: should be object'.format(path))
        for key in schema.get('required', []):
            if key not in value:
                raise ValueError("{}: should have required property '{}'".format(path, key))
        props = schema.get('properties', {})
        extra = schema.get('additionalProperties')
        out = {}
        for key, v in value.items():
            if key in props:
                out[key] = validate(v, props[key], '{}.{}'.format(path.rstrip('.'), key))
            elif isinstance(extra, dict):
                out[key] = validate(v, extra, '{}.{}'.format(path.rstrip('.'), key))
            else:
                out[key] = v
        return out
    return value


# --- JSON-RPC -----------------------------------------------------------------------------

class Protocol:
    def __init__(self, server):
        self.server = server
        self.tools = {t[0]: t for t in TOOLS}

    def handle(self, req):
        if not isinstance(req, dict) or req.get('jsonrpc') != '2.0' or not isinstance(req.get('method'), str):
            rid = req.get('id') if isinstance(req, dict) else None
            return {'jsonrpc': '2.0', 'id': rid, 'error': {'code': -32600, 'message': 'Invalid Request'}}
        method, rid = req['method'], req.get('id')
        params = req.get('params') or {}
        if method == 'initialize':
            return {'jsonrpc': '2.0', 'id': rid, 'result': {
                'protocolVersion': PROTOCOL_VERSION, 'capabilities': {'tools': {}},
                'serverInfo': {'name': SERVER_NAME, 'version': SERVER_VERSION}}}
        if method == 'ping':
            return {'jsonrpc': '2.0', 'id': rid, 'result': {}}
        if method == 'tools/list':
            return {'jsonrpc': '2.0', 'id': rid, 'result': {'tools': tool_list()}}
        if method == 'tools/call':
            name = params.get('name')
            if not name:
                return {'jsonrpc': '2.0', 'id': rid, 'error': {'code': -32602, 'message': 'Missing tool name'}}
            if name not in self.tools:
                return {'jsonrpc': '2.0', 'id': rid, 'error': {'code': -32601, 'message': 'Unknown tool: ' + name}}
            _, _, props, required = self.tools[name]
            try:
                args = validate(params.get('arguments') or {},
                                {'type': 'object', 'properties': props, 'required': required})
            except ValueError as e:
                return {'jsonrpc': '2.0', 'id': rid, 'error': {'code': -32602, 'message': str(e)}}
            try:
                result = getattr(self.server, name)(args)
                return {'jsonrpc': '2.0', 'id': rid, 'result': {
                    'content': [{'type': 'text', 'text': json.dumps(result)}],
                    'structuredContent': result, 'isError': False}}
            except McpError as e:
                text = '[{}] {}'.format(e.code, e.message)
            except Exception as e:
                log('tool', name, 'failed:', repr(e))
                text = '{}: {}'.format(type(e).__name__, e)
            return {'jsonrpc': '2.0', 'id': rid, 'result': {
                'content': [{'type': 'text', 'text': text}], 'isError': True}}
        if rid is None:
            return None
        return {'jsonrpc': '2.0', 'id': rid, 'error': {'code': -32601, 'message': 'Unknown method: ' + method}}

    def handle_text(self, body):
        try:
            req = json.loads(body)
        except ValueError:
            return json.dumps({'jsonrpc': '2.0', 'id': None, 'error': {'code': -32700, 'message': 'Parse error'}})
        resp = self.handle(req)
        return None if resp is None else json.dumps(resp)


def serve_http(protocol, host, port):
    class Handler(BaseHTTPRequestHandler):
        def _cors(self):
            self.send_header('Access-Control-Allow-Origin', '*')
            self.send_header('Access-Control-Allow-Methods', 'POST, OPTIONS')
            self.send_header('Access-Control-Allow-Headers', 'Content-Type')

        def do_OPTIONS(self):
            self.send_response(204)
            self._cors()
            self.end_headers()

        def do_GET(self):
            self.send_response(405)
            self._cors()
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"error":"Method not allowed"}')

        do_DELETE = do_GET

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get('Content-Length') or 0)).decode('utf-8', 'replace')
            reply = protocol.handle_text(body)
            if reply is None:
                self.send_response(204)
                self._cors()
                self.end_headers()
                return
            data = reply.encode('utf-8')
            self.send_response(200)
            self._cors()
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, fmt, *a):
            pass

    httpd = ThreadingHTTPServer((host, port), Handler)
    httpd.daemon_threads = True
    log('MCP HTTP server listening on http://{}:{}'.format(host, port))
    httpd.serve_forever()


def serve_stdio(protocol):
    log('MCP stdio server ready')
    out_lock = threading.Lock()

    def answer(line):
        reply = protocol.handle_text(line)
        if reply is not None:
            with out_lock:
                sys.stdout.write(reply + '\n')
                sys.stdout.flush()

    for line in sys.stdin:
        if line.strip():
            # Tools such as wait_capture block: answer each request on its own thread.
            threading.Thread(target=answer, args=(line,), daemon=True).start()


def find_sigrok_cli(explicit):
    candidates = [explicit, os.environ.get('LOGICVIEW_SIGROK_CLI'),
                  os.path.join(HERE, '..', 'sigrok-cli.exe'), os.path.join(HERE, 'sigrok-cli.exe'),
                  shutil.which('sigrok-cli')]
    for c in candidates:
        if c and os.path.isfile(c):
            return os.path.abspath(c)
    return explicit or 'sigrok-cli'


def main(argv=None):
    p = argparse.ArgumentParser(description='logicview MCP server (Saleae Logic 2 MCP tools on sigrok-cli)')
    p.add_argument('--host', default='127.0.0.1')
    p.add_argument('--port', type=int, default=10530)
    p.add_argument('--stdio', action='store_true', help='speak MCP over stdin/stdout instead of HTTP')
    p.add_argument('--sigrok-cli', help='path to sigrok-cli.exe (default: the app folder next to mcp/)')
    p.add_argument('--hla-script', default=os.path.join(HERE, '..', 'extensions', 'hla.ps1'))
    p.add_argument('--data-dir', default=os.path.join(
        os.environ.get('LOCALAPPDATA') or tempfile.gettempdir(), 'logicview', 'mcp'))
    a = p.parse_args(argv)
    server = Logicview(find_sigrok_cli(a.sigrok_cli), os.path.abspath(a.hla_script), a.data_dir)
    log('sigrok-cli:', server.cli)
    protocol = Protocol(server)
    if a.stdio:
        serve_stdio(protocol)
    else:
        serve_http(protocol, a.host, a.port)


if __name__ == '__main__':
    main()
