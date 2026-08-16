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
//! 복제 테스트는 원본 쪽 가상 디스크가 하나 더 있어야 하므로
//! `XPENOLOGY_TEST_SOURCE` 도 함께 본다. 없으면 그 테스트만 건너뛴다.
//!
//! ```powershell
//! $env:XPENOLOGY_TEST_DISK = "2"
//! $env:XPENOLOGY_TEST_SOURCE = "3"
//! cargo test --features vhd-tests --test vhd_write -- --test-threads=1
//! ```

#![cfg(all(windows, feature = "vhd-tests"))]

use xpenologyusb_lib::core::model::{BusType, DiskInfo};
use xpenologyusb_lib::device::windows::{WindowsRawReader, WindowsRawWriter};
use xpenologyusb_lib::device::{RawReader, RawWriter, UsbEnumerator};

/// 두 버퍼가 처음으로 갈라지는 지점.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(at) => Some(at),
        None if a.len() != b.len() => Some(a.len().min(b.len())),
        None => None,
    }
}

/// 다르면 **어디가** 다른지 보여주며 실패한다.
///
/// 4MiB 짜리 버퍼 둘을 `assert_eq!` 로 비교하면 실패했을 때 벡터를 통째로
/// 덤프한다. CI 로그가 36MB 가 되어 잘렸고, 정작 어느 오프셋이 다른지는
/// 끝내 보이지 않았다. 진단이 되지 않는 실패는 없는 것과 비슷하다.
fn assert_same(actual: &[u8], expected: &[u8], what: &str) {
    if let Some(at) = first_difference(actual, expected) {
        let from = at.saturating_sub(16);
        let to = (at + 16).min(actual.len()).min(expected.len());
        panic!(
            "{what}\n  길이: {} vs {}\n  오프셋 {at} 부터 다르다\n  실제 [{from}..{to}] = {:02x?}\n  기대 [{from}..{to}] = {:02x?}",
            actual.len(),
            expected.len(),
            &actual[from..to],
            &expected[from..to],
        );
    }
}

/// 디스크 앞에서 `len` 바이트를 **건드리지 않고** 읽는다.
///
/// 되읽기에 `RawWriter` 를 쓰면 안 된다. `WindowsRawWriter::open` 은 그 안에서
/// 파티션 테이블을 지우고 **앞 1MiB 를 0 으로 덮는다.** 그걸로 읽으면 방금 쓴
/// 내용을 스스로 지운 뒤 그 0 을 읽게 된다. 실제로 이 테스트가 처음 그렇게
/// 실패했다 — 복제는 멀쩡한데 확인 방법이 대상을 파괴하고 있었다.
fn read_back(disk: &DiskInfo, len: usize, label: &str) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let mut s = WindowsRawReader::new()
        .open(disk)
        .unwrap_or_else(|e| panic!("[{label}] 읽기용으로 열지 못했다: {e:?}"));
    s.read_at(0, &mut buf)
        .unwrap_or_else(|e| panic!("[{label}] 되읽기 실패: {e:?}"));
    s.finish()
        .unwrap_or_else(|e| panic!("[{label}] 마무리 실패: {e:?}"));
    buf
}

/// 환경 변수에서 디스크 번호를 읽는다. 없으면 건너뛴다 — **단, CI 에서는 아니다.**
///
/// 개발자 기계에서 실수로 실제 디스크를 대상으로 도는 일이 없어야 하므로
/// 변수가 없으면 건너뛰는 것이 맞다. 그런데 CI 에서도 똑같이 건너뛰면,
/// 워크플로가 변수를 넘기다 망가졌을 때 잡이 **초록불로 통과한다.** 아무것도
/// 실행하지 않은 것과 전부 통과한 것이 구분되지 않는 것이다.
///
/// 이 프로젝트는 이미 그 실수를 한 적이 있다 — 값을 못 읽은 것을 "매체 없음"
/// 이라는 사실로 읽어 USB 가 통째로 목록에서 사라졌다. 없음은 사실이 아니다.
/// 그래서 CI 안에서는 변수가 빠진 것을 실패로 다룬다.
fn disk_from_env(var: &str) -> Option<u32> {
    let raw = std::env::var(var).ok();
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|n| *n != 0); // 디스크 0 은 어떤 경우에도 대상이 아니다.

    if parsed.is_none() && std::env::var("CI").is_ok() {
        panic!(
            "CI 인데 {var} 가 쓸 수 없는 값이다 (실제 값: {raw:?}). \
             건너뛰면 아무것도 실행하지 않고 초록불이 된다."
        );
    }
    parsed
}

/// 대상 디스크 번호. 없으면 테스트를 건너뛴다 (CI 에서는 실패한다).
fn target_disk() -> Option<u32> {
    disk_from_env("XPENOLOGY_TEST_DISK")
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

/// 원본 디스크 번호. 없으면 복제 테스트를 건너뛴다.
fn source_disk() -> Option<u32> {
    disk_from_env("XPENOLOGY_TEST_SOURCE")
}

/// 파티션 하나가 있는 이미지를 만든다. `end_lba` 에서 끝난다.
fn loader_like_image(bytes: usize, end_lba: u32) -> Vec<u8> {
    let mut v: Vec<u8> = (0..bytes).map(|i| ((i * 2654435761) >> 13) as u8).collect();
    for b in v[..512].iter_mut() {
        *b = 0;
    }
    let off = 446;
    v[off + 4] = 0x83;
    v[off + 8..off + 12].copy_from_slice(&2048u32.to_le_bytes());
    v[off + 12..off + 16].copy_from_slice(&(end_lba - 2048).to_le_bytes());
    v[510] = 0x55;
    v[511] = 0xAA;
    v
}

/// **가상 디스크 하나를 다른 하나로 복제한다.**
///
/// 여기서만 확인되는 것: 원본을 잠그지 않고 읽는 것이 실제로 허용되는지,
/// 복제 도중 대상의 볼륨이 다시 마운트되지 않는지, 두 핸들을 동시에 열어도
/// 문제가 없는지. 단위 테스트의 가짜 장치는 이 중 어느 것도 재현하지 못한다.
#[test]
fn clones_one_virtual_disk_onto_another() {
    let (Some(src_n), Some(dst_n)) = (source_disk(), target_disk()) else {
        eprintln!("XPENOLOGY_TEST_SOURCE 또는 XPENOLOGY_TEST_DISK 가 없어 건너뛴다");
        return;
    };
    assert_ne!(src_n, dst_n, "원본과 대상이 같은 디스크다");

    // 원본에 로더처럼 생긴 이미지를 심는다.
    let image = loader_like_image(6 * 1024 * 1024, 8192); // 4MiB 까지가 파티션
    let src_disk = describe(src_n);
    write_and_verify(&src_disk, &image, "seed-source");
    std::thread::sleep(std::time::Duration::from_secs(3));

    let dst_disk = describe(dst_n);
    let protected = xpenologyusb_lib::device::windows::WindowsEnumerator::new()
        .protected_disk_numbers()
        .expect("보호 목록을 읽지 못했다");

    let summary = xpenologyusb_lib::core::cloner::run(
        xpenologyusb_lib::core::cloner::CloneConfig { verify: true },
        &src_disk,
        &dst_disk,
        &protected,
        &WindowsRawReader::new(),
        &WindowsRawWriter::new(),
        &xpenologyusb_lib::core::pipeline::NeverCancel,
        |_| {},
    )
    .expect("복제에 실패했다");

    // 파티션 끝까지만 복사돼야 한다. 6MiB 장치에서 4MiB 다.
    assert_eq!(summary.bytes_copied, 4 * 1024 * 1024);

    // 대상과 원본을 **둘 다 읽기 전용으로** 되읽어 대조한다.
    //
    // verify:true 가 이미 확인한 것은 "쓴 것" 과 "장치에 있는 것" 의 일치다.
    // 여기서 보는 것은 다른 질문이다 — 대상이 **원본과 같은가.** 잘못된 범위를
    // 복사했다면 검증은 통과하고 이 대조가 실패한다.
    let copied = summary.bytes_copied as usize;
    let from_target = read_back(&dst_disk, copied, "target");
    let from_source = read_back(&src_disk, copied, "source");

    // 심어 둔 `image` 버퍼가 아니라 **지금의 원본**과 비교한다.
    //
    // 윈도우는 유효한 파티션 테이블이 생기면 MBR 디스크 서명(오프셋 440)이
    // 0 인 디스크에 서명을 채워 넣는다. 씨앗을 심은 직후의 버퍼는 그래서 이미
    // 원본이 아니다. 복제본이 같아야 할 대상은 디스크의 현재 내용이다.
    assert_same(&from_target, &from_source, "복제본이 원본과 다르다");

    // 다만 "둘 다 똑같이 비어 있어도" 통과하면 안 되므로, 첫 섹터를 뺀 나머지가
    // 심어 둔 내용 그대로인지도 본다. 윈도우가 건드리는 것은 MBR 뿐이다.
    assert_same(
        &from_target[512..],
        &image[512..copied],
        "복사된 내용이 심어 둔 이미지와 다르다",
    );
}

/// 이미 내용이 있는 대상으로 복제한다. 재쓰기 경로.
#[test]
fn clones_onto_a_target_that_already_has_a_layout() {
    let (Some(src_n), Some(dst_n)) = (source_disk(), target_disk()) else {
        eprintln!("XPENOLOGY_TEST_SOURCE 또는 XPENOLOGY_TEST_DISK 가 없어 건너뛴다");
        return;
    };

    let src_disk = describe(src_n);
    let dst_disk = describe(dst_n);

    // 대상에 파티션 테이블이 남아 있는 상태를 만든다.
    write_and_verify(&dst_disk, &loader_like_image(4 * 1024 * 1024, 4096), "old");
    std::thread::sleep(std::time::Duration::from_secs(3));

    write_and_verify(&src_disk, &loader_like_image(6 * 1024 * 1024, 8192), "seed");
    std::thread::sleep(std::time::Duration::from_secs(3));

    let protected = xpenologyusb_lib::device::windows::WindowsEnumerator::new()
        .protected_disk_numbers()
        .expect("보호 목록을 읽지 못했다");

    xpenologyusb_lib::core::cloner::run(
        xpenologyusb_lib::core::cloner::CloneConfig { verify: false },
        &src_disk,
        &dst_disk,
        &protected,
        &WindowsRawReader::new(),
        &WindowsRawWriter::new(),
        &xpenologyusb_lib::core::pipeline::NeverCancel,
        |_| {},
    )
    .expect("이미 레이아웃이 있는 대상으로의 복제가 실패했다");
}
