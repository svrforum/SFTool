//! Xpenology USB Writer
//!
//! Xpenology 부트로더(m-shell / RR)를 USB 저장장치에 굽는 프로그램.
//!
//! 구조는 검증 가능성을 기준으로 나뉜다:
//!
//! - [`core`] — 플랫폼 무관. 안전 규칙, 릴리스 해석, 진행률 계산.
//!   개발 환경(Linux)과 CI 에서 전부 단위 테스트된다.
//! - `device` — Windows API 를 부르는 유일한 곳. 트레이트 뒤에 격리돼 있어
//!   가짜 구현으로 대체하면 실제 하드웨어 없이 전체 흐름을 테스트할 수 있다.

pub mod core;
pub mod device;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
