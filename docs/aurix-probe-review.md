# 검토: logicview 장비를 aurix_debugger의 디버거 하드웨어로 쓰기

- 작성일: 2026-11-28
- 대상: 이 저장소(logicview 0.1.10)와 [najari/aurix_debugger](https://github.com/najari/aurix_debugger) `0600496`, [najari/probe-rs `tricore`](https://github.com/najari/probe-rs/tree/tricore) `b5c5a3e`
- 다음 문서: [개발지시서(PRD)](prd-aurix-probe.md)

## 1. 결론

WeAct Logic Analyzer V2(U3) 보드를 AURIX 디버그 프로브로 쓸 수 있다. 권장 구성은 **logicview가 TAS 서버 역할을 하고, 그 아래에서 WeAct 보드로 AURIX의 2핀 DAP 선로를 직접 구동**하는 것이다.

- aurix_debugger의 도구(probe-rs 포크, `mcd_tas`, `m25_tcf`, aurix-debugger 앱)는 고치지 않는다. 지금 Infineon TasServer에 붙는 것과 똑같이 TCP로 logicview에 붙는다.
- Infineon TasServer(비공개, Windows 전용)와 DAS 어댑터(miniWiggler 등)가 필요 없어진다.
- 선로 프로토콜(DAP 텔레그램)은 2025년에 공개 분석 자료가 나와서, 비공개 문서 없이 구현할 수 있다.
- FX2 애널라이저(`0925:3881`)는 대상에서 뺀다. fx2lafw 펌웨어는 입력 전용이고, 보드의 입력 버퍼가 출력을 허용하는지 확인되지 않았다.

실기 검증은 아직 못 했다. 이 PC에는 AURIX 보드가 연결되어 있지 않고, WeAct 보드는 드라이버 설치에 실패한 상태다(4.3절). 하드웨어 없이 검증할 수 있는 범위와 실기에서 확인해야 할 것은 8절에 나눠 적었다.

## 2. 지금 구조와 바꾸려는 곳

aurix_debugger의 모든 도구는 TAS 프로토콜(TCP 24817)로 Infineon TasServer에 붙고, TasServer가 DAS 어댑터(FTDI 칩)로 JTAG·DAP 신호를 만든다.

```text
지금
  probe-rs 포크 · mcd_tas · m25_tcf · aurix-debugger 앱
        │ TAS (TCP 24817, 공개: Infineon/tas_client_api)
  Infineon TasServer (비공개, Windows)
        │ FTDI MPSSE
  DAS 어댑터 (miniWiggler, 보드 내장 FT2232)
        │ DAP 2핀 또는 JTAG
  AURIX (Cerberus 버스 마스터 → 메모리 맵 OCDS 레지스터)

바꾼 뒤
  probe-rs 포크 · mcd_tas · m25_tcf · aurix-debugger 앱      ← 그대로
        │ TAS (TCP)
  logicview: lv-tas (TAS 서버) → lv-dap (DAP 텔레그램)        ← 새로 만든다
        │ USB (CMSIS-DAP v2)
  WeAct Logic Analyzer V2, 슬롯 1 펌웨어                      ← 있는 장비
        │ DAP 2핀 (D6 = DAP0, D8 = DAP1, D7 = 리셋)
  AURIX
```

이렇게 나눌 수 있는 이유는 두 가지다.

- **TriCore 디버그는 전부 메모리 맵 레지스터다.** 정지·실행·스텝·브레이크포인트·레지스터가 모두 CSFR 창의 메모리 읽기·쓰기라서, 프로브가 줄 것은 "메모리 읽기·쓰기 + 리셋 + JTAG ID"뿐이다. probe-rs 포크의 `TriCoreBusAccess` 트레이트(`probe-rs/src/architecture/tricore/bus.rs`)가 이 경계이고, TAS 프로브는 이를 PL0 프레임으로 옮길 뿐이다.
- **TAS 와이어는 평문이고 공개돼 있다.** 포크의 `probe-rs-infineon/src/tas/protocol.rs`가 쓰는 메시지는 9종(SERVER_CONNECT, GET_TARGETS, SESSION_START, PING, SET_DEVICE_KEY, DEVICE_CONNECT, PL0 프레임)이고, 포크의 테스트에 가짜 TAS 서버(`tas/tests.rs`)가 있어 기대하는 응답 형식을 그대로 알 수 있다.

## 3. 선로 프로토콜: 공개된 범위

aurix_debugger의 기술 검토(`PROBE_RS_TRICORE_REVIEW.md`, 2026-10-23)는 "네이티브 JTAG/DAP는 프로토콜 일부가 비공개라 장기 과제"로 두었다. 그 뒤 확인한 공개 자료는 다음과 같다.

| 자료 | 내용 | 확인 방법 |
| --- | --- | --- |
| [epozzobon/tricore-things](https://github.com/epozzobon/tricore-things) (`5e82586`, 2025-12-23) | miniWiggler의 USB 트래픽을 분석해 DAP 텔레그램을 다시 구현한 Python 코드, 분석 PDF, TC297·TC397 실제 캡처 7개 | 저장소를 받아 `ftdi_dap.py`, `dap_parser.py`, `ops_parser.py`, `mpsse_parser.py`, `run_demo.py`를 직접 읽음 |
| OpenOCD Gerrit [9958](https://review.openocd.org/c/openocd/+/9958) "jtag: add Infineon DAP (ifxdap) protocol library" (2026-09, GPL-2.0-or-later) | 위 Python을 C로 옮긴 패치 | 조사 에이전트 보고로만 확인. 직접 읽지 않음 |
| Lauterbach `app_tricore_ocds.pdf`, Infineon AP32536·AP32442 | 모드 선택(TRST 레벨), DAP 10핀 커넥터, SPD 타이밍 | 조사 에이전트 보고로만 확인 |

`tricore-things`에서 직접 확인한 프로토콜 요점:

- **텔레그램** (LSB 먼저): 시작 비트 `1`, 명령 5비트, 인자 길이 6비트, 인자(0~63비트), CRC6. 마지막 비트가 1이면 0을 하나 붙이고, 앞쪽을 0으로 채워 바이트 경계에 맞춘다.
- **CRC6**: 초기값 `0x3F`, 비트마다 `b = (bit ^ crc ^ (crc >> 1)) & 1; crc = (b << 5) | (crc >> 1)`, 끝에 `crc ^ (crc >> 1)`.
- **응답**: 0 채움 뒤 시작 비트 `1`, 값(16·32비트), CRC6. 호스트는 정해진 바이트 수만큼 클럭을 주고 시작 비트를 찾는다.
- **명령**: 레지스터 쓰기 8, 블록 쓰기 9, 블록 읽기 10, 동기 16, DAPISC 17, JTAG IR 19, JTAG 리셋 21, JTAG DR 교환 2, 레지스터 읽기 26, IO 클라이언트 선택 28.
- **메모리 접근**: 레지스터 1에 주소를 쓰고, 레지스터 4·6·8에 쓰면 32·16·8비트 쓰기, 레지스터 5·7·9를 읽으면 32·16·8비트 읽기다. 블록은 명령 9·10으로 최대 1024바이트, 블록 읽기 응답 끝에 CRC32가 붙는다.
- **연결 순서**: 느린 클럭(720 kHz) → 리셋 → DAPISC 2회 → IO 클라이언트 1 → 상태 레지스터(11) 확인 → OJCONF(레지스터 14) 설정 → 빠른 클럭(5 MHz).
- **잠긴 칩**: 상태 레지스터가 `0x80`이면 디버그 암호 8워드를 COMDATA로 보낸다.

**라이선스:** `tricore-things`에는 LICENSE 파일이 없다. 코드와 캡처 파일을 이 저장소에 복사하지 않는다. 프로토콜 사실만 참고해 Rust로 새로 쓰고, 캡처는 개발 PC에 받아 두고 시험에서 읽기만 한다(없으면 그 시험은 건너뛴다).

**모르는 것 (검토 시점):** 버스 오류를 알리는 상태 비트의 정확한 의미, EEC(주소 맵 1, MCDS 레지스터)에 닿는 방법, DAP로 JTAG ID를 읽는 공식 도구의 순서.

**구현하면서 캡처에서 확인한 것 (2026-11-28):** 캡처 6개를 직접 풀어 보니 위 요점 중 일부는 Infineon 도구의 실제 동작과 달랐고, 구현은 도구를 따랐다.

- JTAG ID는 JTAG 텔레그램으로 읽는다(JTAG 리셋 또는 IR `0x04` 뒤 DR 32비트). TC297B `0x201E9083`, TC397B `0x20205083`.
- 도구는 리셋 없이 붙는다: 동기 → DAPISC → JTAG 리셋 → 접근. 리셋은 요청받았을 때만 한다.
- 도구는 32비트 접근을 레지스터 4·5가 아니라 1워드 블록 명령으로 한다.
- 거절된 접근은 응답이 없고, 도구는 그 뒤 상태 레지스터를 읽는다. 값은 칩마다 다르다(TC29x `0x20`·`0x60`·`0xA0`, TC39x `0xFE24`). 비트의 뜻은 여전히 모른다.
- 리셋 순서의 DAPISC 설정 뒤에는 응답이 8비트 일찍 온다.
- EEC에 닿는 방법은 캡처에 없다. 도구가 IO 클라이언트 2에서 레지스터 `0xB`·`0xF`를 읽는 것만 보인다(값 `0x1040`·`0x260`).

## 4. 장비 검토

### 4.1 WeAct Logic Analyzer V2 (CH32H417)

[WeActStudio.LogicAnalyzerV2](https://github.com/WeActStudio/WeActStudio.LogicAnalyzerV2) 저장소의 README와 `Slot2_Example`을 받아 읽었다. 회로도는 공개돼 있지 않다.

| 항목 | 확인한 내용 |
| --- | --- |
| 단자 | D0~D15가 MCU 핀에 대응한다(D6 = PF0, D7 = PF1, D8 = PF2 등). 슬롯 1 펌웨어가 D6·D7·D8을 SWD 출력으로 쓰므로 이 단자들은 출력으로 구동된다 |
| 슬롯 0 (`0x08006000`) | 로직 애널라이저. logicview가 쓰는 펌웨어 |
| 슬롯 1 (`0x08052000`) | USB-시리얼 3채널 + **CMSIS-DAP v2 디버거**. WinUSB라 드라이버 설치가 필요 없다. SWD 3핀만 지원한다: D6 = SWCLK, D8 = SWDIO, D7 = nRESET. JTAG와 SWO는 없다 |
| 슬롯 2 (`0x0809E000`) | 사용자 앱 자리(304 KB). SDK 예제 소스(V3F·V5F 두 코어, USB HS CDC, 링커 스크립트, 주변장치 라이브러리)가 공개돼 있다 |
| IO 레벨 | 슬롯 1에서 버튼 짧게 누르기로 3.3 → 2.5 → 1.8 → 1.2 V. 시리얼 포트 보레이트 `2010`~`2033`으로 1.0~3.3 V를 0.1 V 단위로 정할 수도 있다 |
| 슬롯 바꾸기 | 버튼 2초, 시리얼 포트 보레이트 `2049`~`2051`, 또는 슬롯에 펌웨어 쓰기. logicview의 `lvcli fw-update`가 슬롯 쓰기를 이미 한다 |

CMSIS-DAP의 SWD 선로는 "클럭 1개 + 양방향 데이터 1개"로, AURIX DAP 2핀(DAP0 클럭, DAP1 양방향 데이터)과 물리적으로 같은 모양이다. CMSIS-DAP 1.2부터 있는 `DAP_SWD_Sequence` 명령(0x1D)은 SWCLK에 클럭을 주면서 SWDIO로 임의의 비트열을 내보내거나 읽으므로, **슬롯 1 펌웨어를 그대로 두고 DAP 텔레그램을 보낼 수 있다.** 리셋 선은 `DAP_SWJ_Pins`(0x10)로 구동한다.

### 4.2 FX2 애널라이저

대상에서 뺀다. fx2lafw 펌웨어는 포트를 입력으로만 쓰고, Saleae 클론 보드는 흔히 방향이 고정된 입력 버퍼를 거친다. 이 보드의 회로를 확인하지 않았다. 다만 FX2 애널라이저는 **검증 도구**로 쓸 수 있다: WeAct 보드가 프로브로 동작하는 동안 DAP0·DAP1을 FX2로 수집해 파형을 본다(24 MHz 샘플이라 DAP 클럭 5 MHz 이하에서).

### 4.3 이 PC의 상태 (2026-11-28)

| 장치 | 상태 |
| --- | --- |
| WeAct 보드 (`1a86:5537`, 슬롯 0) | 꽂혀 있으나 드라이버 설치 실패(`CM_PROB_FAILED_INSTALL`). `CH375DLL64.DLL` 없음, 관리자 권한 없음. `lvcli devices`가 찾지 못한다 |
| AURIX 보드, DAS 어댑터 | 연결돼 있지 않다 (`058B` 장치 없음) |
| FT2232 (`0403:6010`, "FACTORYAIOT Pro") | 연결돼 있다. 이번 작업에는 쓰지 않는다 |

슬롯 1로 바꾸면 CMSIS-DAP는 WinUSB로 붙으므로 이 PC에서도 드라이버 없이 열릴 것으로 보인다(확인하지 않음). 슬롯을 바꾸려면 보드 버튼을 2초 눌러야 한다.

## 5. 연결 방식 비교

| 방식 | 구성 | aurix_debugger 변경 | 쓸 수 있는 도구 | 판단 |
| --- | --- | --- | --- | --- |
| **A. logicview가 TAS 서버** | `lv-tas`가 TCP에서 TAS를 받고 `lv-dap`으로 선로를 구동 | 없음. 셀렉터의 호스트·포트만 바꿈 | probe-rs 포크, `mcd_tas`, `m25_tcf`, 앱 전부 | **권장** |
| B. probe-rs 포크에 네이티브 프로브 | `probe-rs-infineon`에 `TriCoreBusAccess`를 구현한 새 프로브 | 포크에 새 프로브와 의존 크레이트 추가 | probe-rs 포크 계열만 | 나중에. TCP 왕복을 없애 조금 빠르다 |
| C. MCD DLL | `mcd_tas.dll`처럼 MCD API를 구현 | 없음 | MCD 클라이언트 | A가 되면 `mcd_tas`가 그대로 붙으므로 불필요 |

A를 고르는 이유:

- 도구 네 개가 한 번에 붙는다. B는 probe-rs 계열만 된다.
- 포크는 MIT OR Apache-2.0, 이 저장소의 Rust 코드는 GPL-3.0-or-later다. A는 프로세스가 나뉘어 있어 라이선스가 섞이지 않는다.
- 포크에 이미 있는 TAS 클라이언트와 시험을 그대로 써서, 하드웨어 없이 "실제 클라이언트 ↔ 우리 서버"를 검증할 수 있다.
- 장치를 여는 프로세스가 logicview 하나라서, 같은 보드를 로직 애널라이저(슬롯 0)와 프로브(슬롯 1)로 번갈아 쓰는 흐름을 한곳에서 다룬다.

A의 비용은 TAS 서버를 맞춰야 한다는 것이다: 세션 여러 개(포크는 코어마다 세션을 연다), 리셋 뒤 다른 세션에 `DEV_RESET`(0x44) 알리기, 서버 레지스터(맵 15)의 클럭 설정.

## 6. 선로 구동 방식 비교

| 방식 | 펌웨어 작업 | 속도 | 위험 | 판단 |
| --- | --- | --- | --- | --- |
| **1. 슬롯 1 CMSIS-DAP의 `DAP_SWD_Sequence`** | 없음 | 명령 하나에 64비트씩, USB 왕복이 텔레그램마다 있다. 측정 전 | 펌웨어가 0x1D 명령을 지원하는지, 클럭 에지가 AURIX와 맞는지 실기 확인 필요 | **1차** |
| 2. 슬롯 2 자체 펌웨어 | 큼: 두 코어 프로젝트, USB, 비트 구동, 빌드 도구(MounRiver 프로젝트 → 로컬 `riscv-none-elf-gcc` 12.2로 옮기기) | 텔레그램을 보드 안에서 처리해 가장 빠르다 | 펌웨어 디버깅 수단이 없다(SWD 핀이 USB 2.0과 겹침) | 1이 느리거나 안 될 때 |
| 3. FTDI MPSSE (miniWiggler, Tigard) | 없음 | 공개 캡처와 같은 경로라 가장 확실 | logicview 장비가 아니다. TasServer가 어댑터를 쥐고 있으면 못 연다 | 비교·검증용으로만 |
| 4. `DAP_SWJ_Pins`로 핀 하나씩 | 없음 | 비트마다 USB 왕복. 매우 느리다 | 낮음 | 1이 안 될 때 연결 확인용 |

1을 먼저 하는 이유는 보드에 아무것도 쓰지 않고 시작할 수 있기 때문이다. 선로 계층을 트레이트(`Wire`)로 나눠 두면 2·3은 구현만 더하면 된다.

**클럭 에지:** 공개 구현은 FTDI 명령 `0x19`(하강 에지에 내보내기)와 `0x28`(상승 에지에 읽기)을 쓴다. CMSIS-DAP의 SWD 시퀀스는 클럭을 내린 직후 데이터를 바꾸고, 올리기 직전에 읽는다. 둘 다 "하강에 바꾸고 상승에 잡는" 꼴이라 맞을 것으로 보지만, 클럭 대기 레벨이 다르다(CMSIS-DAP는 High). 실기에서 FX2로 파형을 떠서 확인한다.

## 7. 배선과 전기

| WeAct 단자 | AURIX 신호 | DAP 10핀 커넥터 | 비고 |
| --- | --- | --- | --- |
| D6 (SWCLK) | DAP0 (TCK) | 4 | 클럭 |
| D8 (SWDIO) | DAP1 (TMS) | 2 | 양방향 데이터 |
| D7 (nRESET) | PORST | 10 | 리셋 |
| GND | GND | 3, 5, 9 | |
| — | VREF | 1 | 보드의 IO 전압. WeAct의 IO 레벨을 여기에 맞춘다 |

커넥터 핀 번호는 조사 보고(AP32536, Lauterbach)에 따른 것이다. 연결 전에 대상 보드의 회로도로 확인한다.

- **DAP 모드 진입:** PORST가 풀릴 때 TRST가 High면 DAP, Low면 JTAG다. WeAct 슬롯 1에는 TRST를 구동할 핀이 없으므로, 대상 보드가 TRST를 High로 두는지(풀업, 또는 DAP 커넥터 보드의 기본값) 확인해야 한다.
- **전압:** WeAct의 IO는 최대 3.3 V다. IO가 5 V인 보드(ShieldBuddy TC275는 5 V 보드)에 직접 연결하면 안 된다. CH32H417 핀의 5 V 허용 여부를 확인하지 않았다. 3.3 V 보드(TriBoard의 3.3 V 설정)부터 시험한다.
- **보드 내장 어댑터와 충돌:** ShieldBuddy·TriBoard의 내장 FTDI가 같은 선을 구동한다. 내장 어댑터를 쓰지 않는 상태(TasServer 종료, 보드의 선택 점퍼)에서 외부 커넥터로 연결한다.

## 8. 검증 계획

| 단계 | 하드웨어 | 방법 |
| --- | --- | --- |
| 프로토콜 | 없음 | 텔레그램·CRC6·블록 응답 분석을 단위 시험으로. `tricore-things`의 실제 캡처(TC297·TC397)를 읽어, 우리 인코더가 같은 바이트를 만들고 우리 파서가 캡처의 응답을 같은 값으로 읽는지 대조 |
| 선로 계층 | 없음 | DAP 텔레그램 수준에서 동작하는 가짜 타깃(메모리, 상태 레지스터, 리셋)에 연결·읽기·쓰기 |
| TAS 서버 | 없음 | 포크의 실제 TAS 클라이언트(`probe-rs-infineon`의 예제)를 우리 서버 + 가짜 타깃에 붙여 읽기·쓰기·리셋·여러 세션 |
| 보드 연결 | WeAct(슬롯 1) + 3.3 V AURIX 보드 | `lvcli aurix` 명령으로 연결, JTAG ID, RAM 쓰고 읽기. FX2로 DAP0·DAP1 파형 확인 |
| 디버깅 | 위와 같음 | `probe-rs` 포크와 aurix-debugger 앱을 logicview의 TAS 포트에 붙여 정지·스텝·브레이크포인트, aurix_debugger의 기존 보드 시험 스크립트(`aurix-run`) |
| 속도 | 위와 같음 | 포크의 `perf_measure`로 메모리 읽기·쓰기 속도와 스텝 시간을 TasServer + miniWiggler(6 MHz에서 131 KiB/s, 30 MHz에서 약 1.97 MiB/s) 값과 비교 |

## 9. 위험과 미확인 사항

| 항목 | 영향 | 대응 |
| --- | --- | --- |
| 슬롯 1 펌웨어가 `DAP_SWD_Sequence`를 지원하지 않을 수 있다 | 1차 방식 불가 | 연결할 때 명령을 시험해 보고, 안 되면 `DAP_SWJ_Pins` 방식으로 연결만 확인한 뒤 슬롯 2 펌웨어로 간다 |
| 클럭 에지·대기 레벨이 안 맞을 수 있다 | 통신 불가 또는 간헐 오류 | FX2로 파형 확인. 안 맞으면 슬롯 2 펌웨어 |
| 5 V 보드 | 장비 손상 | 3.3 V 보드부터. 5 V는 레벨 변환기 |
| TRST를 구동하지 못한다 | JTAG 모드로 뜨는 보드에서 DAP 불가 | 보드 설정으로 해결. 안 되면 슬롯 2 펌웨어에서 남는 단자로 TRST 구동 |
| USB 왕복 지연 | 스텝·메모리 보기가 느리다 | 한 USB 패킷에 텔레그램 여러 개를 묶는다. 부족하면 슬롯 2 펌웨어 |
| 디버그 암호로 잠긴 칩 | 연결 불가 | 암호 전송은 공개 구현에 있다. TAS의 `SET_DEVICE_KEY`와 잇는 것은 2차 |
| EEC(맵 1)·MCDS | MCDS 기록 불가 | 범위 밖. 지금의 내장 어댑터에서도 안 된다 |
| 공개 분석 자료에 근거한 구현 | 칩·스텝마다 다른 동작이 있을 수 있다 | 캡처에 있는 TC297·TC397 동작부터. TC275·TC36x는 실기에서 확인 |
| TasServer와 포트 충돌(24817) | 둘 다 못 뜬다 | 포트를 옵션으로. 기본 포트가 쓰이고 있으면 알린다 |

## 10. 출처

- [epozzobon/tricore-things](https://github.com/epozzobon/tricore-things) `5e82586` — 직접 읽음
- [WeActStudio/WeActStudio.LogicAnalyzerV2](https://github.com/WeActStudio/WeActStudio.LogicAnalyzerV2) README, `Firmware/README.md`, `Slot2_Example` — 직접 읽음
- najari/probe-rs `tricore` `b5c5a3e`: `probe-rs/src/architecture/tricore/bus.rs`, `probe-rs-infineon/src/tas/{mod,protocol,tests}.rs` — 직접 읽음
- najari/aurix_debugger `docs/PROBE_RS_TRICORE_REVIEW.md`, `PROBE_RS_TRICORE_PRD.md`, `REPORT.md`, `TRACE32_COMPARISON.md` — 직접 읽음
- OpenOCD Gerrit 9958 (ifxdap), Lauterbach `app_tricore_ocds.pdf`, Infineon AP32536·AP32442, [probe-rs issue #2796](https://github.com/probe-rs/probe-rs/issues/2796) — 조사 에이전트 보고
