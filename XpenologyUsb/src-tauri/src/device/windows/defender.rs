//! Defender 설정을 **읽기만** 한다.
//!
//! 쓰기 전 점검([`crate::core::preflight`])에 넘길 값을 구한다. 판정은 그쪽이
//! 하고 여기서는 값만 옮긴다. 아무것도 바꾸지 않는다 — 설정을 건드리는 것은
//! 프로세스 밖에 흔적을 남기는 일이라 이 프로그램이 하지 않는다
//! (`docs/DEFENDER_FALSE_POSITIVE.md`).
//!
//! 키 위치는 Microsoft 문서에 적혀 있지 않다. Defender 가 실제로 쓰는 자리로
//! 널리 알려진 값을 읽고, 없거나 읽지 못하면 `None` 으로 둔다. `None` 은 경고를
//! 띄우지 않는 쪽으로 판정된다. PowerShell(`Get-MpPreference`)을 띄우는 방법도
//! 있지만, 서명 없는 관리자 프로세스가 PowerShell 을 실행하는 것은 그 자체로
//! 백신이 의심하는 모양이라 쓰지 않는다.

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};

const POLICY_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows Defender\Windows Defender Exploit Guard\Controlled Folder Access";
const LOCAL_KEY: &str =
    r"SOFTWARE\Microsoft\Windows Defender\Windows Defender Exploit Guard\Controlled Folder Access";
const VALUE: &str = "EnableControlledFolderAccess";

/// 제어된 폴더 액세스 설정값. (그룹 정책, 로컬) 순서다.
pub fn controlled_folder_access() -> (Option<u32>, Option<u32>) {
    (read_dword(POLICY_KEY, VALUE), read_dword(LOCAL_KEY, VALUE))
}

fn wide(s: &str) -> Vec<u16> {
    OsString::from(s).encode_wide().chain(Some(0)).collect()
}

fn read_dword(key: &str, value: &str) -> Option<u32> {
    let key = wide(key);
    let value = wide(value);
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // 두 문자열은 호출 동안 살아 있는 NUL 종료 UTF-16 이고, data/size 는 DWORD
    // 하나를 받기에 맞는 크기다. RRF_RT_REG_DWORD 가 다른 형식의 값을 거부한다.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}
