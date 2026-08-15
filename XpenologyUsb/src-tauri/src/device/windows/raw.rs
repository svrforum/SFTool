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
        if disk.number == 0 {
            return Err(DeviceError::IdentityChanged);
        }

        // ── 1. 무엇도 건드리기 전에 신원과 안전을 확인한다 ─────────────────
        //
        // 예전에는 드라이브 문자 제거와 파티션 테이블 삭제가 먼저 실행되고
        // 신원 확인은 그 뒤였다. 그래서 `IdentityChanged` 는 잘못된 장치를
        // **막은 것이 아니라 이미 망가뜨린 뒤 보고**하는 것이었다.
        let observed = {
            let probe = ioctl::open_physical_drive_for_query(disk.number)?;

            // 번호를 장치에서 직접 읽는다. 사용자가 고른 번호를 복사해 비교하면
            // 항상 참인 동어반복이 된다. 디스크 번호는 재사용되므로 이게 핵심이다.
            let actual_number = ioctl::query_device_number(&probe)?;
            if actual_number != disk.number {
                return Err(DeviceError::IdentityChanged);
            }

            let desc = ioctl::query_device_descriptor(&probe)?;
            let size = ioctl::query_length(&probe)?;
            let read_only = ioctl::is_write_protected(&probe);

            DiskInfo {
                number: actual_number,
                friendly_name: desc.friendly_name(),
                size_bytes: size,
                bus_type: BusType::from(desc.bus_type),
                is_system: false,
                is_boot: false,
                boot_from_disk: false,
                is_clustered: false,
                is_read_only: read_only,
                serial: desc.serial.clone(),
                volumes: disk.volumes.clone(),
            }
        };

        // USB 가 아니면 여기서 끝. 장치가 바뀌었다는 뜻이다.
        if observed.bus_type != BusType::Usb {
            return Err(DeviceError::IdentityChanged);
        }
        // 사용자가 고른 그 장치가 맞는지 대조한다.
        crate::core::safety::confirm_identity(disk, &observed)
            .map_err(|_| DeviceError::IdentityChanged)?;
        // 쓰기 금지 매체를 여기서 막는다. 예전에는 이 값이 리터럴 false 라
        // 판정 자체가 존재하지 않았다.
        if observed.is_read_only {
            return Err(DeviceError::WriteDenied);
        }

        // ── 2. 여기서부터 되돌릴 수 없다 ──────────────────────────────────

        // 드라이브 문자를 뗀다. 붙어 있으면 Windows 가 계속 다시 마운트한다.
        for v in &disk.volumes {
            if let Some(letter) = v.drive_letter {
                ioctl::remove_mount_point(letter);
            }
        }

        // 볼륨을 잠근다. 전부 잠그려 들지 않는다 — 하나라도 실패하면 작업이
        // 무너지는데 Windows 11 은 ESP 를 놓아주지 않는 경우가 있다.
        // 다만 **어떤 볼륨이 왜 실패했는지는 남긴다.** 나중에 쓰기가 거부되면
        // 그 원인을 짚을 유일한 단서다.
        let mut locked: Vec<OwnedHandle> = Vec::new();
        let mut lock_failures: Vec<String> = Vec::new();
        for v in &disk.volumes {
            let path = v.guid_path.trim_end_matches('\\');
            match ioctl::open_volume(path) {
                Ok(h) => {
                    if let Err(e) = ioctl::allow_extended_dasd_io(&h) {
                        lock_failures.push(format!("{path}: 경계 검사 해제 실패 ({e:?})"));
                    }
                    match ioctl::lock_volume_with_retry(&h, LOCK_RETRIES, LOCK_RETRY_INTERVAL) {
                        Ok(()) => {
                            if let Err(e) = ioctl::dismount_volume(&h) {
                                lock_failures.push(format!("{path}: 마운트 해제 실패 ({e:?})"));
                            }
                            locked.push(h);
                        }
                        Err(e) => lock_failures.push(format!("{path}: 잠금 실패 ({e:?})")),
                    }
                }
                Err(e) => lock_failures.push(format!("{path}: 열기 실패 ({e:?})")),
            }
        }
        if !disk.volumes.is_empty() && locked.is_empty() {
            return Err(DeviceError::Io {
                code: 0,
                message: format!(
                    "볼륨을 하나도 잠그지 못했습니다:\n{}",
                    lock_failures.join("\n")
                ),
            });
        }

        // 파티션 테이블을 RAW 로. 실패해도 중단하지 않는다 — 어차피 장치
        // 앞부분을 통째로 덮어쓰고 꼬리까지 지운다.
        {
            let prep = open_physical_with_retry(disk.number)?;
            let _ = ioctl::create_disk_raw(&prep);
            let _ = ioctl::update_properties(&prep);
            // 여기서 핸들이 닫힌다. 반드시 닫아야 한다 — 레이아웃 변경으로
            // 장치가 재열거되면 이 핸들은 ERROR_MEDIA_CHANGED 를 내기 시작한다.
        }

        // ── 3. 쓰기용 핸들 ───────────────────────────────────────────────
        let handle = open_physical_with_retry(disk.number)?;
        let sector_size = ioctl::query_sector_size(&handle)?;

        // 정렬된 반송 버퍼. 최소 4096 에 맞춘다 — Microsoft 는 물리 섹터 정렬을
        // 권장하고, 요즘 장치는 논리 512 / 물리 4096(512e)이 흔하다.
        let align = (sector_size as usize).max(4096);
        let bounce = ioctl::AlignedBuf::new(BOUNCE_BYTES, align);

        Ok(Box::new(WindowsSession {
            handle: Some(handle),
            _locked: locked,
            total_bytes: observed.size_bytes,
            observed,
            sector_size,
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
    /// Drop 에서 꺼내기 전에 먼저 닫아야 해서 Option 으로 둔다.
    handle: Option<OwnedHandle>,
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
        self.check_aligned(offset, data.len(), "쓰기")?;
        if offset + data.len() as u64 > self.total_bytes {
            return Err(DeviceError::Io {
                code: 112,
                message: "장치 끝을 넘어선 쓰기".into(),
            });
        }
        self.write_via_bounce(offset, data.len(), Some(data))
    }

    fn zero_tail(&mut self, bytes: u64) -> Result<(), DeviceError> {
        let ss = self.sector_size as u64;
        let len = bytes.div_ceil(ss) * ss;
        let len = len.min(self.total_bytes);
        let start = (self.total_bytes - len) / ss * ss;
        let count = (self.total_bytes - start) / ss * ss;
        if count == 0 {
            return Ok(());
        }
        // 0 을 채우는 것도 같은 경로를 지난다. 예전에는 여기만 별도 버퍼로
        // 직접 써서, 정렬 버퍼도 87 재시도도 받지 못했다.
        self.write_via_bounce(start, count as usize, None)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        self.check_aligned(offset, buf.len(), "읽기")?;

        // 읽기도 정렬된 버퍼를 거친다. 호출부가 준 슬라이스는 정렬돼 있다는
        // 보장이 없고, NO_BUFFERING 핸들은 버퍼 주소까지 본다.
        let ss = self.sector_size as usize;
        let chunk = (self.bounce.len() / ss) * ss;
        let mut done = 0usize;
        while done < buf.len() {
            let n = chunk.min(buf.len() - done);
            // 원시 포인터를 먼저 꺼내 가변 대여를 끝낸다. 그래야 아래에서
            // 핸들을 불변으로 빌릴 수 있다.
            let ptr = self.bounce.as_mut_ptr();
            // 안전성: bounce 는 최소 n 바이트(chunk 는 bounce 길이 이하)를
            // 담을 수 있고, 아래에서 정확히 그만큼만 복사한다.
            ioctl::read_into(self.hnd(), offset + done as u64, ptr, n)?;
            buf[done..done + n].copy_from_slice(&self.bounce.as_slice()[..n]);
            done += n;
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), DeviceError> {
        // 플러시를 빼먹으면 사용자가 USB 를 뽑는 순간 이미지 뒷부분이 사라진다.
        // 이것만 오류로 올린다 — 나머지는 정리 작업이라 실패해도 이미지는 온전하다.
        let flushed = ioctl::flush(self.hnd());
        // 세션을 소비해 정리한다. Drop 과 같은 경로를 쓴다.
        drop(self);
        flushed
    }
}

/// 세션이 어떻게 끝나든 장치를 정상 상태로 되돌린다.
///
/// **취소와 오류에도 이 정리가 실행되게 하려고 Drop 에 둔다.** 예전에는
/// `finish()` 에만 있어서, 사용자가 취소하거나 도중에 실패하면 잠금이 걸린 채
/// 파티션 테이블만 지워진 USB 가 남았다. 탐색기에 아무것도 안 뜨니
/// 사용자에게는 "이 프로그램이 USB 를 망가뜨렸다" 로 보인다.
///
/// `open()` 이 쓰기 한 바이트 전에 이미 레이아웃을 지우므로, 준비 단계 이후의
/// 어떤 중단도 이 상태를 만든다.
impl Drop for WindowsSession {
    fn drop(&mut self) {
        // 파티션 테이블을 다시 읽게 한다. 이게 없으면 볼륨 관리자가
        // 옛 상태를 그대로 들고 있다.
        let _ = ioctl::update_properties(self.hnd());

        // 잠금을 푼다. 남겨두면 USB 를 뽑을 수 없다.
        for h in &self._locked {
            let _ = ioctl::unlock_volume(h);
        }
        self._locked.clear();

        // 물리 핸들을 먼저 닫아야 꺼내기가 성공한다.
        // 필드를 비워 여기서 닫히게 한다.
        let disk_number = self.disk_number;
        if let Some(h) = self.handle.take() {
            drop(h);
        }

        // 꺼내기는 실패해도 치명적이지 않다. 이미지는 이미 쓰였고,
        // 사용자가 직접 안전 제거를 하면 된다.
        if let Ok(h) = ioctl::open_physical_drive_for_query(disk_number) {
            ioctl::allow_media_removal(&h);
            let _ = ioctl::eject_media(&h);
        }
    }
}

impl WindowsSession {
    /// 쓰기용 핸들. Drop 이 비우기 전까지는 항상 존재한다.
    fn hnd(&self) -> &OwnedHandle {
        self.handle
            .as_ref()
            .expect("세션이 살아 있는 동안 핸들은 항상 있다")
    }

    /// 오프셋과 길이가 섹터 배수인지 확인한다.
    ///
    /// 여기서 걸러내지 않으면 Windows 가 ERROR_INVALID_PARAMETER(87) 를 내는데,
    /// 그 코드만 보고는 원인이 정렬이라는 것을 알기 어렵다.
    fn check_aligned(&self, offset: u64, len: usize, what: &str) -> Result<(), DeviceError> {
        let ss = self.sector_size as u64;
        if !offset.is_multiple_of(ss) || !(len as u64).is_multiple_of(ss) {
            return Err(DeviceError::Io {
                code: 87,
                message: format!("{what} 정렬 오류: 오프셋 {offset}, 길이 {len}, 섹터 {ss}"),
            });
        }
        Ok(())
    }

    /// 정렬된 반송 버퍼를 거쳐 쓴다.
    ///
    /// `data` 가 None 이면 0 을 채운다. **모든 쓰기가 이 한 경로를 지나게 한다** —
    /// 예전에는 `write_at` 만 정렬 버퍼와 87 재시도를 쓰고 `zero_tail` 은
    /// 자체 버퍼로 직접 썼다. 같은 핸들, 같은 드라이버인데 한쪽만 보호받았다.
    fn write_via_bounce(
        &mut self,
        offset: u64,
        len: usize,
        data: Option<&[u8]>,
    ) -> Result<(), DeviceError> {
        let ss = self.sector_size as usize;
        let mut chunk = (self.bounce.len() / ss) * ss;
        let mut done = 0usize;

        while done < len {
            let n = chunk.min(len - done);
            match data {
                Some(src) => self.bounce.as_mut_slice()[..n].copy_from_slice(&src[done..done + n]),
                // 0 으로 채운다. 할당된 그대로 넘기면 힙 내용이 장치에 실린다.
                None => self.bounce.as_mut_slice()[..n].fill(0),
            }
            match ioctl::write_raw(self.hnd(), offset + done as u64, self.bounce.as_ptr(), n) {
                Ok(()) => done += n,
                // 일부 드라이버는 한 번에 받는 전송 크기에 상한이 있고, 넘으면
                // ERROR_INVALID_PARAMETER 를 돌려준다. 정렬은 이미 맞은 상태이므로
                // 87 이면 크기 문제로 보고 절반으로 줄인다. 섹터 하나까지 줄여도
                // 실패하면 진짜 오류다.
                Err(DeviceError::Io { code: 87, .. }) if chunk > ss => {
                    chunk = ((chunk / 2) / ss).max(1) * ss;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
