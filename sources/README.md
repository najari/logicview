# 원본 소스

릴리스 `v2.3.3-fx2lafw`를 만든 소스. Sipeed [SLogic](https://github.com/sipeed/SLogic) `release-2.3.3`이
서브모듈로 고정한 커밋을 그대로(수정 없이) 넣었다. 패치(저장소 루트의 `000*.patch`,
`sigrok-cli-patches/`)는 빌드 스크립트가 빌드 폴더에 복사한 뒤 적용한다.

| 폴더 | 원본 저장소 | 커밋 | 릴리스에서 |
|---|---|---|---|
| `libsigrok` | [sipeed/libsigrok](https://github.com/sipeed/libsigrok) | `bf6b346e597d112f48102fe6a7b2318c6d7b49e2` | 패치 0001–0003 적용, fx2lafw 포함 빌드 → `libsigrok-4.dll` |
| `libsigrokdecode` | [sigrokproject/libsigrokdecode](https://github.com/sigrokproject/libsigrokdecode) | `71f451443029322d57376214c330b518efd84f88` | `sigrok-cli.exe` 빌드에 사용 |
| `sigrok-cli` | [sigrokproject/sigrok-cli](https://github.com/sigrokproject/sigrok-cli) | `f44dd91347e7ac797cefc23162b9fcf0b7329f1f` | `sigrok-cli-patches/0001` 적용 → `sigrok-cli.exe` |
| `slogicview` | [sipeed/pulseview](https://github.com/sipeed/pulseview) (`slogicview` 브랜치) | `c334722517d900d06c614d377fdaf5a3a5d06b57` | `slogicview.exe` (SLogic 릴리스 바이너리를 그대로 사용) |

파일은 원본 저장소의 내용과 바이트 단위로 같다 (`.gitattributes`의 `sources/** -text`로 줄바꿈 변환 없음).
각 구성 요소는 원본의 라이선스(GPL-3.0 등, 각 폴더의 `COPYING`)를 따른다.
