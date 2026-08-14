# Xpenology USB Writer — 설계 문서

작성일: 2026-08-15
상태: 승인 대기

## 1. 목적

Xpenology(헤놀로지)를 **베어메탈(네이티브)로 설치**할 때 필요한 부팅 USB를,
프로그램 하나로 만든다.

현재 사용자가 겪는 과정:

1. GitHub에서 m-shell 또는 RR 릴리스를 찾는다
2. 올바른 `.img.gz` / `.img.zip` 에셋을 고른다
3. 압축을 푼다
4. Rufus를 받아 DD 모드로 굽는다
5. 잘못된 디스크를 고르지 않기를 기도한다

이 5단계를 하나의 안내형 프로그램으로 대체한다.

## 2. 범위

### 하는 것

- USB 저장장치 감지 및 안전한 선택
- 최신 로더(m-shell / RR) 자동 확인 및 내려받기
- 압축 해제
- USB에 raw 이미지 쓰기
- 선택적 쓰기 검증
- 한국어 / 영어

### 하지 않는 것 (명시적 비목표)

- **DSM 모델 선택.** m-shell / RR 이미지는 모델 정보가 없는 범용 raw 이미지다.
  기종·시리얼·MAC 설정은 USB로 부팅한 뒤 로더 자체 화면에서 이뤄진다.
  이 프로그램의 책임은 "올바른 이미지를 올바른 USB에 정확히 굽는 것"에서 끝난다.
- DSM 설치 자체, 네트워크 설정, 디스크 구성
- Proxmox 등 가상화 환경 (그쪽은 기존 `pve_xpenol_install.sh`가 담당)
- 사용자가 이미 가진 임의 `.img` 파일 굽기 (v1 제외, 확장 여지로만 남김)

## 3. 사용자 흐름

4단계 위저드. 화면당 결정 하나, 큰 제목 + 하단 단일 CTA.

```
1/4  USB를 선택해 주세요       USB 저장장치만 목록에 표시
2/4  어떤 로더를 쓰시겠어요?    m-shell(강추) / RR, 최신 버전 자동
3/4  데이터가 모두 지워집니다    장치명·용량·기존 볼륨 표시, 빨간 CTA
4/4  USB를 만들고 있어요        확인→내려받기→쓰기→(검증) 진행 표시
 ✓   USB가 준비됐어요          "부팅 후 로더 화면에서 모델 선택" 안내
```

되돌리기(뒤로)는 1~3단계에서 가능하고, 4단계 진입 이후에는 취소만 가능하다.

## 4. 아키텍처

### 스택

- **Tauri 2** — 셸. WebView2를 사용해 배포물이 작다 (목표 10MB 이하)
- **Rust** — 백엔드. Win32 API 직접 호출
- **TypeScript + Vite (프레임워크 없음)** — 프런트엔드. 화면이 4개뿐이라 런타임 프레임워크가 불필요하다

### 계층 분리 (검증 가능성이 설계를 지배한다)

개발 환경이 Linux이고 실제 USB 쓰기는 물리 장비에서만 검증 가능하다.
따라서 **플랫폼 의존 코드를 최소 표면적으로 격리**하는 것이 최우선 구조 원칙이다.

```
src-tauri/src/
├─ core/                 플랫폼 무관. Linux에서 100% 단위 테스트된다
│  ├─ loader.rs          로더 저장소 매핑, 릴리스 에셋 선택, 태그 파싱
│  ├─ download.rs        내려받기 + 압축 해제 (gz / zip)
│  ├─ progress.rs        진행률·속도·잔여시간 계산
│  ├─ verify.rs          해시 계산 및 대조
│  ├─ safety.rs          안전 인터록 판정 (순수 함수)
│  └─ i18n.rs            메시지 사전 (ko / en)
│
├─ device/               트레이트 경계 — 여기가 테스트 가능성의 핵심
│  ├─ mod.rs             trait UsbEnumerator, trait RawWriter
│  ├─ windows/           #[cfg(windows)] 실제 Win32 구현
│  └─ fake.rs            테스트·Linux 개발용 가짜 구현
│
└─ commands.rs           Tauri 커맨드 (프런트엔드 경계)
```

`device`가 트레이트인 덕분에:

- **안전 규칙**("내장 디스크는 절대 목록에 없다")을 가짜 장치 목록으로 테스트할 수 있다
- **전체 파이프라인**(내려받기 → 압축 해제 → 쓰기 → 검증)을 쓰기 대상만 임시 파일로 바꿔
  CI에서 끝까지 돌릴 수 있다. 실제 USB만 아닐 뿐 코드 경로는 동일하다
- Linux에서도 앱이 실행된다 (가짜 장치 표시) → UI를 실제로 띄워보며 다듬는다

## 5. 쓰기 시퀀스

Rufus 및 Microsoft 문서 조사에서 확인된 순서를 그대로 따른다.
이 순서는 임의로 바꾸면 동작하지 않는다.

```
Discover  디스크 열거 → 각 디스크의 볼륨 열거
          FindFirstVolume/FindNextVolume + IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS
          드라이브 문자에 의존하지 않는다 (문자 없는 ESP/ext4 파티션도 쓰기를 막는다)

Confirm   안전 인터록 통과 + 사용자 명시 확인

Acquire   마운트 지점 제거 (DefineDosDevice → DeleteVolumeMountPoint)
          논리 볼륨 핸들을 최소 하나 확보해 잠근다:
            CreateFile → FSCTL_ALLOW_EXTENDED_DASD_IO → FSCTL_LOCK_VOLUME → FSCTL_DISMOUNT_VOLUME
          나머지 파티션은 잠그지 않는다 — 문자 제거 + RAW 레이아웃으로
          "마운트된 볼륨에 속하지 않는 섹터"로 만들어 제한을 푼다
          물리 핸들 잠금은 best-effort. 실패해도 중단하지 않는다
          재시도: SHARING_VIOLATION / ACCESS_DENIED 에만, 150회 × 100ms,
                  1/3 지점부터 FILE_SHARE_WRITE 추가

Prepare   준비용 물리 핸들 #1 열기
          IOCTL_DISK_CREATE_DISK(PARTITION_STYLE_RAW) → IOCTL_DISK_UPDATE_PROPERTIES
          핸들 #1 닫기  ← 재열거를 넘겨 유지하면 ERROR_MEDIA_CHANGED(1110)
          꼬리 1MiB 0으로 덮어쓰기 (IOCTL_DISK_GET_LENGTH_INFO 로 정확한 크기 확보)

Write     쓰기용 물리 핸들 #2 를 새로 연다
          쓰기 길이와 오프셋은 반드시 논리 섹터 크기의 배수 (위반 시 ERROR_INVALID_PARAMETER 87)
          버퍼 주소 정렬은 권장 사항이며 강제는 아니다 — 그래도 정렬해 둔다
          섹터 크기는 장치에서 조회한다 (512 가정 금지, 512 미만이면 중단)
          32MiB × 2 이중 버퍼. 마지막 청크 패딩 버퍼는 명시적으로 0으로 채운다
          짧은 쓰기(TRUE 이면서 written < toWrite)는 오류로 취급해
          청크 시작으로 되감고 4회까지 재시도

Verify    (선택, 기본 꺼짐) 되읽어 해시 대조

Finish    FlushFileBuffers → IOCTL_DISK_UPDATE_PROPERTIES
          → 볼륨 FSCTL_UNLOCK_VOLUME + 닫기 → 물리 핸들 닫기(수 초 걸릴 수 있음)
          → IOCTL_STORAGE_MEDIA_REMOVAL(Prevent=FALSE) → IOCTL_STORAGE_EJECT_MEDIA
```

### 검증 단계에서 뒤집힌 것들

1차 조사 내용 중 다음은 **반증되어 위 시퀀스에서 제외**했다. 기록해 두지 않으면
나중에 "왜 이렇게 안 했지" 하며 되돌리기 쉬운 것들이다.

| 반증된 주장 | 실제 |
|---|---|
| 디스크 위 **모든** 볼륨을 잠근다 | 조사자의 창작이며 fail-closed 로 망가진다. 잠기지 않는 파티션 하나가 전체를 중단시킨다. 논리 볼륨 하나만 확실히 잠그고 나머지는 RAW 레이아웃으로 해결한다 |
| `IOCTL_DISK_DELETE_DRIVE_LAYOUT` 를 쓴다 | Rufus 는 이 호출을 하지 않는다. MBR 전용 의미라 GPT 백업 헤더에 아무 효과가 없다. `CREATE_DISK(RAW)` 가 맞다 |
| Rufus 가 머리 8MB 를 0으로 지운다 | DD 경로에서는 `ClearMBRGPT` 를 건너뛴다. 머리 지우기는 불필요하고, 꼬리 1MiB 지우기는 Rufus 근거가 아니라 우리 판단으로 한다 (이미지가 스틱보다 작을 때 남는 GPT 백업 헤더) |
| 물리 핸들 하나로 준비와 쓰기를 모두 처리 | 재열거를 넘긴 핸들은 `ERROR_MEDIA_CHANGED(1110)` 를 낸다. 준비용과 쓰기용을 분리한다 |
| 버퍼 주소 정렬이 필수 | 문서상 "강제되지 않을 수 있다". 길이·오프셋 정렬만 필수다 |

### 명시적으로 하지 않는 것

**디스크 오프라인 전환을 시도하지 않는다.** `IOCTL_DISK_SET_DISK_ATTRIBUTES`와
`diskpart offline disk`는 이동식 미디어에서 실패한다 ("The operation is not supported
on removable media"). USB 플래시 드라이브에는 볼륨 잠금 + 마운트 해제가 올바른 방법이다.

## 6. 안전 인터록

파괴적 작업이므로 안전 규칙은 기능이 아니라 명세다. 모두 순수 함수로 구현해 테스트한다.

| 규칙 | 근거 |
|---|---|
| 디스크 인덱스 0 거부 | 인덱스 0이 넘어가면 시스템 디스크의 MBR을 지운다 |
| `BusType == USB` 인 장치만 노출 | 내장 디스크가 목록에 뜨는 것 자체를 차단 |
| 디스크 extent가 2개 이상인 볼륨 거부 | RAID / 스팬 볼륨 보호 |
| 이미지 용량 > USB 용량이면 거부 | 중간에 실패해 USB를 망가뜨리는 것 방지 |
| 소스 이미지가 대상 디스크에 있으면 거부 | 자기 자신을 덮어쓰는 것 방지 |
| 쓰기 직전 장치 신원 재확인 | 목록 표시 후 USB가 바뀌었을 수 있다 |

UI 인덱스를 신뢰하지 않고, 쓰기 직전에 디스크 번호를 다시 해석한다.

## 7. 로더 해석 전략

기존 `pve_xpenol_install.sh`는 에셋 URL을 하드코딩한다:

```
m-shell → PeterSuh-Q3/tinycore-redpill : alpine-redpill.<tag>.m-shell.img.gz
RR      → RROrg/rr                     : rr-<tag>.img.zip
```

**하드코딩에 의존하지 않는다.** 릴리스마다 에셋 이름이 바뀔 수 있고, 바뀌면 프로그램이
조용히 망가진다. 대신:

1. GitHub Releases API로 최신 릴리스의 에셋 목록을 가져온다
2. 패턴으로 후보를 고른다 (`*.img.gz`, `*.img.zip` 중 로더별 식별자를 포함하는 것)
3. 후보가 정확히 하나면 사용한다
4. 0개거나 여러 개면 **알려진 패턴을 폴백**으로 시도한다
5. 그것도 실패하면 사용자에게 명확히 알린다 — 조용히 잘못된 파일을 받지 않는다

API 호출은 인증 없이 시간당 60회 제한이 있으나, 이 프로그램은 실행당 1~2회만 호출하므로
문제되지 않는다. 실패 시 폴백 경로가 있다.

## 8. 국제화

한국어 / 영어. 시스템 언어를 감지해 초기값을 정하고, 화면에서 수동 전환할 수 있다.
메시지는 `core/i18n.rs`의 사전에 키로 관리한다 (기존 `pve_xpenol_install.sh`의
`t()` / `tf()` 패턴과 동일한 접근).

## 9. 에러 처리

조용한 실패를 만들지 않는다. 각 실패는 원인과 다음 행동을 함께 제시한다.

| 상황 | 처리 |
|---|---|
| 관리자 권한 없음 | manifest로 상승 요청. 거부되면 이유를 설명하고 종료 |
| 볼륨 잠금 실패 (15초 초과) | 어떤 프로그램이 USB를 쓰고 있을 수 있다고 안내, 재시도 제공 |
| `ERROR_ACCESS_DENIED` (모든 잠금 성공 후) | Defender의 Controlled Folder Access 가능성 안내 |
| 내려받기 실패 | 재시도 / 다른 로더 선택 제공 |
| 쓰기 중 실패 | USB가 불완전한 상태임을 명확히 알린다. 부팅 시도하지 말 것 |
| 쓰기 후 Windows "디스크를 포맷하세요" 대화상자 | **정상이며 취소를 누르라고** 미리 안내 |

마지막 항목이 중요하다. Windows 10 1703부터 이동식 미디어의 모든 파티션이 마운트되므로,
로더 이미지의 리눅스 파티션 때문에 이 대화상자가 반드시 뜬다. 예고 없이 뜨면
사용자는 USB가 망가진 줄 안다.

## 10. 테스트 전략

| 대상 | 방법 | 실행 위치 |
|---|---|---|
| 안전 인터록 | 순수 함수 단위 테스트 | Linux, CI |
| 로더 에셋 선택 | 실제 API 응답 픽스처로 테스트 | Linux, CI |
| 진행률·속도 계산 | 단위 테스트 | Linux, CI |
| 압축 해제 (gz/zip) | 작은 픽스처로 왕복 테스트 | Linux, CI |
| 전체 파이프라인 | `RawWriter`를 임시 파일로 대체한 통합 테스트 | Linux, CI |
| Win32 인터롭 | 컴파일 검증 | CI (windows-latest) |
| 실제 USB 쓰기 및 부팅 | **수동 체크리스트** | 실물 Windows PC |

마지막 줄은 자동화할 수 없다. CI 러너에는 USB가 없다. 이 부분은 체크리스트 문서로
남기고, 자동 검증되지 않았음을 명시한다.

## 11. 빌드 및 릴리스

- 개발: Linux에서 `npm run tauri dev` (가짜 장치로 UI 확인)
- CI: `windows-latest` 러너에서 빌드 및 테스트
- 릴리스: 태그를 밀면 GitHub Actions가 `.exe`를 빌드해 Release에 첨부
- WebView2 부재 환경 대비: 부트스트래퍼 포함 옵션을 켠다

## 12. 미해결 사항 및 리스크

| 항목 | 상태 |
|---|---|
| 로더 에셋 이름 패턴 안정성 | 조사 진행 중. 어느 결과든 7장의 전략이 견딘다 |
| USB 열거 세부 API 선택 | 조사 진행 중. `MSFT_Disk` vs `Win32_DiskDrive` |
| USB 연결 SSD가 이동식으로 보고되지 않는 경우 | 노출 정책 결정 필요 |
| 코드 서명 없음 | SmartScreen 경고가 뜬다. 인증서 도입 여부는 별도 판단 |
| 실물 검증 의존 | 쓰기 경로의 최종 확인은 수동으로만 가능 |
