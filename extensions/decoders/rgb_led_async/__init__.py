##
## Addressable LED decoder for logicview (installed by extensions/hla.ps1 so
## that Saleae HLAs written for the Logic 2 "Addressable LEDs (Async)"
## analyzer can run on it).
##
## This program is free software; you can redistribute it and/or modify
## it under the terms of the GNU General Public License as published by
## the Free Software Foundation; either version 3 of the License, or
## (at your option) any later version.
##

'''
One-wire addressable LEDs (WS2811, WS2812B, WS2813, TM1809, TM1804, UCS1903,
LPD1886 in 24- or 36-bit mode), decoded the way the Logic 2 "Addressable
LEDs (Async)" analyzer does it, with its timing table:

- A low period longer than the controller's reset time starts a new packet;
  pixels are counted from 0 after each reset.
- The first bit after a reset picks the bit value and the speed (low speed
  first, then high speed where the controller has one) from its high and low
  times together; later bits are taken from the high time (0 checked first)
  and their low time must fit too.
- A bit outside the timings is reported and decoding waits for the next
  reset.
- Three channels of 8 bits (12 for LPD1886 36-bit), MSB first; WS2812B and
  WS2813 send green first (GRB), the others red first (RGB).

Python output: ['PIXEL', <index>, <red>, <green>, <blue>] for each pixel.
'''

from .pd import Decoder
