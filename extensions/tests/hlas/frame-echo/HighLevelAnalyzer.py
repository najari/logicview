from saleae.analyzers import HighLevelAnalyzer, AnalyzerFrame


class FrameEcho(HighLevelAnalyzer):
    """Returns each input frame unchanged, so the frame types and data keys
    the bridge builds show up as annotations ("<type>: key=value, ...")."""

    def decode(self, frame: AnalyzerFrame):
        return AnalyzerFrame(frame.type, frame.start_time, frame.end_time,
                             dict(frame.data))
