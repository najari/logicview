# logicview Rust 버전

[개발지시서(PRD)](../docs/prd-rust-logicview.md)에 따라 만드는 Rust + egui 버전. 지금은 **M2 파형 화면** 단계다 (M0은 NFR-1만 남음, M1 완료).
기존 SLogicView 기반 빌드(저장소 루트)와 따로 빌드한다.

## 구성

| 경로 | 내용 |
| --- | --- |
| `crates/lv-fx2lafw` | fx2lafw USB 드라이버 (nusb): 장치 찾기, 펌웨어 업로드, 시작 명령, bulk 스트리밍 |
| `crates/lv-store` | `.lvcap`(SQLite) 쓰기·읽기, zstd 압축, 줌 요약(OR·AND 3단계), `.sr` 불러오기·내보내기, Saleae CSV·바이너리 내보내기 |
| `crates/lv-capture` | 수집 세션: USB 스레드 → 유계 대기열 → 저장 스레드, 진행 상태, 소프트웨어 트리거 |
| `apps/lvcli` | 명령줄 도구: `devices`, `capture`, `bench`, `info`, `export`, `import`, `synth`, `store-bench` |
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

## PRD와 다르게 한 것

- CAP-6: 전송이 STALL로 끝나면 halt를 풀고 **수집을 거기서 끝낸다** (다시 큐에 넣으면 그 사이 샘플이 빠진
  캡처가 되기 때문). 이유는 결과에 "USB 파이프 STALL"로 알린다.
- 장치가 펌웨어를 갖고 있는지는 문자열 대신 버전 요청(`0xB0`)에 답하는지로 판단한다. Windows에서는 제조사
  문자열이 비어 있었다.
- CAP-8: 샘플 수 없이(수동 중지) 트리거를 쓰면 트리거 앞 구간이 없다 (비율을 적용할 전체 길이가 없으므로).
  트리거가 걸리기 전에 멈추면 아무것도 저장하지 않는다.
- STO-6 외에 `raw` 내보내기(샘플마다 1바이트, `sigrok-cli -O binary`와 같은 형식)를 더했다. sigrok 결과와
  바이트 단위로 비교하는 시험에 쓴다.
- 한글 글꼴: Noto Sans KR을 넣지 않았다. eframe 0.37이 시스템 글꼴 대체를 기본으로 하므로 맑은 고딕이
  없는 PC에서도 한글이 나온다.

