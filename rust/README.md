# logicview Rust 버전

[개발지시서(PRD)](../docs/prd-rust-logicview.md)에 따라 만드는 Rust + egui 버전. 지금은 **M0 시제품** 단계다.
기존 SLogicView 기반 빌드(저장소 루트)와 따로 빌드한다.

## 구성

| 경로 | 내용 |
| --- | --- |
| `crates/lv-fx2lafw` | fx2lafw USB 드라이버 (nusb): 장치 찾기, 펌웨어 업로드, 시작 명령, bulk 스트리밍 |
| `crates/lv-store` | `.lvcap`(SQLite) 쓰기·읽기, zstd 압축, 줌 요약(OR·AND 3단계) |
| `crates/lv-capture` | 수집 세션: USB 스레드 → 유계 대기열 → 저장 스레드, 진행 상태 |
| `apps/lvcli` | 명령줄 도구: `devices`, `capture`, `bench`, `info`, `synth`, `store-bench` |
| `apps/logicview` | egui 파형 화면: 열기(버튼·끌어다 놓기), 휠 줌, 끌기 이동, 커서 값 |

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
lvcli bench --rate 24M --samples 50M --runs 5            # 시제품 관문 시험
lvcli info cap.lvcap
lvcli synth --samples 50M -o synth.lvcap                 # 화면 시험용 합성 캡처
logicview cap.lvcap
logicview --bench synth.lvcap                            # 줌·이동을 자동으로 하고 프레임 시간 출력
```

## M0 시험 결과 (2026-11-10)

PC: Intel USB 3.1 호스트 컨트롤러, 애널라이저는 루트 허브 2번 포트에 직접 연결(`usb/2-2`, 허브 없음).
입력 신호 없음(모든 채널 0).

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

## PRD와 다르게 한 것

- CAP-6: 전송이 STALL로 끝나면 halt를 풀고 **수집을 거기서 끝낸다** (다시 큐에 넣으면 그 사이 샘플이 빠진
  캡처가 되기 때문). 이유는 결과에 "USB 파이프 STALL"로 알린다.
- 장치가 펌웨어를 갖고 있는지는 문자열 대신 버전 요청(`0xB0`)에 답하는지로 판단한다. Windows에서는 제조사
  문자열이 비어 있었다.
- 한글 글꼴: Noto Sans KR을 넣지 않았다. eframe 0.37이 시스템 글꼴 대체를 기본으로 하므로 맑은 고딕이
  없는 PC에서도 한글이 나온다.
