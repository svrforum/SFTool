//! USB 를 다른 USB 로 복제한다.
//!
//! 로더를 내려받아 굽는 흐름(`runner`)과 대상 쪽 코드를 전부 공유한다.
//! 다른 것은 바이트가 어디서 오느냐뿐이다 — 네트워크 대신 원본 USB.
//!
//! ## 순서가 중요한 이유
//!
//! 원본 분석이 대상을 여는 것보다 **먼저** 온다. 원본이 GPT 이거나 파티션
//! 테이블이 없어서 거부할 상황에 대상을 이미 지워 놓으면, 사용자는 아무것도
//! 얻지 못한 채 멀쩡한 USB 하나를 잃는다.

use super::layout::{self, Layout, LayoutError};
use super::model::DiskInfo;
use super::pipeline::{Cancel, ProgressEvent, ProgressReporter};
use super::progress::Stage;
use super::safety::{self, Rejection};
use super::sink::{self, SinkError, CHUNK};
use super::source::SessionReader;
use crate::device::{DeviceError, RawReader, RawWriter};
use std::collections::HashSet;

/// 레이아웃 판정을 위해 원본 앞에서 읽는 양.
///
/// MBR(섹터 0)과 GPT 헤더(섹터 1)만 있으면 되지만, 512·4096 어느 섹터 크기에도
/// 배수가 되도록 넉넉히 잡는다.
const HEAD: u64 = 64 * 1024;

/// 복제 설정.
pub struct CloneConfig {
    pub verify: bool,
}

/// 복제가 끝난 뒤의 요약.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CloneSummary {
    pub bytes_copied: u64,
    pub partitions: u32,
    pub verified: bool,
    pub source_name: String,
    pub target_name: String,
}

/// 복제 실패 원인.
#[derive(Debug)]
pub enum CloneError {
    /// 원본에서 복사할 범위를 정하지 못했다.
    Layout(LayoutError),
    /// 안전 규칙에 걸렸다.
    Rejected(Rejection),
    /// 원본이 고를 때와 다른 장치다.
    SourceIdentityChanged,
    /// 대상이 고를 때와 다른 장치다.
    TargetIdentityChanged,
    Device(DeviceError),
    /// 원본을 읽지 못했다.
    Source(String),
    VerifyMismatch,
    Canceled,
}

impl From<DeviceError> for CloneError {
    fn from(e: DeviceError) -> Self {
        CloneError::Device(e)
    }
}

impl From<SinkError> for CloneError {
    fn from(e: SinkError) -> Self {
        match e {
            SinkError::Source(s) => CloneError::Source(s),
            SinkError::Device(d) => CloneError::Device(d),
            SinkError::TooSmall { need, have } => {
                CloneError::Rejected(Rejection::TooSmall { need, have })
            }
            SinkError::VerifyMismatch => CloneError::VerifyMismatch,
            SinkError::Canceled => CloneError::Canceled,
        }
    }
}

/// 원본을 열어 복사할 범위만 알아내고 닫는다.
///
/// 확인 화면에서 "복사할 양" 을 미리 보여주기 위한 것이다. 대상은 건드리지 않는다.
pub fn analyze(reader: &dyn RawReader, source: &DiskInfo) -> Result<Layout, CloneError> {
    let mut s = reader.open(source)?;
    safety::confirm_identity(source, s.observed())
        .map_err(|_| CloneError::SourceIdentityChanged)?;
    let l = read_layout(s.as_mut())?;
    s.finish()?;
    Ok(l)
}

/// 열린 원본 세션에서 레이아웃을 읽는다.
fn read_layout(s: &mut dyn crate::device::ReadSession) -> Result<Layout, CloneError> {
    let sector = s.sector_size();
    let total = s.total_bytes();
    // 장치보다 많이 요구하지 않고, 섹터 배수로 내림한다.
    let want = HEAD.min(total);
    let len = ((want / sector as u64) * sector as u64) as usize;
    let mut head = vec![0u8; len];
    s.read_at(0, &mut head)?;
    layout::parse(&head, total, sector).map_err(CloneError::Layout)
}

/// 원본 A 를 대상 B 로 복제한다.
#[allow(clippy::too_many_arguments)]
pub fn run<F: FnMut(ProgressEvent)>(
    cfg: CloneConfig,
    source: &DiskInfo,
    target: &DiskInfo,
    protected: &HashSet<u32>,
    reader: &dyn RawReader,
    writer: &dyn RawWriter,
    cancel: &dyn Cancel,
    emit: F,
) -> Result<CloneSummary, CloneError> {
    let mut rep = ProgressReporter::new(emit);

    // --- 1. 원본 분석 -------------------------------------------------------
    rep.begin(Stage::Analyzing, Some(source.friendly_name.clone()));
    let mut src = reader.open(source)?;
    safety::confirm_identity(source, src.observed())
        .map_err(|_| CloneError::SourceIdentityChanged)?;
    let plan = read_layout(src.as_mut())?;

    // --- 2. 여기까지 통과해야 대상을 건드린다 --------------------------------
    safety::can_clone(source, target, protected, plan.bytes).map_err(CloneError::Rejected)?;

    if cancel.is_canceled() {
        return Err(CloneError::Canceled);
    }

    rep.begin(Stage::Preparing, None);
    let mut dst = writer.open(target)?;
    safety::confirm_identity(target, dst.observed())
        .map_err(|_| CloneError::TargetIdentityChanged)?;

    // --- 3. 복사 ------------------------------------------------------------
    rep.begin(Stage::Writing, None);
    let mut stream = SessionReader::new(src, plan.bytes, CHUNK);
    let out = sink::stream(
        &mut stream,
        dst.as_mut(),
        Some(plan.bytes),
        cancel,
        &mut rep,
    )?;
    sink::zero_tail(dst.as_mut(), out.bytes)?;

    // --- 4. 검증 (선택) -----------------------------------------------------
    if cfg.verify {
        rep.begin(Stage::Verifying, None);
        sink::verify(dst.as_mut(), out.bytes, &out.hash, cancel, &mut rep)?;
    }

    // --- 5. 마무리 ----------------------------------------------------------
    rep.begin(Stage::Finishing, None);
    dst.finish()?;
    // 원본도 닫는다. 열어 둔 핸들이 남으면 사용자가 USB 를 뽑을 수 없다.
    stream.into_session().finish()?;
    rep.finish();

    Ok(CloneSummary {
        bytes_copied: out.bytes,
        partitions: plan.partitions,
        verified: cfg.verify,
        source_name: source.friendly_name.clone(),
        target_name: target.friendly_name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::pipeline::NeverCancel;
    use crate::device::fake::{FakeEnumerator, FakeReader, FakeWriter};
    use crate::device::UsbEnumerator;

    const SECTOR: u32 = 512;

    /// 파티션 하나가 `end_lba` 에서 끝나는 원본 디스크 이미지를 만든다.
    fn source_image(device_bytes: usize, end_lba: u32) -> Vec<u8> {
        let mut v: Vec<u8> = (0..device_bytes).map(|i| (i % 251) as u8).collect();
        // 앞 512 바이트를 MBR 로 덮는다.
        for b in v[..512].iter_mut() {
            *b = 0;
        }
        let off = 446;
        v[off + 4] = 0x83;
        v[off + 8..off + 12].copy_from_slice(&2048u32.to_le_bytes());
        v[off + 12..off + 16].copy_from_slice(&(end_lba - 2048).to_le_bytes());
        v[510] = 0x55;
        v[511] = 0xAA;
        v
    }

    fn sticks() -> (DiskInfo, DiskInfo, HashSet<u32>) {
        let e = FakeEnumerator::sample();
        let protected = e.protected_disk_numbers().unwrap();
        let usable: Vec<DiskInfo> = e
            .list_disks()
            .unwrap()
            .into_iter()
            .filter(|d| crate::core::safety::availability(d, &protected).is_ready())
            .collect();
        (usable[0].clone(), usable[1].clone(), protected)
    }

    #[test]
    fn a_clone_reproduces_the_source_bytes() {
        let (a, b, protected) = sticks();
        let img = source_image(8 * 1024 * 1024, 8192); // 4MiB 까지가 파티션
        let reader = FakeReader::new(img.clone(), SECTOR);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let s = run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        assert_eq!(s.bytes_copied, 8192 * 512);
        assert_eq!(&writer.contents()[..8192 * 512], &img[..8192 * 512]);
        assert!(writer.was_finished());
    }

    #[test]
    fn nothing_is_written_when_the_source_cannot_be_analyzed() {
        // 이 프로젝트에서 가장 중요한 테스트다. 원본이 로더 USB 가 아니면
        // 대상은 **한 바이트도** 건드려지지 않아야 한다. 순서가 뒤집히면
        // 사용자는 아무것도 얻지 못한 채 멀쩡한 USB 를 잃는다.
        let (a, b, protected) = sticks();
        let reader = FakeReader::new(vec![0u8; 1024 * 1024], SECTOR); // 서명 없음
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let e = run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(e, CloneError::Layout(LayoutError::NoSignature)));
        assert!(writer.write_offsets().is_empty(), "대상에 쓰기가 일어났다");
        assert!(!writer.was_finished());
    }

    #[test]
    fn a_gpt_source_is_refused_before_the_target_is_touched() {
        let (a, b, protected) = sticks();
        let mut img = vec![0u8; 1024 * 1024];
        img[510] = 0x55;
        img[511] = 0xAA;
        img[512..520].copy_from_slice(b"EFI PART");
        let reader = FakeReader::new(img, SECTOR);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let e = run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(e, CloneError::Layout(LayoutError::Gpt)));
        assert!(writer.write_offsets().is_empty());
    }

    #[test]
    fn cloning_a_disk_onto_itself_is_refused_before_the_target_is_touched() {
        let (a, _, protected) = sticks();
        let reader = FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let e = run(
            CloneConfig { verify: false },
            &a,
            &a,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(e, CloneError::Rejected(Rejection::SameDisk)));
        assert!(writer.write_offsets().is_empty());
    }

    #[test]
    fn only_the_partitioned_region_is_copied() {
        // 8MiB 장치에 4MiB 까지만 파티션. 나머지 4MiB 는 옮기지 않는다.
        // 이게 이 기능의 존재 이유다 — 32GB 를 통째로 옮기면 13분이 걸린다.
        let (a, b, protected) = sticks();
        let img = source_image(8 * 1024 * 1024, 8192);
        let reader = FakeReader::new(img, SECTOR);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let s = run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        assert_eq!(s.bytes_copied, 4 * 1024 * 1024);
        assert_eq!(s.partitions, 1);
    }

    #[test]
    fn the_partition_table_is_written_last() {
        // 오프셋 0 을 먼저 쓰면 윈도우가 복사 도중에 볼륨을 마운트해 버리고,
        // 그때부터 쓰기가 거부된다. 굽기 경로에서 실제로 겪은 실패다.
        let (a, b, protected) = sticks();
        let img = source_image(8 * 1024 * 1024, 8192);
        let reader = FakeReader::new(img, SECTOR);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        let offsets = writer.write_offsets();
        assert!(offsets.len() > 1, "쓰기가 한 번뿐이면 순서를 볼 수 없다");
        assert_eq!(*offsets.last().unwrap(), 0, "오프셋 0 이 마지막이 아니다");
        assert!(
            offsets[..offsets.len() - 1].iter().all(|&o| o > 0),
            "오프셋 0 을 두 번 이상 썼다"
        );
    }

    #[test]
    fn verify_catches_a_target_that_lies_about_writing() {
        let (a, b, protected) = sticks();
        let img = source_image(8 * 1024 * 1024, 8192);
        let reader = FakeReader::new(img, SECTOR);
        // 2MiB 이후의 쓰기를 삼키는 불량 USB.
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR).corrupting_after(2 * 1024 * 1024);

        let e = run(
            CloneConfig { verify: true },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(e, CloneError::VerifyMismatch));
    }

    #[test]
    fn a_source_that_disappears_mid_copy_is_reported_as_a_failure() {
        // USB 를 뽑으면 읽기가 실패한다. 그걸 EOF 로 처리하면 잘린 복제본이
        // "성공" 으로 만들어지고, 사용자는 부팅되지 않는 USB 를 손에 쥔다.
        let (a, b, protected) = sticks();
        let reader = FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR)
            .failing_at(2 * 1024 * 1024);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let e = run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(e, CloneError::Source(_)));
    }

    #[test]
    fn cancelling_stops_the_copy() {
        struct Always;
        impl Cancel for Always {
            fn is_canceled(&self) -> bool {
                true
            }
        }
        let (a, b, protected) = sticks();
        let reader = FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let e = run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &Always,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(e, CloneError::Canceled));
        assert!(writer.write_offsets().is_empty());
    }

    #[test]
    fn analyze_reports_the_copy_size_without_touching_anything() {
        let (a, _, _) = sticks();
        let reader = FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR);
        let l = analyze(&reader, &a).unwrap();
        assert_eq!(l.bytes, 4 * 1024 * 1024);
        assert_eq!(l.partitions, 1);
    }

    #[test]
    fn a_source_swapped_after_selection_is_caught() {
        // 목록을 만든 뒤 USB 를 뽑았다 다른 것을 꽂으면 번호가 재사용된다.
        let (a, b, protected) = sticks();
        let mut other = a.clone();
        other.friendly_name = "다른 장치".into();
        let reader =
            FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR).with_observed(other);
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR);

        let e = run(
            CloneConfig { verify: false },
            &a,
            &b,
            &protected,
            &reader,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(e, CloneError::SourceIdentityChanged));
        assert!(writer.write_offsets().is_empty());
    }
}
