# logicview

Sipeed [SLogic](https://github.com/sipeed/SLogic)의 SLogicView 2.3.3(PulseView 기반)에서
FX2 칩 기반 로직 애널라이저(Saleae Logic 호환 클론, USB `0925:3881`, 8채널 24 MHz)를
쓸 수 있게 하는 패치와 스크립트 (Windows).

SLogic 릴리스는 libsigrok를 `--disable-all-drivers --enable-sipeed-slogic-analyzer`로
빌드해서 Sipeed 장비만 인식한다. 여기서는 릴리스와 같은 소스 커밋으로 libsigrok를
fx2lafw 드라이버까지 포함해 다시 빌드하고, 릴리스의 `libsigrok-4.dll`만 교체한다.
Windows 기본 libusb에서 fx2lafw가 제대로 동작하도록 패치 3개를 함께 적용한다.

## 파일

| 파일 | 내용 |
|---|---|
| `0001-usb-poll-fallback-without-libusb-pollfds.patch` | 기본 libusb(Windows)는 `libusb_get_pollfds()`가 NULL → fx2lafw 수집이 시작하자마자 중단되던 문제. 1 ms 폴링으로 대체 |
| `0002-fx2lafw-clear-halt-on-stall.patch` | STALL 후 WinUSB 파이프가 halt 상태로 남는 문제. `libusb_clear_halt()` 후 재전송 |
| `0003-fx2lafw-enable-winusb-raw-io.patch` | WinUSB는 RAW_IO 없이는 bulk IN 요청을 하나씩만 보내서, 24 MHz에서 요청 사이 틈에 FX2 버퍼가 넘침(첫 240 KB 후 데이터 끊김). fx2lafw에서 RAW_IO 켬 |
| `build-libsigrok-fx2lafw.sh` | SLogic의 `sources/libsigrok`를 복사 → 패치 적용 → fx2lafw 포함 빌드 (MSYS2 UCRT64) |
| `assemble-slogicview.ps1` | 릴리스 설치 파일 풀기 + DLL 교체 + `libz-1.dll` + 펌웨어 복사 → 포터블 앱 폴더 |
| `check-imports.sh`, `verify-app.sh` | 교체한 DLL의 import 심볼이 앱 폴더의 DLL에서 모두 해결되는지 검사 |
| `tests/run-capture.ps1` | 앱 실행 → Run → 수집이 끝날 때까지 대기 → 스크린샷 → 로그에서 USB 전송 결과 요약 |
| `tests/slv-set-24m.ps1`, `tests/pv-set-24m.ps1` | SLogicView / PulseView 툴바를 24 MHz, 50 M 샘플로 설정 (`run-capture.ps1 -PreRunScript`용) |
| `tools/install-winusb.ps1` | Zadig 창을 조작해서 애널라이저에 WinUSB 드라이버 설치 |
| `extensions/hla.ps1` | Saleae Logic 2 HLA 확장을 SLogicView 디코더로 설치·목록·삭제 ([아래](#saleae-logic-2-확장hla-사용)) |
| `extensions/compat/` | Logic 2 확장 API 호환 계층: `saleae` 모듈(API 대체 구현) + `saleae_bridge.py`(sigrok 디코더 ↔ HLA 변환) |
| `extensions/template/` | 확장마다 만들어지는 디코더 패키지의 `__init__.py`, `pd.py` |
| `extensions/tests/` | 테스트 HLA 7개, 단위 테스트(`run-tests.ps1`), 합성 캡처·세션 생성기(`make-capture.py`) |

## 준비물

- [MSYS2](https://www.msys2.org/) UCRT64와 빌드 패키지:
  ```sh
  pacman -S --needed autoconf-wrapper automake-wrapper libtool make patch rsync \
    mingw-w64-ucrt-x86_64-{gcc,binutils,pkgconf,glib2,libzip,libusb,libserialport,libftdi,hidapi,nettle}
  ```
- SLogicView 릴리스 설치 파일 `SLogicView-...-windows-x86_64.exe` ([SLogic Releases](https://github.com/sipeed/SLogic/releases))
- 7-Zip (설치 파일 풀기용)
- fx2lafw 펌웨어 `fx2lafw-*.fw`: PulseView 설치 폴더의 `share\sigrok-firmware`에 있다
- 애널라이저에 WinUSB 드라이버: [Zadig](https://zadig.akeo.ie/) 또는 `tools/install-winusb.ps1`

## 빌드와 조립

SLogic 소스 받기 (PowerShell 또는 아무 셸):
```sh
git clone https://github.com/sipeed/SLogic.git
git -C SLogic checkout release-2.3.3
git -C SLogic submodule update --init sources/libsigrok
```
빌드 (MSYS2 UCRT64 셸, `SLogic`과 이 저장소가 있는 폴더에서):
```sh
bash logicview/build-libsigrok-fx2lafw.sh SLogic logicview/build
```
조립 (PowerShell):
```powershell
.\logicview\assemble-slogicview.ps1 -ReleaseExe <SLogicView-...-windows-x86_64.exe> `
  -LibsigrokDll .\logicview\build\prefix\bin\libsigrok-4.dll -Destination <새 앱 폴더>
```
확인 (MSYS2 UCRT64 셸): `bash logicview/verify-app.sh <새 앱 폴더>` → 모든 파일에서 `ALL IMPORTS RESOLVED`

DLL만 교체하므로 릴리스에 들어 있는 libsigrok 서브모듈과 같은 커밋으로 빌드해야 한다
(release-2.3.3 → libsigrok `bf6b346`). 원본 DLL은 앱 폴더에 `libsigrok-4.dll.release`로 남는다.

## 테스트

애널라이저를 꽂은 상태에서 PowerShell로 실행한다. 스크립트가 마우스와 키보드를 조작하므로
실행 중에는 PC를 건드리지 말 것. 로그와 스크린샷은 `tests\out`에 저장된다.

```powershell
# SLogicView 기본 설정 (20 kHz, 1 M 샘플, 약 50초)
.\tests\run-capture.ps1 -Exe <앱 폴더>\slogicview.exe -Tag slv-20k
# SLogicView 24 MHz, 50 M 샘플
.\tests\run-capture.ps1 -Exe <앱 폴더>\slogicview.exe -Tag slv-24m -PreRunScript .\tests\slv-set-24m.ps1
# PulseView 비교 (UI Automation이 없어서 창 기준 좌표를 클릭한다. 창 크기 1016x759 기준)
.\tests\run-capture.ps1 -Exe 'C:\Program Files\sigrok\PulseView\pulseview.exe' -Tag pv-24m `
  -ClickAt 80,43 -PreRunScript .\tests\pv-set-24m.ps1
```

### 결과 (2026-11-08, PC 본체 USB 포트 직접 연결)

| 앱 | 설정 | 결과 |
|---|---|---|
| SLogicView (패치 0001–0003) | 20 kHz, 1 M 샘플 | 50.03초, 1,000,448 바이트, 전송 1954건 모두 성공 |
| SLogicView (패치 0001–0003) | 24 MHz, 50 M 샘플 | 2.12초, 50,186,752 바이트, 전송 209건 모두 성공 |
| PulseView 나이틀리 (비교) | 20 kHz / 24 MHz | 위와 같음 |

외부 USB 허브(VIA `2109:0812`)를 거치면 SLogicView, PulseView 모두 수 초 안에 STALL로
멈췄다. 애널라이저는 PC 본체 포트에 직접 꽂아서 쓸 것.

## Saleae Logic 2 확장(HLA) 사용

Logic 2의 High Level Analyzer(HLA) 확장을 SLogicView에서 디코더로 쓸 수 있다.
`extensions/hla.ps1`이 확장을 libsigrokdecode의 사용자 디코더 폴더
(`%LOCALAPPDATA%\libsigrokdecode\decoders`)에 디코더로 등록하고, 호환 계층이
sigrok 디코더(uart, i2c, spi, can)의 출력을 Logic 2 분석기와 같은 프레임으로 바꿔
HLA에 넘긴다. HLA가 돌려준 프레임은 SLogicView 주석으로 표시된다. 앱 파일은 건드리지 않는다.

```powershell
# GitHub 저장소 주소, 확장 폴더(extension.json이 있는 곳), zip 파일 모두 가능
.\extensions\hla.ps1 install https://github.com/saleae/hla-text-messages
.\extensions\hla.ps1 install C:\work\my-hla -Protocol i2c   # 입력 프로토콜 직접 지정
.\extensions\hla.ps1 list
.\extensions\hla.ps1 remove "Text Messages"
```

설치 후 SLogicView를 다시 시작한다. 디코더 추가 창의 `Saleae HLA` 분류에서
`<확장 이름> [uart]`처럼 고르면 아래 디코더(예: UART)가 함께 추가되고, 이미 있는
UART 디코더에 쌓아도 된다. HLA 설정은 디코더 옵션으로 나온다.

| Logic 2 분석기 | sigrok 디코더 | HLA가 받는 프레임 |
|---|---|---|
| Async Serial | `uart` | `data` (`data`, `error`) |
| I2C | `i2c` | `start`, `address` (`address`, `read`, `ack`), `data` (`data`, `ack`), `stop` |
| SPI | `spi` | `enable`, `result` (`mosi`, `miso`), `disable` |
| CAN | `can` | `identifier_field`, `control_field`, `data_field`, `crc_field`, `ack_field` |

`-Protocol auto`(기본값)는 HLA 코드가 확인하는 프레임 종류와, 이름·설명·README·주석에
나오는 분석기 이름으로 입력 프로토콜을 고른다. 여러 개면 프로토콜마다 디코더를 하나씩
만든다(`hla_<확장>_<프로토콜>`).

제한:
- High Level Analyzer만 지원한다. Measurement 확장(DigitalMeasurement, AnalogMeasurement)은
  SLogicView에 대응하는 기능이 없어서 건너뛴다.
- 1.0 이전의 옛 HLA API(`get_capabilities()`, dict 프레임)는 Logic 2처럼 실행하지 않는다
  (설치할 때 경고, 불러올 때 오류 메시지).
- UART는 sigrok uart의 RX·TX를 모두 HLA로 보낸다. 옵션 `UART line fed to the HLA`로 한쪽만 고를 수 있다.
- SPI는 CS 극성 옵션을 SPI 디코더와 같게 맞춰야 한다(기본 active-low).
- CAN: sigrok can 디코더는 프레임을 한 번에 넘겨서, 필드 경계는 비트 수로 계산한다
  (스터프 비트 무시). `crc`는 디코딩한 필드로 계산한 값(클래식 CAN만)이고, `ack`는 항상 true.
- LIN, 1-Wire, Manchester, I2S 등 그 밖의 Logic 2 분석기 입력은 아직 없다.
- numpy 같은 외부 패키지를 쓰는 HLA는 앱 내장 Python(3.14)에 그 패키지가 없어서 동작하지 않는다.
- HLA의 `print()` 출력은 `%LOCALAPPDATA%\libsigrokdecode\saleae_compat\hla.log`에 쌓인다.
  환경 변수 `SALEAE_HLA_TRACE=1`로 앱을 실행하면 HLA가 낸 프레임도 기록된다.

### 확장 테스트

```powershell
# 테스트 HLA 7개를 임시 폴더에 설치하고 단위 테스트 실행 (MSYS2 Python 3.14 사용)
.\extensions\tests\run-tests.ps1
# 실제 앱 확인: 테스트 HLA를 사용자 폴더에 설치 → 합성 캡처와 세션 생성 → 열기
Get-ChildItem .\extensions\tests\hlas -Directory | ForEach-Object { .\extensions\hla.ps1 install $_.FullName }
C:\msys64\ucrt64\bin\python.exe -I .\extensions\tests\make-capture.py test.sr test.pvs
$env:SALEAE_HLA_TRACE = 1; & <앱 폴더>\slogicview.exe -s test.pvs test.sr
```

결과 (2026-11-08): 단위 테스트 22개 통과. SLogicView 2.3.3에서 합성 캡처(UART "Hi", "ok!",
I2C 쓰기/읽기, SPI 3워드, CAN 표준·확장 프레임)를 테스트 HLA 5개가 기대한 대로 디코딩했고,
HLA 예외는 오류 주석으로, 옛 API 확장은 불러오기 오류로만 처리되었다.

## 라이선스

패치는 libsigrok 소스를 고친 것이므로 libsigrok와 같은 GPL-3.0-or-later를 따른다.
