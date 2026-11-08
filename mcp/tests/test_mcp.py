"""End-to-end test of the logicview MCP server over HTTP.

Starts the server on a free port with a scratch data dir, writes the synthetic
capture from extensions/tests/make-capture.py, and calls every tool.
With --hardware it also captures from the connected analyzer.

Usage: python -I test_mcp.py --sigrok-cli <app>\\sigrok-cli.exe [--hardware]
       <app>\\python.exe -I test_mcp.py --sigrok-cli <app>\\sigrok-cli.exe --server <app>\\mcp\\logicview_mcp.py
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
    sock = socket.socket()
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
    sock.close()
    server = subprocess.Popen([a.python, '-I', a.server, '--port', str(port),
                               '--sigrok-cli', a.sigrok_cli, '--data-dir', os.path.join(work, 'data')],
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


def run(c, work, sr, hardware):
    init = c.rpc('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {},
                                'clientInfo': {'name': 'test', 'version': '0'}})
    check('initialize', init['result']['serverInfo']['name'] == 'logicview', init)
    tools = [t['name'] for t in c.rpc('tools/list')['result']['tools']]
    logic2 = ['get_devices', 'start_capture', 'stop_capture', 'wait_capture', 'load_capture', 'save_capture',
              'close_capture', 'add_analyzer', 'remove_analyzer', 'add_high_level_analyzer',
              'remove_high_level_analyzer', 'export_raw_data_csv', 'export_raw_data_binary',
              'export_data_table_csv', 'legacy_export_analyzer']
    check('tools/list has the Logic 2 tools', all(t in tools for t in logic2) and len(tools) == 16, tools)
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

    # Any sigrok decoder by id
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='uart', settings={'rx': n(0), 'baudrate': n(115200),
                                                                               'format': s('ascii')})
    got = [f['value'] for f in frames(c, cid, r['analyzerId']) if f['type'] == 'rx-data'][:2] if r else e
    check('sigrok decoder by id', got == ['H', 'i'], got)
    r, e = c.call('add_analyzer', captureId=cid, analyzerName='spiflash')
    check('stacked decoder refused', e and 'not a first-level decoder' in e, e)

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
    check('get_devices', devs and devs[0]['deviceId'].startswith('fx2lafw'), e or devs)
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


if __name__ == '__main__':
    sys.exit(main())
