# Xpenology USB Writer

Xpenology(헤놀로지)를 **베어메탈로 설치**할 때 쓰는 부팅 USB를 프로그램 하나로 만든다.

로더를 직접 찾아 받고, 압축을 풀고, Rufus로 굽는 과정을 대체한다.

## 상태

**구현은 끝났고, 실물 검증이 남았다.**

플랫폼 무관 로직은 테스트되고 Windows 전용 코드는 Windows 타겟으로 컴파일되지만,
실제 USB에 써본 적은 없다. CI 러너에는 USB가 없어서 그 부분은 원리적으로 자동 검증이
불가능하다 ([`docs/MANUAL_TEST.md`](docs/MANUAL_TEST.md) 참고). 어디까지가 자동으로
검증되는지는 아래 [검증되지 않는 부분](#검증되지-않는-부분)에 적었다.

빌드된 실행 파일은 [Releases](https://github.com/svrforum/SFTool/releases)에 있다.

## 하는 일

1. USB 저장장치를 감지한다 — **USB 버스에 연결된 장치만** 목록에 나온다
2. 최신 로더(m-shell / RR)를 자동으로 찾아 내려받는다
3. 압축을 푼다
4. USB에 raw 이미지를 쓴다
5. (선택) 쓴 내용을 되읽어 검증한다

## 하지 않는 일

**DSM 모델 선택을 하지 않는다.** m-shell과 RR의 이미지는 모델 정보가 없는 범용 raw
이미지다. 기종·시리얼·MAC 설정은 USB로 부팅한 뒤 로더 자체 화면에서 한다.

이 프로그램의 책임은 "올바른 이미지를 올바른 USB에 정확히 굽는 것"에서 끝난다.

## 안전

되돌릴 수 없는 작업이라 안전 규칙을 기능이 아니라 명세로 다룬다.

- USB 버스(`BusType == 7`)에 연결된 장치만 노출한다. `Removable` 플래그는 쓰지 않는다 —
  USB SSD는 removable=false로, 빈 카드리더는 true로 보고하기 때문에 근거가 되지 못한다
- 디스크 번호 0은 무조건 거부한다
- 시스템 드라이브·윈도우 폴더·실행 파일·페이지파일이 있는 디스크는 커널에 직접 물어
  구한 보호 집합으로 차단한다. WMI 정보가 틀려도 이 방어선은 남는다
- 디스크 번호는 안정적이지 않으므로, 쓰기 직전 열린 핸들에서 장치 신원을 재확인한다

이 규칙들은 전부 순수 함수로 구현돼 실제 하드웨어 없이 테스트된다.

## 개발

### 요구사항

- Rust (stable)
- Node.js
- Linux에서 개발하려면: `libwebkit2gtk-4.1-dev` 등 Tauri 의존성

### 명령

```bash
npm install

# CI 가 하는 검사를 그대로 실행한다. 푸시 전에 이것을 돌릴 것.
./check.sh

# 개발 실행 (Windows 가 아니면 가짜 장치로 뜬다)
npm run tauri dev

# 윈도우 배포물은 CI(windows-latest)에서만 만들어진다
```

`check.sh` 가 `cargo clippy` 만 돌리지 않는 이유가 있다. `#[cfg(windows)]` 로 감싼
코드는 리눅스에서 아예 컴파일되지 않아 검사에서 빠진다. 실제로 그 때문에 로컬이
초록불인 채로 CI 가 세 번 연속 실패했다. 그래서 Windows 타겟도 함께 검사한다:

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get install gcc-mingw-w64-x86-64
```

### 구조

```
src/                  프런트엔드 (TypeScript + Vite, 프레임워크 없음)
src-tauri/src/
  core/               플랫폼 무관. 어디서든 테스트된다
    model.rs          도메인 타입
    safety.rs         안전 인터록 (순수 함수)
    loader.rs         릴리스 에셋 해석
  device/             Windows API를 부르는 유일한 곳. 트레이트 뒤에 격리
docs/specs/           설계 문서
```

Windows 의존 코드를 트레이트 뒤에 두는 이유는 검증 때문이다. 가짜 구현으로 바꾸면
실제 USB 없이도 안전 규칙과 전체 파이프라인을 테스트할 수 있고, Linux에서 앱을 띄워
UI를 확인할 수 있다.

## 검증되지 않는 부분

실제 USB에 쓰고 부팅하는 것은 자동 검증이 불가능하다. CI 러너에는 USB가 없다.
가상 디스크(VHD)를 붙여 쓰기와 복제 경로 자체는 CI가 확인하지만, 버스 타입 판정과
안전 제거와 부팅은 실물 Windows PC에서 수동으로 확인해야 한다.

**`device/windows/` 에는 테스트가 없다.** `#[cfg(windows)]` 뒤에 있어 리눅스에서는
컴파일조차 되지 않고, 그 안은 Win32 호출과 그 결과를 옮기는 배선이라 실제 장치 없이
부를 수 없다. 수정을 하나씩 일부러 되돌려 확인해 보면 이 층은 되돌려도 테스트가 전부
초록불이다 — 판정은 `device/prep.rs` 로 빼놓아 그쪽만 잡힌다. Windows 타겟 clippy 는
컴파일과 린트를 볼 뿐 동작을 보지 않는다. 그래서 이 층에서 사용자에게 나가는 문구는
[`docs/MANUAL_TEST.md`](docs/MANUAL_TEST.md) 의 항목으로만 확인된다.

프런트엔드에도 테스트가 없다. `tsc` 의 타입 검사가 전부다.

## 라이선스

미정.
