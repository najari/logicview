import numpy as np

from saleae.analyzers import HighLevelAnalyzer, AnalyzerFrame


class NumpyMean(HighLevelAnalyzer):
    """Collects UART bytes up to a newline and reports their mean with
    numpy (needs 'hla.ps1 package add numpy')."""

    result_types = {'mean': {'format': 'numpy {{data.version}}: mean {{data.mean}} of {{data.count}} bytes'}}

    def __init__(self):
        self.values, self.start = [], None

    def decode(self, frame: AnalyzerFrame):
        byte = frame.data['data'][0]
        if self.start is None:
            self.start = frame.start_time
        if byte != 0x0A:
            self.values.append(byte)
            return None
        values, start = np.array(self.values, dtype=np.float64), self.start
        self.values, self.start = [], None
        return AnalyzerFrame('mean', start, frame.end_time, {
            'mean': round(float(values.mean()), 2) if values.size else 0.0,
            'count': int(values.size), 'version': np.__version__})
