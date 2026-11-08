# Patterns seen in published HLAs (e.g. Saleae's hla-text-messages): choices
# from dict keys, a tiny minimum number, a result type added in __init__ with
# a triple-brace placeholder.
from saleae.analyzers import (HighLevelAnalyzer, AnalyzerFrame, StringSetting,
                              NumberSetting, ChoicesSetting)

MODES = {'Upper [A]': str.upper, 'Lower [a]': str.lower}


class Dynamic(HighLevelAnalyzer):
    prefix = StringSetting(label='Prefix (optional)')
    timeout = NumberSetting(label='Timeout [s]', min_value=1E-6, max_value=1E4)
    mode = ChoicesSetting(label='Mode', choices=MODES.keys())

    result_types = {'error': {'format': 'Error!'}}

    def __init__(self):
        self.convert = MODES[self.mode]
        self.result_types['word'] = {'format': self.prefix + '{{{data.str}}}'}

    def decode(self, frame: AnalyzerFrame):
        return AnalyzerFrame('word', frame.start_time, frame.end_time,
                             {'str': self.convert(frame.data['data'].decode())})
