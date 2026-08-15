//! Xpenology USB Writer
//!
//! Xpenology 부트로더(m-shell / RR)를 USB 저장장치에 굽는 프로그램.
//!
//! 구조는 검증 가능성을 기준으로 나뉜다:
//!
//! - [`core`] — 플랫폼 무관. 안전 규칙, 릴리스 해석, 진행률 계산, 전체 흐름.
//!   개발 환경(Linux)과 CI 에서 전부 단위 테스트된다.
//! - [`device`] — Windows API 를 부르는 유일한 곳. 트레이트 뒤에 격리돼 있어
//!   가짜 구현으로 대체하면 실제 하드웨어 없이 전체 흐름을 테스트할 수 있다.
//! - [`io_real`] — 네트워크와 압축 해제. 마찬가지로 트레이트 뒤에 있다.

pub mod commands;
pub mod core;
pub mod device;
pub mod io_real;

use core::loader::Loader;
use core::runner::{self, Cancel, RunConfig};
use device::{RawWriter, UsbEnumerator};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager};

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

/// 실행 환경에 맞는 쓰기 구현.
///
/// Windows 가 아닌 곳에서는 메모리에 쓴다. UI 를 끝까지 눌러볼 수 있으면서
/// 개발자의 디스크는 안전하다.
fn writer() -> Box<dyn RawWriter> {
    #[cfg(windows)]
    {
        Box::new(device::windows::WindowsRawWriter::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(device::fake::FakeWriter::new(64 * 1024 * 1024, 512))
    }
}

/// 취소 플래그.
struct Flag(Arc<AtomicBool>);
impl Cancel for Flag {
    fn is_canceled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// 진행 중인 작업의 취소 스위치.
#[derive(Default)]
struct AppState {
    cancel: Arc<AtomicBool>,
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

#[tauri::command]
fn cancel_write(state: tauri::State<'_, AppState>) {
    state.cancel.store(true, Ordering::Relaxed);
}

/// 이미지를 굽는다.
///
/// 오래 걸리는 동기 작업이라 별도 스레드에서 돌리고, 진행 상황은
/// `progress` 이벤트로 흘려보낸다. 이 커맨드 자체는 작업이 끝날 때 반환된다.
#[tauri::command]
async fn write_image(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    disk_number: u32,
    loader: String,
    verify: bool,
) -> Result<core::runner::RunSummary, String> {
    let loader = match loader.as_str() {
        "MShell" => Loader::MShell,
        "Rr" => Loader::Rr,
        other => return Err(format!("알 수 없는 로더: {other}")),
    };

    // 새 작업이므로 이전 취소 신호를 지운다.
    state.cancel.store(false, Ordering::Relaxed);
    let cancel = Arc::clone(&state.cancel);

    tauri::async_runtime::spawn_blocking(move || {
        let enumerator = enumerator();
        let protected = enumerator
            .protected_disk_numbers()
            .map_err(|e| format!("{e:?}"))?;
        let disks = enumerator.list_disks().map_err(|e| format!("{e:?}"))?;

        // 번호로 다시 찾는다. 목록을 만든 뒤 장치가 바뀌었을 수 있으므로
        // 프런트엔드가 보낸 정보를 신뢰하지 않고 지금 상태에서 조회한다.
        let disk = disks
            .into_iter()
            .find(|d| d.number == disk_number)
            .ok_or_else(|| "선택한 USB를 찾을 수 없습니다".to_string())?;

        let io = io_real::RealIo::new()?;
        let w = writer();

        runner::run(
            RunConfig { loader, verify },
            &disk,
            &protected,
            &io,
            w.as_ref(),
            &Flag(cancel),
            |ev| {
                // 이벤트 전송 실패는 창이 닫힌 경우이므로 작업을 멈출 이유가 없다.
                let _ = app.emit("progress", &ev);
            },
        )
        .map_err(|e| format!("{e:?}"))
    })
    .await
    .map_err(|e| format!("작업 스레드 오류: {e}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(AppState::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_disks,
            is_simulated,
            write_image,
            cancel_write
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
