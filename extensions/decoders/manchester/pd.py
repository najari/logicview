##
## Manchester decoder for logicview (see __init__.py).
##
## This program is free software; you can redistribute it and/or modify
## it under the terms of the GNU General Public License as published by
## the Free Software Foundation; either version 3 of the License, or
## (at your option) any later version.
##

import sigrokdecode as srd


class SamplerateError(Exception):
    pass


MODES = ('manchester', 'differential', 'bi-phase mark', 'bi-phase space')


def cells_from_edges(edges, half):
    """Half-bit cells [(start sample, level, kind)] of a burst, from edges
    (sample, level after the edge). kind is 'long' for the first cell of a
    full-bit interval and 'pad' for the two cells after the last edge (the
    line then holds its level until idle)."""
    cells = []
    for (s, level), (t, _) in zip(edges, edges[1:]):
        n = min(2, max(1, int(round((t - s) / half))))
        cells += [(s + k * (t - s) / n, level, 'long' if n == 2 and k == 0 else '')
                  for k in range(n)]
    s, level = edges[-1]
    cells += [(s, level, 'pad'), (s + half, level, 'pad')]
    return cells


def align(cells, edges, half, mode):
    """Put the missing first half in front when the burst starts in the
    middle of a bit, so that cells[0] starts a bit."""
    first_long = next((i for i, c in enumerate(cells) if c[2] == 'long'), None)
    if first_long is None:
        # No full-bit interval: Manchester codes start on a mid-bit edge,
        # bi-phase codes on a bit boundary.
        starts_mid = not mode.startswith('bi-phase')
    elif mode.startswith('bi-phase'):
        # A full-bit interval is a whole bit (no mid-bit transition).
        starts_mid = first_long % 2 == 1
    else:
        # A full-bit interval spans a bit boundary: its second cell starts a bit.
        starts_mid = (first_long + 1) % 2 == 1
    if starts_mid:
        s, level = edges[0]
        cells.insert(0, (s - half, 1 - level, ''))
    return cells


def bits_from_cells(cells, half, mode, rising_is_one, idle_level):
    """[(start, end, bit or None)] for each complete bit; None = no mid-bit
    transition where the code needs one. In Manchester codes a bit made only
    of the cells after the last edge is the line returning to idle and is
    left out. In bi-phase codes it may be a last bit without a mid-bit
    transition, so it is kept (a lone extra bit is dropped with the
    incomplete word)."""
    bits = []
    prev = idle_level
    for i in range(0, len(cells) - 1, 2):
        (start, h0, k0), (_, h1, k1) = cells[i], cells[i + 1]
        if k0 == 'pad' and k1 == 'pad' and not mode.startswith('bi-phase'):
            break
        if mode == 'manchester':
            bit = None if h0 == h1 else int((h1 > h0) == rising_is_one)
        elif mode == 'differential':
            bit = None if h0 == h1 else int(prev == h0)
        elif mode == 'bi-phase mark':
            bit = int(h0 != h1)
        else:
            bit = int(h0 == h1)
        bits.append((int(start), int(start + 2 * half), bit))
        prev = h1
    return bits


class Decoder(srd.Decoder):
    api_version = 3
    id = 'manchester'
    name = 'Manchester'
    longname = 'Manchester and bi-phase line codes'
    desc = 'Manchester, differential Manchester and bi-phase mark/space words.'
    license = 'gplv3+'
    inputs = ['logic']
    outputs = ['manchester']
    tags = ['Encoding']
    channels = (
        {'id': 'data', 'name': 'DATA', 'desc': 'Data line'},
    )
    options = (
        {'id': 'bitrate', 'desc': 'Bit rate (bit/s)', 'default': 9600},
        {'id': 'mode', 'desc': 'Line code', 'default': 'manchester',
         'values': MODES},
        {'id': 'one_edge', 'desc': 'Mid-bit edge of a 1 (Manchester)',
         'default': 'rising', 'values': ('rising', 'falling')},
        {'id': 'word_bits', 'desc': 'Bits per word', 'default': 8},
        {'id': 'bit_order', 'desc': 'Bit order', 'default': 'msb-first',
         'values': ('msb-first', 'lsb-first')},
        {'id': 'skip_bits', 'desc': 'Leading bits to skip in each burst',
         'default': 0},
    )
    annotations = (
        ('bit', 'Bit'),
        ('word', 'Word'),
        ('warning', 'Warning'),
    )
    annotation_rows = (
        ('bits', 'Bits', (0,)),
        ('words', 'Words', (1,)),
        ('warnings', 'Warnings', (2,)),
    )

    def __init__(self):
        self.reset()

    def reset(self):
        self.samplerate = None

    def metadata(self, key, value):
        if key == srd.SRD_CONF_SAMPLERATE:
            self.samplerate = value

    def start(self):
        self.out_ann = self.register(srd.OUTPUT_ANN)
        self.out_python = self.register(srd.OUTPUT_PYTHON)

    def burst(self, edges, half):
        mode = self.options['mode']
        cells = align(cells_from_edges(edges, half), edges, half, mode)
        bits = bits_from_cells(cells, half, mode,
                               self.options['one_edge'] == 'rising',
                               1 - edges[0][1])
        bits = bits[max(0, int(self.options['skip_bits'])):]
        nbits = max(1, int(self.options['word_bits']))
        msb = self.options['bit_order'] == 'msb-first'
        word = []
        for b in bits:
            if b[2] is None:
                self.put(b[0], b[1], self.out_ann,
                         [2, ['No mid-bit transition', 'Error', 'E']])
                word = []
                continue
            self.put(b[0], b[1], self.out_ann, [0, [str(b[2])]])
            word.append(b)
            if len(word) == nbits:
                order = word if msb else word[::-1]
                value = 0
                for w in order:
                    value = (value << 1) | w[2]
                digits = (nbits + 3) // 4
                self.put(word[0][0], word[-1][1], self.out_ann, [1, [
                    'Word: 0x{:0{}X}'.format(value, digits),
                    '0x{:0{}X}'.format(value, digits)]])
                self.put(word[0][0], word[-1][1], self.out_python,
                         ['WORD', value, nbits])
                word = []
        # Fewer bits than a word at the end are usually the line returning
        # to idle; only a partial word of more than one bit is reported.
        if len(word) > 1:
            self.put(word[0][0], word[-1][1], self.out_ann, [2, [
                'Incomplete word ({} bits)'.format(len(word)), 'Incomplete']])

    def decode(self):
        if not self.samplerate:
            raise SamplerateError('Cannot decode without samplerate.')
        half = self.samplerate / float(self.options['bitrate']) / 2.0
        if half < 1:
            raise SamplerateError('Sample rate too low for this bit rate.')
        # More than 1.25 bit times without an edge ends a burst.
        timeout = int(half * 2.5) + 1
        edges = []
        while True:
            try:
                if edges:
                    pins = self.wait([{0: 'e'}, {'skip': timeout}])
                    if not self.matched[0]:
                        self.burst(edges, half)
                        edges = []
                        continue
                else:
                    pins = self.wait({0: 'e'})
            except EOFError:
                if edges:
                    self.burst(edges, half)
                raise
            edges.append((self.samplenum, pins[0]))
