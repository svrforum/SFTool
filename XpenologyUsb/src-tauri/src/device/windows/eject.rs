//! USB 장치 안전 제거.
//!
//! ## `IOCTL_STORAGE_EJECT_MEDIA` 를 쓰지 않는 이유
//!
//! 그것은 **미디어**가 분리되는 장치용이다 — CD 트레이를 열거나 카드리더에서
//! 카드를 빼는 것. USB 메모리는 미디어와 장치가 하나라서 그 제어 코드는
//! 대개 아무 일도 하지 않고 성공을 보고하거나 조용히 실패한다.
//!
//! 실제로 그랬다. 앞서 작업이 끝나면 자동으로 이 IOCTL 을 보냈는데, 결과를
//! `let _ =` 로 버려서 실패를 알 수 없었다. 사용자가 "자동으로 꺼내기하면
//! 1단계에서 USB 도 안 보여야 하는 것 아니냐" 고 지적해서 드러났다 —
//! 꺼내졌다면 목록에서 사라져야 하는데 그대로 있었다.
//!
//! 윈도우 작업 표시줄의 "하드웨어 안전하게 제거"가 쓰는 것은
//! `CM_Request_Device_Eject` 다. 디스크가 아니라 **그 부모인 USB 장치**를
//! 대상으로 해야 한다. 디스크 자체를 꺼내려 하면 거부된다.

use crate::device::DeviceError;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Parent, CM_Request_Device_EjectW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, CONFIGRET,
    CR_SUCCESS, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, PNP_VETO_TYPE, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
};
use windows::Win32::Foundation::{HANDLE, MAX_PATH};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::IOCTL_STORAGE_GET_DEVICE_NUMBER;
use windows::Win32::System::IO::DeviceIoControl;

/// `GUID_DEVINTERFACE_DISK`
const GUID_DEVINTERFACE_DISK: windows::core::GUID =
    windows::core::GUID::from_u128(0x53f56307_b6bf_11d0_94f2_00a0c91efb8b);

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: i32,
}

/// 지정한 디스크 번호의 USB 장치를 안전하게 제거한다.
///
/// 디스크 인터페이스를 훑어 해당 번호의 장치를 찾고, 그 **부모** 장치 노드에
/// 제거를 요청한다. 부모가 USB 장치이고, 그것을 꺼내야 윈도우가 장치를 놓아준다.
pub fn request_eject(disk_number: u32) -> Result<(), DeviceError> {
    let devinst = find_parent_devinst(disk_number)?;

    let mut veto_type = PNP_VETO_TYPE::default();
    let mut veto_name = [0u16; MAX_PATH as usize];

    // 안전성: devinst 는 위에서 얻은 유효한 장치 노드이고,
    // 두 출력 버퍼는 호출 동안 살아 있다.
    let cr =
        unsafe { CM_Request_Device_EjectW(devinst, Some(&mut veto_type), Some(&mut veto_name), 0) };

    if cr == CR_SUCCESS {
        return Ok(());
    }

    // 거부되면 무엇이 막았는지 알려준다. 이름이 비어 있으면 종류만이라도 남긴다.
    let name =
        String::from_utf16_lossy(&veto_name[..veto_name.iter().position(|c| *c == 0).unwrap_or(0)]);
    Err(DeviceError::Io {
        code: cr.0 as i32,
        message: if name.is_empty() {
            format!("제거가 거부되었습니다 (사유 코드 {})", veto_type.0)
        } else {
            format!("제거가 거부되었습니다: {name}")
        },
    })
}

/// 디스크 번호에 해당하는 장치의 **부모** 노드를 찾는다.
fn find_parent_devinst(disk_number: u32) -> Result<u32, DeviceError> {
    // 안전성: 클래스 GUID 는 유효하고, 반환된 목록은 아래에서 반드시 해제한다.
    let set = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_DISK),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }
    .map_err(|e| DeviceError::Io {
        code: 0,
        message: format!("장치 목록을 열지 못했습니다: {e}"),
    })?;

    let mut found: Option<u32> = None;

    for index in 0..256u32 {
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // 안전성: set 은 유효한 목록이고 iface 는 크기가 채워져 있다.
        if unsafe {
            SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVINTERFACE_DISK, index, &mut iface)
        }
        .is_err()
        {
            break; // 더 이상 없다.
        }

        // 상세 정보 버퍼. 가변 길이라 넉넉히 잡는다.
        let mut buf = vec![0u8; 1024];
        // 안전성: 버퍼 앞부분을 구조체로 다루고 cbSize 를 규격대로 채운다.
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        unsafe {
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        let mut devinfo = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // 안전성: buf 는 호출 동안 살아 있고 크기를 정확히 넘긴다.
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &iface,
                Some(detail),
                buf.len() as u32,
                None,
                Some(&mut devinfo),
            )
        }
        .is_err()
        {
            continue;
        }

        // 이 인터페이스의 경로를 열어 디스크 번호를 확인한다.
        // 안전성: detail 은 위 호출이 채운 유효한 구조체다.
        let path = unsafe { PCWSTR((*detail).DevicePath.as_ptr()) };
        if device_number_of(path) != Some(disk_number) {
            continue;
        }

        // 부모(대개 USB 장치)를 얻는다. 디스크 자체는 꺼낼 수 없다.
        let mut parent: u32 = 0;
        // 안전성: devinfo.DevInst 는 위에서 채워진 유효한 노드다.
        let cr: CONFIGRET = unsafe { CM_Get_Parent(&mut parent, devinfo.DevInst, 0) };
        if cr == CR_SUCCESS {
            found = Some(parent);
        }
        break;
    }

    // 안전성: set 은 SetupDiGetClassDevsW 가 돌려준 유효한 목록이다.
    unsafe {
        let _ = SetupDiDestroyDeviceInfoList(set);
    }

    found.ok_or(DeviceError::NotFound { disk_number })
}

/// 장치 경로를 열어 디스크 번호를 읽는다.
fn device_number_of(path: PCWSTR) -> Option<u32> {
    // 안전성: path 는 위에서 얻은 널 종료 문자열이고, 핸들은 아래에서 닫는다.
    let h = unsafe {
        CreateFileW(
            path,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .ok()?;

    let mut info = StorageDeviceNumber::default();
    let mut returned = 0u32;
    // 안전성: info 는 이 스코프에 살아 있고 크기를 정확히 넘긴다.
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut info as *mut _ as *mut _),
            std::mem::size_of::<StorageDeviceNumber>() as u32,
            Some(&mut returned),
            None,
        )
    };
    // 안전성: h 는 방금 연 유효한 핸들이고 여기서만 닫는다.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(h);
    }
    let _: HANDLE = h;

    ok.ok().map(|_| info.device_number)
}
