logicview {VERSION} (Windows x64)
==================================

FX2 칩 로직 애널라이저(Saleae Logic 호환 클론, USB 0925:3881, 8채널, 최대 24 MHz)용 수집·파형·디코딩 앱.
압축을 풀기만 하면 쓸 수 있다. 설치 프로그램, Python, sigrok-cli, VC++ 재배포 패키지가 필요 없다.


1. 처음 한 번: WinUSB 드라이버
------------------------------
애널라이저를 꽂고 Zadig(https://zadig.akeo.ie/)로 0925:3881 장치에 WinUSB 드라이버를 설치한다.
Zadig 창을 조작해 주는 스크립트도 있다 (관리자 권한으로):

  Start-Process pwsh -Verb RunAs -Wait -ArgumentList '-File', 'tools\install-winusb.ps1',
      '-ZadigExe', '<zadig.exe 경로>', '-Log', "$env:TEMP\winusb.log"

PulseView·SLogicView에서 이미 쓰던 애널라이저면 이 단계는 필요 없다.


2. 앱
-----
logicview.exe 를 실행한다.

- 위 줄: 장치, 샘플레이트, 시간(0.1~60초 또는 수동 중지), 트리거(예: 0=f,1=1), 시작/중지.
  샘플 수는 샘플레이트 × 시간이다 (시간 칸에 마우스를 올리면 보인다).
  처음 열 때 펌웨어(firmware\fx2lafw-saleae-logic.fw)를 올리느라 2초쯤 걸린다.
  수집 중에는 파형이 실시간으로 늘어난다. 수집 파일은 %LOCALAPPDATA%\logicview\captures 에 쌓인다.
- 아래 줄: 열기(.lvcap, .sr), 저장(.lvcap, .sr, CSV), 전체 보기·확대·축소, 채널(이름·숨기기),
  디코더(UART, I2C, SPI, CAN 등 sigrok 디코더 131개, 스택, 프리셋), 표(디코딩 결과 검색), MCP.
  디코더 창의 "프리셋" 줄에서 지금 스택(디코더, 채널, 옵션, 이름)을 이름 붙여 저장하고 다시 불러온다.
- 샘플레이트, 시간, 채널 이름·보이기, 트리거 문자열과 앞 구간 비율은 바꿀 때마다
  %LOCALAPPDATA%\logicview\app.db 에 저장되어 다음 실행에 그대로 나온다 (명령줄 --rate 등이 우선).
- 파형: 휠로 확대·축소, 끌어서 이동, 클릭으로 마커 A·B(시간 차와 주파수), 오른쪽 클릭·Esc로 마커 지우기,
  Home으로 전체 보기.


3. 명령줄 (lvcli.exe)
---------------------
  lvcli devices
  lvcli capture -r 4M -s 10M -o cap.lvcap
  lvcli capture -r 1M --time 5 -t 0=f --pre 20 -o cap.lvcap
  lvcli info cap.lvcap
  lvcli export cap.lvcap -f csv | bin | sr | raw
  lvcli import other.sr
  lvcli decoders [uart]
  lvcli decode cap.lvcap -P uart:rx=D0:baudrate=115200 -A uart=rx-data [--save]
  lvcli bench -r 24M -s 50M -n 5            (이 PC의 USB 포트가 24 MHz를 끊김 없이 받는지)


4. MCP (Claude Code 등에서 Saleae Logic 2 MCP 도구로 쓰기)
------------------------------------------------------------
도구: get_devices, start_capture, stop_capture, wait_capture, load_capture, save_capture, close_capture,
add_analyzer, remove_analyzer, add_high_level_analyzer, remove_high_level_analyzer, export_raw_data_csv,
export_raw_data_binary, export_data_table_csv, legacy_export_analyzer, read_analyzer_frames,
query_frames(캡처 파일에 읽기 전용 SQL).

  stdio:  claude mcp add -s user logicview -- "<압축 푼 폴더>\logicview.exe" --mcp-stdio
  HTTP:   앱에서 MCP 버튼을 누르거나  logicview.exe --mcp-http  로 띄운 뒤
          claude mcp add --transport http logicview http://127.0.0.1:10530


5. Saleae HLA (고수준 분석기 확장)
---------------------------------
  pwsh -File extensions\hla.ps1 install <확장 폴더 | .zip | GitHub URL>
  pwsh -File extensions\hla.ps1 list
  pwsh -File extensions\hla.ps1 package add numpy     (HLA가 import하는 Python 패키지, 아래)

설치한 HLA는 디코더 창에서 프로토콜 디코더 위에 쌓아 쓴다 (예: uart 위에 hla_..._uart).
입력: Async Serial(uart), I2C, SPI, CAN, LIN(uart 위), 1-Wire(onewire_link 위), Manchester,
I2S / PCM, Simple Parallel.
numpy처럼 컴파일된 패키지는 MSYS2(https://www.msys2.org/, 기본 C:\msys64, 다른 곳이면 -Msys2 <폴더>)의
UCRT64 패키지에서 가져온다. 순수 Python 패키지는 MSYS2 없이 PyPI에서 받는다.
MCP의 add_high_level_analyzer는 확장 폴더를 받아 알아서 설치한다. 확장 코드는 앱 안의 Python에서
실행되므로 믿을 수 있는 확장만 설치할 것.


6. 문제 해결
------------
- 24 MHz에서 "장치 버퍼가 넘쳐 데이터가 끊김": USB 포트가 속도를 못 따라간 것. 허브를 빼고 PC 본체
  포트에 꽂거나, 다른 본체 포트를 써 보거나, 16 MHz 이하로 수집한다.
- "장치를 열 수 없습니다": 다른 프로그램(PulseView, SLogicView, 다른 logicview)이 장치를 쓰고 있지 않은지,
  WinUSB 드라이버가 설치됐는지 확인한다.


구성
----
  logicview.exe, lvcli.exe   앱과 명령줄 도구
  firmware\                  fx2lafw 펌웨어 (sigrok-firmware-fx2lafw, GPLv2+)
  sigrok\                    디코더 런타임: libsigrokdecode, glib, Python 3.14와 표준 라이브러리, 디코더 스크립트
  extensions\                Saleae HLA 설치 스크립트와 호환 계층
  tools\                     WinUSB 드라이버 설치 스크립트

라이선스: GPL-3.0-or-later (LICENSE.txt). libsigrokdecode와 sigrok 디코더는 GPLv3+,
fx2lafw 펌웨어는 GPLv2+, Python은 PSF 라이선스.
소스: https://github.com/najari/logicview (rust\ 폴더)
