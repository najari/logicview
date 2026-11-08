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
    'hla_i2c_transactions_i2c', 'hla_legacy_api_uart', 'hla_line_reader_uart',
    'hla_spi_burst_spi'], installed)

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

print()
print('FAILED: ' + ', '.join(FAILED) if FAILED else 'all tests passed')
sys.exit(1 if FAILED else 0)
