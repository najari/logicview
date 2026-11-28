# 개발지시서 (PRD): AURIX 디버그 프로브 (logicview × aurix_debugger)

- 작성일: 2026-11-28
- 근거 문서: [검토](aurix-probe-review.md)
- 코드 위치: 이 저장소의 `rust/` (새 크레이트 `lv-dap`, `lv-tas`, `lvcli`의 `aurix` 명령)
- 상대 저장소: [najari/aurix_debugger](https://github.com/najari/aurix_debugger), [najari/probe-rs `tricore`](https://github.com/najari/probe-rs/tree/tricore) — 고치지 않는다

## 0. 구현 현황 (2026-11-28)

A0~A2를 끝냈다. A3(실기)은 장비 연결이 필요해 하지 않았다. 시험 결과와 실기에서 할 일은 [`rust/README.md`의 "AURIX 디버그 프로브"](../rust/README.md) 절에 있다.

| 단계 | 상태 | 확인한 방법 |
| --- | --- | --- |
| A0 프로토콜 | 완료 | 단위 시험, 가짜 타깃, 공개 캡처 6개 대조(텔레그램 97,326개의 바이트 일치, 블록 읽기 9,929개의 CRC32, 캡처 응답을 재생한 연결·블록 전송) |
| A1 선로·도구 | 완료, 실기 미확인 | 가짜 CMSIS-DAP 펌웨어를 거친 시험, `lvcli aurix … --sim` |
| A2 TAS 서버 | 완료 | 우리 시험 클라이언트, 그리고 고치지 않은 probe-rs 포크(`b5c5a3e`)의 `bus_read`·`tas_info`·`tas_clock`·`tas_reset_errors` |
| A3 실기 | 하지 않음 | WeAct 보드(슬롯 1)와 3.3 V AURIX 보드가 필요 |

| 요구사항 | 상태 |
| --- | --- |
| DAP-1 ~ DAP-11 | 구현. DAP-9는 아래처럼 바꿨다 |
| DAP-12 (디버그 암호) | 하지 않음. 잠긴 칩은 오류로 알린다 |
| WIRE-1 ~ WIRE-6 | 구현 |
| WIRE-7 (핀 하나씩 보내는 느린 방식) | 하지 않음 |
| TAS-1 ~ TAS-8 | 구현 |
| TAS-9 (뽑았다 꽂으면 다시 열기) | 하지 않음 |
| CLI-1 ~ CLI-7 | 구현 |
| NFR-1 ~ NFR-5 | 지킴. `cargo test --workspace` 122개 통과, clippy 경고 없음 |

이 문서와 다르게 한 것 (캡처를 분석하고 바꿨다):

- **DAP-9 JTAG ID**: `CBS_JTAGID` 메모리 읽기가 아니라 Infineon 도구처럼 JTAG 텔레그램(IR `0x04`, DR 32비트)으로 읽는다. 메모리 접근이 막힌 칩에서도 읽힌다.
- **32비트 접근**: 레지스터 4·5가 아니라 1워드 블록 명령으로 한다(도구가 그렇게 한다). 레지스터 방식은 8·16비트에만 쓴다.
- **접근 오류**: 상태 비트를 해석하지 않는다. 응답이 없으면 실패로 보고 `IOINFO`를 읽어 값을 오류 문장에 싣는다(도구의 순서). 칩마다 값이 달랐다.
- **응답 위치**: 리셋 순서로 설정한 뒤에는 응답이 8비트 일찍 오고 JTAG ID 응답에는 시작 비트가 없다. 링크가 설정을 기억한다.
- **TAS 대상 조회**: GET_TARGETS와 SESSION_START가 아직 연결 전이면 리셋 없이 붙어 JTAG ID를 채운다(TasServer도 목록에 장치 종류를 싣는다).
- **`--sim`**: JTAG ID를 값으로 받는다(`--sim 0x401DA083`). 포크가 JTAG ID로 칩을 고르기 때문이다.

## 1. 목적

WeAct Logic Analyzer V2 보드를 AURIX(TC2xx·TC3xx) 디버그 프로브로 쓴다. logicview가 TAS 서버가 되어, aurix_debugger의 도구가 Infineon TasServer와 DAS 어댑터 없이 이 보드로 디버깅한다.

이번 버전에서 얻어야 하는 것:

1. DAP 텔레그램 프로토콜을 Rust로 구현하고, 공개된 실제 캡처와 대조해 맞는지 확인한다.
2. WeAct 보드의 슬롯 1 펌웨어(CMSIS-DAP v2)로 DAP 선로를 구동한다. 보드 펌웨어는 고치지 않는다.
3. TAS 서버를 구현해 probe-rs 포크의 TAS 프로브가 고치지 않은 채 붙는다.
4. 명령줄에서 연결·식별·메모리 읽기·쓰기·리셋을 한다.

## 2. 범위

### 포함

| 영역 | 내용 |
| --- | --- |
| 프로토콜 | DAP 텔레그램 인코딩·응답 분석, CRC6, 블록 읽기 CRC32, 연결 순서, 8·16·32비트와 블록 메모리 접근, 리셋 |
| 선로 | `Wire` 트레이트, CMSIS-DAP v2 구현(`DAP_SWD_Sequence`, `DAP_SWJ_Pins`, `DAP_SWJ_Clock`), 가짜 타깃 |
| TAS 서버 | probe-rs 포크가 쓰는 메시지 전부, 세션 여러 개, 리셋 알림, 클럭 레지스터 |
| 도구 | `lvcli aurix probes \| info \| read \| write \| reset \| serve`, 캡처 대조 도구 |
| 문서 | 배선, 사용법, 시험 결과 |

### 제외 (이번 버전)

- 슬롯 2 자체 펌웨어, FTDI MPSSE로 직접 구동 (`Wire` 구현을 더할 수 있게만 둔다)
- JTAG 모드, SPD(한 핀), DAP 3핀 이상
- 디버그 암호 해제, EEC(주소 맵 1)·MCDS
- probe-rs 포크 안의 네이티브 프로브
- logicview 앱(GUI)과 MCP 서버의 프로브 화면·도구
- DAP 선로 디코더(sigrok 디코더)
- 5 V 보드 지원 (레벨 변환기 없이 연결하지 않는다)

## 3. 사용 장면

| 사용자 | 장면 |
| --- | --- |
| AURIX 개발자 | WeAct 보드를 DAP 커넥터에 연결하고 `lvcli aurix serve`를 띄운 뒤, aurix-debugger 앱이나 VS Code(probe-rs)로 디버깅한다 |
| 자동 시험 | `lvcli aurix read 0xF0000464 4`로 연결을 확인하고 `aurix-run` 스크립트를 돌린다 |
| Linux·macOS 사용자 | TasServer 없이 AURIX를 디버깅한다 (이번 버전은 Windows에서만 시험) |

## 4. 구조

```text
rust/
  crates/
    lv-dap/      DAP 텔레그램 프로토콜과 선로
      telegram.rs   인코딩, 응답 분석, CRC6
      wire.rs       Wire 트레이트, 비트열
      link.rs       연결 순서, 메모리 접근, 리셋, 오류 처리
      cmsis.rs      CMSIS-DAP v2 (nusb)
      sim.rs        가짜 타깃 (텔레그램 수준)
      mwtrace.rs    miniWiggler USB 캡처 읽기 (시험·도구용)
    lv-tas/      TAS 서버
      proto.rs      PL2·PL1·PL0 메시지
      server.rs     TCP 서버, 세션, 장치 잠금
  apps/lvcli/    aurix 하위 명령
```

- `lv-dap`은 USB를 모른 채 시험할 수 있어야 한다. 프로토콜 로직은 `Wire`만 쓴다.
- `lv-tas`는 `lv-dap`의 메모리 접근 인터페이스(`Target` 트레이트)만 쓴다. 가짜 타깃과 실제 장치를 같은 서버로 시험한다.
- 다른 logicview 크레이트(`lv-u3` 등)는 고치지 않는다. 슬롯 바꾸기는 이번 버전에서 사용자가 버튼으로 한다.

## 5. 기능 요구사항

우선순위: P0 = 이 버전에 꼭, P1 = 이 버전 목표, P2 = 여유가 있으면.

### 5.1 프로토콜 (DAP)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| DAP-1 | 텔레그램 인코딩: 시작 비트, 명령 5비트, 인자 길이 6비트, 인자, CRC6, LSB 먼저. 인자 없는 명령은 길이 칸이 `0x3F` | P0 |
| DAP-2 | 응답 분석: 0 채움을 건너뛰고 시작 비트를 찾아 16·32비트 값과 CRC6을 확인한다. 시작 비트가 없으면 "응답 없음"으로 구분한다 | P0 |
| DAP-3 | 레지스터 읽기(26)·쓰기(8), IO 클라이언트 선택(28), DAPISC(17), 동기(16) | P0 |
| DAP-4 | 8·16·32비트 메모리 읽기·쓰기: 주소 레지스터 1, 쓰기 레지스터 8·6·4, 읽기 레지스터 9·7·5 | P0 |
| DAP-5 | 블록 읽기(10): 4바이트 정렬, 최대 1024바이트, 응답의 32비트 조각을 모으고 CRC32를 확인한다 | P0 |
| DAP-6 | 블록 쓰기(9): 4바이트 정렬, 최대 1024바이트, 워드마다 조각을 보낸다 | P0 |
| DAP-7 | 연결: 느린 클럭, 리셋 여부 선택, DAPISC 순서, 상태 확인, OJCONF, 빠른 클럭. 잠긴 칩(상태 `0x80`)은 "디버그 암호가 필요하다"는 오류로 알린다 | P0 |
| DAP-8 | 리셋 후 정지(`reset_and_halt`)와 리셋 후 실행 | P0 |
| DAP-9 | JTAG ID: `CBS_JTAGID`(`0xF0000464`) 읽기 | P0 |
| DAP-10 | 접근 오류: 응답 없음, CRC 틀림, 상태 레지스터의 오류를 구분해 돌려준다. CRC 틀림은 한 번 다시 시도한다 | P1 |
| DAP-11 | 정렬되지 않았거나 4의 배수가 아닌 접근을 8·16·32비트와 블록으로 나눈다 | P1 |
| DAP-12 | 디버그 암호 8워드 전송 | P2 |

### 5.2 선로 (WIRE)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| WIRE-1 | `Wire` 트레이트: 비트열 내보내기, 비트 읽기, 리셋 선, 클럭 설정, 모아서 한 번에 실행 | P0 |
| WIRE-2 | CMSIS-DAP v2: USB에서 CMSIS-DAP 인터페이스(문자열에 "CMSIS-DAP", bulk 엔드포인트)를 찾아 연다. `DAP_Info`로 패킷 크기와 이름을 읽는다 | P0 |
| WIRE-3 | `DAP_Connect`(SWD), `DAP_SWJ_Clock`, `DAP_SWJ_Pins`(nRESET), `DAP_SWD_Sequence`. 한 패킷에 들어가는 만큼 시퀀스를 묶는다 | P0 |
| WIRE-4 | 펌웨어가 `DAP_SWD_Sequence`를 모르면(응답 `0xFF`) 그 사실을 오류로 알린다 | P0 |
| WIRE-5 | 가짜 타깃: 텔레그램을 받아 메모리(희소), 상태 레지스터, 리셋, 주입한 오류(응답 없음, CRC 틀림, 버스 오류)를 흉내 낸다 | P0 |
| WIRE-6 | 프로브 목록: VID:PID, 제품 이름, 일련번호 | P1 |
| WIRE-7 | `DAP_SWJ_Pins`만으로 비트를 하나씩 보내는 느린 방식 | P2 |

### 5.3 TAS 서버 (TAS)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| TAS-1 | PL2 길이 틀, PL1 요소. SERVER_CONNECT(0x80), GET_TARGETS(0x86), SESSION_START(0xA1), PING(0xA2), SET_DEVICE_KEY(0xAD), DEVICE_CONNECT(0xA8) | P0 |
| TAS-2 | PL0 프레임: 주소 맵(0x11), 기준 주소(0x15), WR8·16·32·64, RD8·16·32·64, WRBLK, RDBLK. 전송마다 상태(0x80 성공), 실패하면 그 뒤 전송은 실행하지 않는다 | P0 |
| TAS-3 | DEVICE_CONNECT 옵션 0 = 리셋 없이 연결, 3 = 리셋 후 정지. JTAG ID를 돌려준다 | P0 |
| TAS-4 | 세션 여러 개: 연결마다 스레드, 장치는 잠금 하나로 차례대로 | P0 |
| TAS-5 | 한 세션이 리셋하면 다른 세션의 다음 요청 하나를 `DEV_RESET`(0x44)으로 거절한다 | P0 |
| TAS-6 | 맵 15의 `ACC_HW_FREQUENCY`(`0x000C0100`): 읽으면 지금 클럭(Hz), 쓰면 선로 클럭 변경 | P0 |
| TAS-7 | 지원하지 않는 맵·명령은 오류 코드로 답하고 연결을 유지한다 | P0 |
| TAS-8 | 대상 이름은 `LV DAP <프로브 이름> <일련번호>` | P1 |
| TAS-9 | 장치가 뽑히면 요청에 오류로 답하고, 다시 꽂으면 다음 DEVICE_CONNECT에서 다시 연다 | P2 |

### 5.4 도구 (CLI)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| CLI-1 | `lvcli aurix probes`: CMSIS-DAP 프로브 목록 | P0 |
| CLI-2 | `lvcli aurix info [--reset]`: 연결하고 JTAG ID, 상태 레지스터, 클럭을 보여준다 | P0 |
| CLI-3 | `lvcli aurix read <주소> <길이>`, `write <주소> <hex>` | P0 |
| CLI-4 | `lvcli aurix reset [--halt]` | P0 |
| CLI-5 | `lvcli aurix serve [--port 24817] [--sim]`: TAS 서버. `--sim`은 장치 없이 가짜 타깃으로 | P0 |
| CLI-6 | `lvcli aurix trace <캡처.pcap>`: miniWiggler USB 캡처를 텔레그램과 메모리 접근으로 풀어 보여준다 | P1 |
| CLI-7 | 공통 옵션 `--probe <일련번호>`, `--speed <kHz>` | P1 |

## 6. 비기능 요구사항

| ID | 요구사항 |
| --- | --- |
| NFR-1 | 프로토콜·서버 시험은 하드웨어 없이 `cargo test --workspace`로 돈다 |
| NFR-2 | 공개 캡처 대조 시험은 환경 변수 `LV_TRICORE_TRACES`가 가리키는 폴더가 있을 때만 돌고, 없으면 건너뛴다. 캡처와 원본 코드는 저장소에 넣지 않는다 |
| NFR-3 | 기존 logicview 기능(수집, 디코딩, MCP)의 빌드와 시험이 그대로 통과한다. `cargo clippy --workspace --all-targets` 경고 없음 |
| NFR-4 | 쓰기는 요청한 주소에만 한다. 연결 순서가 건드리는 레지스터(OJCONF 등)는 문서에 적는다 |
| NFR-5 | 오류 문장은 한국어로, 무엇이 안 됐고 무엇을 확인할지 적는다 (기존 `lvcli`와 같게) |

## 7. 단계와 완료 기준

| 단계 | 내용 | 완료 기준 | 하드웨어 |
| --- | --- | --- | --- |
| A0 프로토콜 | `lv-dap`의 텔레그램·연결·메모리 접근, 가짜 타깃, 캡처 읽기 | 단위 시험 통과. 공개 캡처의 텔레그램을 우리 인코더로 다시 만들면 바이트가 같고, 캡처의 응답을 우리 파서가 같은 값으로 읽는다 | 없음 |
| A1 선로·도구 | CMSIS-DAP 구현, `lvcli aurix` 명령 | 가짜 CMSIS-DAP 수준 시험(명령 바이트) 통과. `--sim`으로 모든 명령 동작 | 없음 |
| A2 TAS 서버 | `lv-tas`, `lvcli aurix serve` | probe-rs 포크의 TAS 클라이언트(고치지 않음)가 우리 서버 + 가짜 타깃에서 대상 목록, 연결, 읽기·쓰기, 리셋, 세션 2개를 통과 | 없음 |
| A3 실기 | 배선, 연결, 메모리, 파형 확인, 디버깅, 속도 | 검토 문서 8절의 보드 단계 | WeAct(슬롯 1) + 3.3 V AURIX 보드 |
| A4 다음 | 슬롯 2 펌웨어, 암호 해제, 앱·MCP, 선로 디코더, 네이티브 프로브 | 따로 정한다 | — |

A3은 장비 연결이 필요해 이번 작업에서 하지 못한다. A0~A2를 끝내고, A3에서 할 일을 문서에 순서대로 적어 둔다.

## 8. 시험

| 종류 | 내용 |
| --- | --- |
| 단위 | CRC6(알려진 값), 텔레그램 바이트, 응답 분석(채움·시작 비트·CRC 오류·응답 없음), 블록 응답 조립, 접근 나누기, PL0 프레임 해석·응답 |
| 대조 | 공개 캡처(TC297 연결, TC397 디버깅·RAM 쓰기)를 읽어 인코더·파서와 맞는지 |
| 통합 | TCP로 우리 TAS 서버에 붙어 가짜 타깃의 메모리를 읽고 쓰기, 리셋 알림, 클럭 레지스터 |
| 상호 운용 | probe-rs 포크의 `probe-rs-infineon` 예제를 우리 서버에 붙이기 |
| 회귀 | 기존 `cargo test --workspace`, clippy, fmt |

## 9. 위험과 대응

검토 문서 9절을 따른다. 이번 버전에서 특히:

| 위험 | 대응 |
| --- | --- |
| 실기 없이 만든 선로 계층이 실기에서 안 맞는다 | 선로를 `Wire` 뒤에 두고, 프로토콜은 실제 캡처로 따로 검증한다. 실기에서 고칠 곳을 CMSIS-DAP 구현 하나로 좁힌다 |
| 가짜 타깃이 우리 구현의 가정을 그대로 반영한다 | 가짜 타깃의 응답 형식을 캡처의 실제 응답과 대조한다 |
| TAS 응답 형식의 세부(예: 필드의 뜻을 모르는 바이트) | 포크의 가짜 서버가 내는 값과 같게 하고, 포크의 실제 클라이언트로 확인한다 |

## 10. 작업 규칙과 결과 기록

- 단계마다 `cargo fmt`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`를 돌린다.
- 측정하지 않은 것은 측정했다고 쓰지 않는다. 실기에서 확인하지 않은 동작은 "확인하지 않음"으로 적는다.
- 결과는 0절과 `rust/README.md`의 새 절에 날짜와 함께 적는다.
