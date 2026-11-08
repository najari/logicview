# Pre-1.0 HLA API (plain class, get_capabilities, dict frames). Logic 2 no
# longer runs these; the bridge must refuse it with a clear message.
class Hla():
    def get_capabilities(self):
        return {'settings': {}}

    def set_settings(self, settings):
        return {'result_types': {}}

    def decode(self, frame):
        return {'type': 'echo', 'start_time': frame['start_time'],
                'end_time': frame['end_time'], 'data': {}}
