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

## 라이선스

패치는 libsigrok 소스를 고친 것이므로 libsigrok와 같은 GPL-3.0-or-later를 따른다.
