"""Write a synthetic 8-channel capture (sigrok .sr) with UART, I2C, SPI and CAN
traffic, plus a SLogicView/PulseView setup file (.pvs) that stacks the test
HLAs on the matching sigrok decoders. All decoder options stay at defaults.

  D0 UART 115200 8N1    "Hi\n", 1 ms gap, "ok!\n"
  D1/D2 I2C 100 kHz     write 0x50 [10 20], repeated start, read 0x50 [AB] NACK
  D3-D6 SPI mode 0      CLK, MOSI [9F 00 00], MISO [00 EF 40], CS#
  D7 CAN 1 Mbit/s       0x123 [11 22 33], extended 0x1ABCDEF [55]

Usage: python -I make-capture.py <out.sr> <out.pvs>
"""
import sys
import zipfile

RATE = 10_000_000
N = 46_000
events = {ch: [(0, 1)] for ch in range(8)}   # every line idles high


def ev(ch, t, v):
    events[ch].append((int(round(t)), v))


def uart(ch, t, data, baud=115200):
    bit = RATE / baud
    for byte in data:
        for i, b in enumerate([0] + [(byte >> k) & 1 for k in range(8)] + [1]):
            ev(ch, t + i * bit, b)
        t += 11 * bit
    return t


def i2c(t, ops):
    scl, sda = 1, 2
    for op in ops:
        if op in ('start', 'restart'):
            if op == 'restart':
                ev(sda, t, 1); ev(scl, t + 25, 1); t += 50
            ev(sda, t, 0); ev(scl, t + 50, 0); t += 75
        elif op == 'stop':
            ev(sda, t, 0); ev(scl, t + 25, 1); ev(sda, t + 50, 1); t += 100
        else:
            byte, ack = op
            for b in [(byte >> k) & 1 for k in range(7, -1, -1)] + [0 if ack else 1]:
                ev(sda, t, b); ev(scl, t + 25, 1); ev(scl, t + 75, 0); t += 100
    return t


def spi(t, mosi, miso):
    clk, d_mosi, d_miso, cs = 3, 4, 5, 6
    ev(clk, 0, 0)
    ev(cs, t, 0); t += 20
    for mo, mi in zip(mosi, miso):
        for k in range(7, -1, -1):
            ev(d_mosi, t, (mo >> k) & 1); ev(d_miso, t, (mi >> k) & 1)
            ev(clk, t + 5, 1); ev(clk, t + 10, 0); t += 10
        t += 10
    ev(cs, t + 10, 1)
    return t + 30


def bits(value, width):
    return [(value >> i) & 1 for i in range(width - 1, -1, -1)]


def crc15(seq):
    crc = 0
    for b in seq:
        top = ((crc >> 14) & 1) ^ b
        crc = (crc << 1) & 0x7fff
        if top:
            crc ^= 0x4599
    return crc


def can(t, ident, payload, extended=False, bitrate=1_000_000):
    if extended:
        head = [0] + bits(ident >> 18, 11) + [1, 1] + bits(ident & 0x3ffff, 18) + [0, 0, 0]
    else:
        head = [0] + bits(ident, 11) + [0, 0, 0]
    body = head + bits(len(payload), 4) + [b for byte in payload for b in bits(byte, 8)]
    body += bits(crc15(body), 15)
    stuffed, run, last = [], 0, None
    for b in body:
        stuffed.append(b)
        run = run + 1 if b == last else 1
        last = b
        if run == 5:
            stuffed.append(1 - b)
            last, run = 1 - b, 1
    frame = stuffed + [1, 0, 1] + [1] * 7 + [1] * 3   # CRC del, ACK, ACK del, EOF, IFS
    step = RATE / bitrate
    for i, b in enumerate(frame):
        ev(7, t + i * step, b)
    return t + len(frame) * step


t = uart(0, 1000, b'Hi\n')
uart(0, t + 10_000, b'ok!\n')
i2c(25_000, ['start', (0x50 << 1, True), (0x10, True), (0x20, True),
             'restart', ((0x50 << 1) | 1, True), (0xAB, False), 'stop'])
spi(35_000, [0x9F, 0x00, 0x00], [0x00, 0xEF, 0x40])
t = can(40_000, 0x123, [0x11, 0x22, 0x33])
can(t + 200, 0x1ABCDEF, [0x55], extended=True)

samples = bytearray(N)
for ch, evs in events.items():
    evs.sort(key=lambda e: e[0])
    for (start, v), nxt in zip(evs, evs[1:] + [(N, None)]):
        if v:
            for i in range(start, min(nxt[0], N)):
                samples[i] |= 1 << ch

sr_path, pvs_path = sys.argv[1], sys.argv[2]
metadata = ('[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\n'
            'total probes=8\nsamplerate=10 MHz\ntotal analog=0\n' +
            ''.join('probe{}=D{}\n'.format(i + 1, i) for i in range(8)) + 'unitsize=1\n')
with zipfile.ZipFile(sr_path, 'w', zipfile.ZIP_DEFLATED) as z:
    z.writestr('version', '2')
    z.writestr('metadata', metadata)
    z.writestr('logic-1-1', bytes(samples))

stacks = [
    ('Line Reader', ['uart', 'hla_line_reader_uart'], {0: 'D0'}),
    ('Echo Errors', ['uart', 'hla_echo_errors_uart'], {0: 'D0'}),
    ('I2C Transactions', ['i2c', 'hla_i2c_transactions_i2c'], {0: 'D1', 1: 'D2'}),
    ('SPI Burst', ['spi', 'hla_spi_burst_spi'], {0: 'D3', 1: 'D5', 2: 'D4', 3: 'D6'}),
    ('CAN Messages', ['can', 'hla_can_messages_can'], {0: 'D7'}),
]
channel_counts = {'uart': 2, 'i2c': 2, 'spi': 4, 'can': 1}
lines = ['[General]', 'decode_signals={}'.format(len(stacks)), 'generated_signals=0',
         'views=0', 'meta_objs=0', '']
for n, (name, ids, assign) in enumerate(stacks):
    lines.append('[decode_signal{}]'.format(n))
    lines.append('name={}'.format(name))
    lines.append('decoders={}'.format(len(ids)))
    for i, did in enumerate(ids):
        lines += ['decoder{}\\id={}'.format(i, did), 'decoder{}\\visible=true'.format(i),
                  'decoder{}\\options=0'.format(i)]
    count = channel_counts[ids[0]]
    lines.append('channels={}'.format(count))
    for c in range(count):
        lines.append('channel{}\\initial_pin_state=2'.format(c))
        if c in assign:
            lines.append('channel{}\\assigned_signal_name={}'.format(c, assign[c]))
    lines.append('')
with open(pvs_path, 'w', encoding='utf-8') as f:
    f.write('\n'.join(lines))
print('wrote', sr_path, pvs_path)
