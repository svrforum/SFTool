//! 가짜 장치 구현.
//!
//! 두 가지 용도가 있다:
//!
//! 1. **테스트** — 안전 규칙과 전체 파이프라인을 실제 USB 없이 검증한다.
//!    특히 "내장 디스크가 목록에 절대 나오지 않는다"는 규칙은 실물로 테스트하면
//!    실패할 때마다 개발자의 디스크가 지워지므로, 가짜 구현이 유일하게 안전한 방법이다.
//! 2. **개발 환경 실행** — Windows 가 아닌 곳에서 앱을 띄워 UI 를 확인한다.

use super::{DeviceError, RawReader, RawWriter, ReadSession, UsbEnumerator, WriteSession};
use crate::core::model::{BusType, DiskInfo, VolumeInfo};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// 미리 정해둔 디스크 목록을 돌려주는 열거자.
pub struct FakeEnumerator {
    disks: Vec<DiskInfo>,
    protected: HashSet<u32>,
    /// 열거에서 빠진 장치와 그 사유. 부분 실패를 흉내낼 때 채운다.
    skipped: Vec<String>,
}

impl FakeEnumerator {
    pub fn new(disks: Vec<DiskInfo>, protected: HashSet<u32>) -> Self {
        Self {
            disks,
            protected,
            skipped: Vec::new(),
        }
    }

    /// 일부 디스크는 열거되고 일부는 사유와 함께 빠진 상태를 만든다.
    ///
    /// 실물에서 가장 흔한 모양이다 — 내장 SSD 는 잘 읽히고 컨트롤러가 불안한
    /// USB 하나만 용량 조회에 실패한다.
    pub fn with_skipped(mut self, notes: Vec<String>) -> Self {
        self.skipped = notes;
        self
    }

    /// 개발 환경에서 앱을 띄울 때 쓰는 표본.
    ///
    /// 일부러 위험한 항목을 섞어 둔다 — 시스템 NVMe, 부팅 SATA, 빈 카드리더.
    /// 목록에 이것들이 보이면 안전 규칙이 깨진 것이므로 UI 를 보는 것만으로
    /// 회귀를 알아챌 수 있다.
    pub fn sample() -> Self {
        let disks = vec![
            DiskInfo {
                number: 0,
                friendly_name: "Samsung SSD 990 PRO 2TB".into(),
                size_bytes: 2_000_398_934_016,
                bus_type: BusType::Nvme,
                is_system: true,
                is_boot: true,
                boot_from_disk: true,
                is_clustered: false,
                is_read_only: false,
                serial: Some("S6B2NS0T900001".into()),
                volumes: vec![volume("C", 2_000_000_000_000, 1)],
            },
            DiskInfo {
                number: 1,
                friendly_name: "WDC WD40EZAZ-00SF3B0".into(),
                size_bytes: 4_000_787_030_016,
                bus_type: BusType::Sata,
                is_system: false,
                is_boot: false,
                boot_from_disk: false,
                is_clustered: false,
                is_read_only: false,
                serial: Some("WD-WX32D00XYZ".into()),
                volumes: vec![volume("D", 4_000_000_000_000, 1)],
            },
            DiskInfo {
                number: 2,
                friendly_name: "SanDisk Ultra USB 3.0".into(),
                size_bytes: 30_752_000_000,
                bus_type: BusType::Usb,
                is_system: false,
                is_boot: false,
                boot_from_disk: false,
                is_clustered: false,
                is_read_only: false,
                serial: Some("4C530001120607117025".into()),
                volumes: vec![volume("E", 30_700_000_000, 1)],
            },
            DiskInfo {
                number: 3,
                friendly_name: "Samsung Flash Drive FIT".into(),
                size_bytes: 64_055_500_800,
                bus_type: BusType::Usb,
                is_system: false,
                is_boot: false,
                boot_from_disk: false,
                is_clustered: false,
                is_read_only: false,
                serial: None,
                volumes: vec![],
            },
            // 4GB USB — 로더 이미지가 안 들어간다. 감추지 말고 사유와 함께 비활성 표시.
            DiskInfo {
                number: 4,
                friendly_name: "Generic Flash Disk".into(),
                size_bytes: 4_004_511_744,
                bus_type: BusType::Usb,
                is_system: false,
                is_boot: false,
                boot_from_disk: false,
                is_clustered: false,
                is_read_only: false,
                serial: None,
                volumes: vec![volume("F", 4_000_000_000, 1)],
            },
            // 카드가 없는 카드리더. 목록에 나오지 않아야 한다.
            DiskInfo {
                number: 5,
                friendly_name: "Realtek USB Card Reader".into(),
                size_bytes: 0,
                bus_type: BusType::Usb,
                is_system: false,
                is_boot: false,
                boot_from_disk: false,
                is_clustered: false,
                is_read_only: false,
                serial: None,
                volumes: vec![],
            },
        ];
        Self::new(disks, [0].into_iter().collect())
    }
}

fn volume(letter: &str, size: u64, extents: u32) -> VolumeInfo {
    VolumeInfo {
        guid_path: format!(r"\\?\Volume{{fake-{letter}}}\"),
        drive_letter: letter.chars().next(),
        file_system: Some("NTFS".into()),
        size_bytes: size,
        disk_extent_count: extents,
    }
}

impl UsbEnumerator for FakeEnumerator {
    fn list_disks(&self) -> Result<Vec<DiskInfo>, DeviceError> {
        Ok(self.disks.clone())
    }
    fn skipped(&self) -> Vec<String> {
        self.skipped.clone()
    }
    fn protected_disk_numbers(&self) -> Result<HashSet<u32>, DeviceError> {
        Ok(self.protected.clone())
    }
}

/// 메모리 버퍼에 쓰는 가짜 세션.
///
/// 실제 장치 대신 이걸 끼우면 파이프라인 전체를 CI 에서 완주시킬 수 있다.
/// 코드 경로는 동일하고 도착지만 다르다.
pub struct FakeSession {
    observed: DiskInfo,
    sector_size: u32,
    data: Arc<Mutex<Vec<u8>>>,
    finished: Arc<Mutex<bool>>,
    corrupt_after: Option<u64>,
    fail_at: Option<u64>,
    offsets: Arc<Mutex<Vec<u64>>>,
}

impl WriteSession for FakeSession {
    fn observed(&self) -> &DiskInfo {
        &self.observed
    }
    fn sector_size(&self) -> u32 {
        self.sector_size
    }
    fn total_bytes(&self) -> u64 {
        self.data.lock().unwrap().len() as u64
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), DeviceError> {
        // 실제 장치가 강제하는 규칙을 가짜에서도 그대로 강제한다.
        // 그래야 정렬 버그가 실물에서 처음 드러나지 않는다.
        let ss = self.sector_size as u64;
        if !offset.is_multiple_of(ss) || !(buf.len() as u64).is_multiple_of(ss) {
            return Err(DeviceError::Io {
                code: 87,
                message: "정렬되지 않은 쓰기 (길이와 오프셋은 섹터 배수여야 함)".into(),
            });
        }
        // 쓰는 도중 뽑힌 USB 흉내. 여기서부터는 대놓고 실패한다.
        if let Some(limit) = self.fail_at {
            if offset + buf.len() as u64 > limit {
                return Err(DeviceError::MediaChanged {
                    op: "장치 쓰기"
                });
            }
        }
        self.offsets.lock().unwrap().push(offset);
        let mut d = self.data.lock().unwrap();
        let end = offset as usize + buf.len();
        if end > d.len() {
            return Err(DeviceError::Io {
                code: 112,
                message: "장치 끝을 넘어선 쓰기".into(),
            });
        }
        // 불량 장치 흉내: 지정 지점 이후는 성공을 보고하되 저장하지 않는다.
        // 한 번의 쓰기가 그 지점을 걸치는 경우도 있으므로 요청 안에서 잘라낸다.
        if let Some(limit) = self.corrupt_after {
            if offset >= limit {
                return Ok(());
            }
            let allowed = (limit - offset) as usize;
            if allowed < buf.len() {
                d[offset as usize..offset as usize + allowed].copy_from_slice(&buf[..allowed]);
                return Ok(());
            }
        }
        d[offset as usize..end].copy_from_slice(buf);
        Ok(())
    }

    fn zero_tail(&mut self, bytes: u64) -> Result<(), DeviceError> {
        let mut d = self.data.lock().unwrap();
        let len = d.len() as u64;
        let start = len.saturating_sub(bytes) as usize;
        d[start..].fill(0);
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        let d = self.data.lock().unwrap();
        let end = offset as usize + buf.len();
        if end > d.len() {
            return Err(DeviceError::Io {
                code: 112,
                message: "장치 끝을 넘어선 읽기".into(),
            });
        }
        buf.copy_from_slice(&d[offset as usize..end]);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), DeviceError> {
        *self.finished.lock().unwrap() = true;
        Ok(())
    }
}

/// 가짜 쓰기 대상. 쓰인 내용을 검사할 수 있다.
pub struct FakeWriter {
    sector_size: u32,
    /// 디스크 번호 → 저장 버퍼.
    storage: Arc<Mutex<Vec<u8>>>,
    finished: Arc<Mutex<bool>>,
    /// 열 때 실제로 관측되는 장치. 지정하면 TOCTOU 상황을 흉내낼 수 있다.
    observed_override: Option<DiskInfo>,
    /// 쓰기가 일어난 오프셋 순서. 순서를 검증하는 테스트가 쓴다.
    offsets: Arc<Mutex<Vec<u64>>>,
    /// `open` 이 불렸는가 — 즉 **대상이 파괴되기 시작했는가**.
    ///
    /// 이 플래그가 없으면 "대상을 건드리지 않았다" 를 검사할 방법이 없다.
    /// 실제 [`crate::device::windows::WindowsRawWriter::open`] 은 그 안에서
    /// 마운트 지점 제거·파티션 테이블 삭제·앞 1MiB 0으로 덮기를 전부 해치우는데,
    /// 그중 어느 것도 `write_at` 이 아니다. 그래서 `write_offsets()` 가 비어
    /// 있다는 것은 `sink::stream` 이 돌지 않았다는 뜻일 뿐, 디스크가 무사하다는
    /// 뜻이 아니다. 열린 순간이 곧 되돌릴 수 없는 지점이므로 그것을 기록한다.
    opened: Arc<Mutex<bool>>,
    /// 이 오프셋 이후의 쓰기를 조용히 버린다.
    ///
    /// 불량 USB 를 흉내내기 위한 것이다. 싸구려 USB 는 쓰기가 성공했다고
    /// 보고하고도 실제로는 저장하지 않는 경우가 있어서, 검증이 그것을
    /// 잡아내는지 시험하려면 이런 장치가 필요하다.
    corrupt_after: Option<u64>,
    /// 이 오프셋을 넘는 쓰기가 실패한다. 쓰는 도중 뽑힌 USB 를 흉내낸다.
    fail_at: Option<u64>,
}

impl FakeWriter {
    pub fn new(capacity: usize, sector_size: u32) -> Self {
        Self {
            sector_size,
            storage: Arc::new(Mutex::new(vec![0xAA; capacity])),
            finished: Arc::new(Mutex::new(false)),
            observed_override: None,
            corrupt_after: None,
            fail_at: None,
            offsets: Arc::new(Mutex::new(Vec::new())),
            opened: Arc::new(Mutex::new(false)),
        }
    }

    /// 열었을 때 다른 장치가 관측되는 상황을 만든다 (디스크 번호 재사용 재현).
    pub fn with_observed(mut self, observed: DiskInfo) -> Self {
        self.observed_override = Some(observed);
        self
    }

    /// 지정 오프셋 이후의 쓰기를 삼키는 불량 장치로 만든다.
    pub fn corrupting_after(mut self, offset: u64) -> Self {
        self.corrupt_after = Some(offset);
        self
    }

    /// 지정 오프셋부터 쓰기가 실패하는 장치로 만든다.
    ///
    /// `corrupting_after` 와 다르다. 저건 성공을 거짓 보고하는 불량 USB 고,
    /// 이건 쓰는 도중 빠져버린 USB 다. 후자는 **대상이 이미 지워지고 일부만
    /// 쓰인 상태로** 작업이 끝나므로, 사용자에게 그 사실을 알리는지 확인하려면
    /// 이 흉내가 필요하다.
    pub fn failing_at(mut self, offset: u64) -> Self {
        self.fail_at = Some(offset);
        self
    }

    /// 지금까지 쓰인 내용.
    pub fn contents(&self) -> Vec<u8> {
        self.storage.lock().unwrap().clone()
    }

    /// 쓰기가 일어난 오프셋을 순서대로. 파티션 테이블을 마지막에 쓰는지 검증한다.
    pub fn write_offsets(&self) -> Vec<u64> {
        self.offsets.lock().unwrap().clone()
    }

    /// `finish` 가 호출됐는가. 마무리를 빼먹는 회귀를 잡는다.
    pub fn was_finished(&self) -> bool {
        *self.finished.lock().unwrap()
    }

    /// 대상이 열렸는가 — **되돌릴 수 없는 지점을 넘었는가.**
    ///
    /// "인가보다 파괴가 먼저 오는" 회귀를 잡는 유일한 수단이다.
    /// `write_offsets()` 만으로는 잡히지 않는 이유는 필드 주석에 적어 두었다.
    pub fn was_opened(&self) -> bool {
        *self.opened.lock().unwrap()
    }
}

impl RawWriter for FakeWriter {
    fn open(&self, disk: &DiskInfo) -> Result<Box<dyn WriteSession>, DeviceError> {
        // 실제 구현은 이 안에서 이미 디스크를 망가뜨린다. 그 사실을 기록한다.
        *self.opened.lock().unwrap() = true;
        Ok(Box::new(FakeSession {
            observed: self
                .observed_override
                .clone()
                .unwrap_or_else(|| disk.clone()),
            sector_size: self.sector_size,
            data: Arc::clone(&self.storage),
            finished: Arc::clone(&self.finished),
            corrupt_after: self.corrupt_after,
            fail_at: self.fail_at,
            offsets: Arc::clone(&self.offsets),
        }))
    }
}

/// 가짜 원본. 정해진 내용을 돌려준다.
pub struct FakeReader {
    data: Arc<Vec<u8>>,
    sector_size: u32,
    observed_override: Option<DiskInfo>,
    /// 이 오프셋부터 읽기가 실패한다. 도중에 뽑힌 USB 를 흉내낸다.
    fail_at: Option<u64>,
}

impl FakeReader {
    pub fn new(contents: Vec<u8>, sector_size: u32) -> Self {
        Self {
            data: Arc::new(contents),
            sector_size,
            observed_override: None,
            fail_at: None,
        }
    }

    /// 열었을 때 다른 장치가 관측되는 상황을 만든다.
    pub fn with_observed(mut self, observed: DiskInfo) -> Self {
        self.observed_override = Some(observed);
        self
    }

    /// 지정 오프셋부터 읽기가 실패하는 장치로 만든다.
    pub fn failing_at(mut self, offset: u64) -> Self {
        self.fail_at = Some(offset);
        self
    }
}

impl RawReader for FakeReader {
    fn open(&self, disk: &DiskInfo) -> Result<Box<dyn ReadSession>, DeviceError> {
        Ok(Box::new(FakeReadSession {
            observed: self
                .observed_override
                .clone()
                .unwrap_or_else(|| disk.clone()),
            sector_size: self.sector_size,
            data: Arc::clone(&self.data),
            fail_at: self.fail_at,
            finished: false,
        }))
    }
}

pub struct FakeReadSession {
    observed: DiskInfo,
    sector_size: u32,
    data: Arc<Vec<u8>>,
    fail_at: Option<u64>,
    finished: bool,
}

impl FakeReadSession {
    /// `finish` 가 호출됐는가. 원본을 닫지 않는 회귀를 잡는다.
    pub fn was_finished(&self) -> bool {
        self.finished
    }
}

impl ReadSession for FakeReadSession {
    fn observed(&self) -> &DiskInfo {
        &self.observed
    }
    fn sector_size(&self) -> u32 {
        self.sector_size
    }
    fn total_bytes(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        let ss = self.sector_size as u64;
        if !offset.is_multiple_of(ss) || !(buf.len() as u64).is_multiple_of(ss) {
            return Err(DeviceError::Io {
                code: 87,
                message: "정렬되지 않은 읽기 (길이와 오프셋은 섹터 배수여야 함)".into(),
            });
        }
        if let Some(limit) = self.fail_at {
            if offset + buf.len() as u64 > limit {
                return Err(DeviceError::MediaChanged {
                    op: "장치 읽기"
                });
            }
        }
        let end = offset as usize + buf.len();
        if end > self.data.len() {
            return Err(DeviceError::Io {
                code: 112,
                message: "장치 끝을 넘어선 읽기".into(),
            });
        }
        buf.copy_from_slice(&self.data[offset as usize..end]);
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), DeviceError> {
        self.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::safety::{self, Availability};

    /// 가짜 표본을 안전 규칙에 통과시켰을 때, 위험한 장치가 하나도
    /// 보이지 않아야 한다. 이 프로그램에서 가장 중요한 테스트다.
    #[test]
    fn sample_never_exposes_internal_disks() {
        let e = FakeEnumerator::sample();
        let protected = e.protected_disk_numbers().unwrap();
        let visible: Vec<_> = e
            .list_disks()
            .unwrap()
            .into_iter()
            .filter(|d| safety::availability(d, &protected).is_visible())
            .collect();

        for d in &visible {
            assert_eq!(
                d.bus_type,
                BusType::Usb,
                "USB 가 아닌 장치가 목록에 보인다: {}",
                d.friendly_name
            );
            assert_ne!(d.number, 0, "디스크 0 이 목록에 보인다");
        }
        // NVMe 시스템 디스크와 SATA HDD 는 반드시 빠져야 한다.
        assert!(!visible.iter().any(|d| d.friendly_name.contains("990 PRO")));
        assert!(!visible.iter().any(|d| d.friendly_name.contains("WDC")));
        // 카드 없는 리더도 빠진다.
        assert!(!visible.iter().any(|d| d.size_bytes == 0));
    }

    #[test]
    fn sample_shows_undersized_stick_as_disabled_not_hidden() {
        let e = FakeEnumerator::sample();
        let protected = e.protected_disk_numbers().unwrap();
        let disks = e.list_disks().unwrap();
        let small = disks.iter().find(|d| d.number == 4).unwrap();
        let a = safety::availability(small, &protected);
        assert!(a.is_visible(), "4GB 스틱은 사유와 함께 보여야 한다");
        assert!(!a.is_ready());
        assert!(matches!(a, Availability::Disabled(_)));
    }

    #[test]
    fn sample_has_two_ready_sticks() {
        let e = FakeEnumerator::sample();
        let protected = e.protected_disk_numbers().unwrap();
        let ready = e
            .list_disks()
            .unwrap()
            .into_iter()
            .filter(|d| safety::availability(d, &protected).is_ready())
            .count();
        assert_eq!(ready, 2, "32GB 와 64GB 스틱만 선택 가능해야 한다");
    }

    #[test]
    fn fake_session_enforces_sector_alignment() {
        let w = FakeWriter::new(4096, 512);
        let disk = FakeEnumerator::sample().list_disks().unwrap()[2].clone();
        let mut s = w.open(&disk).unwrap();
        // 정렬되지 않은 길이는 실제 장치와 마찬가지로 거부돼야 한다.
        assert!(s.write_at(0, &[0u8; 100]).is_err());
        assert!(s.write_at(1, &[0u8; 512]).is_err());
        assert!(s.write_at(0, &[0u8; 512]).is_ok());
    }

    #[test]
    fn fake_session_round_trips_and_records_finish() {
        let w = FakeWriter::new(2048, 512);
        let disk = FakeEnumerator::sample().list_disks().unwrap()[2].clone();
        let mut s = w.open(&disk).unwrap();
        let payload = [0x42u8; 1024];
        s.write_at(0, &payload).unwrap();
        let mut back = [0u8; 1024];
        s.read_at(0, &mut back).unwrap();
        assert_eq!(back, payload);
        assert!(!w.was_finished());
        s.finish().unwrap();
        assert!(w.was_finished());
    }

    #[test]
    fn zero_tail_clears_the_end() {
        let w = FakeWriter::new(2048, 512);
        let disk = FakeEnumerator::sample().list_disks().unwrap()[2].clone();
        let mut s = w.open(&disk).unwrap();
        s.zero_tail(512).unwrap();
        let c = w.contents();
        assert!(c[..1536].iter().all(|b| *b == 0xAA));
        assert!(c[1536..].iter().all(|b| *b == 0));
    }

    #[test]
    fn observed_override_reproduces_device_swap() {
        // 목록을 만든 뒤 사용자가 USB 를 바꿔 꽂아 같은 번호가 다른 장치를
        // 가리키게 된 상황. 열어보면 다른 장치가 관측된다.
        let disks = FakeEnumerator::sample().list_disks().unwrap();
        let selected = disks[2].clone();
        let mut swapped = disks[3].clone();
        swapped.number = selected.number;

        let w = FakeWriter::new(1024, 512).with_observed(swapped);
        let s = w.open(&selected).unwrap();
        assert!(safety::confirm_identity(&selected, s.observed()).is_err());
    }

    #[test]
    fn a_fake_source_reads_back_what_it_was_given() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let r = FakeReader::new(data.clone(), 512);
        let disk = FakeEnumerator::sample().list_disks().unwrap()[1].clone();
        let mut s = r.open(&disk).unwrap();
        assert_eq!(s.total_bytes(), 4096);
        assert_eq!(s.sector_size(), 512);
        let mut buf = vec![0u8; 1024];
        s.read_at(512, &mut buf).unwrap();
        assert_eq!(buf, data[512..1536]);
    }

    #[test]
    fn a_fake_source_refuses_unaligned_reads_like_a_real_device_does() {
        // 실제 장치가 강제하는 규칙을 가짜에서도 강제해야, 정렬 버그가
        // 실물에서 처음 드러나지 않는다.
        let r = FakeReader::new(vec![0u8; 4096], 512);
        let disk = FakeEnumerator::sample().list_disks().unwrap()[1].clone();
        let mut s = r.open(&disk).unwrap();
        assert!(s.read_at(1, &mut vec![0u8; 512]).is_err());
        assert!(s.read_at(0, &mut vec![0u8; 511]).is_err());
    }
}
