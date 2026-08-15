#!/usr/bin/env bash
#
# CI 가 하는 검사를 로컬에서 그대로 돌린다.
#
# 왜 필요한가: `cargo clippy` 를 그냥 돌리면 `#[cfg(windows)]` 로 감싼 코드가
# 아예 컴파일되지 않아 Windows 전용 코드의 문제를 못 잡는다. 실제로 그것 때문에
# CI 가 세 번 연속 실패했다. Windows 타겟을 명시적으로 함께 검사한다.
#
# 사용법:  ./check.sh
#
# 요구사항 (리눅스에서 Windows 타겟 검사용):
#   rustup target add x86_64-pc-windows-gnu
#   sudo apt-get install gcc-mingw-w64-x86-64

set -euo pipefail
cd "$(dirname "$0")"

WIN_TARGET=x86_64-pc-windows-gnu

echo "==> 프런트엔드 빌드"
npm run build

cd src-tauri

echo "==> 서식"
cargo fmt --all -- --check

echo "==> clippy (호스트)"
cargo clippy --all-targets -- -D warnings

if rustup target list --installed | grep -q "$WIN_TARGET"; then
  echo "==> clippy (Windows 타겟) — cfg(windows) 코드는 여기서만 검사된다"
  cargo clippy --target "$WIN_TARGET" --lib -- -D warnings

  # VHD 통합 테스트는 여기서 컴파일만 확인한다. 실행에는 윈도우와 가상 디스크가
  # 필요해서 CI 의 vhd-write 잡이 담당한다.
  echo "==> VHD 통합 테스트 컴파일 확인"
  cargo check --target "$WIN_TARGET" --features vhd-tests --all-targets
else
  echo "!! Windows 타겟이 설치돼 있지 않아 건너뛴다."
  echo "!! cfg(windows) 코드가 검사되지 않으므로 CI 에서 처음 드러날 수 있다."
  echo "!! rustup target add $WIN_TARGET"
fi

echo "==> 테스트"
cargo test --all-targets

echo
echo "전부 통과했다."
