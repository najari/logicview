from saleae.analyzers import HighLevelAnalyzer, AnalyzerFrame, StringSetting


class CanMessages(HighLevelAnalyzer):
    id_filter = StringSetting(label='Only this ID (hex, empty = all)')

    result_types = {
        'message': {'format': 'ID 0x{{data.id}} [{{data.dlc}}] {{data.bytes}} CRC {{data.crc}}'}
    }

    def __init__(self):
        self.wanted = int(self.id_filter, 16) if self.id_filter else None
        self.start = None

    def decode(self, frame: AnalyzerFrame):
        if frame.type == 'identifier_field':
            self.start = frame.start_time
            self.ident = frame.data['identifier']
            self.payload = []
            self.dlc = 0
            self.crc = None
        elif frame.type == 'control_field':
            self.dlc = frame.data['num_data_bytes']
        elif frame.type == 'data_field':
            value = frame.data['data']
            assert value == value[0]   # usable as int and as bytes
            self.payload.append(value)
        elif frame.type == 'crc_field':
            self.crc = frame.data.get('crc')
        elif frame.type == 'ack_field' and self.start is not None:
            start, self.start = self.start, None
            if self.wanted is not None and self.ident != self.wanted:
                return None
            return AnalyzerFrame('message', start, frame.end_time, {
                'id': f'{self.ident:03X}', 'dlc': self.dlc,
                'bytes': ' '.join(f'{b:02X}' for b in self.payload),
                'crc': f'{self.crc:04X}' if self.crc is not None else '-'})
        return None
