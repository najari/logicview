##
## Manchester decoder for logicview (installed by extensions/hla.ps1 so that
## Saleae HLAs written for the Logic 2 Manchester analyzer can run on it).
##
## This program is free software; you can redistribute it and/or modify
## it under the terms of the GNU General Public License as published by
## the Free Software Foundation; either version 3 of the License, or
## (at your option) any later version.
##

'''
Manchester (IEEE 802.3 or G.E. Thomas), differential Manchester and
bi-phase mark / space (FM1 / FM0) line codes at a known bit rate.

Edges are measured in half-bit cells. A burst ends when the line holds its
level for more than 1.25 bit times; the bits of a burst are grouped into
words of the given size, after skipping the given number of leading bits.
The bit cell boundaries are taken from the first interval of a full bit time
in the burst; without one, the first edge is taken as a mid-bit transition
(Manchester codes) or a bit boundary (bi-phase codes).

Python output: ['WORD', <value>, <bits>] for each word.
'''

from .pd import Decoder
