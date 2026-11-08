from saleae.analyzers import HighLevelAnalyzer, AnalyzerFrame


class SpiBurst(HighLevelAnalyzer):
    result_types = {
        'burst': {'format': 'MOSI {{data.mosi}} / MISO {{data.miso}} ({{data.count}} words)'}
    }

    def __init__(self):
        self.start = None
        self.mosi = b''
        self.miso = b''
        self.count = 0

    def decode(self, frame: AnalyzerFrame):
        if frame.type == 'enable':
            self.start = frame.start_time
            self.mosi, self.miso, self.count = b'', b'', 0
        elif frame.type == 'result':
            if self.start is None:
                self.start = frame.start_time
            self.mosi += frame.data['mosi']
            self.miso += frame.data['miso']
            self.count += 1
        elif frame.type == 'disable' and self.start is not None:
            out = AnalyzerFrame('burst', self.start, frame.end_time, {
                'mosi': self.mosi, 'miso': self.miso, 'count': self.count})
            self.start = None
            return out
        return None
