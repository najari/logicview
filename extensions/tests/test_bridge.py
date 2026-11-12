"""Unit tests for the HLA bridge, run against decoders installed by hla.ps1
into a scratch libsigrokdecode root, with a stand-in sigrokdecode module.

Usage: python -I test_bridge.py <root>   (run-tests.ps1 does the install)
"""
import importlib
import os
import sys
import types

ROOT = os.path.abspath(sys.argv[1])
RATE = 1_000_000

# --- Stand-in for libsigrokdecode's embedded sigrokdecode module ------------
srd = types.ModuleType('sigrokdecode')
srd.OUTPUT_ANN, srd.OUTPUT_PYTHON, srd.SRD_CONF_SAMPLERATE = 0, 1, 10000


class _Decoder:
    def register(self, output_type, **kwargs):
        return output_type

    def put(self, ss, es, output, data):
        self.out.append((ss, es, output, data))


srd.Decoder = _Decoder
sys.modules['sigrokdecode'] = srd
sys.path.insert(0, os.path.join(ROOT, 'decoders'))

FAILED = []


def check(name, condition, detail=''):
    print(('ok   ' if condition else 'FAIL ') + name + (': ' + str(detail) if detail and not condition else ''))
    if not condition:
        FAILED.append(name)


def run(decoder_id, packets, **options):
    mod = importlib.import_module(decoder_id)
    cls = mod.Decoder
    d = cls()
    d.out = []
    d.options = {o['id']: o['default'] for o in cls.options}
    d.options.update(options)
    d.metadata(srd.SRD_CONF_SAMPLERATE, RATE)
    d.start()
    for ss, es, data in packets:
        d.decode(ss, es, data)
    anns = [(ss, es, cls.annotations[a[0]][0], a[1]) for ss, es, o, a in d.out if o == 0]
    return mod, cls, anns


# --- API surface --------------------------------------------------------------
# Loading a decoder package puts saleae_compat on sys.path, as in the app.
importlib.import_module('hla_line_reader_uart')
from saleae.data import GraphTimeDelta, GraphTime  # noqa: E402  (shim, from saleae_compat)
import saleae_bridge  # noqa: E402

check('crc15 check value', saleae_bridge._can_crc15(
    [b for ch in b'123456789' for b in saleae_bridge._bits(ch, 8)]) == 0x059E)
check('delta arithmetic', float(GraphTimeDelta(millisecond=1, microsecond=500)) == 0.0015)
check('delta tolerance', GraphTimeDelta(1e-13) == GraphTimeDelta(0))

# --- Async Serial: "Hi\n" then "ok\n" with a 3 ms gap -------------------------
def uart_char(t, ch, rxtx=0, bit=10):
    bits = [((ch >> i) & 1, t + bit * (i + 1), t + bit * (i + 2)) for i in range(8)]
    return [(t, t + bit, ['STARTBIT', rxtx, 0]),
            (t + bit, t + 9 * bit, ['DATA', rxtx, (ch, bits)]),
            (t + 9 * bit, t + 10 * bit, ['STOPBIT', rxtx, 1]),
            (t, t + 10 * bit, ['FRAME', rxtx, (ch, True)])]


pk, t = [], 0
for ch in b'Hi\n':
    pk += uart_char(t, ch); t += 100
t += 3000
for ch in b'ok\n':
    pk += uart_char(t, ch); t += 100
pk.append((t, t + 10, ['INVALID STARTBIT', 0, 1]))
pk.append((t, t + 10, ['FRAME', 0, (0, False)]))

mod, cls, anns = run('hla_line_reader_uart', pk, prefix='> ', gap_ms=1.0)
texts = [(c, a[0]) for _, _, c, a in anns]
check('uart lines', texts == [('rt-line', '> Hi'), ('rt-gap', 'gap 3 ms'), ('rt-line', '> ok')], texts)
check('uart line span', anns[0][:2] == (0, 300), anns[0][:2])
check('uart options', [o['id'] for o in cls.options] == ['prefix', 'gap_ms', 'line_end', 'hla_uart_line'],
      [o['id'] for o in cls.options])
check('choices values', cls.options[2]['values'] == ('LF', 'CR'))
check('doc', 'Line Reader' in mod.__doc__ and 'Input: uart' in mod.__doc__, mod.__doc__)
_, _, anns = run('hla_line_reader_uart', pk, gap_ms=5000.0)
check('setting validation', anns and anns[0][2] == 'error' and 'greater than 1000' in anns[0][3][0], anns[:1])
_, _, anns = run('hla_line_reader_uart', [(s, e, [d[0], 1, d[2]]) for s, e, d in pk], hla_uart_line='RX')
check('uart line filter', anns == [], anns)

# --- Echo (no result_types, exceptions, sibling import, skipped measurement) --
pk = uart_char(0, ord('a')) + uart_char(100, ord('!')) + uart_char(200, ord('b'))
_, _, anns = run('hla_echo_errors_uart', pk)
got = [(c, a[0]) for _, _, c, a in anns]
check('other frames + error', got == [('other', 'echo: char=A'), ('error', 'ValueError: exclamation mark'),
                                      ('other', 'echo: char=B')], got)
check('measurement skipped', not os.path.isdir(os.path.join(ROOT, 'decoders', 'hla_echo_errors_speed_uart')))

# --- Result type added in __init__, dict-keys choices, tiny minimum -----------
mod, cls, anns = run('hla_dynamic_types_uart', uart_char(0, ord('a')), prefix='# ')
opts = {o['id']: o for o in cls.options}
check('dynamic options', opts['timeout']['default'] == 1e-6 and
      opts['mode']['values'] == ('Upper [A]', 'Lower [a]'), opts)
check('dynamic result type', [(c, a[0]) for _, _, c, a in anns] == [('rt-word', '# A')], anns)
check('dynamic class order', [a[0] for a in cls.annotations] == ['rt-error', 'rt-word', 'other', 'error'],
      cls.annotations)

# --- Pre-1.0 API is refused with a clear message -------------------------------
try:
    importlib.import_module('hla_legacy_api_uart')
    check('legacy refused', False, 'loaded')
except TypeError as e:
    check('legacy refused', 'pre-1.0' in str(e), e)

# --- Protocol detection reads comments and descriptions, not the word "can" ---
installed = sorted(os.listdir(os.path.join(ROOT, 'decoders')))
check('detection', [d for d in installed if d.startswith('hla_')] == [
    'hla_can_messages_can', 'hla_dynamic_types_uart', 'hla_echo_errors_uart',
    'hla_frame_echo_i2s', 'hla_frame_echo_lin', 'hla_frame_echo_manchester',
    'hla_frame_echo_onewire', 'hla_frame_echo_parallel',
    'hla_i2c_transactions_i2c', 'hla_legacy_api_uart', 'hla_line_reader_uart',
    'hla_spi_burst_spi'], installed)
check('manchester decoder installed', 'manchester' in installed, installed)

# --- I2C: write 50 [10 20], repeated start, read 50 [AB] NACK, stop ------------
seq = [('START', None), ('ADDRESS WRITE', 0x50), ('ACK', None), ('DATA WRITE', 0x10), ('ACK', None),
       ('DATA WRITE', 0x20), ('ACK', None), ('START REPEAT', None), ('ADDRESS READ', 0x50), ('ACK', None),
       ('DATA READ', 0xAB), ('NACK', None), ('STOP', None)]
pk = [(i * 10, i * 10 + 8, list(c)) for i, c in enumerate(seq)]
_, _, anns = run('hla_i2c_transactions_i2c', pk)
got = [(s, e, a[0]) for s, e, _, a in anns]
check('i2c transactions', got == [(0, 70, 'W 0x50: 10 20'), (70, 128, 'R 0x50: ab')], got)

# --- SPI: CS low, 3 words, CS high --------------------------------------------
pk = [(0, 0, ['CS-CHANGE', None, 1]), (5, 5, ['CS-CHANGE', 1, 0])]
for i, (mo, mi) in enumerate([(0x9F, 0x00), (0x00, 0xEF), (0x00, 0x40)]):
    s = 10 + i * 20
    pk += [(s, s + 16, ['BITS', [[1, 0, 0]] * 8, [[0, 0, 0]] * 8]), (s, s + 16, ['DATA', mo, mi])]
pk += [(80, 80, ['CS-CHANGE', 0, 1])]
_, _, anns = run('hla_spi_burst_spi', pk)
got = [(s, e, a[0]) for s, e, _, a in anns]
check('spi burst', got == [(5, 80, 'MOSI 0x9F 0x00 0x00 / MISO 0x00 0xEF 0x40 (3 words)')], got)

# --- CAN: standard 0x123 [11 22 33], extended, remote, filter -----------------
pk = [(0, 680, ('standard', 0x123, 'data', 3, [0x11, 0x22, 0x33])),
      (1000, 1900, ('extended', 0x1ABCDEF, 'data', 1, [0x55])),
      (2000, 2440, ('standard', 0x7FF, 'remote', 2, []))]
_, _, anns = run('hla_can_messages_can', pk)
got = [a[0] for _, _, _, a in anns]
check('can messages', len(got) == 3 and got[0].startswith('ID 0x123 [3] 11 22 33 CRC ')
      and got[1].startswith('ID 0x1ABCDEF [1] 55') and got[2].startswith('ID 0x7FF [2]  CRC'), got)
# SOF..ACK delimiter = 60 of the 67 bits counted for a 3-byte standard frame.
check('can span', anns[0][:2] == (0, round(60 * 680 / 67)), anns[0][:2])
_, _, anns = run('hla_can_messages_can', pk, id_filter='7ff')
check('can filter', [a[0][:8] for _, _, _, a in anns] == ['ID 0x7FF'], anns)


def echoed(decoder_id, packets, **options):
    """(start, end, text) of each frame the Frame Echo HLA returns."""
    _, cls, anns = run(decoder_id, packets, **options)
    return cls, [(s, e, a[0]) for s, e, _, a in anns]


# --- LIN on uart: break (with its 0x00 byte), sync, PID 0x12, 2 data, checksum
def lin_byte(t, value, valid=True, rxtx=0):
    return (t, t + 10, ['FRAME', rxtx, (value, valid)])


pk = [lin_byte(0, 0, valid=False), (0, 15, ['BREAK', 0, 0]),
      lin_byte(20, 0x55), lin_byte(30, 0x92), lin_byte(45, 0x01), lin_byte(55, 0x02),
      lin_byte(65, 0x6A), (75, 85, ['IDLE', 0, 0]), (85, 95, ['IDLE', 0, 0])]
cls, got = echoed('hla_frame_echo_lin', pk)
check('lin frames', got == [
    (0, 15, 'header_break'), (20, 30, 'header_sync'), (30, 40, 'header_pid: protected_id=18'),
    (45, 55, 'data: data=1, index=0'), (55, 65, 'data: data=2, index=1'),
    (65, 75, 'checksum: checksum=106')], got)
check('lin stacks on uart', cls.inputs == ['uart'], cls.inputs)
# Idle before the response (response space) does not end the transaction;
# eight data bytes and a checksum end it without idle.
pk = [(0, 15, ['BREAK', 0, 0]), lin_byte(20, 0x55), lin_byte(30, 0x3C),
      (40, 50, ['IDLE', 0, 0]), (50, 60, ['IDLE', 0, 0]), (60, 70, ['IDLE', 0, 0])]
pk += [lin_byte(100 + 10 * i, i) for i in range(9)]
_, got = echoed('hla_frame_echo_lin', pk)
check('lin response space, 8 bytes', [t for _, _, t in got][3:] ==
      ['data: data={}, index={}'.format(i, i) for i in range(8)] + ['checksum: checksum=8'], got)
_, got = echoed('hla_frame_echo_lin', [(s, e, [d[0], 1, d[2]]) for s, e, d in pk], hla_lin_line='TX')
check('lin TX line', len(got) == 12, got)


# --- 1-Wire on onewire_link: match ROM 28 AABBCCDDEEFF crc 0x99, data 0x44; then search
def ow_bits(t, value, n, step=10):
    return [(t + i * step, t + i * step + 8, ['BIT', (value >> i) & 1]) for i in range(n)]


rom = 0x99 << 56 | 0xFFEEDDCCBBAA << 8 | 0x28
pk = [(0, 100, ['RESET/PRESENCE', True])] + ow_bits(200, 0x55, 8) + ow_bits(300, rom, 64) + ow_bits(1000, 0x44, 8)
cls, got = echoed('hla_frame_echo_onewire', pk)
check('onewire match rom', [t for _, _, t in got] == [
    'reset', 'presence', 'rom_command: rom_command=0x55, description=match', 'family_code: family=0x28',
    'id: id={}'.format(0xFFEEDDCCBBAA), 'crc: crc=0x99', 'data: data=0x44'], got)
check('onewire spans', got[2][:2] == (200, 278) and got[3][:2] == (300, 378) and got[5][:2] == (860, 938), got)
check('onewire stacks on onewire_link', cls.inputs == ['onewire_link'], cls.inputs)
search = []
for i in range(64):
    b = (rom >> i) & 1
    search += [(2000 + 30 * i, 2008 + 30 * i, ['BIT', b]), (2010 + 30 * i, 2018 + 30 * i, ['BIT', 1 - b]),
               (2020 + 30 * i, 2028 + 30 * i, ['BIT', b])]
pk = [(0, 100, ['RESET/PRESENCE', False])] + ow_bits(200, 0xEC, 8) + search
_, got = echoed('hla_frame_echo_onewire', pk)
check('onewire alarm search', [t for _, _, t in got] == [
    'reset', 'alarm: rom_command=0xEC', 'family_code: family=0x28',
    'id: id={}'.format(0xFFEEDDCCBBAA), 'crc: crc=0x99'], got)
_, got = echoed('hla_frame_echo_onewire', [(0, 100, ['RESET/PRESENCE', True])] + ow_bits(200, 0x12, 8))
check('onewire invalid command', got[-1][2] == 'invalid_rom_command: rom_command=0x12', got)

# --- I2S, Simple Parallel, Manchester words ------------------------------------
pk = [(0, 32, ['DATA', ['L', 0xFFFF]]), (32, 64, ['DATA', ['R', 0x0001]])]
_, got = echoed('hla_frame_echo_i2s', pk, hla_i2s_format='signed-16')
check('i2s signed', [t for _, _, t in got] == ['data: channel=0, data=-1', 'data: channel=1, data=1'], got)
_, got = echoed('hla_frame_echo_i2s', pk)
check('i2s unsigned', got[0][2] == 'data: channel=0, data=65535', got)
_, got = echoed('hla_frame_echo_parallel', [(0, 5, ['ITEM', [0xA5, 8]]), (0, 5, ['WORD', [0xA5, 8, 1]])])
check('parallel items', got == [(0, 5, 'data: data=165')], got)
cls, got = echoed('hla_frame_echo_manchester', [(0, 80, ['WORD', 0x3C, 8])])
check('manchester words', got == [(0, 80, 'data: data=60')] and cls.inputs == ['manchester'], got)

# --- The manchester decoder: encode bits, decode the edges back ---------------
man = importlib.import_module('manchester.pd')


def encode(bits, mode, rising_is_one=True, idle=0, half=10, t0=100):
    cells, prev = [], idle
    for b in bits:
        if mode == 'manchester':
            h0 = 0 if b == rising_is_one else 1
            h1 = 1 - h0
        elif mode == 'differential':
            h0 = 1 - prev if b == 0 else prev
            h1 = 1 - h0
        else:
            h0 = 1 - prev
            mid = b == 1 if mode == 'bi-phase mark' else b == 0
            h1 = 1 - h0 if mid else h0
        cells += [h0, h1]
        prev = h1
    levels = cells + [idle]
    edges, level = [], idle
    for i, v in enumerate(levels):
        if v != level:
            edges.append((t0 + i * half, v))
            level = v
    return edges


def decode_bits(edges, mode, rising_is_one=True, half=10):
    cells = man.align(man.cells_from_edges(edges, half), edges, half, mode)
    return man.bits_from_cells(cells, half, mode, rising_is_one, 1 - edges[0][1])


patterns = [[1, 0, 1, 1, 0, 0, 1, 0], [0, 1, 1, 0, 1, 0, 0, 1, 1, 1]]
for mode in man.MODES:
    for idle in (0, 1):
        for bits in patterns:
            for rising in (True, False):
                got = [b for _, _, b in decode_bits(encode(bits, mode, rising, idle), mode, rising)]
                # Bi-phase: the edge back to idle can add one bit at the end.
                ok = got[:len(bits)] == bits and len(got) - len(bits) in (
                    (0, 1) if mode.startswith('bi-phase') else (0,))
                check('manchester {} idle {} {}{}'.format(mode, idle, ''.join(map(str, bits)),
                                                         '' if rising else ' falling'), ok, got)
                if mode != 'manchester':
                    break
# Without a full-bit interval the first edge is taken as mid-bit (Manchester
# codes) or a bit boundary (bi-phase): right for these, ambiguous in general.
for mode, idle, bits in (('manchester', 0, [1] * 8), ('bi-phase mark', 0, [1] * 8),
                         ('bi-phase space', 1, [0] * 8)):
    got = [b for _, _, b in decode_bits(encode(bits, mode, True, idle), mode)]
    check('manchester no full bit {} {}'.format(mode, ''.join(map(str, bits))), got == bits, got)
spans = decode_bits(encode([1, 0, 1, 1], 'manchester'), 'manchester')
check('manchester bit spans', [s[:2] for s in spans] == [(100, 120), (120, 140), (140, 160), (160, 180)], spans)

print()
print('FAILED: ' + ', '.join(FAILED) if FAILED else 'all tests passed')
sys.exit(1 if FAILED else 0)
