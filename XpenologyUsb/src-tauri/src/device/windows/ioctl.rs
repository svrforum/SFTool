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
    FILE_BEGIN, FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
    OPEN_EXISTING, STORAGE_BUS_TYPE,
};
use windows::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, CREATE_DISK, DISK_GEOMETRY_EX,
    FSCTL_ALLOW_EXTENDED_DASD_IO, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME,
    GET_LENGTH_INFORMATION, IOCTL_DISK_CREATE_DISK, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
    IOCTL_DISK_GET_LENGTH_INFO, IOCTL_DISK_UPDATE_PROPERTIES, IOCTL_STORAGE_EJECT_MEDIA,
    IOCTL_STORAGE_MEDIA_REMOVAL, IOCTL_STORAGE_QUERY_PROPERTY, PARTITION_STYLE_RAW,
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

fn last_error() -> DeviceError {
    // 안전성: GetLastError 는 스레드 로컬 값을 읽기만 한다.
    let code = unsafe { GetLastError() }.0 as i32;
    match code {
        5 => DeviceError::WriteDenied,     // ERROR_ACCESS_DENIED
        32 => DeviceError::Locked,         // ERROR_SHARING_VIOLATION
        1110 => DeviceError::MediaChanged, // ERROR_MEDIA_CHANGED
        _ => DeviceError::Io {
            code,
            message: format!("Win32 오류 {code}"),
        },
    }
}

/// 조회 전용으로 물리 디스크를 연다.
///
/// 접근 권한 0 으로 열면 관리자 권한 없이도 성공한다. 목록을 보여주는 데
/// 권한 상승을 요구하지 않기 위해 이 형태를 쓴다.
pub fn open_physical_drive_for_query(number: u32) -> Result<OwnedHandle, DeviceError> {
    let path = wide(&format!(r"\\.\PhysicalDrive{number}"));
    // 안전성: path 는 널 종료된 UTF-16 이고 호출 동안 살아 있다.
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
    .map_err(|_| last_error())?;
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    Ok(OwnedHandle(h))
}

/// 읽기/쓰기용으로 물리 디스크를 연다. 관리자 권한이 필요하다.
///
/// `FILE_FLAG_NO_BUFFERING` 을 주는 이유는 캐시를 우회해 섹터 단위로 직접
/// 쓰기 위해서다. 이 플래그가 정렬 규칙을 강제하는 근원이기도 하다.
pub fn open_physical_drive_for_write(
    number: u32,
    share_write: bool,
) -> Result<OwnedHandle, DeviceError> {
    let path = wide(&format!(r"\\.\PhysicalDrive{number}"));
    let share = if share_write {
        FILE_SHARE_READ | FILE_SHARE_WRITE
    } else {
        FILE_SHARE_READ
    };
    // 안전성: 위와 동일.
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
            share,
            None,
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH,
            None,
        )
    }
    .map_err(|_| last_error())?;
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    Ok(OwnedHandle(h))
}

/// 볼륨을 연다. 경로 끝의 역슬래시는 반드시 빼야 한다 —
/// 붙이면 장치가 아니라 파일 시스템 루트가 열린다.
pub fn open_volume(guid_path_no_trailing: &str) -> Result<OwnedHandle, DeviceError> {
    let path = wide(guid_path_no_trailing);
    // 안전성: 위와 동일.
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
            // 볼륨을 열 때 FILE_SHARE_WRITE 는 문서상 필수다.
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|_| last_error())?;
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    Ok(OwnedHandle(h))
}

/// 장치 서술자에서 뽑아낸 값들.
pub struct DeviceDescriptor {
    pub bus_type: u16,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub read_only: bool,
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
    ok.map_err(|_| last_error())?;

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
        read_only: false,
    })
}

/// 장치의 정확한 바이트 크기.
pub fn query_length(h: &OwnedHandle) -> Result<u64, DeviceError> {
    let mut info = GET_LENGTH_INFORMATION::default();
    let mut returned = 0u32;
    // 안전성: info 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    unsafe {
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
    }
    .map_err(|_| last_error())?;
    Ok(info.Length as u64)
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
    .map_err(|_| last_error())?;

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
    .map_err(|_| last_error())?;

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
    .map_err(|_| last_error())?;
    if h == INVALID_HANDLE_VALUE {
        return Err(last_error());
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
        let ptr = unsafe { std::alloc::alloc(layout) };
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
        .map_err(|_| last_error())
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
    .map_err(|_| last_error())?;

    if written as usize != len {
        return Err(DeviceError::Io {
            code: 0,
            message: format!("짧은 쓰기: {written} / {len} 바이트"),
        });
    }
    Ok(())
}

/// 슬라이스를 그대로 쓴다. 정렬은 호출부가 보장한다.
pub fn write_at(h: &OwnedHandle, offset: u64, data: &[u8]) -> Result<(), DeviceError> {
    write_raw(h, offset, data.as_ptr(), data.len())
}

/// 지정 위치에서 읽는다.
pub fn read_at(h: &OwnedHandle, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
    seek(h, offset)?;
    let mut read = 0u32;
    // 안전성: buf 는 호출 동안 살아 있고 크기를 정확히 넘긴다.
    unsafe { ReadFile(h.raw(), Some(buf), Some(&mut read), None) }.map_err(|_| last_error())?;
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
    unsafe { FlushFileBuffers(h.raw()) }.map_err(|_| last_error())
}

// ---------------------------------------------------------------------------
// 잠금과 레이아웃
// ---------------------------------------------------------------------------

/// 인자 없는 제어 코드를 보낸다. 실패해도 무시하는 용도.
fn control(h: &OwnedHandle, code: u32) -> bool {
    let mut returned = 0u32;
    // 안전성: 입출력 버퍼가 없는 제어 코드다.
    unsafe { DeviceIoControl(h.raw(), code, None, 0, None, 0, Some(&mut returned), None).is_ok() }
}

/// 경계 검사를 끈다. 볼륨 끝 섹터에 접근하려면 필요하다.
pub fn allow_extended_dasd_io(h: &OwnedHandle) -> bool {
    control(h, FSCTL_ALLOW_EXTENDED_DASD_IO)
}

/// 볼륨을 잠근다. 열린 파일이 있으면 실패하므로 재시도한다.
pub fn lock_volume_with_retry(
    h: &OwnedHandle,
    retries: u32,
    interval: std::time::Duration,
) -> Result<(), DeviceError> {
    for _ in 0..retries {
        if control(h, FSCTL_LOCK_VOLUME) {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
    Err(DeviceError::Locked)
}

pub fn dismount_volume(h: &OwnedHandle) -> bool {
    control(h, FSCTL_DISMOUNT_VOLUME)
}

pub fn unlock_volume(h: &OwnedHandle) -> bool {
    control(h, FSCTL_UNLOCK_VOLUME)
}

/// 파티션 테이블을 재인식시킨다.
pub fn update_properties(h: &OwnedHandle) -> bool {
    control(h, IOCTL_DISK_UPDATE_PROPERTIES)
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

pub fn eject_media(h: &OwnedHandle) -> bool {
    control(h, IOCTL_STORAGE_EJECT_MEDIA)
}

/// 디스크를 RAW 로 초기화한다.
///
/// `IOCTL_DISK_DELETE_DRIVE_LAYOUT` 을 쓰지 않는 이유는 그것이 MBR 전용이라
/// GPT 백업 헤더를 남기기 때문이다.
pub fn create_disk_raw(h: &OwnedHandle) -> Result<(), DeviceError> {
    let cd = CREATE_DISK {
        PartitionStyle: PARTITION_STYLE_RAW,
        ..Default::default()
    };
    let mut returned = 0u32;
    // 안전성: cd 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    unsafe {
        DeviceIoControl(
            h.raw(),
            IOCTL_DISK_CREATE_DISK,
            Some(&cd as *const _ as *const _),
            std::mem::size_of::<CREATE_DISK>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|_| last_error())
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
