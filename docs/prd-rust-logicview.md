# 개발지시서 (PRD): logicview Rust 버전

- 작성일: 2026-11-10
- 근거 문서: [Rust·egui 전환 검토](rust-egui-review.md)
- 코드 위치: 이 저장소의 `rust/` (기존 SLogicView 기반 빌드와 섞지 않는다)

## 1. 목적

FX2 기반 로직 애널라이저(Saleae Logic 호환 클론, USB `0925:3881`, 8채널 24 MHz)를 위한 Windows용 로직 분석 앱을 Rust로 새로 만든다. 지금의 SLogicView(Qt/C++) + 패치한 libsigrok + 별도 Python MCP 서버 구성을 프로세스 하나로 대체한다.

이번 버전에서 꼭 얻어야 하는 것:

1. 24 MHz, 50 M 샘플을 PC 본체 포트에서 끊김 없이 수집한다 (Windows 타이머·WinUSB 패치 없이).
2. 수집 결과를 SQLite 파일 하나(`.lvcap`)에 저장하고, 그 파일을 화면·MCP가 함께 쓴다.
3. 5천만 샘플 파형을 egui 화면에서 부드럽게 줌·이동한다.
4. 기존 sigrok 디코더(131개)와 Saleae HLA를 계속 쓴다.
5. Logic 2 MCP와 같은 도구를 앱 안에서 제공하고, 디코딩 결과를 SQL로 질의하는 도구를 더한다.

## 2. 범위

### 포함

| 영역 | 내용 |
| --- | --- |
| 장치 | fx2lafw 펌웨어를 쓰는 8채널 FX2 장치. 1차 대상은 `0925:3881` (Saleae Logic 클론) |
| 수집 | 샘플레이트 20 kHz ~ 24 MHz, 샘플 수 지정 또는 수동 중지, 소프트웨어 트리거 |
| 저장 | `.lvcap` (SQLite) 저장·열기, `.sr` 불러오기·내보내기, CSV·Saleae 바이너리 내보내기 |
| 화면 | 파형, 줌·이동, 커서·마커, 채널 이름·표시, 디코더 주석 행, 데이터 표, 한글 표시 |
| 디코더 | libsigrokdecode FFI로 sigrok 디코더 실행, Saleae HLA 직접 실행 |
| 자동화 | 앱 내장 MCP 서버 (HTTP `127.0.0.1:10530`, stdio), 명령줄 도구 `lvcli` |

### 제외 (이번 버전)

- 16채널 모드, 아날로그 채널, 48 MHz 샘플레이트 (8채널 FX2에서 USB 대역을 넘음)
- Sipeed SLogic 등 다른 장비 (구조는 드라이버를 추가할 수 있게 둔다)
- macOS·Linux 배포 (코드는 플랫폼 의존 부분을 모아 두되 시험·배포는 Windows만)
- Saleae `.sal` 파일 호환, Logic 2 측정(Measurement) 확장, C++ 분석기 플러그인

## 3. 사용자와 사용 장면

| 사용자 | 장면 |
| --- | --- |
| 임베디드 개발자 (주 사용자) | 보드의 UART·I2C·SPI·CAN 신호를 수집하고 파형과 디코딩 결과를 본다 |
| Claude Code 사용자 | MCP로 "24 MHz로 1초 수집해서 CAN ID별 프레임 수를 알려줘" 같은 요청을 한다 |
| 자동 시험 | `lvcli`로 수집·내보내기를 스크립트에서 돌린다 |

## 4. 기능 요구사항

번호는 구현·시험에서 그대로 쓴다. 우선순위: P0 = 이 버전에 꼭, P1 = 이 버전 목표, P2 = 여유가 있으면.

### 4.1 장치 (DEV)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| DEV-1 | USB에서 지원 장치를 찾아 목록을 보여준다. 장치마다 VID:PID, 버스·포트 경로, 펌웨어 상태를 표시 | P0 |
| DEV-2 | 펌웨어가 없으면 `fx2lafw-saleae-logic.fw`를 올린다: vendor 요청 `0xA0`로 CPU 리셋(`0xE600`=1), 4 KiB씩 RAM에 쓰기, 리셋 해제. 다시 연결되기를 최대 3,000 ms 기다린다 | P0 |
| DEV-3 | 펌웨어가 올라간 장치(제조사 문자열 `sigrok`, 제품 `fx2lafw`)는 업로드를 건너뛴다 | P0 |
| DEV-4 | 열 때 펌웨어 버전(`0xB0`)이 major 1인지, 칩 리비전(`0xB2`)을 확인한다 | P0 |
| DEV-5 | 펌웨어 파일은 앱 폴더 `firmware\`, 그다음 PulseView 설치 폴더 순서로 찾는다. 못 찾으면 찾은 경로를 모두 적은 오류를 낸다 | P0 |
| DEV-6 | 장치를 뽑으면 수집을 멈추고 지금까지 받은 데이터를 저장한다 | P1 |

### 4.2 수집 (CAP)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| CAP-1 | 샘플레이트: 20, 25, 50, 100, 200, 250, 500 kHz, 1, 2, 3, 4, 6, 8, 12, 16, 24 MHz. 시작 명령의 클럭(48/30 MHz)과 딜레이는 libsigrok와 같은 규칙으로 계산 | P0 |
| CAP-2 | 샘플 수를 정하면 그만큼 받고 멈춘다. 정하지 않으면 사용자가 멈출 때까지 받는다 | P0 |
| CAP-3 | bulk IN `0x82`에 전송을 미리 여러 개 큐에 넣은 뒤 시작 명령(`0xB1`)을 보낸다. 전송 하나 = 10 ms 분량(512 바이트 배수), 최대 32개 | P0 |
| CAP-4 | USB 스레드는 완료된 전송을 곧바로 다시 큐에 넣고, 데이터 블록을 유계 큐로 넘기기만 한다. 파일 쓰기·압축·요약은 다른 스레드가 한다 | P0 |
| CAP-5 | 유계 큐가 가득 차면 데이터를 버리지 않고 수집을 중지하며, "저장이 수집 속도를 따라가지 못함"과 그때까지의 샘플 수를 알린다 | P0 |
| CAP-6 | 전송이 STALL로 끝나면 엔드포인트 halt를 풀고 다시 큐에 넣는다. 빈 전송이 연속 64개를 넘으면 장치가 멈춘 것으로 보고 수집을 끝낸다 | P0 |
| CAP-7 | 수집 중 진행 상태(받은 샘플 수, 처리 지연, 큐 사용량)를 화면·MCP에 알린다 | P1 |
| CAP-8 | 소프트웨어 트리거: 채널별 상승·하강·에지·High·Low 조건의 AND. 트리거 앞 구간 비율(0~100%) 지정 | P1 |
| CAP-9 | 펄스 폭 트리거: 지정 채널의 High 또는 Low 펄스가 최소·최대 폭 조건을 만족할 때 | P2 |

### 4.3 저장 (STO)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| STO-1 | `.lvcap` = SQLite 파일 하나. 스키마는 5장. WAL 모드, `synchronous=NORMAL` | P0 |
| STO-2 | 원시 샘플은 1 MiB 덩어리마다 zstd(레벨 1)로 압축해 `sample_chunk`에 넣는다. 저장 스레드가 여러 덩어리를 한 트랜잭션으로 묶는다 | P0 |
| STO-3 | 덩어리를 넣을 때 줌 1단계 요약(64샘플마다 OR·AND)을 함께 넣고, 수집이 끝나면 2·3단계(4,096·262,144샘플)를 만든다 | P0 |
| STO-4 | 수집 중에도 다른 연결이 읽기 전용으로 열어 지금까지의 데이터를 읽을 수 있다 | P1 |
| STO-5 | `.sr`(sigrok zip, `logic-1-*` 파일) 불러오기·내보내기 | P1 |
| STO-6 | 내보내기: CSV(`Time [s],Channel 0..7`, 변화 지점만), Saleae 바이너리 v0(채널별 `digital_N.bin`) | P1 |
| STO-7 | 파일 형식 버전을 `meta` 표에 적고, 모르는 상위 버전은 열지 않는다 | P0 |

### 4.4 화면 (UI)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| UI-1 | 상단 도구 막대: 장치 선택, 샘플레이트, 샘플 수(또는 수동), 시작·중지, 열기·저장 | P0 |
| UI-2 | 8채널 파형. 마우스 휠로 커서 위치 기준 줌, 끌기로 이동, 전체 보기 버튼. 시간 눈금(자동 단위 s/ms/µs/ns) | P0 |
| UI-3 | 그릴 때 화면 폭보다 항목이 많은 가장 거친 요약 단계를 고르고, 샘플이 픽셀보다 성기면 원시 데이터의 변화 지점을 선으로 그린다 | P0 |
| UI-4 | 5천만 샘플 캡처에서 전체 보기부터 샘플 단위까지 줌·이동할 때 프레임 시간 16 ms 이하 (시험 PC 기준) | P0 |
| UI-5 | 마우스 위치의 시간과 채널 값 표시. 마커 2개를 놓으면 시간 차와 주파수 표시 | P1 |
| UI-6 | 채널 이름 바꾸기, 채널 숨기기 | P1 |
| UI-7 | 한글 표시: Windows 맑은 고딕(`malgun.ttf`)을 읽고, 없으면 앱에 넣은 Noto Sans KR을 쓴다 | P0 |
| UI-8 | 수집 중 파형이 실시간으로 늘어난다 (초당 30회 이하로 다시 그림) | P1 |
| UI-9 | 디코더 주석 행과 데이터 표(가상 스크롤, 검색) | P1 |
| UI-10 | 마지막 설정(샘플레이트, 샘플 수, 채널 이름)을 `app.db`에 기억 | P2 |

### 4.5 디코더 (DEC)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| DEC-1 | libsigrokdecode C API를 FFI로 감싸 sigrok 디코더를 실행하고, 주석을 `frame` 표에 넣는다 | P1 |
| DEC-2 | 디코더 스택(예: UART 위에 다른 디코더)과 옵션 설정 | P1 |
| DEC-3 | Saleae HLA를 내장 Python으로 직접 실행 (`analyzer.kind = 'hla'`, 입력은 다른 분석기의 프레임) | P1 |
| DEC-4 | 디코딩은 백그라운드에서 하고 진행률을 보여준다. 수집이 끝난 데이터에 대해 실행 | P1 |

### 4.6 자동화 (AUT)

| ID | 요구사항 | 우선 |
| --- | --- | --- |
| AUT-1 | `lvcli devices`, `lvcli capture --rate 24M --samples 50M -o x.lvcap`, `lvcli info x.lvcap`, `lvcli export` | P0 |
| AUT-2 | `lvcli bench`: 수집을 N회 반복하고 회마다 받은 샘플 수, 소요 시간, 끊김 여부를 표로 출력 (시제품 관문용) | P0 |
| AUT-3 | 앱 내장 MCP: 지금 Python 서버의 도구 16개를 같은 이름·인자로 제공 | P1 |
| AUT-4 | MCP `query_frames`: 캡처 파일에 읽기 전용 SQL 실행. 행 수 1,000, 실행 시간 2초 상한 | P1 |
| AUT-5 | `logicview --mcp-stdio`: 화면 없이 stdio MCP 서버로 동작 | P1 |

## 5. `.lvcap` 파일 형식 (버전 1)

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- format = 'lvcap', version = '1', app = 'logicview <버전>'

CREATE TABLE capture (
  id INTEGER PRIMARY KEY,
  created_at TEXT NOT NULL,          -- RFC 3339
  device TEXT NOT NULL,              -- 예: 'Saleae Logic (fx2lafw) usb/1-8'
  samplerate_hz INTEGER NOT NULL,
  num_channels INTEGER NOT NULL,     -- 8
  num_samples INTEGER NOT NULL,      -- 수집이 끝나면 확정
  trigger_json TEXT,                 -- 트리거 조건, 트리거 위치(sample)
  status TEXT NOT NULL               -- 'recording' | 'complete' | 'aborted'
);

CREATE TABLE channel (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  idx INTEGER NOT NULL,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (capture_id, idx)
);

CREATE TABLE sample_chunk (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  chunk_idx INTEGER NOT NULL,
  first_sample INTEGER NOT NULL,
  n_samples INTEGER NOT NULL,        -- 마지막 덩어리만 1 MiB보다 작다
  codec TEXT NOT NULL,               -- 'zstd' | 'raw'
  data BLOB NOT NULL,                -- 샘플 하나 = 1바이트 (bit n = 채널 n)
  PRIMARY KEY (capture_id, chunk_idx)
);

CREATE TABLE summary (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  level INTEGER NOT NULL,            -- 1: 64샘플, 2: 4,096샘플, 3: 262,144샘플마다
  chunk_idx INTEGER NOT NULL,        -- 1단계는 sample_chunk와 같은 번호, 2·3단계는 0
  data BLOB NOT NULL,                -- 항목마다 [OR, AND] 2바이트
  PRIMARY KEY (capture_id, level, chunk_idx)
);

CREATE TABLE analyzer (
  id INTEGER PRIMARY KEY,
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  kind TEXT NOT NULL,                -- 'sigrok' | 'hla'
  decoder_id TEXT NOT NULL,          -- 'uart', 'i2c', HLA 패키지 id
  label TEXT NOT NULL,
  settings_json TEXT NOT NULL,
  input_id INTEGER REFERENCES analyzer(id)
);

CREATE TABLE frame (
  id INTEGER PRIMARY KEY,
  analyzer_id INTEGER NOT NULL REFERENCES analyzer(id),
  start_sample INTEGER NOT NULL,
  end_sample INTEGER NOT NULL,
  ann_row INTEGER NOT NULL,
  type TEXT NOT NULL,
  text TEXT NOT NULL,
  data TEXT                          -- JSON
);
CREATE INDEX frame_by_time ON frame(analyzer_id, start_sample);

CREATE TABLE marker (
  capture_id INTEGER NOT NULL REFERENCES capture(id),
  sample INTEGER NOT NULL,
  label TEXT NOT NULL
);
```

- 한 파일에 캡처 하나가 기본이다 (`capture.id = 1`). 표는 여러 캡처를 담을 수 있게 둔다.
- 1단계 요약 덩어리 하나는 1 MiB 샘플 → 16,384항목 → 32 KiB.
- 앱 설정은 `%LOCALAPPDATA%\logicview\app.db`에 둔다 (`setting(key, value)`, `hla_package(...)`).

## 6. 구조

```
rust/
  Cargo.toml                 워크스페이스
  crates/
    lv-fx2lafw/              USB 드라이버 (nusb): 장치 찾기, 펌웨어, 명령, 스트리밍
    lv-store/                .lvcap 읽기·쓰기, 압축, 줌 요약, .sr·CSV·바이너리 변환
    lv-capture/              수집 세션: USB 스레드, 유계 큐, 저장 스레드, 트리거, 진행 상태
    lv-decode/               libsigrokdecode FFI, HLA 실행 (M3)
    lv-mcp/                  MCP 서버 (rmcp) (M4)
  apps/
    logicview/               egui 앱 (eframe)
    lvcli/                   명령줄 도구
```

| 결정 | 내용 |
| --- | --- |
| 툴체인 | Rust stable, `x86_64-pc-windows-msvc` |
| USB | `nusb` 0.2. Windows에서 IN 엔드포인트의 RAW_IO를 nusb가 켠다 |
| 화면 | `eframe`/`egui` (glow 또는 wgpu 백엔드) |
| SQLite | `rusqlite` (`bundled`) |
| 압축 | `zstd` |
| 스레드 간 큐 | `crossbeam-channel` 유계 채널 + 블록 재사용 |
| 오류 | 라이브러리는 `thiserror`, 앱·CLI는 `anyhow` |
| 로그 | `log` + `env_logger`, 앱은 `%LOCALAPPDATA%\logicview\logs\` |

스레드 구성:

```
USB 스레드 ──블록──▶ [유계 큐 A] ──▶ 저장 스레드 (1 MiB 모으기 → zstd → 1단계 요약 → SQLite)
            └─────▶ [유계 큐 B] ──▶ 실시간 보기 탭 (화면이 최근 데이터를 읽음, 가득 차면 버림)
```

- 큐 A가 가득 차면 수집 중지 (CAP-5). 큐 B는 화면용이라 가득 차면 오래된 블록을 버린다.
- 디코더는 수집이 끝난 뒤 파일을 읽어 실행한다 (DEC-4).

## 7. 비기능 요구사항

| ID | 요구사항 |
| --- | --- |
| NFR-1 | 24 MHz, 50 M 샘플 수집을 본체 포트 직결에서 5회 연속 끊김 없이 (`lvcli bench`) |
| NFR-2 | 수집 중 저장 지연이 큐 용량의 50%를 넘지 않음 (24 MHz 기준) |
| NFR-3 | 화면 프레임 시간 16 ms 이하 (UI-4) |
| NFR-4 | 앱 폴더는 실행 파일 + 펌웨어 + (M3부터) 디코더 런타임. M2까지는 실행 파일만으로 동작 |
| NFR-5 | 패닉 없이 오류를 화면·CLI에 문장으로 알린다. 오류 문장은 무엇이 잘못됐고 어떻게 하면 되는지 적는다 |
| NFR-6 | 라이선스 GPL-3.0-or-later (libsigrokdecode·디코더와 같이 배포하므로) |

## 8. 단계와 완료 기준

| 단계 | 내용 | 완료 기준 |
| --- | --- | --- |
| M0 시제품 | lv-fx2lafw, lv-store(샘플·1단계 요약), lv-capture(저장 경로), `lvcli capture/bench`, 파형만 그리는 최소 viewer | NFR-1 통과, `.lvcap` 압축률·쓰기 속도 기록, viewer로 50 M 샘플 줌·이동 프레임 시간 기록, 한글 표시 확인 |
| M1 캡처·저장 | 수동 중지, 진행 상태, 트리거(CAP-8), 2·3단계 요약, `lvcli info/export`, `.sr` 불러오기·내보내기 | 4.1~4.3의 P0·P1 시험 통과 |
| M2 파형 화면 | 4.4의 P0·P1 (UI-9 제외) | UI 시험 항목 통과, 실제 장치로 캡처→저장→다시 열기 |
| M3 디코더·HLA | lv-decode, UI-9 | UART·I2C·SPI·CAN 디코딩 결과가 sigrok-cli 결과와 같음 |
| M4 MCP | lv-mcp, AUT-3~5 | 지금 `mcp/tests/test_mcp.py`를 새 서버에 돌려 통과 |
| M5 호환·배포 | CSV·바이너리 내보내기, 배포 zip, README | 새 PC에서 zip만 풀어 수집·디코딩·MCP 동작 |

## 9. 시험

| 종류 | 내용 |
| --- | --- |
| 단위 | 시작 명령 계산(모든 샘플레이트), 요약 계산, zstd 왕복, `.lvcap` 쓰기·읽기, `.sr` 왕복, 트리거 탐지 |
| 장치 | `lvcli bench --rate 24M --samples 50M --runs 5` (본체 포트 직결), 1 MHz·16 MHz 수동 중지 |
| 화면 | 합성 캡처(50 M 샘플, 채널마다 다른 주기)로 줌·이동 프레임 시간 측정, 한글 채널 이름 |
| 회귀 | 디코딩 결과를 sigrok-cli 출력과 비교 (M3), MCP 종단 테스트 (M4) |

`cargo test`는 장치 없이 통과해야 한다. 장치가 필요한 시험은 `--ignored`로 분리한다.

## 10. 위험과 대응

| 위험 | 대응 |
| --- | --- |
| 24 MHz 수집 끊김 | M0에서 먼저 측정. 전송 크기·개수를 설정으로 노출해 조절. 허브 경유는 안내 |
| 파형 화면 성능 | 줌 요약으로 그릴 양을 픽셀 수에 묶음. 모자라면 wgpu 콜백 |
| SQLite 쓰기 속도 | 샘플만 별도 파일로 빼는 대안 (`sample_chunk.data` 대신 오프셋) |
| 한글 입력 조합 | M0에서 확인. 입력 칸이 적어 영향이 작음 |
| libsigrokdecode 빌드 | 기존 MSYS2 빌드 산출물을 쓰고, 무거우면 pyo3 실행 엔진으로 대체 |
| 일정 | 단계마다 쓸 수 있는 상태로 끊음. 그동안 지금 앱을 계속 배포 |

## 11. 작업 규칙

- 커밋은 단계 안에서도 기능 단위로 나눈다. `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`가 통과한 상태로 커밋한다.
- 요구사항 ID를 커밋 메시지와 시험 이름에 적는다 (예: `CAP-3`).
- 장치 시험 결과(날짜, 연결 포트, 회수, 결과)는 `rust/README.md`에 표로 남긴다.
