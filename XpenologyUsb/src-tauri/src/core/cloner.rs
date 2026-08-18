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
    /// 원본이 고를 때와 다른 장치다. 무엇이 어긋났는지 함께 담는다.
    SourceIdentityChanged(String),
    /// 대상이 고를 때와 다른 장치다. 무엇이 어긋났는지 함께 담는다.
    TargetIdentityChanged(String),
    Device(DeviceError),
    /// 원본을 읽지 못했다.
    Source(String),
    /// `at` 은 처음 어긋난 [`sink::BLOCK`] 의 시작 오프셋.
    VerifyMismatch {
        at: u64,
    },
    /// **대상이 이미 지워진 뒤에** 실패했다.
    ///
    /// `RawWriter::open` 이 그 안에서 대상의 파티션 테이블을 지운다. 그 뒤의
    /// 실패는 — 사용자가 누른 취소까지 포함해 — 원래 내용이 사라진 USB 를
    /// 남긴다. 원본이 멀쩡한 것과는 별개다. 굽기 흐름의 같은 이름과 같은 이유다.
    TargetErased {
        cause: Box<CloneError>,
    },
    Canceled,
}

impl CloneError {
    /// 대상이 지워진 뒤에 난 실패로 표시한다. 원인은 그대로 안에 남는다.
    fn already_erased(cause: CloneError) -> CloneError {
        CloneError::TargetErased {
            cause: Box::new(cause),
        }
    }
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
            // 레이아웃 판정이 최소 한 섹터를 보장하므로 복제에서는 도달할 수
            // 없어야 한다. 도달했다면 원본이 도중에 사라진 것이다.
            SinkError::EmptySource => CloneError::Source("원본에서 읽은 내용이 없습니다".into()),
            SinkError::Device(d) => CloneError::Device(d),
            SinkError::TooSmall { need, have } => {
                CloneError::Rejected(Rejection::TooSmall { need, have })
            }
            SinkError::VerifyMismatch { at } => CloneError::VerifyMismatch { at },
            SinkError::Canceled => CloneError::Canceled,
        }
    }
}

/// 원본을 열어 복사할 범위만 알아내고 닫는다.
///
/// 확인 화면에서 "복사할 양" 을 미리 보여주기 위한 것이다. 대상은 건드리지 않는다.
///
/// 읽기만 하는데도 안전 규칙을 먼저 통과시키는 이유는, 그러지 않으면 이 함수가
/// **아무 디스크나 열 수 있는 통로**가 되기 때문이다. 열거자는 일부러 걸러내지
/// 않고 디스크 0 과 내장 디스크까지 전부 돌려주므로, 여기서 막지 않으면 판정이
/// 장치 계층의 버스 검사 하나에만 걸리게 된다. 인터록은 `safety` 가 약속한
/// 자리에 있어야 한다.
pub fn analyze(
    reader: &dyn RawReader,
    source: &DiskInfo,
    protected: &HashSet<u32>,
) -> Result<Layout, CloneError> {
    safety::is_listable(source, protected).map_err(CloneError::Rejected)?;

    let mut s = reader.open(source)?;
    safety::confirm_identity(source, s.observed())
        .map_err(|m| CloneError::SourceIdentityChanged(m.describe()))?;
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
        .map_err(|m| CloneError::SourceIdentityChanged(m.describe()))?;
    let plan = read_layout(src.as_mut())?;

    // --- 2. 여기까지 통과해야 대상을 건드린다 --------------------------------
    safety::can_clone(source, target, protected, plan.bytes).map_err(CloneError::Rejected)?;

    if cancel.is_canceled() {
        return Err(CloneError::Canceled);
    }

    rep.begin(Stage::Preparing, None);
    let dst = writer.open(target)?;

    // --- 3. 여기서부터 대상은 되돌릴 수 없다 --------------------------------
    //
    // `open()` 안에서 대상의 파티션 테이블이 이미 지워졌다. 아래의 어떤 실패도
    // — 취소를 포함해 — 빈 대상을 남긴다. 감싸는 자리를 한 곳으로 모은 이유는
    // 굽기 흐름과 같다: `?` 하나를 빠뜨리는 것만으로 그 사실이 다시 새어나간다.
    let copied = copy_to_target(&cfg, target, dst, src, &plan, cancel, &mut rep)
        .map_err(CloneError::already_erased)?;

    rep.finish();
    Ok(CloneSummary {
        bytes_copied: copied,
        partitions: plan.partitions,
        verified: cfg.verify,
        source_name: source.friendly_name.clone(),
        target_name: target.friendly_name.clone(),
    })
}

/// 대상을 연 뒤의 모든 단계. 복사한 바이트 수를 돌려준다.
///
/// [`run`] 에서 떼어낸 이유는 오직 하나, **되돌릴 수 없는 지점 이후의 실패를
/// 한 자리에서 감싸기 위해서**다. 흐름 자체는 바뀌지 않았다.
#[allow(clippy::too_many_arguments)]
fn copy_to_target<F: FnMut(ProgressEvent)>(
    cfg: &CloneConfig,
    target: &DiskInfo,
    mut dst: Box<dyn crate::device::WriteSession>,
    src: Box<dyn crate::device::ReadSession>,
    plan: &Layout,
    cancel: &dyn Cancel,
    rep: &mut ProgressReporter<F>,
) -> Result<u64, CloneError> {
    safety::confirm_identity(target, dst.observed())
        .map_err(|m| CloneError::TargetIdentityChanged(m.describe()))?;

    rep.begin(Stage::Writing, None);
    let mut stream = SessionReader::new(src, plan.bytes, CHUNK);
    let out = sink::stream(&mut stream, dst.as_mut(), Some(plan.bytes), cancel, rep)?;
    sink::zero_tail(dst.as_mut(), out.bytes)?;

    // --- 검증 (선택) --------------------------------------------------------
    if cfg.verify {
        // 굽기 경로와 같은 이유로 되읽기 전에 매체로 내려보낸다.
        // 자세한 사정은 `WriteSession::commit` 의 주석에 있다.
        dst.commit()?;
        rep.begin(Stage::Verifying, None);
        sink::verify(dst.as_mut(), out.bytes, &out.hash, &out.blocks, cancel, rep)?;
    }

    // --- 마무리 -------------------------------------------------------------
    rep.begin(Stage::Finishing, None);
    dst.finish()?;
    // 원본도 닫는다. 열어 둔 핸들이 남으면 사용자가 USB 를 뽑을 수 없다.
    stream.into_session().finish()?;
    Ok(out.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::pipeline::NeverCancel;
    use crate::device::fake::{FakeEnumerator, FakeReader, FakeWriter};
    use crate::device::UsbEnumerator;

    const SECTOR: u32 = 512;

    /// 대상이 지워진 뒤의 실패는 원인을 한 겹 안에 담는다. 알맹이만 꺼낸다.
    fn cause(e: &CloneError) -> &CloneError {
        match e {
            CloneError::TargetErased { cause } => cause,
            other => other,
        }
    }

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
        // 열리지도 않아야 한다. 실제 구현은 `open` 안에서 파티션 테이블을 지우고
        // 앞 1MiB 를 0으로 덮으므로, 여기를 보지 않으면 "쓰기가 없었다" 만으로
        // 무사하다고 착각하게 된다.
        assert!(!writer.was_opened(), "대상이 열렸다 — 이미 지워진 뒤다");
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
        assert!(!writer.was_opened(), "대상이 열렸다 — 이미 지워진 뒤다");
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
        assert!(!writer.was_opened(), "대상이 열렸다 — 이미 지워진 뒤다");
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

        assert!(
            matches!(cause(&e), CloneError::VerifyMismatch { .. }),
            "실제: {e:?}"
        );
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

        assert!(matches!(cause(&e), CloneError::Source(_)), "실제: {e:?}");
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
        assert!(!writer.was_opened(), "취소했는데 대상이 열렸다");
        assert!(writer.write_offsets().is_empty());
    }

    #[test]
    fn analyze_reports_the_copy_size_without_touching_anything() {
        let (a, _, protected) = sticks();
        let reader = FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR);
        let l = analyze(&reader, &a, &protected).unwrap();
        assert_eq!(l.bytes, 4 * 1024 * 1024);
        assert_eq!(l.partitions, 1);
    }

    #[test]
    fn analyze_refuses_a_disk_that_should_never_be_listed() {
        // 열거자는 일부러 걸러내지 않고 내장 디스크까지 전부 돌려준다.
        // 이 통로로 그런 디스크를 열 수 있으면 안 된다.
        let e = FakeEnumerator::sample();
        let protected = e.protected_disk_numbers().unwrap();
        let internal = e
            .list_disks()
            .unwrap()
            .into_iter()
            .find(|d| d.bus_type != crate::core::model::BusType::Usb)
            .expect("표본에 내장 디스크가 있어야 한다");
        let reader = FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR);
        assert!(matches!(
            analyze(&reader, &internal, &protected).unwrap_err(),
            CloneError::Rejected(_)
        ));
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

        assert!(matches!(e, CloneError::SourceIdentityChanged { .. }));
        assert!(!writer.was_opened(), "대상이 열렸다 — 이미 지워진 뒤다");
        assert!(writer.write_offsets().is_empty());
    }

    /// 대상을 연 뒤에 실패하면 **대상이 이미 지워졌다는 사실**을 실어야 한다.
    ///
    /// 굽기 흐름과 같은 결함이다. 두 흐름이 같은 `RawWriter::open` 을 쓰므로
    /// 대상이 파괴되는 지점도 같은데, 그 뒤의 실패를 그냥 올리면 사용자는
    /// 원본이 멀쩡하니 아무것도 잃지 않았다고 생각한다. 잃은 것은 대상이다.
    #[test]
    fn a_failure_after_the_target_is_opened_says_the_target_is_already_erased() {
        let (a, b, protected) = sticks();
        let reader = FakeReader::new(source_image(8 * 1024 * 1024, 8192), SECTOR);
        // 복사 도중 대상이 빠진다.
        let writer = FakeWriter::new(16 * 1024 * 1024, SECTOR).failing_at(2 * 1024 * 1024);

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

        assert!(
            writer.was_opened(),
            "이 테스트는 열린 뒤의 실패를 재현해야 한다"
        );
        assert!(
            matches!(e, CloneError::TargetErased { .. }),
            "대상이 지워진 뒤의 실패인데 그대로 올렸다: {e:?}"
        );
        assert!(
            format!("{e:?}").contains("MediaChanged"),
            "원인이 사라졌다: {e:?}"
        );
    }

    /// 신원 확인 실패는 **무엇이 어긋났는지** 담아야 한다.
    #[test]
    fn a_swapped_source_reports_which_field_diverged() {
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

        let text = format!("{e:?}");
        assert!(text.contains("다른 장치"), "어긋난 값이 버려졌다: {text}");
        assert!(text.contains("이름이 다릅니다"), "실제: {text}");
    }
}
