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
use crate::device::prep::{describe, with_write_context, Prep, Touched};
use crate::device::{DeviceError, RawWriter, WriteSession};

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
            return Err(DeviceError::IdentityChanged {
                message: "디스크 0 번은 시스템 디스크입니다".into(),
            });
        }

        // ── 1. 무엇도 건드리기 전에 신원과 안전을 확인한다 ─────────────────
        //
        // 예전에는 드라이브 문자 제거와 파티션 테이블 삭제가 먼저 실행되고
        // 신원 확인은 그 뒤였다. 그래서 `IdentityChanged` 는 잘못된 장치를
        // **막은 것이 아니라 이미 망가뜨린 뒤 보고**하는 것이었다.
        let (observed, sector_size) = {
            let probe = ioctl::open_physical_drive_for_query(disk.number)?;

            // 번호를 장치에서 직접 읽는다. 사용자가 고른 번호를 복사해 비교하면
            // 항상 참인 동어반복이 된다. 디스크 번호는 재사용되므로 이게 핵심이다.
            let actual_number = ioctl::query_device_number(&probe)?;
            if actual_number != disk.number {
                return Err(DeviceError::IdentityChanged {
                    message: format!(
                        "디스크 번호가 다릅니다: {} 번을 골랐는데 장치는 {actual_number} 번이라고 답합니다",
                        disk.number
                    ),
                });
            }

            let desc = ioctl::query_device_descriptor(&probe)?;
            let size = ioctl::query_length(&probe)?;
            let read_only = ioctl::is_write_protected(&probe);
            // 섹터 크기도 **여기서** 묻는다.
            //
            // 예전에는 파티션 테이블을 지운 **뒤에** 물었다. 그래서 섹터 크기가
            // 비정상인 장치를 거부하는 판정(`BadSectorSize`)이 대상을 이미
            // 망가뜨린 다음에 내려졌다 — 거부라기보다 사후 통보였다. 기하 정보는
            // `FILE_ANY_ACCESS` 라 조회용 핸들에서도 읽히므로 앞당기는 데
            // 걸리는 것이 없다.
            let sector_size = ioctl::query_sector_size(&probe)?;

            (
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
                },
                sector_size,
            )
        };

        // VHD 통합 테스트에서만 버스 타입 검사를 넘긴다.
        //
        // 가상 디스크는 `Virtual` 로 보고되므로 이 검사에 걸린다. 검사 자체는
        // 옳고 실제로 제 역할을 했다 — 테스트가 여기서 막혔다. 다만 그 테스트가
        // 확인하려는 것은 버스 판정이 아니라 **Win32 호출 순서**이므로,
        // 이 한 가지만 기능 플래그 뒤에서 완화한다.
        //
        // `vhd-tests` 는 CI 의 전용 잡에서만 켜지고 배포 빌드에는 들어가지
        // 않는다. 버스 판정 자체는 `core::safety` 의 단위 테스트가 전수로
        // 검증하므로, 이 완화가 그 보증을 약하게 만들지 않는다.
        #[cfg(feature = "vhd-tests")]
        let observed = {
            let mut o = observed;
            o.bus_type = disk.bus_type;
            o
        };

        // USB 가 아니면 여기서 끝. 장치가 바뀌었다는 뜻이다.
        if observed.bus_type != BusType::Usb {
            return Err(DeviceError::IdentityChanged {
                message: format!(
                    "USB 가 아닙니다: 연결 방식이 {:?} 입니다",
                    observed.bus_type
                ),
            });
        }
        // 사용자가 고른 그 장치가 맞는지 대조한다.
        crate::core::safety::confirm_identity(disk, &observed).map_err(|m| {
            // **어긋난 항목과 두 값을 그대로 올린다.** 예전에는 `|_|` 로 통째로
            // 버려서, 화면에는 "다른 장치입니다" 와 러스트 토큰 하나만 남았다.
            DeviceError::IdentityChanged {
                message: m.describe(),
            }
        })?;
        // 쓰기 금지 매체를 여기서 막는다. 예전에는 이 값이 리터럴 false 라
        // 판정 자체가 존재하지 않았다.
        if observed.is_read_only {
            return Err(DeviceError::WriteDenied {
                op: "쓰기 금지 매체 확인",
            });
        }

        // ── 2. 여기서부터 되돌릴 수 없다 ──────────────────────────────────
        //
        // 파괴 단계 전체를 `prepare` 안에 몰아넣고, 실패는 여기서 한 번만 받는다.
        //
        // 이렇게 하는 이유는 `WindowsSession` 이 **성공했을 때만** 만들어지기
        // 때문이다. 세션의 `Drop` 이 잠금 해제와 재인식을 맡고 있는데, 준비
        // 도중에 나가는 오류는 세션이 없으니 그 정리를 만나지 못하고, 지금까지
        // 모은 준비 기록도 스택과 함께 사라졌다. 남는 것은 사용자 화면의
        // "탐색기 창을 닫고 다시 시도해 주세요" 한 줄 — 파티션 테이블이 이미
        // 지워져 탐색기에 뜨지도 않는 USB 를 두고 하는 말이었다.
        //
        // **단계의 순서는 하나도 바뀌지 않았다.** 그 순서의 이유는 `prepare`
        // 안의 주석에 그대로 남아 있다.
        let mut prep = Prep::new();
        let mut locked: Vec<OwnedHandle> = Vec::new();

        match Self::prepare(disk, &observed, sector_size, &mut prep, &mut locked) {
            Ok(session) => Ok(Box::new(session)),
            Err(e) => {
                Self::recover(disk.number, &locked, &prep);
                Err(prep.explain(e))
            }
        }
    }
}

impl WindowsRawWriter {
    /// 되돌릴 수 없는 준비 단계.
    ///
    /// 순서는 Rufus 의 DD 경로를 그대로 따른다. 우리가 쓰던 순서는 정반대였고,
    /// 그래서 쓰기가 거부됐다: 볼륨을 먼저 잠근 뒤 파티션 테이블을 지우면
    /// 재열거 과정에서 그 볼륨 객체들이 사라지고, 잠금은 없어진 것을 가리키게
    /// 된다. 새로 만들어진 볼륨은 마운트돼 있고 잠겨 있지 않으므로 커널이
    /// 쓰기를 막는다.
    ///
    /// 잠근 볼륨을 호출부의 `locked` 에 담는 이유는, 여기서 실패해도 호출부가
    /// 그것들을 풀 수 있어야 하기 때문이다.
    fn prepare(
        disk: &DiskInfo,
        observed: &DiskInfo,
        sector_size: u32,
        prep: &mut Prep,
        locked: &mut Vec<OwnedHandle>,
    ) -> Result<WindowsSession, DeviceError> {
        // 2-1. 읽기 전용으로 열고 잠근 뒤, 그 상태에서 드라이브 문자를 뗀다.
        {
            let ro = ioctl::open_physical(disk.number, true, false)?;
            for v in &disk.volumes {
                if let Some(letter) = v.drive_letter {
                    // 되돌리는 코드가 없고 실패를 보고하지도 않는다. 표시라도 남겨야
                    // 여기서 나가는 오류가 "드라이브 문자는 이미 떨어졌다" 를 말할 수 있다.
                    ioctl::remove_mount_point(letter);
                    prep.reached(Touched::Mounts);
                }
            }
            drop(ro); // 잠금 해제 후 닫는다. 다음 단계는 핸들이 없어야 한다.
        }

        // 2-2. 파티션 테이블을 지운다.
        //
        // `IOCTL_DISK_CREATE_DISK` 는 쓰지 않는다 — Rufus 는 DD 경로에서
        // `InitializeDisk` 를 **건너뛴다**. 우리는 그 건너뛰는 경로를 쓰고 있었다.
        // 여기서는 레이아웃만 지우고, 실제 내용은 곧 이미지가 덮는다.
        {
            let h = ioctl::open_physical(disk.number, false, true)?;
            // 다음 줄이 곧 되돌릴 수 없는 지점이다. 성공 여부와 무관하게
            // 표시한다 — 실패했더라도 지워졌는지 아닌지 확신할 수 없다.
            prep.reached(Touched::Layout);
            if let Err(e) = ioctl::delete_drive_layout(&h) {
                prep.note(format!("파티션 테이블 삭제 실패 ({})", describe(&e)));
            }
            if let Err(e) = ioctl::update_properties(&h) {
                prep.note(format!("파티션 테이블 재인식 실패 ({})", describe(&e)));
            }
        }

        // 2-3. 쓰기용 핸들. **잠금이 열기의 일부이고, 실패하면 여기서 끝난다.**
        //      잠기지 않은 물리 핸들로 쓰면 커널이 거부한다.
        let handle = ioctl::open_physical(disk.number, true, true)?;

        // 2-4. 파티션 테이블 자리를 실제로 0 으로 덮는다. **볼륨을 잠그기 전에.**
        //
        // 이 순서가 핵심이다. 이미 로더가 써진 USB 에 다시 쓰면
        // "오프셋 8388608(=8MiB) 에서 거부" 가 났다. 그 앞은 어떤 파티션에도
        // 속하지 않아 써지고, 8MiB 부터는 마운트된 파티션의 섹터라 막힌 것이다.
        //
        // `IOCTL_DISK_DELETE_DRIVE_LAYOUT` 은 MBR 서명만 건드려서 GPT 나 리눅스
        // 파티션을 남긴다. 반면 맨 앞 1MiB 는 MBR·GPT 헤더·GPT 항목이 놓이는
        // 자리이고 **어떤 볼륨에도 속하지 않으므로**, 볼륨이 마운트된 상태에서도
        // 쓸 수 있다. 여기를 지우고 재인식시키면 볼륨 자체가 사라진다.
        //
        // Rufus 가 볼륨을 하나만 잠그고도 되는 이유가 이것이다 — 그 시점에는
        // 파티션이 이미 지워져 있다. 지우지 않은 채 잠금만 하나로 줄이면
        // 나머지 파티션에서 막힌다.
        //
        // 섹터 크기와 용량은 신원 확인 때 이 장치에서 이미 읽은 값을 쓴다.
        // 여기서 다시 물으면 되돌릴 수 없는 창 안에 나갈 구멍이 둘 늘어날 뿐이고,
        // 세션이 실제로 쓰는 값도 결국 그때 읽은 `observed.size_bytes` 다.
        {
            let ss = sector_size as u64;
            let head = (1024 * 1024u64).min(observed.size_bytes) / ss * ss;
            if head > 0 {
                let mut buf = ioctl::AlignedBuf::new(head as usize, (ss as usize).max(4096));
                buf.as_mut_slice().fill(0);
                if let Err(e) = ioctl::write_raw(&handle, 0, buf.as_ptr(), head as usize) {
                    prep.note(format!("파티션 영역 지우기 실패 ({})", describe(&e)));
                }
            }
        }
        if let Err(e) = ioctl::update_properties(&handle) {
            prep.note(format!("레이아웃 갱신 실패 ({})", describe(&e)));
        }

        // 2-5. 그래도 남아 있는 볼륨은 **전부** 잠근다.
        //
        // 위에서 지웠으면 보통 하나도 남지 않는다. 그래도 윈도우가 다시 마운트하는
        // 경우가 있어서 남은 것은 빠짐없이 잠근다. 하나만 잠그고 넘어가면
        // 잠기지 않은 볼륨의 섹터에서 쓰기가 거부된다 — 실제로 그렇게 실패했다.
        //
        // 훑기가 실패했는지를 따로 받는 이유는 [`ioctl::enumerate_volumes_reporting`]
        // 에 적어 두었다. 요약하면, 빈 목록이 "잠글 것이 없었다" 와 "아무것도
        // 확인하지 못했다" 를 구별하지 못한다.
        let (volumes, scanned) = ioctl::enumerate_volumes_reporting();
        if !scanned {
            prep.note("볼륨 목록을 읽지 못했습니다 (FindFirstVolumeW 실패)");
        }
        for (disk_no, v) in volumes {
            if disk_no != disk.number {
                continue;
            }
            let path = v.guid_path.trim_end_matches('\\');
            match ioctl::open_volume(path, true) {
                Ok(h) => {
                    if let Err(e) = ioctl::dismount_volume(&h) {
                        prep.note(format!("{path}: 마운트 해제 실패 ({})", describe(&e)));
                    }
                    locked.push(h);
                }
                Err(e) => prep.note(format!("{path}: 잠금 실패 ({})", describe(&e))),
            }
        }

        let prep_notes = prep.summary(locked.len(), scanned);

        // 정렬 자체는 이제 필수가 아니다 (NO_BUFFERING 을 쓰지 않는다).
        // 그래도 섹터 배수로 쓰는 편이 드라이버에 친절해서 유지한다.
        let align = (sector_size as usize).max(4096);
        let bounce = ioctl::AlignedBuf::new(BOUNCE_BYTES, align);

        Ok(WindowsSession {
            handle: Some(handle),
            _locked: std::mem::take(locked),
            total_bytes: observed.size_bytes,
            observed: observed.clone(),
            sector_size,
            disk_number: disk.number,
            bounce,
            prep_notes,
        })
    }

    /// 세션을 만들지 못했을 때의 뒷정리.
    ///
    /// [`Drop for WindowsSession`](WindowsSession) 과 같은 일을 한다 — 잡고 있던
    /// 볼륨 잠금을 풀고, 볼륨 관리자에게 파티션 테이블을 다시 읽으라고 알린다.
    /// 세션이 없으니 그 `Drop` 이 돌지 않아서 여기서 손으로 해야 한다. 이게
    /// 없으면 볼륨 관리자는 지워지기 전의 레이아웃을 그대로 들고 있게 되고,
    /// 사용자는 USB 를 뽑았다 꽂기 전까지 아무것도 맞지 않는 상태를 본다.
    ///
    /// 재인식용 핸들은 **재시도하지 않는 조회용 열기**로 얻는다.
    /// [`ioctl::open_physical`] 은 Win32 5·32 에 최대 15초를 태우는데, 방금
    /// 그것 때문에 실패해 이미 오래 기다린 사용자를 여기서 또 붙잡을 이유가 없다.
    /// `IOCTL_DISK_UPDATE_PROPERTIES` 는 `FILE_ANY_ACCESS` 라 조회용 핸들로도 간다.
    fn recover(disk_number: u32, locked: &[OwnedHandle], prep: &Prep) {
        for h in locked {
            let _ = ioctl::unlock_volume(h);
        }
        if prep.touched() >= Touched::Layout {
            if let Ok(h) = ioctl::open_physical_drive_for_query(disk_number) {
                let _ = ioctl::update_properties(&h);
            }
        }
    }
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
    /// 준비 단계에서 무엇이 됐고 무엇이 안 됐는지.
    ///
    /// 세션에서 나가는 **모든** 오류에 이것이 실린다. 예전에는 쓰기 거부 한
    /// 갈래에만 붙어서, 정작 자주 나는 잠금 실패는 이름 한 단어로 올라갔다.
    prep_notes: String,
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
        self.check_aligned(offset, data.len(), "이미지 쓰기")?;
        if offset + data.len() as u64 > self.total_bytes {
            return Err(self.explain(
                DeviceError::Io {
                    code: 112,
                    message: "장치 끝을 넘어선 쓰기".into(),
                },
                "이미지 쓰기",
                Some(offset),
            ));
        }
        self.write_via_bounce(offset, data.len(), Some(data), "이미지 쓰기")
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
        self.write_via_bounce(start, count as usize, None, "장치 끝 지우기")
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        self.check_aligned(offset, buf.len(), "되읽기 검증")?;

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
            let at = offset + done as u64;
            if let Err(e) = ioctl::read_into(self.hnd(), at, ptr, n) {
                return Err(self.explain(e, "되읽기 검증", Some(at)));
            }
            buf[done..done + n].copy_from_slice(&self.bounce.as_slice()[..n]);
            done += n;
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), DeviceError> {
        // 플러시를 빼먹으면 사용자가 USB 를 뽑는 순간 이미지 뒷부분이 사라진다.
        // 이것만 오류로 올린다 — 나머지는 정리 작업이라 실패해도 이미지는 온전하다.
        //
        // 여기서 실패하면 이미지 전체는 이미 쓰였지만 마지막 부분이 매체에
        // 닿았다는 보장이 없다. 예전에는 이 실패가 `WriteDenied` 한 단어로
        // 올라가서, 화면에는 "장치가 쓰기를 거부했습니다 / 백신을 확인하세요"
        // 가 떴다 — 쓰기는 다 끝난 뒤였는데도.
        let flushed =
            ioctl::flush(self.hnd()).map_err(|e| self.explain(e, "마무리(캐시 플러시)", None));
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

        // **여기서 꺼내지 않는다.** 예전에는 자동으로 꺼냈는데, 실패해도
        // 알 수 없었고 사용자가 제어할 수도 없었다. 꺼내기는 완료 화면의
        // 버튼으로 옮겼다 — 사용자가 누르고 결과를 확인할 수 있어야 한다.
        let _ = disk_number;
    }
}

impl WindowsSession {
    /// 쓰기용 핸들. Drop 이 비우기 전까지는 항상 존재한다.
    fn hnd(&self) -> &OwnedHandle {
        self.handle
            .as_ref()
            .expect("세션이 살아 있는 동안 핸들은 항상 있다")
    }

    /// 세션에서 나가는 모든 오류가 지나는 한 곳.
    ///
    /// 세션이 살아 있다는 것은 준비 단계를 다 지났다는 뜻이고, 곧 대상이 이미
    /// 지워진 뒤라는 뜻이다. 예전에는 준비 기록이 `WriteDenied` 한 갈래에만
    /// 붙었고 나머지 갈래는 변형 이름만 올라갔다.
    fn explain(&self, e: DeviceError, what: &str, at: Option<u64>) -> DeviceError {
        with_write_context(e, what, at, &self.prep_notes)
    }

    /// 오프셋과 길이가 섹터 배수인지 확인한다.
    ///
    /// 여기서 걸러내지 않으면 Windows 가 ERROR_INVALID_PARAMETER(87) 를 내는데,
    /// 그 코드만 보고는 원인이 정렬이라는 것을 알기 어렵다.
    fn check_aligned(&self, offset: u64, len: usize, what: &str) -> Result<(), DeviceError> {
        let ss = self.sector_size as u64;
        if !offset.is_multiple_of(ss) || !(len as u64).is_multiple_of(ss) {
            return Err(self.explain(
                DeviceError::Io {
                    code: 87,
                    message: format!("정렬 오류: 오프셋 {offset}, 길이 {len}, 섹터 {ss}"),
                },
                what,
                Some(offset),
            ));
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
        what: &str,
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
            // Rufus 는 쓰기 실패에 4회까지 재시도하며 사이에 5초를 쉰다.
            // USB 컨트롤러가 잠깐 응답하지 않는 일이 실제로 있어서,
            // 첫 실패에 포기하면 멀쩡한 장치를 불량으로 판정하게 된다.
            let mut result =
                ioctl::write_raw(self.hnd(), offset + done as u64, self.bounce.as_ptr(), n);
            let mut tries = 1;
            while result.is_err() && tries < 4 {
                // 87 은 크기 문제일 수 있으므로 재시도가 아니라 아래에서 분할로 다룬다.
                if matches!(result, Err(DeviceError::Io { code: 87, .. })) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
                result =
                    ioctl::write_raw(self.hnd(), offset + done as u64, self.bounce.as_ptr(), n);
                tries += 1;
            }
            match result {
                Ok(()) => done += n,
                // 일부 드라이버는 한 번에 받는 전송 크기에 상한이 있고, 넘으면
                // ERROR_INVALID_PARAMETER 를 돌려준다. 정렬은 이미 맞은 상태이므로
                // 87 이면 크기 문제로 보고 절반으로 줄인다. 섹터 하나까지 줄여도
                // 실패하면 진짜 오류다.
                Err(DeviceError::Io { code: 87, .. }) if chunk > ss => {
                    chunk = ((chunk / 2) / ss).max(1) * ss;
                }
                // **모든** 실패에 준비 단계 상태와 위치를 함께 올린다.
                //
                // 예전에는 쓰기 거부 한 갈래에만 붙어 있었다. 그런데 실제로 자주
                // 나는 것은 쓰는 도중 윈도우가 볼륨을 다시 마운트해 생기는
                // 잠금 실패이고, 그건 여기 catch-all 로 빠져 이름 한 단어만
                // 올라갔다 — 정작 어느 볼륨을 못 잠갔는지 준비 기록이 알고 있는데도.
                Err(e) => return Err(self.explain(e, what, Some(offset + done as u64))),
            }
        }
        Ok(())
    }
}
