//! 전체 작업 흐름.
//!
//! 해석 → 내려받기 → 압축 해제 → 준비 → 쓰기 → (검증) → 마무리.
//!
//! 입출력은 전부 트레이트나 주입된 함수 뒤에 있어서, 가짜 구현으로
//! 이 흐름 전체를 실제 USB 없이 완주시킬 수 있다.

use super::loader::{Loader, Release, ResolvedImage};
use super::pipeline::ProgressReporter;
use super::progress::Stage;
use super::safety::{self, Rejection};
use crate::core::model::DiskInfo;
use crate::device::{DeviceError, RawWriter};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;

/// 장치에 한 번에 보내는 크기. 섹터 배수로 맞춰 쓴다.
const CHUNK: usize = 8 * 1024 * 1024;

/// 장치 끝에서 지울 크기.
///
/// 이미지가 USB 보다 작으면 끝에 남은 옛 GPT 백업 헤더 때문에 Windows 가
/// 지워진 파티션 테이블을 되살린다.
const TAIL_ZERO: u64 = 1024 * 1024;

/// 작업 실패 원인.
#[derive(Debug)]
pub enum RunError {
    /// 릴리스를 해석하지 못했다.
    Resolve(String),
    /// 내려받기 실패.
    Download(String),
    /// 압축 해제 실패.
    Extract(String),
    /// 안전 규칙에 걸렸다.
    Rejected(Rejection),
    /// 쓰기 직전 확인에서 다른 장치로 판명됐다.
    IdentityChanged,
    /// 장치 오류.
    Device(DeviceError),
    /// 검증 불일치.
    VerifyMismatch { offset: u64 },
    /// 사용자가 취소했다.
    Canceled,
}

impl From<DeviceError> for RunError {
    fn from(e: DeviceError) -> Self {
        RunError::Device(e)
    }
}

/// 바깥에서 주입하는 입출력.
///
/// 네트워크와 파일 접근을 트레이트로 빼서, 테스트에서는 고정된 응답과
/// 메모리 버퍼로 대체한다.
pub trait Io {
    /// 릴리스 목록을 가져온다.
    fn fetch_releases(&self, url: &str) -> Result<Vec<Release>, String>;

    /// 이미지를 내려받는다.
    ///
    /// 진행 콜백은 (누적 바이트, 전체 바이트) 를 받는다.
    ///
    /// `should_stop` 이 참을 돌려주면 즉시 중단해야 한다. 이 인자가 없던 시절에는
    /// 취소가 **내려받기 전체가 끝난 뒤에야** 확인돼서, 1.3GB 를 받는 도중
    /// 취소를 눌러도 30분을 기다려야 했다. 사용자는 그냥 강제 종료한다.
    fn download(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
        should_stop: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, String>;

    /// 압축을 푼 스트림을 연다.
    ///
    /// gz 와 zip 을 모두 다뤄야 한다. zip 은 내부 항목 중 이미지 하나를 고른다.
    fn open_decompressed(&self, data: Vec<u8>, name: &str) -> Result<Box<dyn Read + Send>, String>;
}

/// 취소 신호. 쓰기 도중에도 확인한다.
pub trait Cancel {
    fn is_canceled(&self) -> bool;
}

/// 취소를 지원하지 않는 기본 구현.
pub struct NeverCancel;
impl Cancel for NeverCancel {
    fn is_canceled(&self) -> bool {
        false
    }
}

/// 작업 설정.
pub struct RunConfig {
    pub loader: Loader,
    pub verify: bool,
}

/// 전체 작업을 수행한다.
#[allow(clippy::too_many_arguments)]
pub fn run<F: FnMut(super::pipeline::ProgressEvent)>(
    cfg: RunConfig,
    disk: &DiskInfo,
    protected: &HashSet<u32>,
    io: &dyn Io,
    writer: &dyn RawWriter,
    cancel: &dyn Cancel,
    emit: F,
) -> Result<(), RunError> {
    let mut rep = ProgressReporter::new(emit);

    // --- 1. 어떤 이미지를 받을지 정한다 -------------------------------------
    rep.begin(Stage::Resolving, None);
    let releases = io
        .fetch_releases(&cfg.loader.releases_api_url())
        .map_err(RunError::Resolve)?;
    let image: ResolvedImage = super::loader::resolve(cfg.loader, &releases)
        .map_err(|e| RunError::Resolve(format!("{e:?}")))?;

    // 용량 확인은 압축을 푼 크기로 해야 맞다. 여기서는 아직 모르므로
    // 최소 용량 규칙만 먼저 적용하고, 실제 크기 확인은 쓰기 단계에서 한다.
    safety::can_write(disk, protected, 0, None).map_err(RunError::Rejected)?;

    if cancel.is_canceled() {
        return Err(RunError::Canceled);
    }

    // --- 2. 내려받기 --------------------------------------------------------
    rep.begin(
        Stage::Downloading,
        Some(format!("{} {}", cfg.loader.display_name(), image.tag)),
    );
    let mut last = std::time::Instant::now();
    let mut prev_done = 0u64;
    let compressed = io
        .download(
            &image.download_url,
            &mut |done, total| {
                let now = std::time::Instant::now();
                let dt = now.duration_since(last).as_secs_f64();
                rep.update(done, total, dt, done.saturating_sub(prev_done));
                last = now;
                prev_done = done;
            },
            &|| cancel.is_canceled(),
        )
        .map_err(|e| {
            if cancel.is_canceled() {
                RunError::Canceled
            } else {
                RunError::Download(e)
            }
        })?;

    if cancel.is_canceled() {
        return Err(RunError::Canceled);
    }

    // --- 3. 압축 해제 + 쓰기 ------------------------------------------------
    // 푼 내용을 파일로 떨구지 않고 장치로 바로 흘려보낸다.
    // 3GB 짜리 임시 파일을 만들지 않으므로 디스크 여유가 없는 기계에서도 동작한다.
    rep.begin(Stage::Extracting, None);
    let mut stream = io
        .open_decompressed(compressed, &image.asset_name)
        .map_err(RunError::Extract)?;

    rep.begin(Stage::Preparing, None);
    let mut session = writer.open(disk)?;

    // 쓰기 직전 신원 확인. 목록을 만든 뒤 USB 가 바뀌었을 수 있다.
    safety::confirm_identity(disk, session.observed()).map_err(|_| RunError::IdentityChanged)?;

    let sector = session.sector_size() as usize;
    let capacity = session.total_bytes();

    rep.begin(Stage::Writing, None);
    let mut buf = vec![0u8; CHUNK];
    let mut offset: u64 = 0;
    let mut last_t = std::time::Instant::now();
    // 쓰면서 해시를 쌓아 둔다. 검증을 켰을 때 되읽은 내용과 대조하기 위한 것으로,
    // 여기서 계산해 두지 않으면 이미지를 다시 내려받아야 한다.
    let mut write_hasher = Sha256::new();

    loop {
        if cancel.is_canceled() {
            return Err(RunError::Canceled);
        }

        // 청크를 채운다. Read 는 요청보다 적게 줄 수 있으므로 반복해서 채운다.
        let mut filled = 0usize;
        while filled < buf.len() {
            match stream.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(RunError::Extract(e.to_string())),
            }
        }
        if filled == 0 {
            break;
        }

        // 장치에는 섹터 배수로만 쓸 수 있다. 마지막 조각은 올림하고
        // 남는 부분을 0 으로 채운다 — 할당된 그대로 두면 힙 내용이 USB 에 실린다.
        let padded = filled.div_ceil(sector) * sector;
        if padded > filled {
            buf[filled..padded].fill(0);
        }

        if offset + padded as u64 > capacity {
            return Err(RunError::Rejected(Rejection::TooSmall {
                need: offset + padded as u64,
                have: capacity,
            }));
        }

        session.write_at(offset, &buf[..padded])?;
        write_hasher.update(&buf[..padded]);
        offset += padded as u64;

        let now = std::time::Instant::now();
        rep.update(
            offset,
            None, // 푼 크기를 미리 알 수 없어 불확정으로 표시한다
            now.duration_since(last_t).as_secs_f64(),
            padded as u64,
        );
        last_t = now;
    }

    // 이미지가 USB 보다 작으면 끝에 옛 GPT 백업 헤더가 남는다.
    //
    // 지우는 범위가 방금 쓴 이미지를 침범해서는 안 된다. 무조건 마지막 1MiB 를
    // 지우면 이미지가 장치 끝까지 닿는 경우 이미지를 훼손한다.
    // 이미지가 끝난 지점 이후만 지운다.
    let tail_start = capacity.saturating_sub(TAIL_ZERO).max(offset);
    if tail_start < capacity {
        session.zero_tail(capacity - tail_start)?;
    }

    let written_hash = write_hasher.finalize();

    // --- 4. 검증 (선택) -----------------------------------------------------
    //
    // 쓰는 동안 계산해 둔 해시와, 장치에서 되읽어 계산한 해시를 비교한다.
    // 이미지를 다시 내려받지 않고도 "쓴 것과 장치에 있는 것이 같은가" 를
    // 실제로 확인할 수 있다. 불량 USB 는 쓰기가 성공했다고 보고하고도
    // 내용이 다른 경우가 있어서, 이 대조가 유일하게 그것을 잡는다.
    if cfg.verify {
        rep.begin(Stage::Verifying, None);
        let mut back = vec![0u8; CHUNK];
        let mut hasher = Sha256::new();
        let mut pos = 0u64;
        let mut last_v = std::time::Instant::now();

        while pos < offset {
            if cancel.is_canceled() {
                return Err(RunError::Canceled);
            }
            let n = (CHUNK as u64).min(offset - pos) as usize;
            // 읽기도 섹터 배수여야 한다.
            let n = (n / sector) * sector;
            if n == 0 {
                break;
            }
            session.read_at(pos, &mut back[..n])?;
            hasher.update(&back[..n]);
            pos += n as u64;

            let now = std::time::Instant::now();
            rep.update(
                pos,
                Some(offset),
                now.duration_since(last_v).as_secs_f64(),
                n as u64,
            );
            last_v = now;
        }

        let read_back = hasher.finalize();
        if read_back.as_slice() != written_hash.as_slice() {
            return Err(RunError::VerifyMismatch { offset: 0 });
        }
    }

    // --- 5. 마무리 ----------------------------------------------------------
    rep.begin(Stage::Finishing, None);
    session.finish()?;
    rep.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::loader::Asset;
    use crate::device::fake::{FakeEnumerator, FakeWriter};
    use crate::device::UsbEnumerator;

    /// 고정된 응답을 돌려주는 가짜 입출력.
    struct FakeIo {
        payload: Vec<u8>,
    }

    impl Io for FakeIo {
        fn fetch_releases(&self, _url: &str) -> Result<Vec<Release>, String> {
            Ok(vec![Release {
                tag_name: "v1.4.2.8".into(),
                draft: false,
                prerelease: false,
                assets: vec![Asset {
                    name: "alpine-redpill.v1.4.2.8.m-shell-5GB.img.gz".into(),
                    size: 100,
                    browser_download_url: "https://example.invalid/img".into(),
                }],
            }])
        }

        fn download(
            &self,
            _url: &str,
            on_progress: &mut dyn FnMut(u64, Option<u64>),
            _should_stop: &dyn Fn() -> bool,
        ) -> Result<Vec<u8>, String> {
            let total = self.payload.len() as u64;
            on_progress(total / 2, Some(total));
            on_progress(total, Some(total));
            Ok(self.payload.clone())
        }

        fn open_decompressed(
            &self,
            data: Vec<u8>,
            _name: &str,
        ) -> Result<Box<dyn Read + Send>, String> {
            Ok(Box::new(std::io::Cursor::new(data)))
        }
    }

    fn usb() -> DiskInfo {
        FakeEnumerator::sample().list_disks().unwrap()[2].clone()
    }

    #[test]
    fn full_run_writes_the_image_to_the_device() {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let io = FakeIo {
            payload: payload.clone(),
        };
        let writer = FakeWriter::new(1024 * 1024, 512);
        let mut events = Vec::new();

        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |e| events.push(e),
        )
        .expect("작업이 성공해야 한다");

        // 이미지가 장치 앞부분에 그대로 쓰여야 한다.
        let written = writer.contents();
        assert_eq!(&written[..payload.len()], &payload[..]);
        // 마무리가 호출돼야 한다. 빠뜨리면 플러시 없이 끝난다.
        assert!(writer.was_finished(), "finish 가 호출되지 않았다");
    }

    #[test]
    fn every_stage_is_reported_in_order() {
        let io = FakeIo {
            payload: vec![7u8; 2048],
        };
        let writer = FakeWriter::new(1024 * 1024, 512);
        let mut events = Vec::new();
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |e| events.push(e),
        )
        .unwrap();

        let seen: Vec<Stage> = events.iter().map(|e| e.stage).collect();
        for s in [
            Stage::Resolving,
            Stage::Downloading,
            Stage::Extracting,
            Stage::Preparing,
            Stage::Writing,
            Stage::Finishing,
        ] {
            assert!(seen.contains(&s), "{s:?} 단계가 보고되지 않았다");
        }
        // 마지막 이벤트에는 모든 단계가 완료로 찍혀야 한다.
        assert!(events.last().unwrap().completed.contains(&Stage::Finishing));
    }

    #[test]
    fn tail_is_zeroed_so_an_old_gpt_backup_cannot_survive() {
        let io = FakeIo {
            payload: vec![1u8; 1024],
        };
        // 0xAA 로 채워진 장치. 꼬리가 0 이 돼야 한다.
        let writer = FakeWriter::new(4 * 1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        let c = writer.contents();
        let tail = &c[c.len() - TAIL_ZERO as usize..];
        assert!(tail.iter().all(|b| *b == 0), "장치 끝이 지워지지 않았다");
    }

    #[test]
    fn padding_is_zeroed_not_leaked_heap() {
        // 섹터 배수가 아닌 크기. 남는 부분이 0 이어야 한다.
        let payload = vec![9u8; 700];
        let io = FakeIo {
            payload: payload.clone(),
        };
        let writer = FakeWriter::new(1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        let c = writer.contents();
        assert_eq!(&c[..700], &payload[..]);
        // 700 -> 1024 로 올림되며 채워진 부분.
        assert!(c[700..1024].iter().all(|b| *b == 0), "패딩이 0 이 아니다");
    }

    #[test]
    fn refuses_when_the_device_is_a_different_one() {
        // 목록을 만든 뒤 USB 가 바뀐 상황.
        let disks = FakeEnumerator::sample().list_disks().unwrap();
        let selected = disks[2].clone();
        let mut swapped = disks[3].clone();
        swapped.number = selected.number;

        let io = FakeIo {
            payload: vec![0u8; 512],
        };
        let writer = FakeWriter::new(1024 * 1024, 512).with_observed(swapped);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &selected,
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, RunError::IdentityChanged), "실제: {err:?}");
    }

    #[test]
    fn refuses_a_protected_disk() {
        let io = FakeIo {
            payload: vec![0u8; 512],
        };
        let writer = FakeWriter::new(1024 * 1024, 512);
        let protected: HashSet<u32> = [2].into_iter().collect();
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &protected,
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, RunError::Rejected(_)), "실제: {err:?}");
    }

    #[test]
    fn stops_when_the_image_exceeds_the_device() {
        let io = FakeIo {
            payload: vec![3u8; 8192],
        };
        // 장치가 이미지보다 작다.
        let writer = FakeWriter::new(4096, 512);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, RunError::Rejected(_)) || matches!(err, RunError::Device(_)));
    }

    #[test]
    fn verify_passes_on_a_healthy_device() {
        let io = FakeIo {
            payload: (0..8192u32).map(|i| (i % 253) as u8).collect(),
        };
        let writer = FakeWriter::new(1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: true,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("정상 장치에서는 검증이 통과해야 한다");
    }

    #[test]
    fn verify_catches_a_device_that_lies_about_writing() {
        // 불량 USB 는 쓰기가 성공했다고 보고하고도 내용이 다를 수 있다.
        // 검증이 실제로 의미를 가지려면 이 경우를 잡아야 한다.
        let io = FakeIo {
            payload: (0..8192u32).map(|i| (i % 253) as u8).collect(),
        };
        let writer = FakeWriter::new(1024 * 1024, 512).corrupting_after(4096);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: true,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(err, RunError::VerifyMismatch { .. }),
            "손상을 잡지 못했다: {err:?}"
        );
    }

    #[test]
    fn verify_is_skipped_when_not_requested() {
        // 검증을 끄면 손상된 장치라도 통과한다. 기본값이 꺼짐이므로
        // 이 동작을 명시적으로 기록해 둔다.
        let io = FakeIo {
            payload: vec![1u8; 8192],
        };
        let writer = FakeWriter::new(1024 * 1024, 512).corrupting_after(4096);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("검증을 끄면 통과해야 한다");
    }

    struct AlwaysCancel;
    impl Cancel for AlwaysCancel {
        fn is_canceled(&self) -> bool {
            true
        }
    }

    #[test]
    fn cancel_is_honoured_before_anything_is_written() {
        let io = FakeIo {
            payload: vec![5u8; 4096],
        };
        let writer = FakeWriter::new(1024 * 1024, 512);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &AlwaysCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, RunError::Canceled));
        // 장치는 손대지 않은 상태여야 한다.
        assert!(writer.contents().iter().all(|b| *b == 0xAA));
    }
}
