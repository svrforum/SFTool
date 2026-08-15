//! raw 디스크 쓰기.
//!
//! 순서를 임의로 바꾸면 동작하지 않는다. 각 단계가 왜 그 자리에 있는지 남겨 둔다.
//!
//! ```text
//! 마운트 지점 제거 → 논리 볼륨 잠금 → 준비용 물리 핸들로 RAW 레이아웃 → 핸들 닫기
//!   → 쓰기용 물리 핸들 새로 열기 → 정렬 쓰기 → 플러시 → 잠금 해제 → 꺼내기
//! ```

use super::ioctl::{self, OwnedHandle};
use crate::core::model::{BusType, DiskInfo};
use crate::device::{DeviceError, RawWriter, WriteSession};
use std::thread::sleep;
use std::time::Duration;

/// 잠금 재시도. 100ms × 150 = 15초.
///
/// 잠금은 경합한다. 탐색기가 방금 꽂힌 USB 를 훑고 있거나 백신이 검사 중이면
/// 첫 시도는 거의 항상 실패한다. 즉시 포기하면 사용자에게는 그냥 고장으로 보인다.
const LOCK_RETRIES: u32 = 150;
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// 재시도 도중 공유 모드를 완화하는 시점.
///
/// 처음에는 배타적으로 열어 다른 프로그램이 끼어들지 못하게 하고,
/// 그래도 안 되면 FILE_SHARE_WRITE 를 허용해 본다.
const SHARE_WRITE_AFTER: u32 = LOCK_RETRIES / 3;

/// 정렬된 반송 버퍼 크기. 한 번에 이보다 큰 쓰기는 나눠서 보낸다.
const BOUNCE_BYTES: usize = 8 * 1024 * 1024;

pub struct WindowsRawWriter;

impl WindowsRawWriter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsRawWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl RawWriter for WindowsRawWriter {
    fn open(&self, disk: &DiskInfo) -> Result<Box<dyn WriteSession>, DeviceError> {
        // 방어선 하나 더. 안전 판정을 이미 통과했더라도 여기서 다시 막는다.
        // 이 함수를 호출하는 모든 경로가 검사를 거쳤는지 신뢰하지 않는다.
        if disk.number == 0 {
            return Err(DeviceError::IdentityChanged);
        }

        // 1. 드라이브 문자를 떼어낸다.
        //    문자가 붙어 있으면 Windows 가 계속 볼륨을 다시 마운트한다.
        for v in &disk.volumes {
            if let Some(letter) = v.drive_letter {
                ioctl::remove_mount_point(letter);
            }
        }

        // 2. 논리 볼륨을 잠근다.
        //    전부 잠그려 들지 않는다. 하나라도 실패하면 작업 전체가 무너지는데,
        //    실제로 Windows 11 은 ESP 를 놓아주지 않는 경우가 있다.
        //    잠긴 핸들을 최소 하나 확보하면 제한 규칙을 만족한다.
        let mut locked: Vec<OwnedHandle> = Vec::new();
        for v in &disk.volumes {
            let path = v.guid_path.trim_end_matches('\\');
            if let Ok(h) = ioctl::open_volume(path) {
                // 경계 검사를 꺼야 볼륨 끝 섹터에 접근할 수 있다.
                ioctl::allow_extended_dasd_io(&h);
                if ioctl::lock_volume_with_retry(&h, LOCK_RETRIES, LOCK_RETRY_INTERVAL).is_ok() {
                    ioctl::dismount_volume(&h);
                    locked.push(h);
                }
            }
        }
        // 볼륨이 있는데 하나도 못 잠갔다면 진행할 수 없다.
        // 볼륨이 아예 없는 경우(빈 디스크)는 잠글 것이 없으므로 정상이다.
        if !disk.volumes.is_empty() && locked.is_empty() {
            return Err(DeviceError::Locked);
        }

        // 3. 준비용 물리 핸들로 파티션 테이블을 RAW 로 만든다.
        //    DELETE_DRIVE_LAYOUT 은 쓰지 않는다 — MBR 만 건드려서
        //    GPT 백업 헤더에 아무 효과가 없다.
        {
            let prep = open_physical_with_retry(disk.number)?;
            ioctl::create_disk_raw(&prep)?;
            ioctl::update_properties(&prep);
            // 여기서 핸들이 drop 되며 닫힌다. 반드시 닫아야 한다 —
            // 레이아웃 변경으로 장치가 재열거되면 이 핸들은
            // ERROR_MEDIA_CHANGED 를 내기 시작한다.
        }

        // 4. 쓰기용 핸들을 새로 연다.
        let handle = open_physical_with_retry(disk.number)?;

        // 5. 쓰기 직전 신원 확인.
        //    디스크 번호는 재사용된다. 목록을 만든 뒤 사용자가 USB 를 바꿔 꽂았다면
        //    같은 번호가 다른 장치를 가리킬 수 있다.
        let desc = ioctl::query_device_descriptor(&handle)?;
        let observed_size = ioctl::query_length(&handle)?;
        let observed = DiskInfo {
            number: disk.number,
            friendly_name: desc.friendly_name(),
            size_bytes: observed_size,
            bus_type: BusType::from(desc.bus_type),
            is_system: false,
            is_boot: false,
            boot_from_disk: false,
            is_clustered: false,
            is_read_only: desc.read_only,
            serial: desc.serial.clone(),
            volumes: disk.volumes.clone(),
        };
        if observed.bus_type != BusType::Usb {
            return Err(DeviceError::IdentityChanged);
        }

        let sector_size = ioctl::query_sector_size(&handle)?;

        // 정렬된 반송 버퍼. NO_BUFFERING 핸들은 버퍼 주소가 섹터 경계에
        // 맞기를 기대하는데, Rust 의 기본 할당은 그것을 보장하지 않는다.
        // 복사 비용은 메모리 대역폭 기준이라 USB 쓰기 속도에 비하면 없는 것과 같다.
        let bounce = ioctl::AlignedBuf::new(BOUNCE_BYTES, sector_size as usize);

        Ok(Box::new(WindowsSession {
            handle,
            _locked: locked,
            observed,
            sector_size,
            total_bytes: observed_size,
            disk_number: disk.number,
            bounce,
        }))
    }
}

/// 물리 디스크를 재시도하며 연다.
///
/// 공유 위반과 접근 거부에만 재시도한다. 그 외의 오류는 기다린다고 나아지지 않는다.
fn open_physical_with_retry(number: u32) -> Result<OwnedHandle, DeviceError> {
    let mut last = DeviceError::Locked;
    for attempt in 0..LOCK_RETRIES {
        let share_write = attempt >= SHARE_WRITE_AFTER;
        match ioctl::open_physical_drive_for_write(number, share_write) {
            Ok(h) => return Ok(h),
            Err(e @ (DeviceError::Locked | DeviceError::WriteDenied)) => {
                last = e;
                sleep(LOCK_RETRY_INTERVAL);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

pub struct WindowsSession {
    handle: OwnedHandle,
    /// 쓰기가 끝날 때까지 붙잡고 있어야 하는 볼륨 잠금들.
    /// 이름을 쓰지 않지만 drop 시점이 중요하므로 필드로 들고 있는다.
    _locked: Vec<OwnedHandle>,
    observed: DiskInfo,
    sector_size: u32,
    total_bytes: u64,
    disk_number: u32,
    /// 정렬된 반송 버퍼. 호출부가 준 슬라이스가 정렬돼 있다는 보장이 없으므로
    /// 여기로 복사한 뒤 쓴다.
    bounce: ioctl::AlignedBuf,
}

impl WriteSession for WindowsSession {
    fn observed(&self) -> &DiskInfo {
        &self.observed
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DeviceError> {
        let ss = self.sector_size as u64;
        // 여기서 걸러내지 않으면 Windows 가 ERROR_INVALID_PARAMETER(87) 를 내는데,
        // 그 오류만 보고는 원인이 정렬이라는 것을 알기 어렵다.
        if offset % ss != 0 || data.len() as u64 % ss != 0 {
            return Err(DeviceError::Io {
                code: 87,
                message: format!("정렬 오류: 오프셋 {offset}, 길이 {}, 섹터 {ss}", data.len()),
            });
        }
        if offset + data.len() as u64 > self.total_bytes {
            return Err(DeviceError::Io {
                code: 112,
                message: "장치 끝을 넘어선 쓰기".into(),
            });
        }

        // 정렬된 버퍼를 거쳐 쓴다. 반송 버퍼보다 큰 요청은 나눠 보내되,
        // 조각도 섹터 배수를 유지해야 하므로 버퍼 크기를 섹터로 내림해 쓴다.
        let chunk = (self.bounce.len() / ss as usize) * ss as usize;
        let mut written = 0usize;
        while written < data.len() {
            let n = chunk.min(data.len() - written);
            self.bounce.as_mut_slice()[..n].copy_from_slice(&data[written..written + n]);
            ioctl::write_raw(
                &self.handle,
                offset + written as u64,
                self.bounce.as_ptr(),
                n,
            )?;
            written += n;
        }
        Ok(())
    }

    fn zero_tail(&mut self, bytes: u64) -> Result<(), DeviceError> {
        let ss = self.sector_size as u64;
        // 지울 길이를 섹터 배수로 올림한다.
        let len = ((bytes + ss - 1) / ss) * ss;
        let len = len.min(self.total_bytes);
        let start = self.total_bytes - len;
        // 시작점도 섹터 경계에 맞춘다.
        let start = (start / ss) * ss;

        let chunk = (4 * 1024 * 1024 / ss * ss).max(ss) as usize;
        let zeros = vec![0u8; chunk];
        let mut pos = start;
        while pos < self.total_bytes {
            let n = chunk.min((self.total_bytes - pos) as usize);
            // 마지막 조각도 섹터 배수여야 한다.
            let n = (n as u64 / ss * ss) as usize;
            if n == 0 {
                break;
            }
            ioctl::write_at(&self.handle, pos, &zeros[..n])?;
            pos += n as u64;
        }
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        let ss = self.sector_size as u64;
        if offset % ss != 0 || buf.len() as u64 % ss != 0 {
            return Err(DeviceError::Io {
                code: 87,
                message: "정렬되지 않은 읽기".into(),
            });
        }
        ioctl::read_at(&self.handle, offset, buf)
    }

    fn finish(self: Box<Self>) -> Result<(), DeviceError> {
        // 플러시를 빼먹으면 사용자가 USB 를 뽑는 순간 이미지 뒷부분이 사라진다.
        ioctl::flush(&self.handle)?;
        ioctl::update_properties(&self.handle);

        // 잠금을 풀고 핸들을 닫는다. 순서가 중요하다 —
        // 물리 핸들이 열려 있는 채로 꺼내기를 시도하면 실패한다.
        let Self {
            handle,
            _locked,
            disk_number,
            ..
        } = *self;
        for h in &_locked {
            ioctl::unlock_volume(h);
        }
        drop(_locked);
        drop(handle);

        // 꺼내기는 실패해도 치명적이지 않다. 이미지는 이미 다 쓰였다.
        // 사용자가 직접 안전 제거를 하면 된다.
        if let Ok(h) = ioctl::open_physical_drive_for_query(disk_number) {
            ioctl::allow_media_removal(&h);
            ioctl::eject_media(&h);
        }
        Ok(())
    }
}
