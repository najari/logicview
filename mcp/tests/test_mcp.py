"""End-to-end test of the logicview MCP server over HTTP.

Starts the server on a free port with a scratch data dir, writes the synthetic
capture from extensions/tests/make-capture.py, and calls every tool.
With --hardware it also captures from the connected analyzer.

Usage: python -I test_mcp.py --sigrok-cli <app>\\sigrok-cli.exe [--hardware]
       <app>\\python.exe -I test_mcp.py --sigrok-cli <app>\\sigrok-cli.exe --server <app>\\mcp\\logicview_mcp.py
       python -I test_mcp.py --sigrok-cli <app>\\sigrok-cli.exe --server rust\\target\\release\\logicview.exe
         (the Rust app's built-in server: logicview --mcp-http; it also has query_frames)
"""
import argparse
import csv
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
HLAS = os.path.join(REPO, 'extensions', 'tests', 'hlas')
FAILED = []


def check(name, ok, detail=''):
    print(('ok   ' if ok else 'FAIL ') + name + ('' if ok or not detail else ': ' + str(detail)[:600]))
    if not ok:
        FAILED.append(name)


class Client:
    def __init__(self, url):
        self.url, self.n = url, 0

    def rpc(self, method, params=None):
        self.n += 1
        body = json.dumps({'jsonrpc': '2.0', 'id': self.n, 'method': method, 'params': params or {}}).encode()
        req = urllib.request.Request(self.url, body, {'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=300) as r:
            return json.loads(r.read())

    def call(self, tool, **args):
        resp = self.rpc('tools/call', {'name': tool, 'arguments': args})
        if 'error' in resp:
            return None, resp['error']['message']
        res = resp['result']
        if res.get('isError'):
            return None, res['content'][0]['text']
        return res['structuredContent'], None


def s(v):
    return {'stringValue': v}


def n(v):
    return {'numberValue': v}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--sigrok-cli', required=True)
    ap.add_argument('--python', default=sys.executable)
    ap.add_argument('--server', default=os.path.join(REPO, 'mcp', 'logicview_mcp.py'),
                    help='server script (e.g. <app>\\mcp\\logicview_mcp.py to test an assembled app)')
    ap.add_argument('--hardware', action='store_true')
    a = ap.parse_args()

    work = tempfile.mkdtemp(prefix='lv-mcp-')
    sr, pvs = os.path.join(work, 'test.sr'), os.path.join(work, 'test.pvs')
    subprocess.run([a.python, '-I', os.path.join(REPO, 'extensions', 'tests', 'make-capture.py'), sr, pvs],
                   check=True, capture_output=True)
    subprocess.run([a.python, '-I', os.path.join(REPO, 'extensions', 'tests', 'make-capture-more.py'),
                    os.path.join(work, 'more.sr'), os.path.join(work, 'led.sr')], check=True, capture_output=True)
    sock = socket.socket()
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
    sock.close()
    if a.server.lower().endswith('.exe'):
        cmd = [a.server, '--mcp-http']
    else:
        cmd = [a.python, '-I', a.server]
    server = subprocess.Popen(cmd + ['--port', str(port), '--sigrok-cli', a.sigrok_cli,
                                     '--data-dir', os.path.join(work, 'data')],
                              stderr=open(os.path.join(work, 'server.log'), 'w'))
    c = Client('http://127.0.0.1:{}'.format(port))
    for _ in range(50):
        try:
            c.rpc('ping')
            break
        except OSError:
            time.sleep(0.2)
    try:
        run(c, work, sr, a.hardware)
    finally:
        server.kill()
        print('\nwork dir:', work)
    print('FAILED: ' + ', '.join(FAILED) if FAILED else 'all tests passed')
    return 1 if FAILED else 0


def frames(c, cid, aid, limit=500):
    r, e = c.call('read_analyzer_frames', captureId=cid, analyzerId=aid, limit=limit)
    return r['frames'] if r else e


def echo(c, cid, aid):
    """Add the Frame Echo HLA on an analyzer; its frame texts."""
    r, e = c.call('add_high_level_analyzer', captureId=cid, extensionDirectory=os.path.join(HLAS, 'frame-echo'),
                  hlaName='Frame Echo', inputAnalyzerId=aid)
    return [f['value'] for f in frames(c, cid, r['analyzerId'])] if r else e


def run_more(c, work):
    """LIN, 1-Wire, Manchester, I2S / PCM, Simple Parallel and Addressable
    LEDs (Async) with Logic 2's setting names, and HLAs on them."""
    r, e = c.call('load_capture', filepath=os.path.join(work, 'more.sr'))
    check('load more.sr', r is not None, e)
    cid = r['captureId']

    r, e = c.call('add_analyzer', captureId=cid, analyzerName='LIN',
                  settings={'Serial': n(0), 'Bit Rate (Bits/s)': n(19200), 'LIN Version': s('Version 2.x')})
    check('add LIN', r is not None, e)
    got = [f['value'] for f in frames(c, cid, r['analyzerId'])] if r else e
    check('LIN frames (lin decoder)', 'Data: 0x01' in got and 'Data: 0x02' in got and
          any(v.startswith('Checksum: 0x6A') for v in got), got)
    got = echo(c, cid, r['analyzerId']) if r else e
    check('LIN HLA', got == ['header_break', 'header_sync', 'header_pid: protected_id=18', 'data: data=1, index=0',
                             'data: data=2, index=1', 'checksum: checksum=106'], got)

    r, e = c.call('add_analyzer', captureId=cid, analyzerName='1-Wire',
                  settings={'1-Wire': n(1), 'Overdrive only mode': {'boolValue': False}})
    check('add 1-Wire (bool setting)', r is not None, e)
    got = [f['value'] for f in frames(c, cid, r['analyzerId'])] if r else e
    check('1-Wire frames (onewire_network)', any('0xcc' in v for v in got) and 'Data: 0x44' in got, got)
    got = echo(c, cid, r['analyzerId']) if r else e
    check('1-Wire HLA', got == ['reset', 'presence', 'rom_command: rom_command=0xCC, description=skip',
                                'data: data=0x44'], got)

    r, e = c.call('add_analyzer', captureId=cid, analyzerName='Manchester',
                  settings={'Manchester': n(2), 'Bit Rate (Bits/s)': n(100000),
                            'Edge Polarity': s('negative edge is binary zero'),
                            'Significant Bit': s('Most Significant Bit Sent First'),
                            'Tolerance': s('5% of period')})
    check('add Manchester', r is not None, e)
    got = [f['value'] for f in frames(c, cid, r['analyzerId'])] if r else e
    check('Manchester frames', got == ['Word: 0xA5', 'Word: 0x3C'], got)
    got = echo(c, cid, r['analyzerId']) if r else e
    check('Manchester HLA', got == ['data: data=165', 'data: data=60'], got)

    i2s = {'CLOCK channel': n(3), 'FRAME': n(4), 'DATA': n(5)}
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='I2S / PCM',
                  settings=dict(i2s, **{'Signed/Unsigned': s("Signed (two's complement)"),
                                        'Audio Bit Depth (bits/sample)': s('16 Bits/Word')}))
    check('add I2S / PCM', r is not None, e)
    got = echo(c, cid, r['analyzerId']) if r else e
    check('I2S HLA (signed 16-bit from the analyzer settings)', got == [
        'data: channel=0, data=-1', 'data: channel=1, data=1', 'data: channel=0, data=-32768',
        'data: channel=1, data=32767', 'data: channel=0, data=0'], got)
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='I2S / PCM',
                  settings=dict(i2s, **{'CLOCK State': s('Falling edge')}))
    check('I2S setting sigrok cannot do is refused', e and '"Rising edge"' in e, e)

    r, e = c.call('add_analyzer', captureId=cid, analyzerName='Simple Parallel',
                  settings={'D0': n(7), 'Clock': n(6), 'Clock State': s('Rising edge')})
    check('add Simple Parallel', r is not None, e)
    got = echo(c, cid, r['analyzerId']) if r else e
    check('Simple Parallel HLA', got == ['data: data=1', 'data: data=0', 'data: data=1', 'data: data=1'], got)
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='Simple Parallel', settings={'Clock': n(6)})
    check('Simple Parallel needs a data channel', e and 'at least one channel' in e, e)
    c.call('close_capture', captureId=cid)

    r, e = c.call('load_capture', filepath=os.path.join(work, 'led.sr'))
    cid = r['captureId'] if r else None
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='Addressable LEDs (Async)',
                  settings={'LED Channel': n(0), 'LED Controller': s('WS2812B')})
    check('add Addressable LEDs (Async)', r is not None, e)
    got = [f['value'] for f in frames(c, cid, r['analyzerId']) if f['type'] == 'pixel'] if r else e
    check('LED frames', got == ['#0 R:255 G:0 B:0', '#1 R:0 G:255 B:0', '#2 R:0 G:0 B:255',
                                '#0 R:18 G:52 B:86', '#1 R:171 G:205 B:239'], got)
    got = echo(c, cid, r['analyzerId']) if r else e
    check('LED HLA', got[:2] == ['pixel: index=0, red=255, green=0, blue=0',
                                 'pixel: index=1, red=0, green=255, blue=0'] and len(got) == 5, got)
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='Addressable LEDs (Async)',
                  settings={'LED Channel': n(0), 'LED Controller': s('WS9999')})
    check('LED controller choices listed', e and '"LPD1886 - 36 bit"' in e, e)
    c.call('close_capture', captureId=cid)


def run(c, work, sr, hardware):
    init = c.rpc('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {},
                                'clientInfo': {'name': 'test', 'version': '0'}})
    check('initialize', init['result']['serverInfo']['name'] == 'logicview', init)
    tools = [t['name'] for t in c.rpc('tools/list')['result']['tools']]
    logic2 = ['get_devices', 'start_capture', 'stop_capture', 'wait_capture', 'load_capture', 'save_capture',
              'close_capture', 'add_analyzer', 'remove_analyzer', 'add_high_level_analyzer',
              'remove_high_level_analyzer', 'export_raw_data_csv', 'export_raw_data_binary',
              'export_data_table_csv', 'legacy_export_analyzer']
    extra = {'read_analyzer_frames', 'query_frames'}
    check('tools/list has the Logic 2 tools', all(t in tools for t in logic2) and 'read_analyzer_frames' in tools
          and len(tools) in (16, 17) and set(tools) - set(logic2) <= extra, tools)
    check('unknown tool', 'Unknown tool' in c.rpc('tools/call', {'name': 'nope'})['error']['message'])

    r, e = c.call('load_capture', filepath=sr)
    check('load_capture', r and 'captureId' in r, e)
    cid = r['captureId']
    check('load .sal refused', 'sigrok session' in (c.call('load_capture', filepath='x.sal')[1] or ''))

    # Async Serial + HLA
    r, e = c.call('add_analyzer', captureId=str(cid), analyzerName='Async Serial', analyzerLabel='Serial',
                  settings={'Input Channel': n(0), 'Bit Rate (Bits/s)': n(115200)})
    check('add Async Serial (captureId as string)', r is not None, e)
    uart = r['analyzerId']
    values = [f['value'] for f in frames(c, cid, uart)]
    check('serial frames', values[:3] == ['48', '69', '0A'], values)
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='Async Serial', settings={'Baud': n(9600)})
    check('bad setting name lists settings', e and 'Input Channel' in e and 'Bit Rate' in e, e)
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='Async Serial',
                  settings={'Input Channel': n(0), 'Parity Bit': s('Mark')})
    check('bad choice lists choices', e and 'Even Parity Bit' in e, e)

    r, e = c.call('add_high_level_analyzer', captureId=cid, extensionDirectory=os.path.join(HLAS, 'line-reader'),
                  hlaName='LineReader', inputAnalyzerId=uart, settings={'Prefix': s('>'), 'gap_ms': n(1)})
    check('add HLA (label and attribute setting names)', r is not None, e)
    lines = [(f['type'], f['value']) for f in frames(c, cid, r['analyzerId'])] if r else []
    check('HLA frames', lines == [('line', '>Hi'), ('gap', 'gap 1.009 ms'), ('line', '>ok!')], lines)
    r2, e = c.call('add_high_level_analyzer', captureId=cid, extensionDirectory=os.path.join(HLAS, 'line-reader'),
                   hlaName='LineReader', inputAnalyzerId=uart, settings={'gap_ms': n(5000)})
    check('HLA setting validation', e and 'greater than 1000' in e, e)
    r2, e = c.call('add_high_level_analyzer', captureId=cid, extensionDirectory=os.path.join(HLAS, 'line-reader'),
                   hlaName='Nope', inputAnalyzerId=uart)
    check('unknown HLA name', e and '"LineReader"' in e, e)

    # I2C, SPI, CAN with their HLAs
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='I2C', settings={'SDA': n(2), 'SCL': n(1)})
    check('add I2C', r is not None, e)
    i2c = r['analyzerId']
    r, e = c.call('add_high_level_analyzer', captureId=cid, extensionDirectory=os.path.join(HLAS, 'i2c-transactions'),
                  hlaName='I2cTransactions', inputAnalyzerId=i2c)
    got = [f['value'] for f in frames(c, cid, r['analyzerId'])] if r else e
    check('I2C HLA', got == ['W 0x50: 10 20', 'R 0x50: ab'], got)

    r, e = c.call('add_analyzer', captureId=cid, analyzerName='SPI',
                  settings={'MOSI': n(4), 'MISO': n(5), 'Clock': n(3), 'Enable': n(6)})
    check('add SPI', r is not None, e)
    spi = r['analyzerId']
    r, e = c.call('add_high_level_analyzer', captureId=cid, extensionDirectory=os.path.join(HLAS, 'spi-burst'),
                  hlaName='SpiBurst', inputAnalyzerId=spi)
    got = [f['value'] for f in frames(c, cid, r['analyzerId'])] if r else e
    check('SPI HLA', got == ['MOSI 0x9F 0x00 0x00 / MISO 0x00 0xEF 0x40 (3 words)'], got)

    r, e = c.call('add_analyzer', captureId=cid, analyzerName='CAN', settings={'CAN': n(7)})
    check('add CAN', r is not None, e)
    r, e = c.call('add_high_level_analyzer', captureId=cid, extensionDirectory=os.path.join(HLAS, 'can-messages'),
                  hlaName='CanMessages', inputAnalyzerId=r['analyzerId'])
    got = [f['value'] for f in frames(c, cid, r['analyzerId'])] if r else e
    check('CAN HLA', len(got) == 2 and got[0].startswith('ID 0x123 [3] 11 22 33') and '0x1ABCDEF' in got[1], got)

    run_more(c, work)

    # Any sigrok decoder by id
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='uart', settings={'rx': n(0), 'baudrate': n(115200),
                                                                               'format': s('ascii')})
    got = [f['value'] for f in frames(c, cid, r['analyzerId']) if f['type'] == 'rx-data'][:2] if r else e
    check('sigrok decoder by id', got == ['H', 'i'], got)
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='spiflash')
    check('stacked decoder refused', e and 'not a first-level decoder' in e, e)

    if 'query_frames' in tools:
        r, e = c.call('query_frames', captureId=cid, sql="SELECT text FROM frame f JOIN analyzer a ON a.id = "
                      "f.analyzer_id WHERE a.kind = 'hla' AND f.type LIKE '%.rt-line' ORDER BY f.start_sample")
        got = [row[0] for row in r['rows']] if r else e
        check('query_frames (SQL on the capture file)', got == ['>Hi', '>ok!'], got)
        r, e = c.call('query_frames', captureId=cid, sql='DELETE FROM frame')
        check('query_frames is read-only', e and 'read-only' in e, e)

    # Exports
    table = os.path.join(work, 'out', 'table.csv')
    r, e = c.call('export_data_table_csv', captureId=cid, filepath=table)
    rows = list(csv.reader(open(table, encoding='utf-8'))) if r is not None else []
    check('data table', rows and rows[0] == ['name', 'type', 'start_time', 'duration', 'value'] and
          any(row[4] == '>ok!' for row in rows) and any(row[0] == 'Serial' and row[4] == 'H' for row in rows),
          e or rows[:5])
    r, e = c.call('export_data_table_csv', captureId=cid, filepath=table, analyzers=[{'analyzerId': uart, 'radixType': 3}],
                  filter={'query': '0a'}, exportColumns=['start_time', 'value'])
    rows = list(csv.reader(open(table, encoding='utf-8'))) if r is not None else []
    # sigrok uart data annotations start after the start bit (8.7 us at 115200 Bd).
    check('data table filter/columns/radix', rows == [['start_time', 'value'], ['0.000299700', '0A'],
                                                      ['0.001681600', '0A']], e or rows)
    legacy = os.path.join(work, 'out', 'legacy.csv')
    r, e = c.call('legacy_export_analyzer', captureId=cid, analyzerId=uart, filepath=legacy, radixType=2)
    rows = list(csv.reader(open(legacy, encoding='utf-8'))) if r is not None else []
    check('legacy export (decimal)', rows[:2] == [['Time [s]', 'Type', 'Value'], ['0.000108700', 'rx-data', '72']],
          e or rows[:3])

    raw = os.path.join(work, 'out', 'raw')
    r, e = c.call('export_raw_data_csv', captureId=cid, directory=raw, analogDownsampleRatio=1,
                  logicChannels={'digitalChannels': [0]})
    rows = list(csv.reader(open(os.path.join(raw, 'digital.csv'), encoding='utf-8'))) if r is not None else []
    check('raw csv', rows[:3] == [['Time [s]', 'Channel 0'], ['0.000000000', '1'], ['0.000100000', '0']], e or rows[:3])
    r, e = c.call('export_raw_data_binary', captureId=cid, directory=raw, analogDownsampleRatio=1,
                  logicChannels={'digitalChannels': [0, 7]})
    ok = r is not None
    if ok:
        blob = open(os.path.join(raw, 'digital_0.bin'), 'rb').read()
        ident, ver, typ, init, begin, end, count = struct.unpack('<8siiIddQ', blob[:44])
        times = struct.unpack('<{}d'.format(count), blob[44:])
        ok = (ident, ver, typ, init, begin, round(end, 6), times[0]) == (b'<SALEAE>', 0, 0, 1, 0.0, 0.0046, 0.0001) \
            and len(blob) == 44 + 8 * count and count == len(rows) - 2
    check('raw binary (Logic 2 format)', ok, e)
    r, e = c.call('export_raw_data_csv', captureId=cid, directory=raw, analogDownsampleRatio=1,
                  logicChannels={'analogChannels': [0]})
    check('analog export refused', e and 'Analog' in e, e)

    saved = os.path.join(work, 'out', 'copy.sr')
    r, e = c.call('save_capture', captureId=cid, filepath=saved)
    check('save_capture', r is not None and os.path.getsize(saved) == os.path.getsize(sr), e)
    check('save .sal refused', 'sigrok' in (c.call('save_capture', captureId=cid, filepath='x.sal')[1] or ''))
    r, e = c.call('remove_analyzer', captureId=cid, analyzerId=uart)
    check('remove_analyzer (also its HLAs)', r is not None and 'does not exist' in str(frames(c, cid, uart)), e)
    r, e = c.call('close_capture', captureId=cid)
    check('close_capture keeps loaded file', r is not None and os.path.exists(sr), e)
    check('closed capture is gone', 'does not exist' in (c.call('wait_capture', captureId=cid)[1] or ''))

    if not hardware:
        return
    r, e = c.call('get_devices')
    devs = r['devices'] if r else []
    check('get_devices', bool(devs), e or devs)
    ids = [d['deviceId'] for d in devs]
    fx2 = [i for i in ids if i.startswith('fx2lafw:')]
    u3 = [i for i in ids if i.startswith('weact-u3:')]
    if fx2:
        run_fx2(c, work, fx2[0])
    if u3:
        run_u3(c, work, u3[0])


def capture_length(c, work, cid, name):
    import zipfile
    saved = os.path.join(work, 'hw', name)
    c.call('save_capture', captureId=cid, filepath=saved)
    with zipfile.ZipFile(saved) as z:
        return sum(i.file_size for i in z.infolist() if i.filename.startswith('logic'))


def run_u3(c, work, dev):
    """WeAct Logic Analyzer V2 (Rust server only)."""
    os.makedirs(os.path.join(work, 'hw'), exist_ok=True)
    for rate, secs in ((20_000_000, 0.25), (200_000_000, 0.1)):
        r, e = c.call('start_capture', deviceId=dev,
                      logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0, 1, 2, 3, 4, 5, 6, 7]},
                                                'digitalSampleRate': rate, 'digitalThresholdVolts': 1.5},
                      captureConfiguration={'timedCaptureMode': {'durationSeconds': secs}})
        check(f'U3 timed capture at {rate // 1_000_000} MHz', r is not None, e)
        if r:
            cid = r['captureId']
            r2, e = c.call('wait_capture', captureId=cid)
            check(f'U3 wait capture at {rate // 1_000_000} MHz', r2 is not None, e)
            n = capture_length(c, work, cid, f'u3-{rate}.sr')
            check(f'U3 capture length = {int(rate * secs)} samples', n == int(rate * secs), n)
            c.call('close_capture', captureId=cid)
    r, e = c.call('start_capture', deviceId=dev,
                  logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0]},
                                            'digitalSampleRate': 20_000_000, 'digitalThresholdVolts': 3.3})
    check('U3 threshold out of range', e and 'out of range' in e and '0.6 V to 1.5 V' in e, e)
    r, e = c.call('start_capture', deviceId=dev,
                  logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0]},
                                            'digitalSampleRate': 24_000_000})
    check('U3 invalid samplerate lists its rates', e and '200000000' in e and '24000000' not in e, e)


def run_fx2(c, work, dev):
    r, e = c.call('start_capture', logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0, 1]},
                                                             'digitalSampleRate': 1000000},
                  captureConfiguration={'timedCaptureMode': {'durationSeconds': 0.25}})
    check('start timed capture', r is not None, e)
    if r:
        cid = r['captureId']
        r2, e = c.call('wait_capture', captureId=cid)
        check('wait timed capture', r2 is not None, e)
        r2, e = c.call('export_raw_data_csv', captureId=cid, directory=os.path.join(work, 'hw'), analogDownsampleRatio=1)
        check('timed capture export', r2 is not None, e)
        saved = os.path.join(work, 'hw', 'timed.sr')
        c.call('save_capture', captureId=cid, filepath=saved)
        import zipfile
        with zipfile.ZipFile(saved) as z:
            size = len(z.read('logic-1-1'))
        check('timed capture length = 250000 samples', size == 250000, size)
        c.call('close_capture', captureId=cid)
    r, e = c.call('start_capture', logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0]},
                                                             'digitalSampleRate': 16000000})
    check('start manual capture at 16 MHz', r is not None, e)
    if r:
        time.sleep(2.0)   # opening the device takes ~0.6 s before data flows
        t = time.time()
        r2, e = c.call('stop_capture', captureId=r['captureId'])
        check('stop manual capture', r2 is not None and time.time() - t < 10, e)
        saved = os.path.join(work, 'hw', 'manual.sr')
        c.call('save_capture', captureId=r['captureId'], filepath=saved)
        import zipfile
        with zipfile.ZipFile(saved) as z:
            size = sum(i.file_size for i in z.infolist() if i.filename.startswith('logic'))
        check('manual capture has at least 1 s at 16 MHz', size >= 16_000_000, size)
    r, e = c.call('start_capture', logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0]},
                                                             'digitalSampleRate': 5000000})
    check('invalid samplerate lists rates', e and '24000000' in e, e)
    r, e = c.call('start_capture', logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0]},
                                                             'digitalSampleRate': 1000000},
                  captureConfiguration={'digitalCaptureMode': {'triggerType': 3, 'triggerChannelIndex': 0}})
    check('pulse trigger refused', e and 'Pulse triggers' in e, e)
    r, e = c.call('start_capture', deviceId=dev,
                  logicDeviceConfiguration={'logicChannels': {'digitalChannels': [0]},
                                            'digitalSampleRate': 1000000, 'digitalThresholdVolts': 1.5})
    check('fx2 threshold refused', e and 'does not support a configurable threshold' in e, e)


if __name__ == '__main__':
    sys.exit(main())
