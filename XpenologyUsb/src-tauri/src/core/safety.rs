//! 안전 인터록.
//!
//! 이 프로그램은 디스크를 되돌릴 수 없게 덮어쓴다. 잘못된 장치를 고르면
//! 사용자의 데이터가 영구히 사라진다. 그래서 여기 있는 규칙들은 기능이 아니라 명세다.
//!
//! 전부 순수 함수라서 실제 하드웨어 없이 전수 테스트된다.
//!
//! ## 설계 원칙: fail-closed
//!
//! 판정에 필요한 정보가 없거나 애매하면 **거부**한다. "아마 USB일 것"으로 통과시키지 않는다.
//! 목록에 안 뜨는 USB는 불편이지만, 목록에 뜨는 내장 디스크는 재앙이다.

use super::model::{BusType, DiskInfo};
use std::collections::HashSet;

/// 로더 이미지를 담기 위한 최소 USB 용량.
///
/// m-shell 표준판이 압축 해제 후 3.03GB, RR 이 3.76GB 다. 명목 4GB 스틱은 실제 포맷
/// 용량이 3.7GB 안팎이라 여유가 거의 없거나 아예 모자란다. 이미지 크기는 릴리스마다
/// 커지는 추세이므로 4GB 는 아예 지원 대상에서 제외한다.
pub const MIN_USB_BYTES: u64 = 8 * 1000 * 1000 * 1000;

/// 목록에서의 디스크 상태.
///
/// "숨김"과 "비활성"을 구분하는 것이 중요하다. 쓸 수 없는 USB 를 목록에서 아예 없애면
/// 사용자는 장치가 인식되지 않았다고 오해하고 USB 를 다시 꽂거나 포트를 바꾸며 헤맨다.
/// 위험해서 감춰야 하는 것(내장 디스크)과 이유를 알려줘야 하는 것(읽기 전용, 용량 부족)은
/// 다른 처리를 받아야 한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// 선택 가능.
    Ready,
    /// 목록에 보이되 선택할 수 없다. 사유를 함께 표시한다.
    Disabled(Rejection),
    /// 목록에 아예 나타나지 않는다. 내장 디스크 등 보여주면 위험한 것들.
    Hidden(Rejection),
}

impl Availability {
    pub fn is_ready(&self) -> bool {
        matches!(self, Availability::Ready)
    }
    pub fn is_visible(&self) -> bool {
        !matches!(self, Availability::Hidden(_))
    }
}

/// 디스크를 대상 목록에서 제외하는 이유.
///
/// UI에 그대로 노출할 수 있도록 사유를 구분해 둔다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// USB 버스가 아니다. 내장 디스크를 막는 1차 방어선.
    NotUsb(BusType),
    /// 디스크 번호 0. 관례상 부팅 디스크이며, 실수로 넘어가면 시스템을 파괴한다.
    DiskZero,
    /// 시스템/부팅/클러스터 디스크로 표시돼 있다.
    SystemDisk,
    /// 읽기 전용.
    ReadOnly,
    /// 보호 집합에 속한다 (시스템 드라이브·윈도우 폴더·실행 파일·페이지파일이 있는 디스크).
    Protected,
    /// 용량이 0. 카드가 없는 카드리더 등.
    NoMedia,
    /// 스팬/RAID 볼륨을 포함한다.
    SpannedVolume,
    /// 어떤 로더 이미지도 담을 수 없을 만큼 작다.
    BelowMinimumCapacity { have: u64, minimum: u64 },
    /// 선택한 이미지가 이 디스크보다 크다.
    TooSmall { need: u64, have: u64 },
    /// 소스 이미지가 대상 디스크 위에 있다. 자기 자신을 덮어쓰게 된다.
    SourceOnTarget,
}

impl Rejection {
    /// 사용자에게 보여줄 사유인가?
    ///
    /// `NotUsb`, `Protected`, `SystemDisk`, `DiskZero`는 애초에 목록에 없어야 하므로
    /// 설명할 일이 없다. 나머지는 왜 못 쓰는지 알려주는 편이 낫다.
    pub fn is_user_facing(&self) -> bool {
        matches!(
            self,
            Rejection::NoMedia
                | Rejection::TooSmall { .. }
                | Rejection::BelowMinimumCapacity { .. }
                | Rejection::SourceOnTarget
                | Rejection::SpannedVolume
                | Rejection::ReadOnly
        )
    }
}

/// 목록에서 **감춰야** 하는 이유가 있는가.
///
/// 여기 걸리는 것들은 사용자에게 보여주는 것 자체가 위험하거나 의미가 없다.
/// 내장 디스크는 애초에 선택지로 존재해서는 안 된다.
///
/// ## 판정 방향에 대하여
///
/// `is_system` 같은 플래그는 **참일 때만** 배제한다. "정보가 없으면 배제"로 만들면
/// 안 되는데, `BootFromDisk` 는 ESP 가 여러 개인 시스템(SSD 두 장 꽂은 흔한 구성)에서
/// 모든 디스크에 대해 값이 설정되지 않는 것으로 문서화돼 있기 때문이다.
/// 그런 기계에서 "값 없음 = 배제"를 적용하면 목록이 통째로 비어버린다.
///
/// 반대로 `bus_type`, `number`, `size_bytes` 는 **적극적으로 확인돼야** 하는 값이다.
/// 이것들을 못 읽었다면 열거 계층에서 아예 항목을 만들지 않는다.
pub fn hidden_reason(disk: &DiskInfo, protected: &HashSet<u32>) -> Option<Rejection> {
    // 1차: USB 버스만. 이게 유일하게 신뢰할 수 있는 판정이다.
    if disk.bus_type != BusType::Usb {
        return Some(Rejection::NotUsb(disk.bus_type));
    }

    // 디스크 0은 무조건 거부. 열거가 잘못돼 0이 흘러들어오면
    // 시스템 디스크의 MBR을 지우게 된다.
    if disk.number == 0 {
        return Some(Rejection::DiskZero);
    }

    // 명시적으로 참인 경우에만 배제한다 (위 주석 참고).
    if disk.is_system || disk.is_boot || disk.boot_from_disk || disk.is_clustered {
        return Some(Rejection::SystemDisk);
    }

    // WMI와 무관하게 커널에 직접 물어 만든 보호 집합.
    // WMI 정보가 틀리거나 조작돼도 이 방어선은 남는다.
    if protected.contains(&disk.number) {
        return Some(Rejection::Protected);
    }

    // 카드 없는 카드리더. 표시할 것이 없다.
    if disk.size_bytes == 0 {
        return Some(Rejection::NoMedia);
    }

    // 스팬/RAID 볼륨이 하나라도 있으면 건드리지 않는다.
    if disk.volumes.iter().any(|v| v.disk_extent_count > 1) {
        return Some(Rejection::SpannedVolume);
    }

    None
}

/// 목록에서의 상태를 판정한다.
///
/// 감출 것은 감추고, 쓸 수 없는 것은 **이유와 함께 비활성으로** 보여준다.
pub fn availability(disk: &DiskInfo, protected: &HashSet<u32>) -> Availability {
    if let Some(r) = hidden_reason(disk, protected) {
        return Availability::Hidden(r);
    }

    // 여기부터는 "보이지만 못 쓰는" 사유들. 감추지 않는다 —
    // 목록에 없으면 사용자는 장치가 인식되지 않았다고 오해한다.
    if disk.is_read_only {
        return Availability::Disabled(Rejection::ReadOnly);
    }
    if disk.size_bytes < MIN_USB_BYTES {
        return Availability::Disabled(Rejection::BelowMinimumCapacity {
            have: disk.size_bytes,
            minimum: MIN_USB_BYTES,
        });
    }

    Availability::Ready
}

/// 이 디스크를 사용자에게 보여줘도 되는가 (감춤 판정만).
pub fn is_listable(disk: &DiskInfo, protected: &HashSet<u32>) -> Result<(), Rejection> {
    match hidden_reason(disk, protected) {
        Some(r) => Err(r),
        None => Ok(()),
    }
}

/// 이 디스크에 **실제로 쓸** 수 있는가.
///
/// `is_listable`을 통과한 뒤에 추가로 확인한다. 목록에 보이는 것과
/// 쓰기를 허용하는 것은 다른 판단이다.
///
/// `source_image_disk`는 내려받은 이미지 파일이 올라가 있는 디스크 번호다.
/// 알 수 없으면 None을 넘긴다.
pub fn can_write(
    disk: &DiskInfo,
    protected: &HashSet<u32>,
    image_size: u64,
    source_image_disk: Option<u32>,
) -> Result<(), Rejection> {
    match availability(disk, protected) {
        Availability::Hidden(r) | Availability::Disabled(r) => return Err(r),
        Availability::Ready => {}
    }

    if image_size > disk.size_bytes {
        return Err(Rejection::TooSmall {
            need: image_size,
            have: disk.size_bytes,
        });
    }

    // 소스 이미지가 대상 위에 있으면 쓰는 도중 자기 자신을 지운다.
    if source_image_disk == Some(disk.number) {
        return Err(Rejection::SourceOnTarget);
    }

    Ok(())
}

/// 쓰기 직전, 열린 핸들에서 읽어온 실제 장치 정보가
/// 사용자가 고른 그 장치가 맞는지 확인한다.
///
/// 디스크 번호는 안정적이지 않다. 목록을 만든 뒤 사용자가 USB를 뽑았다 꽂으면
/// 같은 번호가 다른 장치를 가리킬 수 있다 (TOCTOU). 그래서 번호만으로는 부족하고,
/// 핸들에서 직접 읽은 속성이 선택 당시와 일치하는지 대조한다.
pub fn confirm_identity(selected: &DiskInfo, observed: &DiskInfo) -> Result<(), IdentityMismatch> {
    if selected.number != observed.number {
        return Err(IdentityMismatch::Number {
            expected: selected.number,
            actual: observed.number,
        });
    }
    if observed.bus_type != BusType::Usb {
        return Err(IdentityMismatch::BusType(observed.bus_type));
    }
    if selected.size_bytes != observed.size_bytes {
        return Err(IdentityMismatch::Size {
            expected: selected.size_bytes,
            actual: observed.size_bytes,
        });
    }
    if selected.friendly_name != observed.friendly_name {
        return Err(IdentityMismatch::Name {
            expected: selected.friendly_name.clone(),
            actual: observed.friendly_name.clone(),
        });
    }
    Ok(())
}

/// 쓰기 직전 신원 확인 실패 사유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityMismatch {
    Number { expected: u32, actual: u32 },
    BusType(BusType),
    Size { expected: u64, actual: u64 },
    Name { expected: String, actual: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::VolumeInfo;

    fn usb_disk(number: u32) -> DiskInfo {
        DiskInfo {
            number,
            friendly_name: "SanDisk Ultra".into(),
            size_bytes: 32 * 1024 * 1024 * 1024,
            bus_type: BusType::Usb,
            is_system: false,
            is_boot: false,
            boot_from_disk: false,
            is_clustered: false,
            is_read_only: false,
            serial: Some("ABC123".into()),
            volumes: vec![],
        }
    }

    fn vol(extents: u32) -> VolumeInfo {
        VolumeInfo {
            guid_path: r"\\?\Volume{00000000-0000-0000-0000-000000000000}\".into(),
            drive_letter: Some('E'),
            file_system: Some("NTFS".into()),
            size_bytes: 32 * 1024 * 1024 * 1024,
            disk_extent_count: extents,
        }
    }

    fn none() -> HashSet<u32> {
        HashSet::new()
    }

    #[test]
    fn plain_usb_stick_is_listable() {
        assert_eq!(is_listable(&usb_disk(2), &none()), Ok(()));
    }

    // --- 내장 디스크가 목록에 뜨지 않는다는 것이 이 프로그램의 최우선 안전 요구사항 ---

    #[test]
    fn every_non_usb_bus_is_rejected() {
        // USB 외의 모든 버스 타입은 예외 없이 거부돼야 한다.
        let buses = [
            BusType::Unknown,
            BusType::Scsi,
            BusType::Atapi,
            BusType::Ata,
            BusType::Ieee1394,
            BusType::Ssa,
            BusType::FibreChannel,
            BusType::Raid,
            BusType::IScsi,
            BusType::Sas,
            BusType::Sata,
            BusType::Sd,
            BusType::Mmc,
            BusType::Virtual,
            BusType::FileBackedVirtual,
            BusType::StorageSpaces,
            BusType::Nvme,
            BusType::Other,
        ];
        for bus in buses {
            let mut d = usb_disk(3);
            d.bus_type = bus;
            assert_eq!(
                is_listable(&d, &none()),
                Err(Rejection::NotUsb(bus)),
                "버스 {bus:?} 가 목록에 노출됐다"
            );
        }
    }

    #[test]
    fn nvme_system_drive_never_listed() {
        // 실수로 NVMe 시스템 디스크가 들어와도 막혀야 한다.
        let mut d = usb_disk(0);
        d.bus_type = BusType::Nvme;
        d.is_system = true;
        d.friendly_name = "Samsung SSD 990 PRO".into();
        assert!(is_listable(&d, &none()).is_err());
    }

    #[test]
    fn disk_zero_rejected_even_if_usb() {
        // 열거 버그로 0이 흘러들어오는 경우를 막는 방어선.
        assert_eq!(is_listable(&usb_disk(0), &none()), Err(Rejection::DiskZero));
    }

    #[test]
    fn system_flags_reject() {
        for set in [
            |d: &mut DiskInfo| d.is_system = true,
            |d: &mut DiskInfo| d.is_boot = true,
            |d: &mut DiskInfo| d.boot_from_disk = true,
            |d: &mut DiskInfo| d.is_clustered = true,
        ] {
            let mut d = usb_disk(2);
            set(&mut d);
            assert_eq!(is_listable(&d, &none()), Err(Rejection::SystemDisk));
        }
    }

    #[test]
    fn protected_set_overrides_clean_usb_disk() {
        // WMI가 "깨끗한 USB"라고 해도 커널이 보호 대상이라 하면 거부한다.
        let d = usb_disk(2);
        let protected: HashSet<u32> = [2].into_iter().collect();
        assert_eq!(is_listable(&d, &protected), Err(Rejection::Protected));
    }

    // --- 숨김과 비활성의 구분 ---
    //
    // 쓸 수 없는 USB 를 목록에서 없애면 사용자는 장치 인식 실패로 오해하고
    // 포트를 바꿔가며 헤맨다. 위험해서 감출 것과 이유를 알려줄 것을 구분한다.

    #[test]
    fn read_only_disk_is_shown_but_disabled() {
        let mut d = usb_disk(2);
        d.is_read_only = true;
        // 감추지 않는다.
        assert_eq!(is_listable(&d, &none()), Ok(()));
        assert_eq!(
            availability(&d, &none()),
            Availability::Disabled(Rejection::ReadOnly)
        );
        assert!(availability(&d, &none()).is_visible());
        assert!(!availability(&d, &none()).is_ready());
    }

    #[test]
    fn undersized_disk_is_shown_but_disabled() {
        // 4GB 스틱. 로더 이미지가 3.0~3.8GB 라 실질적으로 못 쓴다.
        let mut d = usb_disk(2);
        d.size_bytes = 4 * 1000 * 1000 * 1000;
        assert_eq!(
            availability(&d, &none()),
            Availability::Disabled(Rejection::BelowMinimumCapacity {
                have: 4_000_000_000,
                minimum: MIN_USB_BYTES
            })
        );
        assert!(availability(&d, &none()).is_visible());
    }

    #[test]
    fn eight_gb_disk_is_ready() {
        let mut d = usb_disk(2);
        d.size_bytes = MIN_USB_BYTES;
        assert_eq!(availability(&d, &none()), Availability::Ready);
    }

    #[test]
    fn internal_disk_is_hidden_not_disabled() {
        let mut d = usb_disk(2);
        d.bus_type = BusType::Nvme;
        let a = availability(&d, &none());
        assert!(!a.is_visible(), "내장 디스크가 목록에 보이면 안 된다");
        assert!(matches!(a, Availability::Hidden(_)));
    }

    #[test]
    fn undersized_disk_cannot_be_written() {
        let mut d = usb_disk(2);
        d.size_bytes = 4 * 1000 * 1000 * 1000;
        assert!(can_write(&d, &none(), 1024, None).is_err());
    }

    #[test]
    fn read_only_disk_cannot_be_written() {
        let mut d = usb_disk(2);
        d.is_read_only = true;
        assert_eq!(can_write(&d, &none(), 1024, None), Err(Rejection::ReadOnly));
    }

    #[test]
    fn missing_negative_evidence_does_not_hide_the_disk() {
        // BootFromDisk 등은 ESP 가 여러 개인 시스템에서 어떤 디스크에도 설정되지 않는다.
        // 그런 경우 "값 없음"을 배제 근거로 쓰면 목록이 통째로 비어버린다.
        // 이 모델에서는 bool 이므로 열거 계층이 null 을 false 로 매핑하면 되고,
        // false 는 배제 사유가 아니어야 한다.
        let d = usb_disk(2); // 모든 플래그 false
        assert_eq!(availability(&d, &none()), Availability::Ready);
    }

    #[test]
    fn empty_card_reader_rejected() {
        let mut d = usb_disk(2);
        d.size_bytes = 0;
        assert_eq!(is_listable(&d, &none()), Err(Rejection::NoMedia));
    }

    #[test]
    fn spanned_volume_rejected() {
        let mut d = usb_disk(2);
        d.volumes = vec![vol(1), vol(3)];
        assert_eq!(is_listable(&d, &none()), Err(Rejection::SpannedVolume));
    }

    #[test]
    fn single_extent_volumes_are_fine() {
        let mut d = usb_disk(2);
        d.volumes = vec![vol(1), vol(1)];
        assert_eq!(is_listable(&d, &none()), Ok(()));
    }

    // --- 쓰기 허용 판정 ---

    #[test]
    fn image_larger_than_disk_rejected() {
        let d = usb_disk(2);
        let too_big = d.size_bytes + 1;
        assert_eq!(
            can_write(&d, &none(), too_big, None),
            Err(Rejection::TooSmall {
                need: too_big,
                have: d.size_bytes
            })
        );
    }

    #[test]
    fn image_exactly_disk_size_allowed() {
        let d = usb_disk(2);
        assert_eq!(can_write(&d, &none(), d.size_bytes, None), Ok(()));
    }

    #[test]
    fn source_image_on_target_rejected() {
        let d = usb_disk(2);
        assert_eq!(
            can_write(&d, &none(), 1024, Some(2)),
            Err(Rejection::SourceOnTarget)
        );
    }

    #[test]
    fn source_image_elsewhere_is_fine() {
        let d = usb_disk(2);
        assert_eq!(can_write(&d, &none(), 1024, Some(0)), Ok(()));
    }

    #[test]
    fn can_write_still_applies_listable_rules() {
        // can_write 는 is_listable 을 반드시 통과시켜야 한다.
        let mut d = usb_disk(2);
        d.bus_type = BusType::Nvme;
        assert_eq!(
            can_write(&d, &none(), 1024, None),
            Err(Rejection::NotUsb(BusType::Nvme))
        );
    }

    // --- TOCTOU 방어 ---

    #[test]
    fn identity_confirmed_when_same_device() {
        let d = usb_disk(2);
        assert_eq!(confirm_identity(&d, &d.clone()), Ok(()));
    }

    #[test]
    fn identity_rejects_swapped_device_at_same_number() {
        // 사용자가 USB를 뽑았다 다른 걸 꽂아 같은 번호를 받은 상황.
        let selected = usb_disk(2);
        let mut observed = usb_disk(2);
        observed.friendly_name = "Samsung BAR".into();
        observed.size_bytes = 64 * 1024 * 1024 * 1024;
        assert!(confirm_identity(&selected, &observed).is_err());
    }

    #[test]
    fn identity_rejects_size_change() {
        let selected = usb_disk(2);
        let mut observed = usb_disk(2);
        observed.size_bytes += 1;
        assert_eq!(
            confirm_identity(&selected, &observed),
            Err(IdentityMismatch::Size {
                expected: selected.size_bytes,
                actual: selected.size_bytes + 1
            })
        );
    }

    #[test]
    fn identity_rejects_non_usb_observed() {
        // 핸들에서 읽은 실제 버스가 USB가 아니면 즉시 중단.
        let selected = usb_disk(2);
        let mut observed = usb_disk(2);
        observed.bus_type = BusType::Nvme;
        assert_eq!(
            confirm_identity(&selected, &observed),
            Err(IdentityMismatch::BusType(BusType::Nvme))
        );
    }

    #[test]
    fn identity_rejects_number_change() {
        let selected = usb_disk(2);
        let observed = usb_disk(3);
        assert_eq!(
            confirm_identity(&selected, &observed),
            Err(IdentityMismatch::Number {
                expected: 2,
                actual: 3
            })
        );
    }
}
