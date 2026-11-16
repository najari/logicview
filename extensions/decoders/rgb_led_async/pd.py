##
## Addressable LED decoder for logicview (see __init__.py).
##
## This program is free software; you can redistribute it and/or modify
## it under the terms of the GNU General Public License as published by
## the Free Software Foundation; either version 3 of the License, or
## (at your option) any later version.
##

import sigrokdecode as srd

ns = 1e-9
us = 1e-6


class SamplerateError(Exception):
    pass


class Controller:
    """Timings as (min, nominal, max) seconds. bits[v] is the
    ((high), (low)) timing of a bit of value v."""

    def __init__(self, bits_per_channel, reset, low_speed, high_speed, grb):
        self.bits_per_channel = bits_per_channel
        self.reset = reset
        self.speeds = [low_speed] + ([high_speed] if high_speed else [])
        self.grb = grb


# From Saleae's async-rgb-led-analyzer (AsyncRgbLedAnalyzerSettings.cpp).
CONTROLLERS = {
    'WS2811': Controller(
        8, 50 * us,
        (((350 * ns, 500 * ns, 650 * ns), (1850 * ns, 2000 * ns, 2150 * ns)),
         ((1050 * ns, 1200 * ns, 1350 * ns), (1150 * ns, 1300 * ns, 1450 * ns))),
        (((175 * ns, 250 * ns, 325 * ns), (925 * ns, 1000 * ns, 1075 * ns)),
         ((525 * ns, 600 * ns, 675 * ns), (1225 * ns, 1300 * ns, 1375 * ns))),
        False),
    'WS2812B': Controller(
        8, 50 * us,
        (((200 * ns, 400 * ns, 550 * ns), (700 * ns, 850 * ns, 1050 * ns)),
         ((650 * ns, 800 * ns, 1050 * ns), (200 * ns, 450 * ns, 600 * ns))),
        None, True),
    'WS2813': Controller(
        8, 50 * us,
        (((300 * ns, 375 * ns, 450 * ns), (300 * ns, 875 * ns, 100 * us)),
         ((750 * ns, 875 * ns, 1000 * ns), (300 * ns, 375 * ns, 100 * us))),
        None, True),
    'TM1809': Controller(
        8, 24 * us,
        (((450 * ns, 600 * ns, 750 * ns), (1050 * ns, 1200 * ns, 1350 * ns)),
         ((1050 * ns, 1200 * ns, 1350 * ns), (450 * ns, 600 * ns, 750 * ns))),
        (((250 * ns, 320 * ns, 390 * ns), (530 * ns, 600 * ns, 670 * ns)),
         ((530 * ns, 600 * ns, 670 * ns), (250 * ns, 320 * ns, 390 * ns))),
        False),
    'TM1804': Controller(
        8, 10 * us,
        (((850 * ns, 1 * us, 1150 * ns), (1850 * ns, 2 * us, 2150 * ns)),
         ((1850 * ns, 2 * us, 2150 * ns), (850 * ns, 1 * us, 1150 * ns))),
        None, False),
    'UCS1903': Controller(
        8, 24 * us,
        (((350 * ns, 500 * ns, 650 * ns), (1850 * ns, 2000 * ns, 2150 * ns)),
         ((1850 * ns, 2000 * ns, 2150 * ns), (350 * ns, 500 * ns, 650 * ns))),
        (((175 * ns, 250 * ns, 325 * ns), (925 * ns, 1000 * ns, 1075 * ns)),
         ((925 * ns, 1000 * ns, 1075 * ns), (175 * ns, 250 * ns, 325 * ns))),
        False),
    'LPD1886-24': Controller(
        8, 24 * us,
        (((150 * ns, 200 * ns, 280 * ns), (500 * ns, 600 * ns, 10 * us)),
         ((450 * ns, 600 * ns, 9 * us), (150 * ns, 200 * ns, 10 * us))),
        None, False),
    'LPD1886-36': Controller(
        12, 24 * us,
        (((150 * ns, 200 * ns, 280 * ns), (500 * ns, 600 * ns, 10 * us)),
         ((450 * ns, 600 * ns, 9 * us), (150 * ns, 200 * ns, 10 * us))),
        None, False),
}


def within(t, timing):
    return timing[0] <= t <= timing[2]


def first_bit(c, high, low):
    """(speed index, bit) of the first bit after a reset, from its high and
    low times; None when no timing fits."""
    for speed, bits in enumerate(c.speeds):
        for value in (0, 1):
            if within(high, bits[value][0]) and within(low, bits[value][1]):
                return speed, value
    return None


def next_bit(c, speed, high, low):
    """Bit value from the high time (0 first), checked against the low time
    unless the bit ends in a reset (low is None); None when invalid."""
    bits = c.speeds[speed]
    for value in (0, 1):
        if within(high, bits[value][0]):
            if low is None or within(low, bits[value][1]):
                return value
            return None
    return None


def pixel(c, values):
    """(red, green, blue) from the three channel values in wire order."""
    if c.grb:
        return values[1], values[0], values[2]
    return values[0], values[1], values[2]


class Decoder(srd.Decoder):
    api_version = 3
    id = 'rgb_led_async'
    name = 'Addressable LEDs'
    longname = 'Addressable LEDs (async, one wire)'
    desc = 'WS2811/WS2812B/WS2813/TM1809/TM1804/UCS1903/LPD1886 pixels.'
    license = 'gplv3+'
    inputs = ['logic']
    outputs = ['rgb_led_async']
    tags = ['Display']
    channels = (
        {'id': 'din', 'name': 'DIN', 'desc': 'LED data line'},
    )
    options = (
        {'id': 'controller', 'desc': 'LED controller', 'default': 'WS2811',
         'values': tuple(CONTROLLERS)},
    )
    annotations = (
        ('bit', 'Bit'),
        ('pixel', 'Pixel'),
        ('reset', 'Reset'),
        ('warning', 'Warning'),
    )
    annotation_rows = (
        ('bits', 'Bits', (0,)),
        ('pixels', 'Pixels', (1, 2)),
        ('warnings', 'Warnings', (3,)),
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

    def warn(self, ss, es, text):
        self.put(ss, es, self.out_ann, [3, [text, 'Warning', 'W']])

    def sync(self, reset_samples):
        """Wait for a low period longer than the reset time; return at the
        rising edge after it."""
        self.wait({0: 'l'})
        while True:
            start = self.samplenum
            self.wait([{0: 'r'}, {'skip': reset_samples + 1}])
            if not self.matched[0]:
                self.wait({0: 'r'})
                self.put(start, self.samplenum, self.out_ann, [2, ['Reset', 'R']])
                return
            self.wait({0: 'f'})

    def decode(self):
        if not self.samplerate:
            raise SamplerateError('Cannot decode without samplerate.')
        c = CONTROLLERS.get(self.options['controller'], CONTROLLERS['WS2811'])
        rate = float(self.samplerate)
        reset_samples = max(1, int(c.reset * rate))
        self.sync(reset_samples)
        while True:
            # A packet: from here (a rising edge) until a reset or an error.
            index, first, speed = 0, True, 0
            values, bits = [], []
            while True:
                rise = self.samplenum
                self.wait({0: 'f'})
                fall = self.samplenum
                high = (fall - rise) / rate
                self.wait([{0: 'r'}, {'skip': reset_samples}])
                is_reset = not self.matched[0]
                low = None if is_reset else (self.samplenum - fall) / rate
                if first:
                    found = None if is_reset else first_bit(c, high, low)
                    if found is None:
                        self.warn(rise, fall, 'No valid first bit after the reset'
                                  if not is_reset else 'No complete bit between resets')
                        bit = None
                    else:
                        speed, bit = found
                        first = False
                else:
                    bit = next_bit(c, speed, high, low)
                    if bit is None:
                        self.warn(rise, self.samplenum, 'Bit timing out of range')
                if bit is None:
                    break
                if is_reset:
                    end = fall + int(c.speeds[speed][bit][1][1] * rate)
                else:
                    end = self.samplenum - 1
                self.put(rise, end, self.out_ann, [0, [str(bit)]])
                bits.append((rise, end, bit))
                if len(bits) == c.bits_per_channel:
                    value = 0
                    for b in bits:
                        value = (value << 1) | b[2]
                    values.append((bits[0][0], bits[-1][1], value))
                    bits = []
                    if len(values) == 3:
                        r, g, b = pixel(c, [v[2] for v in values])
                        ss, es = values[0][0], values[-1][1]
                        digits = 3 if c.bits_per_channel > 8 else 2
                        self.put(ss, es, self.out_ann, [1, [
                            '#{} R:{} G:{} B:{}'.format(index, r, g, b),
                            '#{:0{d}X}{:0{d}X}{:0{d}X}'.format(r, g, b, d=digits),
                            '#{}'.format(index)]])
                        self.put(ss, es, self.out_python, ['PIXEL', index, r, g, b])
                        index += 1
                        values = []
                if is_reset:
                    if values or bits:
                        self.warn(rise, end, 'Incomplete pixel before the reset')
                    self.put(end, self.samplenum, self.out_ann, [2, ['Reset', 'R']])
                    self.wait({0: 'r'})
                    break
            if bit is None:
                self.sync(reset_samples)
