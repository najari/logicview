from saleae.analyzers import HighLevelAnalyzer, AnalyzerFrame


class I2cTransactions(HighLevelAnalyzer):
    result_types = {
        'transaction': {
            'format': '{{data.dir}} 0x{{data.address}}: {{data.bytes}}{{data.nak}}'
        }
    }

    def __init__(self):
        self.start = None
        self.reset()

    def reset(self):
        self.address = None
        self.read = False
        self.data = b''
        self.nak = ''

    def finish(self, end_time):
        if self.start is None or self.address is None:
            return None
        frame = AnalyzerFrame('transaction', self.start, end_time, {
            'dir': 'R' if self.read else 'W',
            'address': f'{self.address:02X}',
            'bytes': self.data.hex(' '),
            'nak': self.nak,
        })
        self.reset()
        return frame

    def decode(self, frame: AnalyzerFrame):
        if frame.type == 'start':
            out = self.finish(frame.start_time)   # repeated start
            self.start = frame.start_time
            return out
        if frame.type == 'address':
            self.address = frame.data['address'][0]
            self.read = frame.data['read']
            self.nak = '' if frame.data['ack'] else ' (address NAK)'
        elif frame.type == 'data':
            self.data += frame.data['data']
        elif frame.type == 'stop':
            out = self.finish(frame.end_time)
            self.start = None
            return out
        return None
