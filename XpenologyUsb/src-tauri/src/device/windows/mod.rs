//! Windows 구현.
//!
//! ## WMI 를 쓰지 않는 이유
//!
//! 흔한 접근은 `root\Microsoft\Windows\Storage` 의 `MSFT_Disk` 를 WMI 로 조회하는
//! 것이다. C# 에서는 자연스럽지만 Rust 에서는 COM 초기화와 변형(variant) 처리가
//! 붙어 배보다 배꼽이 커진다.
//!
//! 그런데 정작 필요한 값은 전부 IOCTL 로 직접 얻을 수 있다. `MSFT_Disk.BusType` 이
//! 신뢰할 만한 이유 자체가 "커널의 STORAGE_BUS_TYPE 을 그대로 전달하기 때문"인데,
//! 그 원본을 `IOCTL_STORAGE_QUERY_PROPERTY` 로 바로 물으면 중간 단계가 사라진다.
//!
//! WMI 로만 얻을 수 있는 것은 `IsSystem` / `IsBoot` / `BootFromDisk` 플래그다.
//! 이것들은 포기하는 대신, 시스템 경로에서 역산한 보호 디스크 집합으로 대체한다.
//! 오히려 이쪽이 강하다 — 플래그는 값이 없거나 틀릴 수 있지만, "C: 를 담고 있는
//! 디스크 번호"는 커널이 지금 이 순간 답하는 사실이다.

mod eject;
mod ioctl;
mod raw;
mod read;

pub use raw::WindowsRawWriter;
pub use read::WindowsRawReader;

/// USB 를 안전하게 제거한다.
///
/// 볼륨 관리자에게 미디어 제거를 허용시킨 뒤 꺼내기를 요청한다.
/// 실패하면 그 이유를 그대로 올린다 — 대개 무언가 아직 장치를 붙잡고 있다는 뜻이라
/// 사용자가 탐색기 창을 닫고 다시 시도하면 된다.
pub fn eject(disk_number: u32) -> Result<(), DeviceError> {
    // 먼저 볼륨 관리자에게 제거를 허용시킨다. 실패해도 다음 단계를 막지 않는다.
    if let Ok(h) = ioctl::open_physical(disk_number, false, false) {
        ioctl::allow_media_removal(&h);
    }
    // 실제 안전 제거. USB 스틱은 미디어 꺼내기가 아니라 장치 제거여야 한다.
    eject::request_eject(disk_number)
}

use super::{DeviceError, UsbEnumerator};
use crate::core::model::{BusType, DiskInfo, VolumeInfo};
use std::collections::HashSet;

/// 물리 디스크 열거자.
pub struct WindowsEnumerator {
    /// 훑어볼 디스크 번호 상한.
    ///
    /// Windows 는 디스크 번호를 촘촘하게 배정하지 않으므로 위쪽에 구멍이 있을 수
    /// 있다. 넉넉하게 잡되, 존재하지 않는 번호는 열기 실패로 조용히 건너뛴다.
    max_disks: u32,
}

impl WindowsEnumerator {
    pub fn new() -> Self {
        Self { max_disks: 64 }
    }
}

impl Default for WindowsEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbEnumerator for WindowsEnumerator {
    fn list_disks(&self) -> Result<Vec<DiskInfo>, DeviceError> {
        // 볼륨 → 디스크 매핑을 먼저 만들어 둔다. 디스크마다 전체 볼륨을
        // 다시 훑으면 O(n²) 이 되고, 볼륨 열거는 느리다.
        let volumes = ioctl::enumerate_volumes();

        let mut disks = Vec::new();
        // 열거에서 빠진 장치와 그 이유. 목록이 비었을 때 원인을 알려면 필요하다.
        let mut skipped: Vec<String> = Vec::new();
        for number in 0..self.max_disks {
            // 조회 전용으로 연다. 접근 권한 0 이면 관리자가 아니어도 열린다.
            // 목록 표시에 권한 상승을 요구하지 않기 위해서다.
            let Ok(handle) = ioctl::open_physical_drive_for_query(number) else {
                continue;
            };

            let desc = match ioctl::query_device_descriptor(&handle) {
                Ok(d) => d,
                Err(e) => {
                    // 버스 타입을 알 수 없으면 목록에 올리지 않는다.
                    // 안전 판정의 1차 근거가 없는 장치이기 때문이다.
                    // 다만 왜 빠졌는지는 남긴다 — 조용히 사라지면 원인을 찾을 수 없다.
                    skipped.push(format!("디스크 {number}: 장치 정보 조회 실패 ({e:?})"));
                    continue;
                }
            };

            // 용량 조회 실패를 0 으로 바꾸지 않는다. 0 은 안전 규칙에서
            // "미디어 없음"을 뜻해서, 오류가 곧 "목록에서 감춤"이 되어버린다.
            // 0.1.1 에서 모든 USB 가 사라진 원인이 정확히 이 변환이었다.
            let size = match ioctl::query_length(&handle) {
                Ok(s) => s,
                Err(e) => {
                    skipped.push(format!("디스크 {number}: 용량 조회 실패 ({e:?})"));
                    continue;
                }
            };

            let mine: Vec<VolumeInfo> = volumes
                .iter()
                .filter(|(disk_no, _)| *disk_no == number)
                .map(|(_, v)| v.clone())
                .collect();

            disks.push(DiskInfo {
                number,
                friendly_name: desc.friendly_name(),
                size_bytes: size,
                bus_type: BusType::from(desc.bus_type),
                // IOCTL 로는 이 플래그들을 알 수 없다. false 로 두고
                // 보호 디스크 집합이 그 역할을 대신한다.
                is_system: false,
                is_boot: false,
                boot_from_disk: false,
                is_clustered: false,
                // 장치에 직접 묻는다. 예전에는 서술자에 없는 값을
                // 리터럴 false 로 적어 넣어서, 쓰기 금지 판정이 한 번도
                // 동작하지 않았다.
                is_read_only: ioctl::is_write_protected(&handle),
                serial: desc.serial.clone(),
                volumes: mine,
            });
        }

        // 하나도 못 찾았다면 이유를 오류에 담아 올린다. 빈 목록만 보여주면
        // 사용자는 USB 를 다시 꽂아보는 것 말고 할 수 있는 게 없다.
        if disks.is_empty() && !skipped.is_empty() {
            return Err(DeviceError::Io {
                code: 0,
                message: format!("디스크를 열거하지 못했습니다:\n{}", skipped.join("\n")),
            });
        }
        Ok(disks)
    }

    fn protected_disk_numbers(&self) -> Result<HashSet<u32>, DeviceError> {
        Ok(ioctl::protected_disk_numbers())
    }
}
