"""Write a synthetic 8-channel capture (sigrok .sr) for the inputs added after
the first four: LIN, 1-Wire, Manchester, I2S and Simple Parallel.

  D0 LIN 19200 baud     break, sync, PID 0x92 (id 0x12), data 01 02, checksum 6A
  D1 1-Wire             reset, presence, skip ROM (0xCC), data 0x44
  D2 Manchester         IEEE 802.3, 100 kbit/s, MSB first: A5 3C
  D3/D4/D5 I2S          SCK 1 MHz, WS, SD: 16-bit L FFFF, R 0001, L 8000, R 7FFF
  D6/D7 parallel        CLK, D0: items 1 0 1 1

Usage: python -I make-capture-more.py <out.sr>
"""
import sys
import zipfile

RATE = 10_000_000
N = 50_000
events = {ch: [(0, 1)] for ch in range(8)}   # lines idle high ...
for ch in (2, 3, 4, 5, 6, 7):                 # ... except these, idle low
    events[ch] = [(0, 0)]


def ev(ch, t, v):
    events[ch].append((int(round(t)), v))


def uart_bits(ch, t, bits, baud):
    bit = RATE / baud
    for i, b in enumerate(bits):
        ev(ch, t + i * bit, b)
    return t + len(bits) * bit


def lin(t, pid, data, baud=19200):
    t = uart_bits(0, t, [0] * 13 + [1], baud)          # break, delimiter
    for byte in [0x55, pid] + data + [lin_checksum(pid, data)]:
        t = uart_bits(0, t, [0] + [(byte >> k) & 1 for k in range(8)] + [1], baud)
    return t


def lin_checksum(pid, data):
    s = pid
    for d in data:
        s += d
        s = (s & 0xff) + (s >> 8)
    return ~s & 0xff


US = RATE / 1_000_000


def onewire(t, data_bytes):
    ch = 1
    ev(ch, t, 0); t += 500 * US; ev(ch, t, 1)          # reset pulse
    ev(ch, t + 30 * US, 0); ev(ch, t + 150 * US, 1)    # presence
    t += 500 * US
    for byte in data_bytes:
        for k in range(8):
            low = 5 if (byte >> k) & 1 else 65
            ev(ch, t, 0); ev(ch, t + low * US, 1)
            t += 72 * US
    return t


def manchester(t, data, bitrate=100_000):
    half = RATE / bitrate / 2
    level, cells = 0, []
    for byte in data:
        for k in range(7, -1, -1):
            cells += [0, 1] if (byte >> k) & 1 else [1, 0]   # IEEE: rising = 1
    for c in cells + [0]:
        if c != level:
            ev(2, t, c)
            level = c
        t += half
    return t


def i2s(t, words, bits=16, period=10):
    # Standard I2S: WS changes one SCK before the MSB of the next word; data
    # and WS change on the falling edge, are sampled on the rising edge.
    stream = []
    for chan, value in words:
        stream += [(chan, (value >> k) & 1) for k in range(bits - 1, -1, -1)]
    for i, (chan, bit) in enumerate(stream):
        nxt = stream[i + 1][0] if i + 1 < len(stream) else 1 - chan
        ev(4, t, nxt); ev(5, t, bit)
        ev(3, t + period / 2, 1); ev(3, t + period, 0)
        t += period
    return t


def parallel(t, items, period=20):
    for b in items:
        ev(7, t, b)
        ev(6, t + period / 2, 1); ev(6, t + period, 0)
        t += period
    return t


lin(1000, 0x92, [0x01, 0x02])
onewire(1000, [0xCC, 0x44])
manchester(1000, [0xA5, 0x3C])
i2s(1000, [(1, 0x1234), (0, 0xFFFF), (1, 0x0001), (0, 0x8000), (1, 0x7FFF), (0, 0)])
parallel(1000, [1, 0, 1, 1])

samples = bytearray(N)
for ch, evs in events.items():
    evs.sort(key=lambda e: e[0])
    for (start, v), nxt in zip(evs, evs[1:] + [(N, None)]):
        if v:
            for i in range(start, min(nxt[0], N)):
                samples[i] |= 1 << ch

metadata = ('[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\n'
            'total probes=8\nsamplerate=10 MHz\ntotal analog=0\n' +
            ''.join('probe{}=D{}\n'.format(i + 1, i) for i in range(8)) + 'unitsize=1\n')
with zipfile.ZipFile(sys.argv[1], 'w', zipfile.ZIP_DEFLATED) as z:
    z.writestr('version', '2')
    z.writestr('metadata', metadata)
    z.writestr('logic-1-1', bytes(samples))
print('wrote', sys.argv[1])
