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

pub mod commands;
pub mod core;
pub mod device;

use device::UsbEnumerator;

/// 실행 환경에 맞는 열거자.
///
/// Windows 가 아닌 곳에서는 가짜 표본을 쓴다. 앱이 개발 환경에서도 그대로 떠서
/// UI 흐름을 실제로 확인할 수 있고, 표본에 위험한 장치를 섞어뒀기 때문에
/// 안전 규칙이 깨지면 화면만 봐도 드러난다.
fn enumerator() -> Box<dyn UsbEnumerator> {
    #[cfg(windows)]
    {
        Box::new(device::windows::WindowsEnumerator::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(device::fake::FakeEnumerator::sample())
    }
}

#[tauri::command]
fn list_disks() -> Result<Vec<commands::DiskEntry>, String> {
    commands::list_disks_with(enumerator().as_ref())
}

/// 개발용 가짜 데이터로 실행 중인가. UI 에 배너를 띄우기 위한 것.
///
/// 개발 환경 화면을 실제 동작으로 착각해 "USB 가 목록에 보인다" 고 판단하는 일을
/// 막는다.
#[tauri::command]
fn is_simulated() -> bool {
    !cfg!(windows)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![list_disks, is_simulated])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
