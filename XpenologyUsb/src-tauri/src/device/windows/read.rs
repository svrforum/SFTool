//! 원본 디스크 읽기.
//!
//! 쓰기 경로(`raw.rs`)와 파일을 나눠 둔 이유는 방향을 코드 구조로 못 박기
//! 위해서다. 여기에는 잠그거나 마운트를 해제하거나 레이아웃을 지우는 코드가
//! **없다.** 원본은 사용자가 이미 잘 쓰고 있는 USB 이므로 복제가 그것을
//! 건드릴 이유가 없다.
//!
//! 마운트된 볼륨이 있어도 물리 디스크의 읽기는 커널이 허용한다.
//! 제약이 걸리는 것은 쓰기뿐이다.

use super::ioctl::{self, OwnedHandle};
use crate::core::model::{BusType, DiskInfo};
use crate::device::{DeviceError, RawReader, ReadSession};

pub struct WindowsRawReader;

impl WindowsRawReader {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsRawReader {
    fn default() -> Self {
        Self::new()
    }
}

impl RawReader for WindowsRawReader {
    fn open(&self, disk: &DiskInfo) -> Result<Box<dyn ReadSession>, DeviceError> {
        let path = format!(r"\\.\PhysicalDrive{}", disk.number);
        // 잠그지 않는다(lock=false), 쓰기 권한도 받지 않는다(write_access=false).
        // 그래서 공유는 읽기·쓰기 모두 허용된 채로 열린다.
        let handle = ioctl::get_handle(&path, false, false, "원본 디스크 열기")?;

        // 번호를 장치에서 직접 읽는다. 고른 번호를 복사해 비교하면 동어반복이 된다.
        let actual_number = ioctl::query_device_number(&handle)?;
        if actual_number != disk.number {
            return Err(DeviceError::IdentityChanged);
        }

        let desc = ioctl::query_device_descriptor(&handle)?;
        let size = ioctl::query_length(&handle)?;

        let observed = DiskInfo {
            number: actual_number,
            friendly_name: desc.friendly_name(),
            size_bytes: size,
            bus_type: BusType::from(desc.bus_type),
            is_system: false,
            is_boot: false,
            boot_from_disk: false,
            is_clustered: false,
            is_read_only: false,
            serial: desc.serial.clone(),
            volumes: disk.volumes.clone(),
        };

        // VHD 통합 테스트에서만 버스 타입 검사를 넘긴다. 이유는 raw.rs 와 같다 —
        // 가상 디스크는 Virtual 로 보고되지만, 그 테스트가 확인하려는 것은
        // 버스 판정이 아니라 Win32 호출 순서다.
        #[cfg(feature = "vhd-tests")]
        let observed = {
            let mut o = observed;
            o.bus_type = disk.bus_type;
            o
        };

        if observed.bus_type != BusType::Usb {
            return Err(DeviceError::IdentityChanged);
        }

        let sector = ioctl::query_sector_size(&handle)?;

        Ok(Box::new(WindowsReadSession {
            handle: Some(handle),
            observed,
            sector,
            total: size,
        }))
    }
}

struct WindowsReadSession {
    handle: Option<OwnedHandle>,
    observed: DiskInfo,
    sector: u32,
    total: u64,
}

impl WindowsReadSession {
    fn hnd(&self) -> Result<&OwnedHandle, DeviceError> {
        self.handle.as_ref().ok_or(DeviceError::MediaChanged)
    }
}

impl ReadSession for WindowsReadSession {
    fn observed(&self) -> &DiskInfo {
        &self.observed
    }
    fn sector_size(&self) -> u32 {
        self.sector
    }
    fn total_bytes(&self) -> u64 {
        self.total
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        let ss = self.sector as u64;
        if !offset.is_multiple_of(ss) || !(buf.len() as u64).is_multiple_of(ss) {
            return Err(DeviceError::Io {
                code: 87,
                message: format!("정렬되지 않은 읽기 (오프셋 {offset}, 길이 {})", buf.len()),
            });
        }
        if offset + buf.len() as u64 > self.total {
            return Err(DeviceError::Io {
                code: 112,
                message: "장치 끝을 넘어선 읽기".into(),
            });
        }
        let len = buf.len();
        let h = self.hnd()?;
        // 안전성: 포인터와 길이는 같은 슬라이스에서 나왔으므로 정확히 그만큼
        // 쓸 수 있는 메모리를 가리킨다.
        ioctl::read_into(h, offset, buf.as_mut_ptr(), len)
    }

    fn finish(mut self: Box<Self>) -> Result<(), DeviceError> {
        // 읽기만 했으므로 되돌릴 것이 없다. 핸들만 닫는다.
        self.handle = None;
        Ok(())
    }
}
