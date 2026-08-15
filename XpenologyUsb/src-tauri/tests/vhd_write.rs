//! 가상 디스크(VHD)에 실제로 쓰는 통합 테스트.
//!
//! ## 왜 필요한가
//!
//! 이 프로그램의 버그는 거의 전부 실행해봐야만 드러났다 — 컴파일도 되고
//! clippy 도 통과하고 단위 테스트도 초록불인데, 실제 장치에 쓰는 순간 실패했다.
//! 그때마다 사용자가 대신 테스트해 주는 수밖에 없었고, 한 번 왕복에 빌드 15분이
//! 들었다.
//!
//! CI 러너에 USB 는 없지만 **가상 디스크는 붙일 수 있다.** VHD 를 attach 하면
//! `\\.\PhysicalDriveN` 으로 잡히고, 볼륨을 만들면 마운트도 된다. 지금까지
//! 실패한 것들은 USB 고유 특성이 아니라 Win32 호출 순서 문제였으므로 여기서
//! 재현된다:
//!
//! - 접근 마스크가 틀려 열기가 `ERROR_INVALID_PARAMETER` 로 거부되던 것
//! - 잠금을 파티션 삭제보다 먼저 해서 쓰기가 거부되던 것
//! - 파티션이 남아 있는 장치에 다시 쓰면 실패하던 것 (오프셋 8MiB 거부)
//!
//! ## 재현할 수 없는 것
//!
//! 버스 타입 판정(VHD 는 USB 가 아니다), 실제 USB 컨트롤러의 전송 제한,
//! `CM_Request_Device_Eject`, 그리고 부팅. 그것들은 여전히 실물이 필요하다.
//!
//! ## 실행 방법
//!
//! CI 의 windows-latest 잡이 VHD 를 만들어 붙인 뒤 디스크 번호를
//! `XPENOLOGY_TEST_DISK` 환경 변수로 넘긴다. 그 변수가 없으면 테스트는 건너뛴다 —
//! 실수로 개발자의 실제 디스크를 대상으로 도는 일이 없어야 한다.
//!
//! ```powershell
//! $env:XPENOLOGY_TEST_DISK = "2"
//! cargo test --features vhd-tests --test vhd_write -- --test-threads=1
//! ```

#![cfg(all(windows, feature = "vhd-tests"))]

use xpenologyusb_lib::core::model::{BusType, DiskInfo};
use xpenologyusb_lib::device::windows::WindowsRawWriter;
use xpenologyusb_lib::device::{RawWriter, UsbEnumerator, WriteSession};

/// 대상 디스크 번호. 없으면 테스트를 건너뛴다.
fn target_disk() -> Option<u32> {
    std::env::var("XPENOLOGY_TEST_DISK")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|n| *n != 0) // 디스크 0 은 어떤 경우에도 대상이 아니다.
}

/// 열거자에서 대상 디스크 정보를 가져온다.
fn describe(number: u32) -> DiskInfo {
    let e = xpenologyusb_lib::device::windows::WindowsEnumerator::new();
    let disks = e.list_disks().expect("디스크를 열거하지 못했다");
    let mut d = disks
        .into_iter()
        .find(|d| d.number == number)
        .unwrap_or_else(|| panic!("디스크 {number} 를 찾지 못했다"));
    // VHD 의 버스 타입은 Virtual 이다. 쓰기 경로는 USB 를 요구하므로
    // 테스트에서만 USB 로 바꿔 통과시킨다. 안전 규칙 자체는 별도 단위 테스트가
    // 담당하고, 여기서 검증하는 것은 Win32 호출 순서다.
    d.bus_type = BusType::Usb;
    d
}

/// 되풀이 가능한 시험용 패턴.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i as u32).wrapping_mul(2654435761) >> 16) as u8 ^ seed)
        .collect()
}

/// 이미지를 쓰고 되읽어 같은지 확인한다.
fn write_and_verify(disk: &DiskInfo, data: &[u8], label: &str) {
    let writer = WindowsRawWriter::new();
    let mut session = writer
        .open(disk)
        .unwrap_or_else(|e| panic!("[{label}] 장치를 열지 못했다: {e:?}"));

    let ss = session.sector_size() as usize;
    let padded = data.len().div_ceil(ss) * ss;
    let mut buf = vec![0u8; padded];
    buf[..data.len()].copy_from_slice(data);

    session
        .write_at(0, &buf)
        .unwrap_or_else(|e| panic!("[{label}] 쓰기에 실패했다: {e:?}"));

    let mut back = vec![0u8; padded];
    session
        .read_at(0, &mut back)
        .unwrap_or_else(|e| panic!("[{label}] 되읽기에 실패했다: {e:?}"));

    assert_eq!(
        &back[..data.len()],
        data,
        "[{label}] 쓴 내용과 장치의 내용이 다르다"
    );

    session
        .finish()
        .unwrap_or_else(|e| panic!("[{label}] 마무리에 실패했다: {e:?}"));
}

/// 빈 장치에 쓴다. 가장 기본적인 경로.
#[test]
fn writes_to_a_clean_disk() {
    let Some(number) = target_disk() else {
        eprintln!("XPENOLOGY_TEST_DISK 가 없어 건너뛴다");
        return;
    };
    let disk = describe(number);
    write_and_verify(&disk, &pattern(4 * 1024 * 1024, 0x5A), "clean");
}

/// **이미 무언가 써진 장치에 다시 쓴다.**
///
/// 이것이 이 파일의 존재 이유다. 로더가 써진 USB 에 다시 쓰면
/// "오프셋 8388608 에서 거부" 로 실패했는데, 원인은 앞선 쓰기가 만든 파티션이
/// 남아 마운트돼 있고 그 볼륨이 잠기지 않은 것이었다. 두 번 연속 쓰면 재현된다.
#[test]
fn rewrites_a_disk_that_already_has_a_layout() {
    let Some(number) = target_disk() else {
        eprintln!("XPENOLOGY_TEST_DISK 가 없어 건너뛴다");
        return;
    };
    let disk = describe(number);

    // 첫 번째: 파티션 테이블처럼 보이는 것을 포함해 쓴다.
    let mut first = pattern(8 * 1024 * 1024, 0x11);
    // MBR 서명을 넣어 윈도우가 파티션으로 인식하게 만든다.
    first[510] = 0x55;
    first[511] = 0xAA;
    write_and_verify(&disk, &first, "first");

    // 윈도우가 새 레이아웃을 인식할 시간을 준다.
    std::thread::sleep(std::time::Duration::from_secs(3));

    // 두 번째: 같은 장치에 다시. 여기서 거부되면 회귀다.
    let second = pattern(8 * 1024 * 1024, 0x22);
    write_and_verify(&disk, &second, "rewrite");
}

/// 이미지보다 장치가 클 때 꼬리를 지워도 이미지가 상하지 않는지.
#[test]
fn zeroing_the_tail_does_not_touch_the_image() {
    let Some(number) = target_disk() else {
        eprintln!("XPENOLOGY_TEST_DISK 가 없어 건너뛴다");
        return;
    };
    let disk = describe(number);

    let writer = WindowsRawWriter::new();
    let mut session = writer.open(&disk).expect("장치를 열지 못했다");
    let ss = session.sector_size() as usize;

    let data = pattern(2 * 1024 * 1024, 0x33);
    let padded = data.len().div_ceil(ss) * ss;
    let mut buf = vec![0u8; padded];
    buf[..data.len()].copy_from_slice(&data);
    session.write_at(0, &buf).expect("쓰기 실패");

    // 장치 끝 1MiB 를 지운다. 이미지는 앞쪽에 있으므로 무사해야 한다.
    session.zero_tail(1024 * 1024).expect("꼬리 지우기 실패");

    let mut back = vec![0u8; padded];
    session.read_at(0, &mut back).expect("되읽기 실패");
    assert_eq!(
        &back[..data.len()],
        &data[..],
        "꼬리 지우기가 이미지를 훼손했다"
    );

    session.finish().expect("마무리 실패");
}

/// 정렬되지 않은 요청은 거부돼야 한다. 실제 장치에서도 그런지 확인한다.
#[test]
fn rejects_unaligned_requests() {
    let Some(number) = target_disk() else {
        eprintln!("XPENOLOGY_TEST_DISK 가 없어 건너뛴다");
        return;
    };
    let disk = describe(number);
    let writer = WindowsRawWriter::new();
    let mut session = writer.open(&disk).expect("장치를 열지 못했다");

    let ss = session.sector_size() as usize;
    assert!(
        session.write_at(1, &vec![0u8; ss]).is_err(),
        "정렬되지 않은 오프셋이 통과했다"
    );
    assert!(
        session.write_at(0, &vec![0u8; ss - 1]).is_err(),
        "정렬되지 않은 길이가 통과했다"
    );
    session.finish().expect("마무리 실패");
}
