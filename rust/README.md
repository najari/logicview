# logicview Rust 버전

[개발지시서(PRD)](../docs/prd-rust-logicview.md)에 따라 만드는 Rust + egui 버전. M5까지 마쳤다. M0의 NFR-1(24 MHz를 본체 포트에서 5회 연속)은 2026-11-11에 본체 포트 `usb/1-8`에서 통과했다.
기존 SLogicView 기반 빌드(저장소 루트)와 따로 빌드한다.

## 구성

| 경로 | 내용 |
| --- | --- |
| `crates/lv-fx2lafw` | fx2lafw USB 드라이버 (nusb): 장치 찾기, 펌웨어 업로드, 시작 명령, bulk 스트리밍 |
| `crates/lv-store` | `.lvcap`(SQLite) 쓰기·읽기, zstd 압축, 줌 요약(OR·AND 3단계), `.sr` 불러오기·내보내기, Saleae CSV·바이너리 내보내기 |
| `crates/lv-capture` | 수집 세션: USB 스레드 → 유계 대기열 → 저장 스레드, 진행 상태, 소프트웨어 트리거 |
| `crates/lv-decode` | libsigrokdecode를 실행 중에 읽어 들여 디코더 스택 실행 (Saleae HLA 포함) |
| `crates/lv-mcp` | MCP 서버: Saleae Logic 2 MCP 도구 15개 + `read_analyzer_frames` + `query_frames`, HTTP·stdio |
| `apps/lvcli` | 명령줄 도구: `devices`, `capture`, `bench`, `info`, `export`, `import`, `decoders`, `decode`, `synth`, `store-bench` |
| `apps/logicview` | egui 앱: 장치에서 수집(실시간 파형), `.lvcap`·`.sr` 열기, `.lvcap`·`.sr`·CSV로 저장, 휠 줌, 끌기 이동, 커서 값, 마커 2개, 채널 이름·숨기기 |

## 빌드

Rust stable(MSVC). Visual Studio의 C++ 빌드 도구가 있어야 SQLite·zstd가 빌드된다.

```powershell
cd rust
cargo build --release
cargo test --workspace
```

실행 파일: `target\release\lvcli.exe`, `target\release\logicview.exe`.
펌웨어 `fx2lafw-saleae-logic.fw`는 `LOGICVIEW_FIRMWARE_DIR`, 실행 파일 옆 `firmware\`,
PulseView 설치 폴더 순서로 찾는다.

## 사용

```powershell
lvcli devices
lvcli capture --rate 24M --samples 50M -o cap.lvcap      # --samples 없으면 Ctrl+C까지
lvcli capture -r 1M --time 5 -o cap.lvcap                # 5초 뒤 멈춤 (Ctrl+C와 같은 수동 중지)
lvcli capture -r 4M -s 1M -t 0=f,1=1 --pre 20 -o cap.lvcap --names TX,RX
                                                         # 트리거: D0 하강 AND D1 High, 앞 20%
lvcli bench --rate 24M --samples 50M --runs 5            # 시제품 관문 시험
lvcli info cap.lvcap
lvcli export cap.lvcap -f csv                            # cap.csv (Time [s],Channel 0..)
lvcli export cap.lvcap -f bin -c 0,1                     # cap\digital_0.bin, digital_1.bin
lvcli export cap.lvcap -f sr                             # cap.sr (PulseView·sigrok-cli에서 열림)
lvcli export cap.lvcap -f raw                            # cap.bin (샘플마다 1바이트)
lvcli import other.sr                                    # other.lvcap
lvcli synth --samples 50M -o synth.lvcap                 # 화면 시험용 합성 캡처
logicview cap.lvcap                                      # .sr도 열림 (임시 폴더에 .lvcap으로 바꿔 엶)
logicview --rate 1M --samples manual --trigger 0=f       # 수집 도구 막대 미리 설정
logicview --bench synth.lvcap                            # 줌·이동을 자동으로 하고 프레임 시간 출력
```

## M0 시험 결과 (2026-11-10)

PC: Intel USB 3.1 호스트 컨트롤러, 애널라이저는 루트 허브 2번 포트에 직접 연결(`usb/2-2`, 허브 없음).
입력 신호 없음(빈 입력은 모든 채널 1).

### 장치 (NFR-1)

| 설정 | 결과 |
| --- | --- |
| 펌웨어 업로드 | 첫 연결에서 업로드, 2,014 ms 뒤 fx2lafw 1.4로 다시 연결. 이후에는 1 ms 안에 열림 |
| 1 MHz, 1 M 샘플 | 1.003 s, 전송 98건 모두 성공 |
| 12 MHz, 50 M 샘플 | 3회 모두 통과 (4.170 s) |
| 16 MHz, 50 M 샘플 | 5회 모두 통과 (3.13 s) |
| 24 MHz, 50 M 샘플, 기본(240 KB × 32) | 5회 중 2회, 다시 5회 중 1회 통과. 실패한 회는 0.5~50 M 샘플 사이 아무 데서나 장치가 데이터를 멈춤 |
| 24 MHz, 512 KiB × 32 | 5회 중 2회 |
| 24 MHz, 64 KiB × 64 | 5회 중 0회 |

통과한 24 MHz 수집은 2.090~2.097 s(이론값 2.083 s)에 끝났고, 저장 대기열은 최대 1%만 찼다. 끊기는 원인은
저장이 아니라 USB 쪽이다. 같은 포트에서 기존 SLogicView·sigrok-cli도 24 MHz가 끊겼고(2026-11-08 저녁),
2026-11-09에는 다른 본체 포트(`usb/1-8`)에서 24 MHz가 통과했다. **NFR-1은 이 포트에서 통과하지 못했다.
다른 본체 포트로 옮겨 `lvcli bench`를 다시 돌려야 한다.**

다시 꽂은 뒤 재시험 (2026-11-10, 같은 `usb/2-2`, Intel `8086:51ED` 루트 포트 2, AC 전원·고성능 전원 구성표):

| 시험 | 24 MHz × 50 M 샘플 결과 |
| --- | --- |
| `lvcli bench` 5회 | 1회 통과. 실패한 4회는 7.4 M, 43.0 M, 32.2 M, 37.0 M 샘플에서 멈춤, 대기열 0% |
| sigrok-cli (릴리스 앱 폴더) 3회 | 3회 모두 실패: 5.5 M, 2.0 M, 2.2 M 샘플에서 "Device only sent" |
| CPU 한 코어를 계속 바쁘게 돌리며 `lvcli bench` 5회 | 1회 통과 (CPU 절전 상태 탓이 아님) |

같은 포트에서 libsigrok가 더 일찍 끊기므로 Rust 드라이버 문제는 아니다. 같은 컨트롤러의 루트 포트 8
(11-09에 5회 모두 통과한 `usb/1-8`과 같은 포트 번호)에는 지금 웹캠이 꽂혀 있다.

허브 두 단 경유(`usb/2-3.3.4`, 루트 포트 3 → 허브 → 허브 포트 4): 24 MHz 5회 모두 실패(1.4~9.8 M 샘플에서 멈춤),
16 MHz × 50 M 샘플은 3회 모두 통과(3.134 s).

본체 포트 `usb/1-8`(허브 없음)에서 재시험 (2026-11-11):

| 시험 | 24 MHz × 50 M 샘플 결과 |
| --- | --- |
| `lvcli bench --rate 24M --samples 50M --runs 5` | **5회 모두 통과**: 2.091 s(실효 23.91 MHz), 대기열 0%, 마무리 14~20 ms |

**NFR-1은 본체 포트 `usb/1-8`에서 통과했다.**

### 저장 (NFR-2)

`lvcli store-bench --samples 200M` (240 KB 블록으로 넣기, zstd 레벨 1, SQLite WAL):

| 데이터 | 속도 | 압축 |
| --- | --- | --- |
| 무작위 (압축 안 됨, 최악) | 211 MB/s | 1.0배 |
| 모두 0 (최선) | 1,528 MB/s | 20,549배 |

24 MHz 수집은 24 MB/s가 필요하므로 최악의 경우에도 약 9배 여유가 있다.

### 화면 (UI-4, UI-7)

합성 캡처 50 M 샘플(24 MHz, 채널마다 UART·클럭·PWM·버스트·잡음 등, 저장 2.4 MiB)을
`logicview --bench`로 전체 보기 → 샘플 단위까지 확대 → 축소 → 네 배율에서 이동, 350프레임:

| 항목 | 평균 | p95 | 최대 |
| --- | --- | --- | --- |
| 파형 만들기 | 0.08 ms | 0.13 ms | 3.24 ms |
| 프레임 전체 CPU | 0.87 ms | 1.34 ms | 4.27 ms |

목표(프레임 16 ms 이하)를 넉넉히 넘는다. 최대값은 새 덩어리를 처음 읽어 압축을 풀 때다.
한글 채널 이름은 맑은 고딕으로 제대로 표시된다. 한글 입력(IME)은 M0 화면에 입력 칸이 없어 아직 확인하지 않았다.

## M1 시험 결과 (2026-11-10)

트리거 문법은 sigrok-cli와 같다: `채널=조건`을 쉼표로 이은 AND. 조건은 `r` 상승, `f` 하강, `e` 에지, `1` High,
`0` Low. 에지 조건은 앞 샘플이 있어야 하므로 첫 샘플에서는 걸리지 않고, 레벨 조건만 있으면 첫 샘플에서도 걸린다.
트리거 앞 구간은 `--samples`의 `--pre`%이고, 트리거 전에 받은 샘플이 모자라면 그만큼 뒤를 더 받아 샘플 수를 채운다.
트리거 위치는 `capture.trigger_json`에 `{"conditions":"0=r","pre_percent":10,"sample":N}`으로 남는다.

| 시험 | 결과 |
| --- | --- |
| 단위 테스트 (`cargo test`) | 27개 통과: 트리거 조건·블록 경계 에지·앞 구간 링 버퍼·트리거 전 중지, `.sr` 왕복, CSV·바이너리 형식 |
| `.sr` 내보내기 → sigrok-cli | 합성 5 M 샘플: `sigrok-cli --show`가 채널 이름(한글)·샘플레이트·샘플 수를 읽고, `-O binary` 결과가 원본과 SHA-256 같음 |
| libsigrok가 쓴 `.sr` 불러오기 | sigrok-cli로 다시 저장한 파일: 원본과 SHA-256 같음. 32채널 `slogic32-demo.sr`(unitsize 4): 앞 8채널이 sigrok-cli 출력과 같음 |
| 수동 중지 (장치, `usb/2-3.3.1`) | 1 MHz `--time 2`: 2,017,280 샘플, 16 MHz `--time 1`: 16,185,856 샘플, 둘 다 빈 전송 0, 상태 `complete` |
| 트리거 (장치) | 4 MHz, 1 M 샘플, `0=1`: 첫 샘플에서 걸림, 1,000,000 샘플 저장, `trigger_json` 기록 |
| 트리거가 안 걸림 (장치) | `0=f`, 입력 없음, `--time 1.5`: "트리거 전에 중지, 저장한 것 없음", 빈 파일 지움, 종료 코드 1 |
| logicview로 `.sr` 열기 | libsigrok가 쓴 `.sr`을 열어 8채널 파형 표시 |

장치를 뽑았을 때(DEV-6)는 USB 스레드가 `Disconnected`로 끝나고 그때까지 받은 샘플을 `aborted` 상태로 저장한다.
실제로 뽑아 보는 시험은 아직 하지 않았다.

## M2 파형 화면 (2026-11-10)

앱 사용법:

- 위 줄: 장치, 샘플레이트, 샘플 수(또는 수동 중지), 트리거, **시작/중지**. 장치 목록은 2초마다 백그라운드에서 다시 찾는다.
  수집 파일은 `%LOCALAPPDATA%\logicview\captures\capture-<UTC 시각>.lvcap`에 바로 쓰인다.
- 수집 중에는 파형이 0.1초마다 늘어난다(UI-8). 저장 스레드가 0.25초마다 트랜잭션을 커밋하고, 덜 찬 덩어리도
  써 둔다(나중에 꽉 찬 덩어리로 바꿔 씀). 확대·이동하면 따라가기를 멈추고, "전체 보기"로 다시 따라간다.
- 아래 줄: 열기, **저장**(`.lvcap`은 `VACUUM INTO`로 파일 하나로 복사, `.sr`, CSV는 보이는 채널만), 전체 보기·확대·축소,
  **채널…** 창(이름 바꾸기, 숨기기; 열린 파일의 `channel` 표에 저장되고 다음 수집 이름으로도 쓰임).
- 파형 클릭: 마커 A, 다시 클릭: B, 그다음은 가까운 마커를 옮김. 클릭한 행의 채널 에지가 8픽셀 안에 있으면 그 에지에 붙는다.
  오른쪽 클릭·Esc: 마커 지우기. 상태 줄에 A·B 시각과 Δ(주파수)를 보인다. 트리거 위치는 빨간 `T` 선.

| 시험 (앱, UI Automation으로 버튼 조작) | 결과 |
| --- | --- |
| 16 MHz, 10 M 샘플, 시작 | "완료 · 10,000,000 샘플 · 0.631 s", 파형 표시 |
| 1 MHz, 수동 중지, 시작 → 3초 → 중지 | 수집 중 파형이 늘어남(받음 3,010,560일 때 화면 2,867,200), 중지 후 4,638,720 샘플 `complete` |
| 4 MHz, 1 M 샘플, 트리거 `0=1` | "완료 · 1,000,000 샘플 · 0.263 s · 트리거 샘플 0" |
| 수집 끝난 파일 | 앱이 읽던 연결을 닫고 WAL을 합쳐 `.lvcap` 하나만 남음 |
| 합성 캡처, 마커 12,000·36,000 샘플(`--markers`) | "A 500 µs · B 1.5 ms · Δ 1 ms (1 kHz)", D2 1 kHz 에지와 일치 |
| 채널 창: D0 이름 "TX 송신", D5·D6 숨김 | 화면에서 빠지고, 파일에 저장되어 다시 열어도 같음 (`lvcli info`에 "숨긴 채널") |
| 저장… → `.lvcap` | 1.5 MB 한 파일로 복사됨 |
| `logicview --bench` 50 M 샘플 | 프레임 CPU 평균 0.66 ms, 최대 5.51 ms (UI-4 유지) |

아직 하지 않은 것: 한글 입력기(IME) 조합 입력 확인(자동화는 입력기를 거치지 않음), 실제 마우스 클릭으로 마커 놓기
(winit이 보낸 창 메시지를 받지 않아 `--markers`로 대신 시험), 저장 대화상자에서 `.sr`·CSV 고르기(저장 함수는
단위 테스트·`lvcli`로 시험), UI-10(마지막 설정 기억, P2).

## M3 디코더 (2026-11-10)

디코더 런타임은 기존 앱 폴더(`SLogicView-2.3.3-fx2lafw`)의 libsigrokdecode·glib·Python·디코더 스크립트를 그대로 쓴다.
실행 중에 DLL을 읽어 들이므로(import 라이브러리 없음) 빌드에는 필요 없다. 찾는 순서: `LOGICVIEW_SIGROK_DIR`,
실행 파일 옆 `sigrok\`, 실행 파일 폴더. 런타임이 없으면 디코더 기능만 꺼지고 나머지는 동작한다.

```powershell
$env:LOGICVIEW_SIGROK_DIR = 'C:\...\SLogicView-2.3.3-fx2lafw'
lvcli decoders                       # 131개 (sigrok-cli -L과 같은 수)
lvcli decoders uart                  # 채널, 옵션(기본값·고를 값), 주석 클래스·행
lvcli decode cap.lvcap -P uart:rx=D0:baudrate=9600 -A uart=rx-data
lvcli decode cap.lvcap -P i2c:scl=SCL:sda=SDA,eeprom24xx --save --label EEPROM
```

- `-P`·`-A`는 sigrok-cli와 같은 문법이다. 채널은 `0`~`7`, `D0`~`D7`, 캡처의 채널 이름 중 무엇으로든 적을 수 있다.
  출력은 `sigrok-cli --protocol-decoder-samplenum --protocol-decoder-ann-class`와 같다.
- `--save`는 결과를 `.lvcap`의 `analyzer`·`frame` 표에 넣는다.
  - `frame.type`은 `<디코더>.<클래스>`이고, `ann_row`는 스택 전체에서 센 주석 행 번호다.
  - 행 이름은 `analyzer.settings_json`의 `rows`에 들어 있다.
- Saleae HLA(DEC-3)는 `extensions\hla.ps1 install`로 sigrok 디코더(`hla_<확장>_<프로토콜>`)로 설치한다.
  - 설치 폴더는 `%LOCALAPPDATA%\libsigrokdecode\decoders`이고, 다른 폴더는 `SIGROKDECODE_DIR`로 알려 준다.
  - 설치한 HLA는 프로토콜 디코더 위에 쌓아 쓴다(예: `uart:rx=D0,hla_line_reader_uart`).
  - HLA가 든 분석기는 `kind = 'hla'`로 저장된다.

앱:

- **디코더…** 창에서 디코더를 고른다. 찾기 칸에 입력하면 목록이 걸러진다.
  - 채널과 옵션을 정한다. 채널은 이름이 맞으면 저절로 고른다. 옵션은 고를 값이 있으면 목록으로 나온다.
  - "위에 쌓을 디코더"에는 출력이 맞는 디코더만 나온다.
  - **디코딩**을 누르면 백그라운드에서 돈다(DEC-4). 진행률을 보여 주고 취소할 수 있다.
  - 수집 중에는 디코딩할 수 없다.
- 결과는 파일에 저장되고 파형 아래에 주석 행으로 그려진다(UI-9).
  - 프레임이 있는 행만 그린다. 한 화면에 5,000개가 넘으면 확대하라고 알린다.
  - 표에서 분석기마다 "파형에 보이기"를 끌 수 있다.
- **표**: 분석기별 프레임 목록이다(가상 스크롤, 텍스트·종류 검색, 삭제). 줄을 클릭하면 그 프레임으로 가서 강조한다.

| 시험 | 결과 |
| --- | --- |
| `make-capture.py` 신호(10 MHz, 46,000 샘플)를 7가지 스택(uart, uart ascii, i2c, spi, can, spi+spiflash, i2c+eeprom24xx)으로 `lvcli decode` | 7가지 모두 sigrok-cli 출력과 줄 순서까지 같음 (77·77·55·56·174·59·73줄) |
| 시험 HLA 4개(line-reader, i2c-transactions, spi-burst, can-messages)를 프로토콜 디코더 위에 쌓아 디코딩 | 4가지 모두 sigrok-cli와 같음 ("Hi", "ok!", "W 0x50: 10 20", "R 0x50: ab", SPI burst, CAN 0x123·0x1ABCDEF) |
| 합성 5 M 샘플 24 MHz, `uart:rx=UART TX -A uart=rx-data` | sigrok-cli와 같은 85줄, 0.16 s (sigrok-cli 0.15 s) |
| 합성 50 M 샘플 | uart 주석 9,196개 0.63 s, pwm 주석 62,496개 0.37 s |
| 앱: `test.sr` 열기 → D0 이름 RX → 디코더… → uart → 디코딩 (UI Automation) | RX가 D0에 저절로 배정, 77 프레임 저장, 주석 행 2개와 표 표시, 검색 "rx-data" 63 프레임 |
| `cargo test` | 런타임이 있으면 libsigrokdecode로 UART 디코딩과 오류 문장까지 시험, 없으면 건너뜀 |

아직 하지 않은 것: 표에서 줄 클릭으로 이동(마우스 자동화가 안 되어 직접 확인 필요), 디코더 옵션 프리셋 저장,
배포 zip에 디코더 런타임 넣기(M5).

## M4 MCP (2026-11-10)

Python 서버(`mcp/logicview_mcp.py`)와 같은 도구 이름·인자·결과·오류 문장을 쓰는 MCP 서버를 앱에 넣었다.
수집은 lv-capture, 디코딩과 HLA는 lv-decode, 파일은 lv-store가 맡아 sigrok-cli를 부르지 않는다.

```powershell
logicview --mcp-stdio                       # 창 없이 stdio (AUT-5)
logicview --mcp-http --port 10530           # 창 없이 HTTP (Logic 2와 같은 http://127.0.0.1:10530)
#   --data-dir D (기본 %LOCALAPPDATA%\logicview\mcp), --sigrok-dir D, --hla-script extensions\hla.ps1
claude mcp add --transport http logicview http://127.0.0.1:10530   # 앱에서 MCP를 켰을 때
claude mcp add logicview -- <경로>\logicview.exe --mcp-stdio --sigrok-dir <런타임 폴더>
```

- 앱 도구 막대의 **MCP**를 누르면 같은 서버가 앱 안에서 HTTP로 뜬다(AUT-3). 앱과 같은 장치를 쓰므로,
  앱에서 수집하는 동안 MCP 수집은 "장치를 열 수 없음"으로 실패한다.
- 캡처는 데이터 폴더의 `.lvcap`으로 다룬다. `load_capture`는 `.sr`과 `.lvcap`을 받는다(`.sr`은 복사해서 바꿈).
  `save_capture`는 `.sr`(불러온 `.sr`이면 바이트 그대로 복사)과 `.lvcap`으로 쓴다.
- 분석기 결과는 MCP 응답용으로 메모리에도 들고, 캡처 파일의 `analyzer`·`frame` 표에도 넣는다.
- `query_frames`(AUT-4): 캡처 파일에 읽기 전용 SQL 한 문장을 돌린다.
  - 쓰기 문장은 준비 단계에서 거부한다.
  - 1,000행을 넘으면 `truncated`가 되고, 2초가 넘으면 중단한다.
- HLA는 `add_high_level_analyzer`가 `extensions\hla.ps1`로 데이터 폴더의 `hla\decoders`에 설치한다. 이 폴더는
  디코더 런타임이 시작될 때 `SIGROKDECODE_PATH`로 검색 경로에 들어가므로, 실행 중에 설치한 HLA도 불러온다.
- stdio 모드는 stdin을 읽기 전에 디코더 런타임(내장 Python)을 먼저 시작한다. Windows에서는 한 스레드가 동기
  파이프를 읽으며 막혀 있으면 같은 핸들을 쓰는 다른 호출도 막히는데, Python이 시작하면서 stdin을 살펴보기 때문이다.

| 시험 | 결과 |
| --- | --- |
| `mcp/tests/test_mcp.py --server rust\target\release\logicview.exe` | 34개 모두 통과 (`query_frames` 2개 포함) |
| 같은 시험 `--hardware` (장치 `usb/2-3.3.1`) | 10개 더 통과: 1 MHz 0.25 s 수집이 250,000 샘플, 16 MHz 수동 중지 1 s 이상, 샘플레이트·펄스 트리거 오류 |
| 같은 시험을 기존 Python 서버로 | 그대로 통과 (시험은 도구 16개 또는 `query_frames`까지 17개를 받음) |
| `logicview --mcp-stdio`로 initialize → tools/list → load_capture → add_analyzer(CAN) → query_frames | 응답 정상, `can.bit` 140개 등 |
| 앱에서 MCP 켜고 HTTP로 `get_devices` | `fx2lafw:usb/2-3.3.1` |

## M5 배포 (2026-11-10)

```powershell
.\scripts\package.ps1 -RuntimeDir C:\...\SLogicView-2.3.3-fx2lafw     # dist\logicview-0.1.1-windows-x64.zip
.\scripts\test-package.ps1 -Zip dist\logicview-0.1.1-windows-x64.zip -Python <python.exe> [-Hardware]
```

- zip에는 다음이 들어 있다 (954개 파일, 풀면 56 MB, zip 22 MB).
  - `logicview.exe`, `lvcli.exe`
  - `firmware\fx2lafw-saleae-logic.fw`
  - `sigrok\`: 디코더 런타임
    - DLL 14개(13.5 MB, `dumpbin /dependents`로 고름)
    - Python 3.14 표준 라이브러리(ensurepip·venv 등 제외)
    - 디코더 131개
  - `extensions\`(HLA 설치), `tools\install-winusb.ps1`, `README.txt`, `LICENSE.txt`
- 실행 파일은 C 런타임을 정적으로 링크한다(`.cargo/config.toml`의 `crt-static`). VC++ 재배포 패키지가 없는 PC에서도 뜬다.
- 디코더 런타임은 실행 파일 옆 `sigrok\`에서, HLA 설치 스크립트는 실행 파일 옆 `extensions\hla.ps1`에서 찾는다.

`test-package.ps1`은 zip을 빈 폴더에 풀고 새 PC처럼 돌린다. PATH에는 Windows 시스템 폴더만 두고,
`LOGICVIEW_*`·`PYTHONHOME`·`SIGROKDECODE_*` 환경 변수는 지운다.

| 시험 (2026-11-10, `-Hardware`) | 결과 |
| --- | --- |
| 디코더 불러오기 | 131개, 경고 없음 |
| uart·can 디코딩, `.sr` 불러오기, CSV 내보내기 | 통과 |
| 장치 찾기, 1 MHz × 1 M 샘플 수집 | 통과 (`usb/2-3.3.1`) |
| `mcp/tests/test_mcp.py`를 패키지의 `logicview.exe`로 (HLA 설치는 PATH의 Windows PowerShell 5.1) | 44개 모두 통과 |
| 패키지의 앱을 같은 환경에서 띄워 디코더 창 열기 | 디코더 목록 표시 |

남은 것:

- 펌웨어 업로드 경로(`firmware\`)는 시험 때 장치에 이미 펌웨어가 올라가 있어 확인하지 못했다.
  애널라이저를 뽑았다 꽂은 뒤 패키지의 `lvcli devices`(펌웨어 "없음")와 `lvcli capture`로 확인한다.
- 다른 PC에서 실제로 풀어 보는 시험은 아직 하지 않았다.

## 다시 빌드·시험 (2026-11-11)

저장소를 새로 받아 위 절차대로 다시 빌드하고 시험했다. Rust 1.98.1 (`stable-x86_64-pc-windows-msvc`).
디코더 런타임은 릴리스 `v2.3.3-fx2lafw` zip(2026-11-09 빌드)과 같은 앱 폴더. 애널라이저는 본체 포트 `usb/1-8`.

| 시험 | 결과 |
| --- | --- |
| `cargo build --release` | 성공 |
| `cargo test --workspace` (`LOGICVIEW_SIGROK_DIR` 지정) | 42개 모두 통과 (libsigrokdecode 런타임 시험 포함) |
| `lvcli bench --rate 24M --samples 50M --runs 5` | 5회 모두 통과 (위 NFR-1) |
| `mcp/tests/test_mcp.py --server target\release\logicview.exe --hardware` | 44개 모두 통과 (`query_frames` 2개 포함) |
| `scripts\package.ps1` | `logicview-0.1.1-windows-x64.zip`: 954개 파일, 풀면 56.3 MB, zip 22.3 MB |
| `scripts\test-package.ps1 -Hardware` | 7개 모두 통과 (디코더 불러오기, uart·can 디코딩, `.sr` 불러오기·CSV, 장치 찾기, 1 MHz 수집, MCP 시험) |
| 패키지의 `logicview.exe`를 Windows 시스템 폴더만 있는 PATH로 실행 | 창이 뜸 |

펌웨어 업로드 경로(`firmware\`)는 이번에도 장치에 펌웨어가 이미 올라가 있어 확인하지 못했다.

## PRD와 다르게 한 것

- CAP-6: 전송이 STALL로 끝나면 halt를 풀고 **수집을 거기서 끝낸다** (다시 큐에 넣으면 그 사이 샘플이 빠진
  캡처가 되기 때문). 이유는 결과에 "USB 파이프 STALL"로 알린다.
- 장치가 펌웨어를 갖고 있는지는 문자열 대신 버전 요청(`0xB0`)에 답하는지로 판단한다. Windows에서는 제조사
  문자열이 비어 있었다.
- CAP-8: 샘플 수 없이(수동 중지) 트리거를 쓰면 트리거 앞 구간이 없다 (비율을 적용할 전체 길이가 없으므로).
  트리거가 걸리기 전에 멈추면 아무것도 저장하지 않는다.
- STO-6 외에 `raw` 내보내기(샘플마다 1바이트, `sigrok-cli -O binary`와 같은 형식)를 더했다. sigrok 결과와
  바이트 단위로 비교하는 시험에 쓴다.
- DEC-3: HLA를 별도 Python 실행기로 돌리지 않고, 기존 브리지(`extensions/compat/saleae_bridge.py`)로 감싼 sigrok
  디코더로 libsigrokdecode 안에서 돌린다. 입력은 저장된 프레임이 아니라 아래 디코더의 Python 출력이다. sigrok-cli·
  SLogicView와 결과가 같고, 같은 HLA 설치를 함께 쓴다.
- AUT-3: MCP 서버는 rmcp 대신 표준 라이브러리로 JSON-RPC와 HTTP·stdio를 직접 처리한다. Python 서버와 같은
  동작(인자 형 변환, 오류 문장)을 맞추기 쉽고 의존이 적다. `deviceId`는 `fx2lafw:<위치>` (예: `fx2lafw:usb/1-8`).
- 한글 글꼴: Noto Sans KR을 넣지 않았다. eframe 0.37이 시스템 글꼴 대체를 기본으로 하므로 맑은 고딕이
  없는 PC에서도 한글이 나온다.

