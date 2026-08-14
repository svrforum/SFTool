//! 도메인 타입. 플랫폼 의존성이 없으므로 어느 OS에서든 테스트된다.

use serde::{Deserialize, Serialize};

/// Windows STORAGE_BUS_TYPE. 값은 커널이 그대로 넘겨주는 것이라 의미가 고정돼 있다.
///
/// USB 판정에 이것만 쓴다. `MediaRemovable`이나 `MediaType`은 쓰지 않는다 —
/// USB SSD/HDD는 removable=false 로, 카드리더는 카드가 없어도 true 로 보고하기 때문에
/// 둘 다 USB 여부의 근거가 되지 못한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum BusType {
    Unknown = 0,
    Scsi = 1,
    Atapi = 2,
    Ata = 3,
    Ieee1394 = 4,
    Ssa = 5,
    FibreChannel = 6,
    Usb = 7,
    Raid = 8,
    IScsi = 9,
    Sas = 10,
    Sata = 11,
    Sd = 12,
    Mmc = 13,
    Virtual = 14,
    FileBackedVirtual = 15,
    StorageSpaces = 16,
    Nvme = 17,
    Other = 0xFFFF,
}

impl From<u16> for BusType {
    fn from(v: u16) -> Self {
        match v {
            0 => BusType::Unknown,
            1 => BusType::Scsi,
            2 => BusType::Atapi,
            3 => BusType::Ata,
            4 => BusType::Ieee1394,
            5 => BusType::Ssa,
            6 => BusType::FibreChannel,
            7 => BusType::Usb,
            8 => BusType::Raid,
            9 => BusType::IScsi,
            10 => BusType::Sas,
            11 => BusType::Sata,
            12 => BusType::Sd,
            13 => BusType::Mmc,
            14 => BusType::Virtual,
            15 => BusType::FileBackedVirtual,
            16 => BusType::StorageSpaces,
            17 => BusType::Nvme,
            _ => BusType::Other,
        }
    }
}

/// 디스크 위의 볼륨 하나.
///
/// 드라이브 문자가 없는 볼륨(ESP, MSR, 리눅스 파티션)도 반드시 포함해야 한다.
/// 문자가 없어도 마운트돼 있으면 raw 쓰기를 막기 때문이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInfo {
    /// `\\?\Volume{GUID}\` 형태의 볼륨 이름.
    pub guid_path: String,
    /// 드라이브 문자. 배정돼 있지 않으면 None.
    pub drive_letter: Option<char>,
    /// 파일 시스템 이름 (NTFS, FAT32, ...). 인식 불가면 None.
    pub file_system: Option<String>,
    pub size_bytes: u64,
    /// 이 볼륨이 차지하는 디스크 extent 개수.
    /// 2 이상이면 스팬/RAID 볼륨이므로 대상에서 거부한다.
    pub disk_extent_count: u32,
}

/// 물리 디스크 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskInfo {
    /// PhysicalDrive 번호. `\\.\PhysicalDriveN` 의 N.
    ///
    /// 주의: 이 번호는 안정적이지 않다. 장치를 뽑았다 꽂으면 재사용될 수 있으므로
    /// 쓰기 직전에 열린 핸들 위에서 반드시 재확인해야 한다.
    pub number: u32,
    pub friendly_name: String,
    pub size_bytes: u64,
    pub bus_type: BusType,
    pub is_system: bool,
    pub is_boot: bool,
    pub boot_from_disk: bool,
    pub is_clustered: bool,
    pub is_read_only: bool,
    /// USB 브리지가 제공하는 시리얼. 신뢰할 수 없다 —
    /// 빈 문자열이거나 같은 모델 전체가 공유하는 값일 수 있어 식별 키로 쓰지 않는다.
    pub serial: Option<String>,
    pub volumes: Vec<VolumeInfo>,
}

impl DiskInfo {
    /// 목록 갱신 시 같은 장치인지 비교하기 위한 합성 키.
    ///
    /// 시리얼 단독으로는 식별할 수 없어서 여러 속성을 묶는다.
    pub fn identity_key(&self) -> String {
        format!(
            "{:?}|{}|{}|{}",
            self.bus_type,
            self.size_bytes,
            self.friendly_name,
            self.serial.as_deref().unwrap_or("")
        )
    }
}
