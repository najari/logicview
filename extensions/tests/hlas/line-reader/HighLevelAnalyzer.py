from saleae.analyzers import (HighLevelAnalyzer, AnalyzerFrame, StringSetting,
                              NumberSetting, ChoicesSetting)
from saleae.data import GraphTimeDelta


class LineReader(HighLevelAnalyzer):
    prefix = StringSetting(label='Prefix')
    gap_ms = NumberSetting(label='Report gaps longer than (ms, 0 = off)',
                           min_value=0, max_value=1000)
    line_end = ChoicesSetting(choices=['LF', 'CR'])

    result_types = {
        'line': {'format': '{{data.prefix}}{{data.text}}'},
        'gap': {'format': 'gap {{data.ms}} ms'},
    }

    def __init__(self):
        self.text = ''
        self.start = None
        self.last_end = None
        self.end_char = '\n' if self.line_end == 'LF' else '\r'
        self.gap = GraphTimeDelta(millisecond=self.gap_ms) if self.gap_ms else None
        print(f'Line Reader: prefix={self.prefix!r} gap_ms={self.gap_ms} line_end={self.line_end}')

    def decode(self, frame: AnalyzerFrame):
        if frame.type != 'data' or 'error' in frame.data:
            return None
        out = []
        if (self.gap is not None and self.last_end is not None
                and frame.start_time - self.last_end > self.gap):
            ms = float(frame.start_time - self.last_end) * 1000
            out.append(AnalyzerFrame('gap', self.last_end, frame.start_time,
                                     {'ms': round(ms, 3)}))
        char = frame.data['data'].decode('ascii', 'replace')
        if self.start is None:
            self.start = frame.start_time
        self.last_end = frame.end_time
        if char == self.end_char:
            out.append(AnalyzerFrame('line', self.start, frame.end_time,
                                     {'text': self.text, 'prefix': self.prefix}))
            self.text = ''
            self.start = None
        else:
            self.text += char
        return out or None
