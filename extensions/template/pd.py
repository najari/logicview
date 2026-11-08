# Installed by logicview extensions/hla.ps1: runs the Saleae Logic 2 HLA
# extension in ./ext as a libsigrokdecode protocol decoder (see hla.json).
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
# <libsigrokdecode data dir>/decoders/<this decoder> -> <data dir>/saleae_compat
_compat = os.path.join(os.path.dirname(os.path.dirname(_here)), 'saleae_compat')
if _compat not in sys.path:
    sys.path.insert(0, _compat)

import saleae_bridge

Decoder, DOC = saleae_bridge.make_decoder(_here)
