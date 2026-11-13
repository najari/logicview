# logicview

Sipeed [SLogic](https://github.com/sipeed/SLogic)의 SLogicView 2.3.3(PulseView 기반)에서
FX2 칩 기반 로직 애널라이저(Saleae Logic 호환 클론, USB `0925:3881`, 8채널 24 MHz)를
쓸 수 있게 하는 패치와 스크립트 (Windows).

SLogic 릴리스는 libsigrok를 `--disable-all-drivers --enable-sipeed-slogic-analyzer`로
빌드해서 Sipeed 장비만 인식한다. 여기서는 릴리스와 같은 소스 커밋으로 libsigrok를
fx2lafw 드라이버까지 포함해 다시 빌드하고, 릴리스의 `libsigrok-4.dll`만 교체한다.
Windows 기본 libusb에서 fx2lafw가 제대로 동작하도록 패치 3개를 함께 적용한다.

## Rust 버전 (logicview 0.1.4)

`rust/`는 SLogicView + 패치한 libsigrok + Python MCP 서버 구성을 실행 파일 하나로 바꾼 새 버전이다
(libsigrok 없이 USB를 직접 다루고, 디코더만 libsigrokdecode를 실행 중에 읽어 쓴다).
배포 zip(`logicview-0.1.4-windows-x64.zip`, 22 MB)은 압축만 풀면 쓴다. 설치 프로그램, Python,
sigrok-cli, VC++ 재배포 패키지가 필요 없다.

```powershell
cd rust
.\scripts\package.ps1 -RuntimeDir <SLogicView 앱 폴더>          # dist\logicview-<버전>-windows-x64.zip
.\scripts\test-package.ps1 -Zip dist\logicview-0.1.4-windows-x64.zip -Python <python.exe> -Hardware
```

사용법은 zip 안의 `README.txt`(원본 `rust/packaging/README.txt`), 개발 기록은 [rust/README.md](rust/README.md).

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
| `extensions/hla.ps1` | Saleae Logic 2 HLA 확장을 SLogicView 디코더로 설치·목록·삭제, HLA가 쓰는 Python 패키지 추가 ([아래](#saleae-logic-2-확장hla-사용)) |
| `extensions/compat/` | Logic 2 확장 API 호환 계층: `saleae` 모듈(API 대체 구현) + `saleae_bridge.py`(sigrok 디코더 ↔ HLA 변환) |
| `extensions/template/` | 확장마다 만들어지는 디코더 패키지의 `__init__.py`, `pd.py` |
| `extensions/decoders/manchester/` | Manchester·Bi-phase 디코더 (sigrok에 없어서, Manchester 입력 HLA용으로 함께 설치) |
| `extensions/tests/` | 테스트 HLA 8개, numpy HLA(`hla-packages/`), 단위 테스트(`run-tests.ps1`), 합성 캡처·세션 생성기(`make-capture.py`, `make-capture-more.py`) |
| `build-sigrok-cli.sh` | 같은 SLogic 커밋의 libsigrokdecode + sigrok-cli 빌드 (MCP 서버용, MSYS2 UCRT64) |
| `sigrok-cli-patches/0001-windows-1ms-timer-resolution.patch` | sigrok-cli가 Windows 타이머 해상도를 1 ms로 올림. 기본 15.6 ms에서는 USB 이벤트 처리가 밀려 24 MHz 수집이 끊김 |
| `mcp/logicview_mcp.py` | Saleae Logic 2 MCP 서버와 같은 도구를 제공하는 MCP 서버 ([아래](#mcp-서버)) |
| `mcp/start-mcp.cmd` | 앱 폴더에서 MCP 서버 실행 |
| `mcp/tests/test_mcp.py` | MCP 서버 종단 테스트 (HTTP로 모든 도구 호출, `--hardware`로 실제 캡처) |
| `rust/` | Rust + egui로 새로 만든 버전: 수집·파형·디코딩·HLA·MCP를 실행 파일 하나로 ([아래](#rust-버전-logicview-012), [rust/README.md](rust/README.md), [PRD](docs/prd-rust-logicview.md)) |
| `docs/` | [Rust·egui 전환 검토](docs/rust-egui-review.md), [개발지시서(PRD)](docs/prd-rust-logicview.md) |

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

빌드에 쓰는 원본 소스(SLogic `release-2.3.3`이 고정한 libsigrok, libsigrokdecode, sigrok-cli,
SLogicView 커밋)는 이 저장소의 [`sources/`](sources/README.md)에 들어 있다. 빌드 스크립트의 첫 인자로
이 저장소(`logicview`)를 주면 된다. SLogic 저장소를 따로 받아 쓸 수도 있다 (PowerShell 또는 아무 셸):
```sh
git clone https://github.com/sipeed/SLogic.git
git -C SLogic checkout release-2.3.3
git -C SLogic submodule update --init sources/libsigrok sources/libsigrokdecode sources/sigrok-cli
```
빌드 (MSYS2 UCRT64 셸, 이 저장소가 있는 폴더에서. SLogic을 따로 받았으면 첫 인자를 `SLogic`으로). 두 번째 줄(MCP 서버용
sigrok-cli)은 `pacman -S mingw-w64-ucrt-x86_64-python`이 더 필요하다:
```sh
bash logicview/build-libsigrok-fx2lafw.sh logicview logicview/build
bash logicview/build-sigrok-cli.sh logicview logicview/build
```
조립 (PowerShell). `-SigrokCli`를 주면 `sigrok-cli.exe`, `python.exe`(앱에 들어 있는
Python 3.14 런타임으로 실행되는 MSYS2 실행 파일), `mcp\`도 넣는다. `extensions\`는 항상 들어간다:
```powershell
.\logicview\assemble-slogicview.ps1 -ReleaseExe <SLogicView-...-windows-x86_64.exe> `
  -LibsigrokDll .\logicview\build\prefix\bin\libsigrok-4.dll `
  -SigrokCli .\logicview\build\prefix\bin\sigrok-cli.exe -Destination <새 앱 폴더>
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

같은 날 저녁 재시험(장치 위치 `usb/2-2`)에서는 24 MHz가 SLogicView와 sigrok-cli 모두
수백 ms 뒤 끊겼다(장치가 전송을 멈추고 이후 전송은 시간 초과). 12 MHz와 16 MHz는 4번씩
모두 끝까지 받았다. 소프트웨어는 같으므로 USB 연결 상태 때문으로 보았고, 다음 날 시험(아래)에서
허브를 거친 연결이면 24 MHz가 끊기고 본체 포트에 직접 꽂으면 끝까지 받는 것을 확인했다.

### 결과 (2026-11-09, `sources/`에서 다시 빌드)

`sources/`의 소스로 위 빌드·조립 절차를 그대로 따라 만든 앱(MSYS2 UCRT64: gcc 16.2, glib 2.90.1,
SLogicView 설치 파일 `SLogicView-SLogic-2.3.3-windows-x86_64.exe`, fx2lafw 펌웨어
`sigrok-firmware-fx2lafw-bin-0.1.7`). `verify-app.sh`는 6개 파일 모두 `ALL IMPORTS RESOLVED`.
도구 버전이 달라서 DLL은 이전 릴리스 zip(2026-11-08 빌드)과 바이트 단위로 같지 않다.
릴리스 zip은 2026-11-09에 이 빌드로 바꿨다. 이전 zip과 달라진 파일은 `libsigrok-4.dll`, `sigrok-cli.exe` 둘뿐이고
펌웨어는 이전 zip과 같은 PulseView 설치본의 것(15개)을 그대로 넣었다 (아래 시험은 0.1.7 펌웨어로 했다).

| 연결 | 앱 | 설정 | 결과 |
|---|---|---|---|
| 허브 경유 (`usb/1-1.4`) | SLogicView | 20 kHz, 1 M 샘플 | 50.03초, 1,000,448 바이트, 전송 1954건 모두 성공 |
| 허브 경유 (`usb/1-1.4`) | SLogicView | 24 MHz, 50 M 샘플 | 약 0.7초 뒤 끊김 (17,290,240 바이트, 성공 72건, 시간 초과 66건) |
| 허브 경유 (`usb/1-1.4`) | sigrok-cli | 12 / 16 MHz, 50 M 샘플 | 1회 / 2회 모두 끝까지 받음 |
| 허브 경유 (`usb/1-1.4`) | sigrok-cli | 24 MHz, 50 M 샘플 | 새 빌드 5회 중 1회, 이전 zip 3회 중 0회 끝까지 받음 |
| 본체 직결 (`usb/1-8`) | SLogicView | 24 MHz, 50 M 샘플 | 2.10초, 50,186,752 바이트, 전송 209건 모두 성공 |
| 본체 직결 (`usb/1-8`) | sigrok-cli | 24 MHz, 50 M 샘플 | 새 빌드 5회 모두, 이전 zip 5회 중 4회 끝까지 받음 (1회 34.5 MB에서 끊김) |

같은 앱이 허브를 거치면 24 MHz에서 끊기고 본체 포트에 직접 꽂으면 끝까지 받았다.
다시 빌드한 앱은 이전 zip과 같은 수준으로 동작한다. 24 MHz로 수집하려면 애널라이저를
USB 허브 없이 PC 본체 포트에 직접 꽂을 것 (허브 경유라면 16 MHz 이하는 정상).

## Saleae Logic 2 확장(HLA) 사용

Logic 2의 High Level Analyzer(HLA) 확장을 SLogicView에서 디코더로 쓸 수 있다.
`extensions/hla.ps1`이 확장을 libsigrokdecode의 사용자 디코더 폴더
(`%LOCALAPPDATA%\libsigrokdecode\decoders`)에 디코더로 등록하고, 호환 계층이
sigrok 디코더(uart, i2c, spi, can, onewire_link, i2s, parallel)와 함께 설치하는 manchester
디코더의 출력을 Logic 2 분석기와 같은 프레임으로 바꿔 HLA에 넘긴다. HLA가 돌려준 프레임은 SLogicView 주석으로 표시된다. 앱 파일은 건드리지 않는다.

```powershell
# GitHub 저장소 주소, 확장 폴더(extension.json이 있는 곳), zip 파일 모두 가능
.\extensions\hla.ps1 install https://github.com/saleae/hla-text-messages
.\extensions\hla.ps1 install C:\work\my-hla -Protocol i2c   # 입력 프로토콜 직접 지정
.\extensions\hla.ps1 list
.\extensions\hla.ps1 remove "Text Messages"
.\extensions\hla.ps1 package add numpy                     # HLA가 import하는 Python 패키지 (아래)
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
| LIN | `uart` | `header_break`, `header_sync`, `header_pid` (`protected_id`), `data` (`data`, `index`), `checksum` (`checksum`) |
| 1-Wire | `onewire_link` | `reset`, `presence`, `rom_command` (`rom_command`, `description`), `family_code`, `id`, `crc`, `data`, `invalid_rom_command`, `alarm` |
| Manchester | `manchester` (함께 설치) | `data` (`data`) |
| I2S / PCM | `i2s` | `data` (`channel`, `data`) |
| Simple Parallel | `parallel` | `data` (`data`) |

프레임 종류와 키는 Saleae의 [frame format reference](https://www.saleae.com/support/extensions-api/hla-frame-format-reference/analyzer-frame-types)를 따른다.
`-Protocol`에 쓰는 이름은 `uart`, `i2c`, `spi`, `can`, `lin`, `onewire`, `manchester`, `i2s`, `parallel`이다.

`-Protocol auto`(기본값)는 HLA 코드가 확인하는 프레임 종류와, 이름·설명·README·주석에
나오는 분석기 이름으로 입력 프로토콜을 고른다. 여러 개면 프로토콜마다 디코더를 하나씩
만든다(`hla_<확장>_<프로토콜>`). 아무 단서가 없으면 `uart`, `i2c`, `spi`, `can` 넷을 만든다.

제한:
- High Level Analyzer만 지원한다. Measurement 확장(DigitalMeasurement, AnalogMeasurement)은
  SLogicView에 대응하는 기능이 없어서 건너뛴다.
- 1.0 이전의 옛 HLA API(`get_capabilities()`, dict 프레임)는 Logic 2처럼 실행하지 않는다
  (설치할 때 경고, 불러올 때 오류 메시지).
- UART는 sigrok uart의 RX·TX를 모두 HLA로 보낸다. 옵션 `UART line fed to the HLA`로 한쪽만 고를 수 있다.
- SPI는 CS 극성 옵션을 SPI 디코더와 같게 맞춰야 한다(기본 active-low).
- CAN: sigrok can 디코더는 프레임을 한 번에 넘겨서, 필드 경계는 비트 수로 계산한다
  (스터프 비트 무시). `crc`는 디코딩한 필드로 계산한 값(클래식 CAN만)이고, `ack`는 항상 true.
- LIN: uart 디코더의 보드레이트를 LIN 속도에 맞춘다(옵션 `UART line carrying LIN`으로 RX/TX 선택).
  헤더 프레임은 바로 보내고, 응답 바이트는 트랜잭션이 끝날 때(다음 break, 두 프레임 시간의 idle,
  또는 데이터 8개와 체크섬) 한꺼번에 보내며 마지막 바이트를 `checksum`으로 한다. 그래서
  `data_or_checksum`은 보내지 않는다. `protected_id`는 문서대로 6비트 ID다.
- 1-Wire: onewire_link가 리셋과 프레즌스를 한 번에 알려서 `reset`과 `presence`는 같은 구간이다.
  Resume(0xA5)과 Overdrive Skip(0x3C)은 `skip`, Overdrive Match(0x69)는 `match`로 보낸다.
- Manchester: sigrok에는 Manchester 디코더가 없어서 `extensions/decoders/manchester`를 함께 설치한다.
  IEEE 802.3·G.E. Thomas Manchester, 차동 Manchester, Bi-phase Mark·Space와 비트레이트, 워드 비트 수,
  비트 순서, 앞에서 건너뛸 비트 수를 옵션으로 고른다. 비트 경계는 버스트 안의 첫 "한 비트 길이"
  간격으로 맞추고, 그런 간격이 없으면(예: 같은 비트만 계속) 첫 에지를 Manchester는 비트 중간,
  Bi-phase는 비트 경계로 본다.
- I2S / PCM: sigrok i2s는 원래 워드를 넘기므로, 부호는 옵션 `hla_i2s_format`
  (`unsigned`, `signed-8`…`signed-32`)으로 정한다.
- Async RGB LED(`pixel`)는 sigrok의 rgb_led_ws281x가 Python 출력을 내지 않아 아직 없다.
- HLA의 `print()` 출력은 `%LOCALAPPDATA%\libsigrokdecode\saleae_compat\hla.log`에 쌓인다.
  환경 변수 `SALEAE_HLA_TRACE=1`로 앱을 실행하면 HLA가 낸 프레임도 기록된다.

### HLA가 쓰는 Python 패키지

앱 내장 Python은 MSYS2 UCRT64 빌드(3.14)라서, 컴파일된 확장 모듈은 ABI가 `cp314-mingw_x86_64_ucrt_gnu`인
것만 불러온다. PyPI의 Windows wheel(`win_amd64`)은 쓸 수 없다. `hla.ps1 package`가 패키지를
`%LOCALAPPDATA%\libsigrokdecode\saleae_compat\site-packages`에, 필요한 DLL을 `saleae_compat\bin`에 넣고,
호환 계층이 두 곳을 `sys.path`와 DLL 검색 경로에 넣는다.

```powershell
.\extensions\hla.ps1 package add numpy                # MSYS2에 있으면 MSYS2에서 (기본 C:\msys64)
.\extensions\hla.ps1 package add crc -From pypi       # 순수 Python wheel(py3-none-any)
.\extensions\hla.ps1 package add numpy -Msys2 D:\msys64
.\extensions\hla.ps1 package list
.\extensions\hla.ps1 package remove numpy
```

- `-From auto`(기본값)는 MSYS2에 `mingw-w64-ucrt-x86_64-python-<이름>`이 있으면 그것을, 없으면 PyPI를 쓴다.
- MSYS2에서 가져올 때는 그 MSYS2에 `pacman -S`로 설치한 뒤, 패키지와 의존 패키지(Python 자체 제외)의
  `site-packages` 파일과 DLL을 복사한다. MSYS2의 Python 버전이 3.14가 아니면 거절한다.
- PyPI에서는 순수 Python wheel만 받고, 환경 표시(`;`)가 없는 `Requires-Dist`도 따라 받는다.
- 확장 폴더 안에 함께 든 순수 Python 모듈은 지금처럼 그대로 불러온다.

### 확장 테스트

```powershell
# 테스트 HLA 8개를 임시 폴더에 설치하고 단위 테스트 실행 (MSYS2 Python 3.14 사용)
.\extensions\tests\run-tests.ps1
# LIN, 1-Wire, Manchester, I2S, Parallel 합성 캡처 (앱의 sigrok-cli나 lvcli decode로 확인)
C:\msys64\ucrt64\bin\python.exe -I .\extensions\tests\make-capture-more.py more.sr
# 실제 앱 확인: 테스트 HLA를 사용자 폴더에 설치 → 합성 캡처와 세션 생성 → 열기
Get-ChildItem .\extensions\tests\hlas -Directory | ForEach-Object { .\extensions\hla.ps1 install $_.FullName }
C:\msys64\ucrt64\bin\python.exe -I .\extensions\tests\make-capture.py test.sr test.pvs
$env:SALEAE_HLA_TRACE = 1; & <앱 폴더>\slogicview.exe -s test.pvs test.sr
```

결과 (2026-11-08): 단위 테스트 22개 통과. SLogicView 2.3.3에서 합성 캡처(UART "Hi", "ok!",
I2C 쓰기/읽기, SPI 3워드, CAN 표준·확장 프레임)를 테스트 HLA 5개가 기대한 대로 디코딩했고,
HLA 예외는 오류 주석으로, 옛 API 확장은 불러오기 오류로만 처리되었다.

결과 (2026-11-12, 입력 5개와 패키지 추가):

| 시험 | 결과 |
|---|---|
| `run-tests.ps1` | 60개 통과 (새 입력 어댑터, manchester 디코더 4가지 방식·두 idle 레벨, 설치 감지 포함) |
| 앱 폴더의 sigrok-cli(내장 Python 3.14.9)로 `make-capture-more.py` 캡처를 `frame-echo` HLA로 디코딩 | LIN `header_break` → `header_sync` → `header_pid` 18 → `data` 1, 2 → `checksum` 106. 1-Wire `reset`, `presence`, `rom_command` 0xCC `skip`, `data` 0x44. Manchester `data` 165, 60. I2S `signed-16`로 −1, 1, −32768, 32767, 0. Parallel 1, 0, 1, 1 |
| 같은 캡처를 Rust `lvcli decode`(lv-decode)로 | 위와 같은 프레임 |
| `package add numpy` (MSYS2 2.5.4, 930개 파일, DLL 7개) 뒤 `tests/hla-packages/numpy-mean` HLA를 앱의 sigrok-cli로 (PATH는 Windows 시스템 폴더만) | "Hi" 평균 88.5, "ok!" 평균 83.67. 패키지 없는 폴더에서는 `No module named 'numpy'` |
| `package add crc` (PyPI 8.0.0) → `list` → `remove` | 지운 뒤 `site-packages`, `bin`에 남은 파일 없음 (`__pycache__` 포함). Windows PowerShell 5.1과 PowerShell 7 모두 |

## MCP 서버

Saleae Logic 2의 MCP 서버(Settings > Automation > MCP Server)와 같은 도구 이름·인자·결과
형식을 제공하는 MCP 서버. AI 도구(Claude Code, Codex 등)가 이 애널라이저로 캡처하고,
디코딩하고, 결과를 내보낼 수 있다. 실제 작업은 앱 폴더의 `sigrok-cli.exe`가 하므로
SLogicView와 같은 드라이버·디코더·HLA 호환 계층을 쓴다. Python 표준 라이브러리만 쓴다.

```powershell
<앱 폴더>\mcp\start-mcp.cmd              # HTTP, http://127.0.0.1:10530 (Logic 2와 같은 기본 포트)
claude mcp add --transport http logicview http://127.0.0.1:10530
# 또는 클라이언트가 서버를 직접 띄우는 stdio 방식
claude mcp add logicview -- "<앱 폴더>\python.exe" -I "<앱 폴더>\mcp\logicview_mcp.py" --stdio
```

| 도구 | 동작 |
|---|---|
| `get_devices` | 연결된 애널라이저 목록 (`deviceId` 예: `fx2lafw:conn=2.1`) |
| `start_capture` | 디지털 채널, 샘플레이트, 수동/시간 지정/에지 트리거 모드로 캡처 시작 |
| `stop_capture`, `wait_capture` | 캡처 중지(정상 종료), 끝날 때까지 대기 |
| `load_capture`, `save_capture`, `close_capture` | sigrok 세션 파일(.sr) 열기·저장, 캡처 닫기 |
| `add_analyzer`, `remove_analyzer` | "Async Serial", "I2C", "SPI", "CAN"(Logic 2 설정 이름) 또는 sigrok 디코더 id로 디코딩 |
| `add_high_level_analyzer`, `remove_high_level_analyzer` | Logic 2 HLA 확장 폴더를 지정해 분석기 위에 실행 |
| `export_raw_data_csv`, `export_raw_data_binary` | 원시 데이터를 CSV(변화 시점마다 한 줄) 또는 Logic 2 바이너리 형식(`digital_N.bin`)으로 |
| `export_data_table_csv`, `legacy_export_analyzer` | 디코딩 결과를 CSV로 (필터, 열 선택, 진법 지원) |
| `read_analyzer_frames` | logicview 추가 도구: 디코딩 결과를 파일 없이 바로 반환 |

Logic 2와 다른 점:
- 캡처 파일은 `.sal`이 아니라 sigrok 세션 파일 `.sr`이다 (SLogicView에서 바로 열린다).
- 아날로그 채널, 임계 전압, 글리치 필터, 펄스 폭 트리거는 하드웨어에 없어서 오류로 알린다.
  에지 트리거는 상승/하강 + 다른 채널 레벨 조건을 지원하고 10 %는 트리거 이전 데이터다.
- 데이터 테이블 CSV는 `name, type, start_time, duration, value` 열이다 (값은 디코더가 표시하는 글자).
- 캡처를 시작하면 장치를 여는 데 약 0.6초가 걸린 뒤 데이터가 들어온다.
- SLogicView가 장치를 열어 둔 동안에는 캡처할 수 없다 (파일 분석은 가능).
- 캡처와 설치된 HLA는 `%LOCALAPPDATA%\logicview\mcp`에 둔다.

테스트:
```powershell
<앱 폴더>\python.exe -I .\mcp\tests\test_mcp.py --python <앱 폴더>\python.exe `
  --sigrok-cli <앱 폴더>\sigrok-cli.exe --server <앱 폴더>\mcp\logicview_mcp.py --hardware
```
결과 (2026-11-08): 원본 릴리스에서 새로 조립한 앱 폴더로 42개 통과. 합성 캡처로 모든 도구와 오류
처리를 확인했다. 실제 장치로는 장치 검색, 1 MHz 0.25초 캡처(정확히 250,000 샘플), 16 MHz 수동
캡처와 중지를 확인했다. 트리거 캡처는 입력 신호가 없어서 실제로 시험하지 못했다.

결과 (2026-11-09): `sources/`에서 다시 빌드·조립한 앱 폴더(본체 직결 `usb/1-8`)로 같은 42개 통과.
트리거 캡처는 이번에도 입력 신호가 없어서 실제로 시험하지 못했다.

## 라이선스

패치는 libsigrok 소스를 고친 것이므로 libsigrok와 같은 GPL-3.0-or-later를 따른다.
