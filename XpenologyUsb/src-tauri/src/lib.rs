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

pub mod cache;
pub mod commands;
pub mod core;
pub mod device;
pub mod io_real;

use core::loader::Loader;
use core::runner::{self, Cancel, RunConfig};
use core::selection::{self, Selections};
use core::{failure::Failure, loader::ResolvedImage, runner::Io};
use device::{RawWriter, UsbEnumerator};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

struct LocalSelection {
    file: std::fs::File,
    name: String,
    source_disks: HashSet<u32>,
}

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

/// 실행 환경에 맞는 읽기 구현.
fn reader() -> Box<dyn device::RawReader> {
    #[cfg(windows)]
    {
        Box::new(device::windows::WindowsRawReader::new())
    }
    #[cfg(not(windows))]
    {
        // 개발 환경에서는 파티션이 하나 있는 가짜 원본을 돌려준다.
        // UI 흐름을 끝까지 눌러볼 수 있으면서 개발자의 디스크는 안전하다.
        let mut img = vec![0x5Au8; 16 * 1024 * 1024];
        img[..512].fill(0);
        img[446 + 4] = 0x83;
        img[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        img[446 + 12..446 + 16].copy_from_slice(&14336u32.to_le_bytes());
        img[510] = 0x55;
        img[511] = 0xAA;
        Box::new(device::fake::FakeReader::new(img, 512))
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
    busy: Arc<AtomicBool>,
    selections: Mutex<Selections>,
    images: Arc<Mutex<BTreeMap<String, ResolvedImage>>>,
    local: Arc<Mutex<Option<LocalSelection>>>,
}

#[tauri::command]
fn list_disks(state: tauri::State<'_, AppState>) -> Result<commands::DiskList, String> {
    let mut selections = state.selections.lock().map_err(|e| e.to_string())?;
    commands::list_disks_register_with(enumerator().as_ref(), |d| selections.register(d))
}

struct OperationGuard(Arc<AtomicBool>);
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl AppState {
    fn begin(&self) -> Result<OperationGuard, String> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "이미 작업이 진행 중입니다".to_string())?;
        self.cancel.store(false, Ordering::Relaxed);
        Ok(OperationGuard(Arc::clone(&self.busy)))
    }
    fn selected(&self, id: &str) -> Result<core::model::DiskInfo, String> {
        self.selections
            .lock()
            .map_err(|e| e.to_string())?
            .selected(id)
    }
}

/// 개발용 가짜 데이터로 실행 중인가. UI 에 배너를 띄우기 위한 것.
///
/// 개발 환경 화면을 실제 동작으로 착각해 "USB 가 목록에 보인다" 고 판단하는 일을
/// 막는다.
#[tauri::command]
fn is_simulated() -> bool {
    !cfg!(windows)
}

/// USB 를 안전하게 제거한다.
///
/// 자동으로 하지 않고 버튼으로 둔 이유는, 자동 꺼내기는 실패해도 알 수 없고
/// 사용자가 다시 시도할 방법도 없기 때문이다. 눌러서 결과를 보는 편이 낫다.
#[tauri::command]
fn eject_disk(state: tauri::State<'_, AppState>, selection_id: String) -> Result<(), Failure> {
    let _operation = state.begin().map_err(|e| Failure::new("busy", e))?;
    let selected = state
        .selected(&selection_id)
        .map_err(|e| Failure::new("identity_changed", e))?;
    let enumerator = enumerator();
    let current = enumerator.list_disks().map_err(Failure::from)?;
    let disk =
        selection::resolve(&selected, &current).map_err(|e| Failure::new("identity_changed", e))?;
    let protected = enumerator.protected_disk_numbers().map_err(Failure::from)?;
    core::safety::is_listable(&disk, &protected)
        .map_err(|e| Failure::new("rejected", format!("{e:?}")))?;
    #[cfg(windows)]
    {
        device::windows::eject(&disk).map_err(Failure::from)
    }
    #[cfg(not(windows))]
    {
        let _ = disk;
        Ok(())
    }
}

/// 원본 USB 를 분석해 복사할 양을 알려준다.
///
/// 대상은 건드리지 않는다. 확인 화면이 "복사할 양 4.98 GB" 를 보여주려면
/// 복제를 시작하기 **전에** 이 값이 필요하다.
#[tauri::command]
fn analyze_source(
    state: tauri::State<'_, AppState>,
    selection_id: String,
) -> Result<commands::SourcePlan, Failure> {
    let enumerator = enumerator();
    let protected = enumerator.protected_disk_numbers().map_err(Failure::from)?;
    let disks = enumerator.list_disks().map_err(Failure::from)?;
    let selected = state
        .selected(&selection_id)
        .map_err(|e| Failure::new("identity_changed", e))?;
    let disk =
        selection::resolve(&selected, &disks).map_err(|e| Failure::new("identity_changed", e))?;

    core::cloner::analyze(reader().as_ref(), &disk, &protected)
        .map(commands::SourcePlan::from)
        .map_err(Failure::from)
}

/// USB 를 다른 USB 로 복제한다.
#[tauri::command]
async fn clone_disk(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    source: String,
    target: String,
    verify: bool,
) -> Result<core::cloner::CloneSummary, Failure> {
    let operation = state.begin().map_err(|e| Failure::new("busy", e))?;
    let selected_source = state
        .selected(&source)
        .map_err(|e| Failure::new("identity_changed", e))?;
    let selected_target = state
        .selected(&target)
        .map_err(|e| Failure::new("identity_changed", e))?;
    let cancel = Arc::clone(&state.cancel);

    tauri::async_runtime::spawn_blocking(move || {
        let _operation = operation;
        let enumerator = enumerator();
        let protected = enumerator.protected_disk_numbers().map_err(Failure::from)?;
        let disks = enumerator.list_disks().map_err(Failure::from)?;

        let src = selection::resolve(&selected_source, &disks)
            .map_err(|e| Failure::new("identity_changed", e))?;
        let dst = selection::resolve(&selected_target, &disks)
            .map_err(|e| Failure::new("identity_changed", e))?;

        core::cloner::run(
            core::cloner::CloneConfig { verify },
            &src,
            &dst,
            &protected,
            reader().as_ref(),
            writer().as_ref(),
            &Flag(cancel),
            |ev| {
                let _ = app.emit("progress", &ev);
            },
        )
        .map_err(Failure::from)
    })
    .await
    .map_err(|e| Failure::new("generic", format!("Worker: {e}")))?
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
    selection_id: String,
    loader: String,
    verify: bool,
    image_id: Option<String>,
    use_cache: Option<bool>,
) -> Result<core::runner::RunSummary, Failure> {
    let loader = match loader.as_str() {
        "MShell" => Loader::MShell,
        "Rr" => Loader::Rr,
        other => {
            return Err(Failure::new(
                "image_invalid",
                format!("Unknown loader: {other}"),
            ))
        }
    };

    let operation = state.begin().map_err(|e| Failure::new("busy", e))?;
    let selected = state
        .selected(&selection_id)
        .map_err(|e| Failure::new("identity_changed", e))?;
    let cancel = Arc::clone(&state.cancel);
    let image = image_id
        .map(|id| {
            state
                .images
                .lock()
                .map_err(|e| Failure::new("generic", e.to_string()))?
                .get(&id)
                .cloned()
                .ok_or_else(|| Failure::new("image_invalid", "Release selection expired"))
        })
        .transpose()?;
    if image.as_ref().is_some_and(|i| i.loader != loader) {
        return Err(Failure::new(
            "image_invalid",
            "Loader does not match selected release",
        ));
    }
    let directory = if use_cache.unwrap_or(true) {
        app.path().app_cache_dir().ok().map(|p| p.join("images"))
    } else {
        None
    };

    tauri::async_runtime::spawn_blocking(move || {
        let _operation = operation;
        let enumerator = enumerator();
        let protected = enumerator.protected_disk_numbers().map_err(Failure::from)?;
        let disks = enumerator.list_disks().map_err(Failure::from)?;

        let disk = selection::resolve(&selected, &disks)
            .map_err(|e| Failure::new("identity_changed", e))?;

        let io = cache::CachedIo {
            inner: io_real::RealIo::new().map_err(|e| Failure::new("network", e))?,
            directory,
        };
        let w = writer();

        let emit = |ev| {
            let _ = app.emit("progress", &ev);
        };
        let cfg = RunConfig { loader, verify };
        let result = if let Some(image) = image {
            runner::run_resolved(
                cfg,
                &disk,
                &protected,
                &io,
                w.as_ref(),
                &Flag(cancel),
                image,
                core::pipeline::ProgressReporter::new(emit),
            )
        } else {
            runner::run(cfg, &disk, &protected, &io, w.as_ref(), &Flag(cancel), emit)
        };
        result.map_err(Failure::from)
    })
    .await
    .map_err(|e| Failure::new("generic", format!("Worker: {e}")))?
}

#[derive(serde::Serialize)]
struct ImageEntry {
    id: String,
    tag: String,
    asset_name: String,
    size_bytes: u64,
    checksum: bool,
}

fn parse_loader(loader: &str) -> Result<Loader, Failure> {
    match loader {
        "MShell" => Ok(Loader::MShell),
        "Rr" => Ok(Loader::Rr),
        _ => Err(Failure::new("image_invalid", "Unknown loader")),
    }
}

#[tauri::command]
async fn release_images(
    state: tauri::State<'_, AppState>,
    loader: String,
) -> Result<Vec<ImageEntry>, Failure> {
    let loader = parse_loader(&loader)?;
    let images = Arc::clone(&state.images);
    tauri::async_runtime::spawn_blocking(move || {
        let io = io_real::RealIo::new().map_err(|e| Failure::new("network", e))?;
        let releases = io
            .fetch_releases(&loader.releases_api_url())
            .map_err(|e| Failure::new("network", e))?;
        let mut registry = images
            .lock()
            .map_err(|e| Failure::new("generic", e.to_string()))?;
        let mut entries = Vec::new();
        for release in &releases {
            if let Ok(image) = core::loader::resolve(loader, std::slice::from_ref(release)) {
                use sha2::{Digest, Sha256};
                let bytes = serde_json::to_vec(&image)
                    .map_err(|e| Failure::new("generic", e.to_string()))?;
                let id: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                entries.push(ImageEntry {
                    id: id.clone(),
                    tag: image.tag.clone(),
                    asset_name: image.asset_name.clone(),
                    size_bytes: image.compressed_size,
                    checksum: image.checksum_url.is_some(),
                });
                registry.entry(id).or_insert(image);
            }
        }
        if entries.is_empty() {
            return Err(Failure::new("network", "No stable release image available"));
        }
        Ok(entries)
    })
    .await
    .map_err(|e| Failure::new("generic", e.to_string()))?
}

#[derive(serde::Serialize)]
struct LocalEntry {
    name: String,
    size_bytes: u64,
}

#[tauri::command]
async fn pick_local_image(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<LocalEntry>, Failure> {
    let operation = state.begin().map_err(|e| Failure::new("busy", e))?;
    let local = Arc::clone(&state.local);
    let cancel = Arc::clone(&state.cancel);
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = operation;
        let Some(file) = app
            .dialog()
            .file()
            .add_filter("Disk images", &["img", "gz", "zip"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = file
            .into_path()
            .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
        #[cfg(windows)]
        let source_disks = device::windows::disks_for_path(&path).map_err(Failure::from)?;
        #[cfg(not(windows))]
        let source_disks = HashSet::new();
        let io = io_real::RealIo::new().map_err(|e| Failure::new("network", e))?;
        let snapshot = core::local::prepare(&path, &io, &Flag(cancel))?;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("local.img")
            .to_string();
        let size_bytes = snapshot
            .metadata()
            .map_err(|e| Failure::new("image_invalid", e.to_string()))?
            .len();
        *local
            .lock()
            .map_err(|e| Failure::new("generic", e.to_string()))? = Some(LocalSelection {
            file: snapshot,
            name: name.clone(),
            source_disks,
        });
        Ok(Some(LocalEntry { name, size_bytes }))
    })
    .await
    .map_err(|e| Failure::new("generic", e.to_string()))?
}

#[tauri::command]
async fn write_local_image(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    selection_id: String,
    verify: bool,
) -> Result<runner::RunSummary, Failure> {
    let operation = state.begin().map_err(|e| Failure::new("busy", e))?;
    let selected = state
        .selected(&selection_id)
        .map_err(|e| Failure::new("identity_changed", e))?;
    let cancel = Arc::clone(&state.cancel);
    let (mut snapshot, name) = {
        let local = state
            .local
            .lock()
            .map_err(|e| Failure::new("generic", e.to_string()))?;
        let local = local
            .as_ref()
            .ok_or_else(|| Failure::new("image_invalid", "Select a local image first"))?;
        if local.source_disks.contains(&selected.number) {
            return Err(Failure::new("rejected", "SourceOnTarget"));
        }
        (
            local
                .file
                .try_clone()
                .map_err(|e| Failure::new("image_invalid", e.to_string()))?,
            local.name.clone(),
        )
    };
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::Seek;
        let _operation = operation;
        snapshot
            .rewind()
            .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
        let enumerator = enumerator();
        let protected = enumerator.protected_disk_numbers().map_err(Failure::from)?;
        let disk = selection::resolve(&selected, &enumerator.list_disks().map_err(Failure::from)?)
            .map_err(|e| Failure::new("identity_changed", e))?;
        core::local::run(
            snapshot,
            &name,
            verify,
            &disk,
            &protected,
            writer().as_ref(),
            &Flag(cancel),
            |ev| {
                let _ = app.emit("progress", &ev);
            },
        )
    })
    .await
    .map_err(|e| Failure::new("generic", e.to_string()))?
}

#[tauri::command]
fn clear_cache(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), Failure> {
    let _operation = state.begin().map_err(|e| Failure::new("busy", e))?;
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| Failure::new("generic", e.to_string()))?;
    cache::clear(&dir.join("images")).map_err(|e| Failure::new("generic", e.to_string()))
}

#[tauri::command]
fn diagnostics() -> serde_json::Value {
    #[cfg(not(windows))]
    let os = std::env::consts::OS.to_string();
    #[cfg(windows)]
    let os = {
        use windows::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        if unsafe { GetVersionExW(&mut version) }.is_ok() {
            format!(
                "Windows {}.{} build {}",
                version.dwMajorVersion, version.dwMinorVersion, version.dwBuildNumber
            )
        } else {
            "Windows".to_string()
        }
    };
    serde_json::json!({ "app_version": env!("CARGO_PKG_VERSION"), "os": os, "arch": std::env::consts::ARCH })
}

#[tauri::command]
fn request_exit(app: tauri::AppHandle, state: tauri::State<'_, AppState>) {
    state.cancel.store(true, Ordering::Relaxed);
    let busy = Arc::clone(&state.busy);
    tauri::async_runtime::spawn(async move {
        while busy.load(Ordering::Acquire) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        app.exit(0);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.state::<AppState>().busy.load(Ordering::Acquire) {
                    api.prevent_close();
                    let _ = window.emit("close-blocked", ());
                }
            }
        })
        .setup(|app| {
            app.manage(AppState::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_disks,
            is_simulated,
            write_image,
            cancel_write,
            eject_disk,
            analyze_source,
            clone_disk,
            release_images,
            pick_local_image,
            write_local_image,
            clear_cache,
            diagnostics,
            request_exit
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod command_tests {
    use super::*;
    #[test]
    fn concurrent_jobs_cannot_reset_cancellation() {
        let state = AppState::default();
        let first = state.begin().unwrap();
        state.cancel.store(true, Ordering::Relaxed);
        assert!(state.begin().is_err());
        assert!(state.cancel.load(Ordering::Relaxed));
        drop(first);
        let _second = state.begin().unwrap();
        assert!(!state.cancel.load(Ordering::Relaxed));
    }
}
