//! Win32 호출을 감싸는 얇은 계층.
//!
//! 여기 있는 함수들은 unsafe 를 쓰지만 밖으로는 안전한 타입만 내보낸다.
//! 각 unsafe 블록이 왜 안전한지는 호출 지점에 적어 둔다.

use crate::core::model::VolumeInfo;
use crate::device::DeviceError;
use std::collections::HashSet;
use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, MAX_PATH,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, DefineDosDeviceW, DeleteVolumeMountPointW, FindFirstVolumeW, FindNextVolumeW,
    FindVolumeClose, FlushFileBuffers, GetVolumeInformationW, GetVolumePathNamesForVolumeNameW,
    ReadFile, SetFilePointerEx, WriteFile, DDD_REMOVE_DEFINITION, FILE_ATTRIBUTE_NORMAL,
    FILE_BEGIN, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
    OPEN_EXISTING, STORAGE_BUS_TYPE,
};
use windows::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, DISK_GEOMETRY_EX, FSCTL_ALLOW_EXTENDED_DASD_IO,
    FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME, GET_LENGTH_INFORMATION,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_DISK_UPDATE_PROPERTIES,
    IOCTL_STORAGE_EJECT_MEDIA, IOCTL_STORAGE_MEDIA_REMOVAL, IOCTL_STORAGE_QUERY_PROPERTY,
    PREVENT_MEDIA_REMOVAL, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY, VOLUME_DISK_EXTENTS,
};
use windows::Win32::System::IO::DeviceIoControl;

/// 소유권을 갖는 핸들. Drop 에서 반드시 닫는다.
///
/// 이 프로그램은 잠금을 오래 들고 있으므로 핸들 누수가 곧 "USB 를 뽑을 수 없음"
/// 으로 이어진다. 수동 CloseHandle 에 의존하지 않는다.
pub struct OwnedHandle(HANDLE);

// 안전성: HANDLE 은 커널 객체를 가리키는 불투명한 값이고, 커널 핸들은
// 스레드에 묶여 있지 않다. 이 타입이 단독 소유하므로 다른 스레드로
// 넘겨도 경합이 생기지 않는다. 쓰기 작업을 백그라운드 스레드에서 돌리려면
// 이 표시가 필요하다.
unsafe impl Send for OwnedHandle {}

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // 안전성: 이 핸들은 CreateFileW 가 돌려준 것이고 소유권이 여기 있다.
            // Drop 은 한 번만 호출되므로 이중 close 가 없다.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsString::from(s).encode_wide().chain(Some(0)).collect()
}

/// 마지막 Win32 오류를, **어느 작업에서 났는지와 함께** 담는다.
///
/// 앞서 이 함수는 작업 이름 없이 "Win32 오류 87" 만 남겼다. 그 문구로는
/// 열기가 실패한 것인지, 레이아웃 초기화인지, 쓰기인지 알 수 없어서
/// 사용자가 보고해도 원인을 좁힐 수 없었다. 오류가 어디서 났는지 모르면
/// 오류를 보고받는 의미가 없다.
fn last_error_in(op: &str) -> DeviceError {
    // 안전성: GetLastError 는 스레드 로컬 값을 읽기만 한다.
    let code = unsafe { GetLastError() }.0 as i32;
    match code {
        5 => DeviceError::WriteDenied,     // ERROR_ACCESS_DENIED
        32 => DeviceError::Locked,         // ERROR_SHARING_VIOLATION
        1110 => DeviceError::MediaChanged, // ERROR_MEDIA_CHANGED
        _ => DeviceError::Io {
            code,
            message: format!("{op} 실패: Win32 오류 {code}{}", explain(code)),
        },
    }
}

/// 자주 나오는 오류 코드에 짧은 설명을 붙인다.
///
/// 숫자만 있으면 사용자는 검색밖에 할 수 없다.
fn explain(code: i32) -> &'static str {
    match code {
        1 => " (지원되지 않는 요청)",
        6 => " (잘못된 핸들)",
        19 => " (쓰기 금지된 매체)",
        21 => " (장치가 준비되지 않음)",
        87 => " (잘못된 매개변수 — 대개 섹터 정렬이나 구조체 크기 문제)",
        112 => " (공간 부족)",
        433 => " (장치를 찾을 수 없음)",
        1117 => " (입출력 장치 오류)",
        _ => "",
    }
}

/// 조회 전용으로 물리 디스크를 연다.
///
/// **읽기 권한으로 연다.** 권한 0 으로 열면 관리자 권한 없이도 열리기는 하지만,
/// 그 핸들로는 `IOCTL_DISK_GET_LENGTH_INFO` 가 실패한다. 이 제어 코드는
/// `FILE_READ_ACCESS` 를 요구하기 때문이다 (코드 475228 의 access 비트 = 1).
/// 반면 `IOCTL_STORAGE_QUERY_PROPERTY` 는 `FILE_ANY_ACCESS` 라 통과한다.
///
/// 그래서 권한 0 으로 열면 "버스 타입은 읽히는데 용량만 못 읽는" 상태가 되고,
/// 용량 0 은 안전 규칙에서 미디어 없음으로 해석돼 **모든 디스크가 목록에서
/// 사라진다.** 0.1.1 이 USB 를 하나도 찾지 못한 원인이 이것이었다.
///
/// 이 프로그램은 매니페스트로 항상 관리자 권한을 받으므로 읽기로 여는 데 문제가 없다.
/// 그래도 권한을 못 받는 상황을 대비해 0 으로 한 번 더 시도한다. 그 경우 용량은
/// `FILE_ANY_ACCESS` 인 기하 정보에서 얻는다.
pub fn open_physical_drive_for_query(number: u32) -> Result<OwnedHandle, DeviceError> {
    let path = wide(&format!(r"\\.\PhysicalDrive{number}"));
    for access in [GENERIC_R, 0] {
        // 안전성: path 는 널 종료된 UTF-16 이고 호출 동안 살아 있다.
        let r = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        if let Ok(h) = r {
            if h != INVALID_HANDLE_VALUE {
                return Ok(OwnedHandle(h));
            }
        }
    }
    Err(last_error_in("물리 디스크 열기(조회)"))
}

const GENERIC_R: u32 = 0x8000_0000;
const GENERIC_RW: u32 = 0x8000_0000 | 0x4000_0000;

/// 잠금 재시도 예산. Rufus 와 같은 15초.
const LOCK_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
/// 열기 재시도. 150회 × 100ms.
const OPEN_RETRIES: u32 = 150;
const OPEN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
/// 이 횟수를 넘기면 공유 쓰기를 허용해 본다.
const SHARE_WRITE_AFTER: u32 = OPEN_RETRIES / 3;

/// 장치를 연다. Rufus 의 `GetHandle` 과 같은 모양.
///
/// **`FILE_FLAG_NO_BUFFERING` 을 쓰지 않는다.** Rufus 는 DD 쓰기에도
/// `FILE_ATTRIBUTE_NORMAL` 만 쓴다. 캐시 우회는 이 작업에 이득이 없으면서
/// 버퍼 주소 정렬 요구를 만들어내고, 그게 `ERROR_INVALID_PARAMETER(87)` 의
/// 원인이 된다. 우리가 겪은 정렬 문제는 전부 여기서 자초한 것이었다.
///
/// `lock` 이 참이면 잠금이 **열기의 일부**다. 잠금에 실패하면 핸들을 닫고
/// 실패를 돌려준다. 잠기지 않은 물리 핸들로 쓰면 커널이 거부하기 때문에,
/// best-effort 로 넘어가면 그 뒤 쓰기에서 `ERROR_ACCESS_DENIED` 가 난다.
pub fn get_handle(
    path: &str,
    lock: bool,
    write_access: bool,
    op: &str,
) -> Result<OwnedHandle, DeviceError> {
    let wide_path = wide(path);
    let access = if write_access { GENERIC_RW } else { GENERIC_R };
    // Rufus 는 잠글 때 공유 쓰기를 주지 않는다. 둘은 함께 움직인다.
    let mut share_write = !lock;

    let mut last = DeviceError::Locked;
    for attempt in 0..OPEN_RETRIES {
        let share = if share_write {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        } else {
            FILE_SHARE_READ
        };

        // 안전성: wide_path 는 널 종료된 UTF-16 이고 호출 동안 살아 있다.
        let r = unsafe {
            CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                access,
                share,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };

        match r {
            Ok(h) if h != INVALID_HANDLE_VALUE => {
                let handle = OwnedHandle(h);
                if !lock {
                    return Ok(handle);
                }
                // 경계 검사를 끄고 잠근다. 잠금은 열기의 일부다.
                let _ = control(&handle, FSCTL_ALLOW_EXTENDED_DASD_IO, "경계 검사 해제");
                match lock_within(&handle, LOCK_BUDGET) {
                    Ok(()) => return Ok(handle),
                    Err(e) => {
                        // 잠기지 않은 핸들은 쓸모가 없다. 닫고 실패로 돌린다.
                        drop(handle);
                        return Err(e);
                    }
                }
            }
            _ => {
                // 안전성: GetLastError 는 스레드 로컬 값을 읽기만 한다.
                let code = unsafe { GetLastError() }.0 as i32;
                // 기다려서 나아질 수 있는 것에만 재시도한다.
                if code != 5 && code != 32 {
                    return Err(last_error_in(op));
                }
                last = if code == 5 {
                    DeviceError::WriteDenied
                } else {
                    DeviceError::Locked
                };
                if attempt >= SHARE_WRITE_AFTER {
                    share_write = true;
                }
                std::thread::sleep(OPEN_INTERVAL);
            }
        }
    }
    Err(last)
}

/// 예산 안에서 볼륨 잠금을 시도한다. 벽시계 기준이라 재시도 횟수에 의존하지 않는다.
fn lock_within(h: &OwnedHandle, budget: std::time::Duration) -> Result<(), DeviceError> {
    let deadline = std::time::Instant::now() + budget;
    let mut last = DeviceError::Locked;
    loop {
        match control(h, FSCTL_LOCK_VOLUME, "볼륨 잠금") {
            Ok(()) => return Ok(()),
            Err(e @ (DeviceError::Locked | DeviceError::WriteDenied)) => last = e,
            Err(DeviceError::Io { code: 32, .. }) => {}
            // 기다려도 달라지지 않는 오류는 즉시 포기한다.
            Err(e) => return Err(e),
        }
        if std::time::Instant::now() >= deadline {
            return Err(last);
        }
        std::thread::sleep(OPEN_INTERVAL);
    }
}

/// 물리 디스크를 연다.
pub fn open_physical(number: u32, lock: bool, write: bool) -> Result<OwnedHandle, DeviceError> {
    get_handle(
        &format!(r"\\.\PhysicalDrive{number}"),
        lock,
        write,
        if write {
            "물리 디스크 열기(쓰기)"
        } else {
            "물리 디스크 열기(읽기)"
        },
    )
}

/// 볼륨을 연다. 경로 끝의 역슬래시는 반드시 빼야 한다 —
/// 붙이면 장치가 아니라 파일 시스템 루트가 열린다.
///
/// **읽기 전용으로 연다.** Rufus 도 그렇게 한다 — 볼륨에 직접 쓰지 않고
/// 잠그기만 할 것이므로 쓰기 권한이 필요 없고, 요구하지 않는 편이 잠길 확률이 높다.
pub fn open_volume(guid_path_no_trailing: &str, lock: bool) -> Result<OwnedHandle, DeviceError> {
    get_handle(guid_path_no_trailing, lock, false, "볼륨 열기")
}

/// 장치 서술자에서 뽑아낸 값들.
pub struct DeviceDescriptor {
    pub bus_type: u16,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
}

impl DeviceDescriptor {
    /// 목록에 표시할 이름.
    ///
    /// 제조사와 제품명이 따로 오므로 합친다. 둘 다 없으면 빈 문자열 대신
    /// 최소한의 표시를 만든다 — 이름이 비면 사용자가 장치를 구분할 수 없다.
    pub fn friendly_name(&self) -> String {
        let v = self.vendor.as_deref().unwrap_or("").trim().to_string();
        let p = self.product.as_deref().unwrap_or("").trim().to_string();
        match (v.is_empty(), p.is_empty()) {
            (false, false) => format!("{v} {p}"),
            (true, false) => p,
            (false, true) => v,
            (true, true) => "USB 저장장치".to_string(),
        }
    }
}

/// 서술자 안의 오프셋 기반 문자열을 꺼낸다.
///
/// `STORAGE_DEVICE_DESCRIPTOR` 는 문자열을 구조체 뒤쪽 버퍼에 두고 오프셋으로
/// 가리킨다. 오프셋 0 은 "없음"을 뜻한다.
fn descriptor_string(buf: &[u8], offset: u32) -> Option<String> {
    if offset == 0 || offset as usize >= buf.len() {
        return None;
    }
    let start = offset as usize;
    let end = buf[start..].iter().position(|b| *b == 0)? + start;
    let s = String::from_utf8_lossy(&buf[start..end]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 장치 서술자를 조회한다. 버스 타입이 여기서 나온다.
pub fn query_device_descriptor(h: &OwnedHandle) -> Result<DeviceDescriptor, DeviceError> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0; 1],
    };
    // 서술자 뒤에 문자열이 붙으므로 넉넉히 잡는다.
    let mut buf = vec![0u8; 1024];
    let mut returned = 0u32;

    // 안전성: query 와 buf 는 호출 동안 살아 있고, 크기를 정확히 넘긴다.
    // buf 는 STORAGE_DEVICE_DESCRIPTOR 보다 크므로 넘침이 없다.
    let ok = unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr() as *mut _),
            buf.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    ok.map_err(|_| last_error_in("장치 정보 조회"))?;

    if (returned as usize) < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return Err(DeviceError::Io {
            code: 0,
            message: "장치 서술자가 너무 짧습니다".into(),
        });
    }

    // 안전성: 위에서 크기를 확인했으므로 구조체로 읽어도 된다.
    let desc = unsafe { &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };

    Ok(DeviceDescriptor {
        bus_type: STORAGE_BUS_TYPE(desc.BusType.0).0 as u16,
        vendor: descriptor_string(&buf, desc.VendorIdOffset),
        product: descriptor_string(&buf, desc.ProductIdOffset),
        serial: descriptor_string(&buf, desc.SerialNumberOffset),
    })
}

/// 커널이 보고하는 이 핸들의 디스크 번호.
///
/// 신원 확인이 의미를 가지려면 번호를 **장치에서 읽어야** 한다. 예전에는
/// 사용자가 고른 번호를 그대로 복사해 넣고 비교해서, 번호 비교가 항상
/// 참인 동어반복이었다. 디스크 번호는 재사용되므로 이 확인이 핵심이다.
pub fn query_device_number(h: &OwnedHandle) -> Result<u32, DeviceError> {
    // IOCTL_STORAGE_GET_DEVICE_NUMBER, FILE_ANY_ACCESS 라 권한 0 핸들에서도 된다.
    const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
    #[repr(C)]
    #[derive(Default)]
    struct StorageDeviceNumber {
        device_type: u32,
        device_number: u32,
        partition_number: i32,
    }
    let mut info = StorageDeviceNumber::default();
    let mut returned = 0u32;
    // 안전성: info 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut info as *mut _ as *mut _),
            std::mem::size_of::<StorageDeviceNumber>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|_| last_error_in("디스크 번호 조회"))?;
    Ok(info.device_number)
}

/// 장치의 정확한 바이트 크기.
///
/// 두 경로를 시도한다. `IOCTL_DISK_GET_LENGTH_INFO` 가 더 정확하지만
/// `FILE_READ_ACCESS` 를 요구해서 권한 없이 연 핸들에서는 실패한다.
/// 그때는 `FILE_ANY_ACCESS` 인 기하 정보의 `DiskSize` 로 대신한다.
///
/// 둘 다 실패하면 **오류를 반환한다.** 예전에는 실패를 0 으로 바꿨는데,
/// 0 은 안전 규칙에서 "미디어 없음"을 뜻해서 장치가 조용히 목록에서 사라졌다.
/// 알 수 없는 값을 그럴듯한 값으로 바꾸지 않는다.
pub fn query_length(h: &OwnedHandle) -> Result<u64, DeviceError> {
    let mut info = GET_LENGTH_INFORMATION::default();
    let mut returned = 0u32;
    // 안전성: info 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    let ok = unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_DISK_GET_LENGTH_INFO,
            None,
            0,
            Some(&mut info as *mut _ as *mut _),
            std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
            Some(&mut returned),
            None,
        )
    };
    if ok.is_ok() && info.Length > 0 {
        return Ok(info.Length as u64);
    }

    // 폴백: 기하 정보. 권한이 없어도 읽힌다.
    let mut geo = DISK_GEOMETRY_EX::default();
    let mut returned2 = 0u32;
    // 안전성: geo 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            None,
            0,
            Some(&mut geo as *mut _ as *mut _),
            std::mem::size_of::<DISK_GEOMETRY_EX>() as u32,
            Some(&mut returned2),
            None,
        )
    }
    .map_err(|_| last_error_in("장치 용량 조회"))?;

    if geo.DiskSize <= 0 {
        return Err(DeviceError::Io {
            code: 0,
            message: "장치 용량을 알 수 없습니다".into(),
        });
    }
    Ok(geo.DiskSize as u64)
}

/// 논리 섹터 크기.
///
/// 512 를 가정하지 않는다. 4Kn 장치가 존재하고, 가정이 틀리면 모든 쓰기가
/// `ERROR_INVALID_PARAMETER` 로 실패한다.
pub fn query_sector_size(h: &OwnedHandle) -> Result<u32, DeviceError> {
    let mut geo = DISK_GEOMETRY_EX::default();
    let mut returned = 0u32;
    // 안전성: geo 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            None,
            0,
            Some(&mut geo as *mut _ as *mut _),
            std::mem::size_of::<DISK_GEOMETRY_EX>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|_| last_error_in("섹터 크기 조회"))?;

    let ss = geo.Geometry.BytesPerSector;
    if ss < 512 || !ss.is_power_of_two() {
        return Err(DeviceError::BadSectorSize(ss));
    }
    Ok(ss)
}

/// 이 볼륨이 올라가 있는 디스크 번호들.
///
/// 볼륨 하나가 여러 디스크에 걸칠 수 있다 (미러링, 저장소 공간).
/// 첫 extent 만 보면 그런 구성에서 보호가 새어나간다.
pub fn volume_disk_numbers(h: &OwnedHandle) -> Result<Vec<(u32, u32)>, DeviceError> {
    // extent 여러 개를 담을 수 있게 넉넉히 잡는다.
    let mut buf = vec![0u8; 4096];
    let mut returned = 0u32;
    // 안전성: buf 는 호출 동안 살아 있고 크기를 정확히 넘긴다.
    unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            None,
            0,
            Some(buf.as_mut_ptr() as *mut _),
            buf.len() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|_| last_error_in("볼륨-디스크 매핑 조회"))?;

    // 안전성: 위 호출이 성공했으므로 버퍼 앞부분은 유효한 구조체다.
    let ext = unsafe { &*(buf.as_ptr() as *const VOLUME_DISK_EXTENTS) };
    let count = ext.NumberOfDiskExtents as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        // 안전성: NumberOfDiskExtents 가 알려준 개수만큼만 읽고,
        // 버퍼 크기를 넘지 않는지 확인한다.
        let off = std::mem::offset_of!(VOLUME_DISK_EXTENTS, Extents)
            + i * std::mem::size_of::<windows::Win32::System::Ioctl::DISK_EXTENT>();
        if off + std::mem::size_of::<windows::Win32::System::Ioctl::DISK_EXTENT>() > buf.len() {
            break;
        }
        let e = unsafe {
            &*(buf.as_ptr().add(off) as *const windows::Win32::System::Ioctl::DISK_EXTENT)
        };
        out.push((e.DiskNumber, count as u32));
    }
    Ok(out)
}

/// 시스템의 모든 볼륨을 훑어 (디스크 번호, 볼륨 정보) 쌍을 만든다.
///
/// 드라이브 문자에 의존하지 않는다. 문자 없는 볼륨(ESP, MSR, 리눅스 파티션)도
/// 마운트돼 있으면 raw 쓰기를 막기 때문에 반드시 포함해야 한다.
pub fn enumerate_volumes() -> Vec<(u32, VolumeInfo)> {
    let mut out = Vec::new();
    let mut name = [0u16; MAX_PATH as usize + 1];

    // 안전성: name 은 충분히 크고, 반환 핸들은 아래에서 반드시 닫는다.
    let find = match unsafe { FindFirstVolumeW(&mut name) } {
        Ok(h) => h,
        Err(_) => return out,
    };

    loop {
        let guid = String::from_utf16_lossy(
            &name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())],
        );
        // 장치를 열려면 끝의 역슬래시를 떼야 한다.
        let device_path = guid.trim_end_matches('\\').to_string();

        if let Ok(h) = open_volume_for_query(&device_path) {
            if let Ok(extents) = volume_disk_numbers(&h) {
                let (letter, fs, size) = volume_details(&guid);
                for (disk_no, extent_count) in extents {
                    out.push((
                        disk_no,
                        VolumeInfo {
                            guid_path: guid.clone(),
                            drive_letter: letter,
                            file_system: fs.clone(),
                            size_bytes: size,
                            disk_extent_count: extent_count,
                        },
                    ));
                }
            }
        }

        name = [0u16; MAX_PATH as usize + 1];
        // 안전성: find 는 유효한 검색 핸들이고 name 은 충분히 크다.
        if unsafe { FindNextVolumeW(find, &mut name) }.is_err() {
            break;
        }
    }

    // 안전성: find 는 FindFirstVolumeW 가 돌려준 유효한 핸들이다.
    unsafe {
        let _ = FindVolumeClose(find);
    }
    out
}

/// 조회 전용 볼륨 핸들. 권한 상승 없이 열린다.
fn open_volume_for_query(device_path: &str) -> Result<OwnedHandle, DeviceError> {
    let path = wide(device_path);
    // 안전성: path 는 널 종료 UTF-16 이고 호출 동안 살아 있다.
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|_| last_error_in("볼륨 열기(조회)"))?;
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error_in("볼륨 열기(조회)"));
    }
    Ok(OwnedHandle(h))
}

/// 드라이브 문자, 파일 시스템 이름, 용량.
///
/// 실패는 치명적이지 않다 — 표시용 정보이므로 없으면 없는 대로 진행한다.
fn volume_details(guid_with_slash: &str) -> (Option<char>, Option<String>, u64) {
    let path = wide(guid_with_slash);
    let mut names = vec![0u16; 512];
    let mut len = 0u32;
    // 안전성: 두 버퍼 모두 호출 동안 살아 있고 크기를 정확히 넘긴다.
    let letter = unsafe {
        GetVolumePathNamesForVolumeNameW(PCWSTR(path.as_ptr()), Some(&mut names), &mut len)
    }
    .ok()
    .and_then(|_| {
        let s = String::from_utf16_lossy(&names[..len.min(names.len() as u32) as usize]);
        s.chars().next().filter(|c| c.is_ascii_alphabetic())
    });

    let mut fs_name = [0u16; 32];
    // 안전성: 버퍼가 충분하고 호출 동안 살아 있다.
    let fs = unsafe {
        GetVolumeInformationW(
            PCWSTR(path.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut fs_name),
        )
    }
    .ok()
    .map(|_| {
        String::from_utf16_lossy(&fs_name[..fs_name.iter().position(|c| *c == 0).unwrap_or(0)])
    })
    .filter(|s| !s.is_empty());

    (letter, fs, 0)
}

/// 절대 건드리면 안 되는 디스크 번호들.
///
/// WMI 플래그 대신 쓰는 방어선이다. 시스템 드라이브, 윈도우 폴더, 실행 중인
/// 프로그램이 놓인 위치를 커널에 물어 디스크 번호로 환산한다.
///
/// 볼륨이 여러 extent 에 걸칠 수 있으므로 **모든 extent 를 합집합으로** 모은다.
/// 미러링된 C: 나 저장소 공간에서 첫 extent 만 보면 보호가 새어나간다.
pub fn protected_disk_numbers() -> HashSet<u32> {
    let mut out = HashSet::new();

    let mut roots: Vec<String> = Vec::new();
    if let Ok(v) = std::env::var("SystemDrive") {
        roots.push(format!(r"\\.\{}", v.trim_end_matches('\\')));
    }
    if let Ok(v) = std::env::var("windir") {
        if let Some(d) = drive_prefix(Path::new(&v)) {
            roots.push(d);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = drive_prefix(&exe) {
            roots.push(d);
        }
    }
    // 임시 폴더도 포함한다. 내려받은 이미지가 여기 놓이므로,
    // 대상 디스크와 같으면 자기 자신을 덮어쓰게 된다.
    if let Some(d) = drive_prefix(&std::env::temp_dir()) {
        roots.push(d);
    }

    for r in roots {
        if let Ok(h) = open_volume_for_query(&r) {
            if let Ok(extents) = volume_disk_numbers(&h) {
                for (n, _) in extents {
                    out.insert(n);
                }
            }
        }
    }

    // 아무것도 못 알아냈다면 최소한 디스크 0 은 지킨다.
    // 판정 실패가 보호 해제로 이어지지 않게 한다.
    if out.is_empty() {
        out.insert(0);
    }
    out
}

/// 경로에서 `\\.\C:` 형태의 볼륨 장치 경로를 만든다.
fn drive_prefix(p: &Path) -> Option<String> {
    let s = p.to_string_lossy();
    let mut it = s.chars();
    let c = it.next()?;
    if !c.is_ascii_alphabetic() || it.next() != Some(':') {
        return None;
    }
    Some(format!(r"\\.\{}:", c.to_ascii_uppercase()))
}

// ---------------------------------------------------------------------------
// 쓰기 경로
// ---------------------------------------------------------------------------

/// 섹터 정렬된 버퍼.
///
/// `FILE_FLAG_NO_BUFFERING` 으로 연 핸들은 버퍼 주소가 섹터 경계에 맞기를
/// 요구한다. 문서상 "강제되지 않을 수 있다" 지만 지키지 않을 이유가 없다 —
/// 어긋났을 때 나는 오류가 원인을 짐작하기 어려운 종류다.
///
/// Rust 의 기본 할당은 정렬을 보장하지 않으므로 직접 할당한다.
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    layout: std::alloc::Layout,
}

// 안전성: 내부 포인터는 이 타입이 단독 소유하며 다른 스레드와 공유되지 않는다.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize, align: usize) -> Self {
        let align = align.max(std::mem::align_of::<u8>()).next_power_of_two();
        let layout = std::alloc::Layout::from_size_align(len, align).expect("잘못된 버퍼 레이아웃");
        // 안전성: len > 0 이고 layout 이 유효하다. 실패하면 아래에서 중단한다.
        // alloc_zeroed 를 쓴다. as_mut_slice 가 초기화되지 않은 메모리에
        // &mut [u8] 을 내주지 않게 하기 위해서다.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Self { ptr, len, layout }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // 안전성: ptr 은 len 바이트의 유효한 할당이고 이 타입이 소유한다.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub fn as_slice(&self) -> &[u8] {
        // 안전성: ptr 은 len 바이트의 유효한 할당이고 이 타입이 소유한다.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // 안전성: ptr 과 layout 은 alloc 에 넘긴 것과 같은 쌍이다.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) }
    }
}

/// 파일 포인터를 옮긴다.
fn seek(h: &OwnedHandle, offset: u64) -> Result<(), DeviceError> {
    let mut new = 0i64;
    // 안전성: new 는 이 스코프에 살아 있다.
    unsafe { SetFilePointerEx(h.raw(), offset as i64, Some(&mut new), FILE_BEGIN) }
        .map_err(|_| last_error_in("파일 위치 이동"))
}

/// 지정 위치에 쓴다.
///
/// `WriteFile` 은 성공을 반환하면서도 요청보다 적게 쓸 수 있다.
/// 그 경우를 성공으로 취급하면 이미지에 구멍이 생긴다.
pub fn write_raw(
    h: &OwnedHandle,
    offset: u64,
    ptr: *const u8,
    len: usize,
) -> Result<(), DeviceError> {
    seek(h, offset)?;
    let mut written = 0u32;
    // 안전성: ptr 은 len 바이트의 유효한 읽기 가능 메모리이고 호출 동안 살아 있다.
    unsafe {
        WriteFile(
            h.raw(),
            Some(std::slice::from_raw_parts(ptr, len)),
            Some(&mut written),
            None,
        )
    }
    .map_err(|_| last_error_in("장치 쓰기"))?;

    if written as usize != len {
        return Err(DeviceError::Io {
            code: 0,
            message: format!("짧은 쓰기: {written} / {len} 바이트"),
        });
    }
    Ok(())
}

/// 정렬된 버퍼로 직접 읽는다.
///
/// # Safety
/// `ptr` 은 최소 `len` 바이트를 담을 수 있는 유효한 쓰기 가능 메모리여야 한다.
pub fn read_into(
    h: &OwnedHandle,
    offset: u64,
    ptr: *mut u8,
    len: usize,
) -> Result<(), DeviceError> {
    seek(h, offset)?;
    let mut read = 0u32;
    // 안전성: 호출부가 ptr 이 len 바이트를 담을 수 있음을 보장한다.
    let slice = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
    // 안전성: slice 는 호출 동안 살아 있다.
    unsafe { ReadFile(h.raw(), Some(slice), Some(&mut read), None) }
        .map_err(|_| last_error_in("장치 읽기"))?;
    if read as usize != len {
        return Err(DeviceError::Io {
            code: 0,
            message: format!("짧은 읽기: {read} / {len} 바이트"),
        });
    }
    Ok(())
}

/// 지정 위치에서 읽는다.
#[allow(dead_code)]
pub fn read_at(h: &OwnedHandle, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
    seek(h, offset)?;
    let mut read = 0u32;
    // 안전성: buf 는 호출 동안 살아 있고 크기를 정확히 넘긴다.
    unsafe { ReadFile(h.raw(), Some(buf), Some(&mut read), None) }
        .map_err(|_| last_error_in("장치 읽기"))?;
    if read as usize != buf.len() {
        return Err(DeviceError::Io {
            code: 0,
            message: format!("짧은 읽기: {read} / {} 바이트", buf.len()),
        });
    }
    Ok(())
}

/// 캐시를 장치까지 내린다.
pub fn flush(h: &OwnedHandle) -> Result<(), DeviceError> {
    // 안전성: 유효한 핸들이다.
    unsafe { FlushFileBuffers(h.raw()) }.map_err(|_| last_error_in("캐시 플러시"))
}

// ---------------------------------------------------------------------------
// 잠금과 레이아웃
// ---------------------------------------------------------------------------

/// 인자 없는 제어 코드를 보낸다.
///
/// **오류 코드를 보존한다.** 예전에는 `bool` 을 돌려줬는데, 그 한 줄이 하류
/// 전체를 오염시켰다. 잠금 재시도는 "다른 프로그램이 쓰는 중"과 "이 장치가 그
/// IOCTL 을 지원하지 않음"을 구분하지 못해 후자에도 15초를 다 태웠고,
/// 마운트 해제나 속성 갱신 실패는 흔적조차 남지 않았다.
fn control(h: &OwnedHandle, code: u32, op: &str) -> Result<(), DeviceError> {
    let mut returned = 0u32;
    // 안전성: 입출력 버퍼가 없는 제어 코드다.
    unsafe { DeviceIoControl(h.raw(), code, None, 0, None, 0, Some(&mut returned), None) }
        .map_err(|_| last_error_in(op))
}

/// 이 장치가 쓰기 금지 상태인가.
///
/// `STORAGE_DEVICE_DESCRIPTOR` 에는 쓰기 금지 여부가 없어서, 예전에는
/// `read_only: false` 라고 **적어 넣었다.** 조회한 것처럼 보이지만 리터럴이었고,
/// 그래서 쓰기 금지 판정이 실제로는 한 번도 동작하지 않았다.
///
/// `IOCTL_DISK_IS_WRITABLE` 은 `FILE_ANY_ACCESS` 라 권한 0 핸들에서도 물을 수 있다.
/// 성공하면 쓸 수 있고, `ERROR_WRITE_PROTECT(19)` 면 쓰기 금지다.
/// **그 외의 오류는 "모른다"로 두고 쓸 수 있는 쪽으로 간다** — 판정 실패를
/// 금지로 바꾸면 모든 디스크를 감춰버렸던 실수를 반대 방향으로 되풀이하게 된다.
pub fn is_write_protected(h: &OwnedHandle) -> bool {
    const IOCTL_DISK_IS_WRITABLE: u32 = 0x0007_0024;
    match control(h, IOCTL_DISK_IS_WRITABLE, "쓰기 가능 여부 조회") {
        Ok(()) => false,
        Err(DeviceError::Io { code: 19, .. }) => true,
        Err(_) => false,
    }
}

/// 볼륨을 잠근다. 열린 파일이 있으면 실패하므로 재시도한다.
///
pub fn dismount_volume(h: &OwnedHandle) -> Result<(), DeviceError> {
    control(h, FSCTL_DISMOUNT_VOLUME, "볼륨 마운트 해제")
}

pub fn unlock_volume(h: &OwnedHandle) -> Result<(), DeviceError> {
    control(h, FSCTL_UNLOCK_VOLUME, "볼륨 잠금 해제")
}

/// 파티션 테이블을 재인식시킨다.
pub fn update_properties(h: &OwnedHandle) -> Result<(), DeviceError> {
    control(h, IOCTL_DISK_UPDATE_PROPERTIES, "파티션 테이블 재인식")
}

pub fn allow_media_removal(h: &OwnedHandle) -> bool {
    let prevent = PREVENT_MEDIA_REMOVAL {
        PreventMediaRemoval: false,
    };
    let mut returned = 0u32;
    // 안전성: prevent 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_STORAGE_MEDIA_REMOVAL,
            Some(&prevent as *const _ as *const _),
            std::mem::size_of::<PREVENT_MEDIA_REMOVAL>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .is_ok()
    }
}

pub fn eject_media(h: &OwnedHandle) -> Result<(), DeviceError> {
    control(h, IOCTL_STORAGE_EJECT_MEDIA, "미디어 꺼내기")
}

/// 파티션 테이블을 지운다.
///
/// `IOCTL_DISK_CREATE_DISK` 를 쓰지 않는다. Rufus 는 DD 경로에서
/// `InitializeDisk`(= CREATE_DISK)를 **건너뛴다** —
///
/// ```c
/// if ((boot_type != BT_IMAGE) || (img_report.is_iso && !write_as_image)) {
///     if ((!ClearMBRGPT(...)) || (!InitializeDisk(hPhysicalDrive))) {
/// ```
///
/// DD 모드에서는 이 조건이 거짓이라 실행되지 않는다. 우리는 Rufus 가 의도적으로
/// 피하는 경로를 쓰고 있었다.
///
/// 목적은 "쓰려는 섹터가 마운트된 볼륨에 속하지 않게" 만드는 것뿐이고,
/// 그 뒤 이미지가 장치 앞부분을 통째로 덮으므로 레이아웃만 지우면 된다.
pub fn delete_drive_layout(h: &OwnedHandle) -> Result<(), DeviceError> {
    // IOCTL_DISK_DELETE_DRIVE_LAYOUT
    const CODE: u32 = 0x0007_C100;
    control(h, CODE, "파티션 테이블 삭제")
}

/// 드라이브 문자를 뗀다.
///
/// 문자가 붙어 있으면 Windows 가 볼륨을 계속 다시 마운트한다.
/// 실패는 무시한다 — 문자가 이미 없을 수 있다.
pub fn remove_mount_point(letter: char) {
    let dos = wide(&format!("{}:", letter.to_ascii_uppercase()));
    // 안전성: dos 는 널 종료 UTF-16 이고 호출 동안 살아 있다.
    unsafe {
        let _ = DefineDosDeviceW(DDD_REMOVE_DEFINITION, PCWSTR(dos.as_ptr()), PCWSTR::null());
    }
    let mount = wide(&format!(r"{}:\", letter.to_ascii_uppercase()));
    // 안전성: 위와 동일.
    unsafe {
        let _ = DeleteVolumeMountPointW(PCWSTR(mount.as_ptr()));
    }
}
