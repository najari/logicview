from saleae.analyzers import HighLevelAnalyzer, AnalyzerFrame

from helper import upper   # sibling module, imported the way Logic 2 allows


class Echo(HighLevelAnalyzer):
    def decode(self, frame: AnalyzerFrame):
        if frame.data.get('data') == b'!':
            raise ValueError('exclamation mark')
        return AnalyzerFrame('echo', frame.start_time, frame.end_time,
                             {'char': upper(frame.data['data'])})
