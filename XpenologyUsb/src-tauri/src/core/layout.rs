//! 디스크 앞부분에서 "복사해야 할 길이" 를 알아낸다.
//!
//! USB 복제는 장치 전체를 옮기지 않는다. 32GB USB 에 5GB 로더가 들어 있으면
//! 나머지 27GB 는 아무 의미 없는 공간이고, 그걸 통째로 옮기면 13분이 걸린다.
//! 파티션 테이블이 선언한 마지막 파티션의 끝이 곧 의미 있는 데이터의 끝이므로
//! 거기까지만 복사한다.
//!
//! 여기에는 입출력이 없다. 바이트를 받아 숫자를 돌려줄 뿐이라 하드웨어 없이
//! 전수 검증된다.

/// 인식한 파티션 방식.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Scheme {
    Mbr,
}

/// 복사 계획.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Layout {
    /// 앞에서부터 이만큼 복사하면 된다.
    pub bytes: u64,
    pub scheme: Scheme,
    /// 확인 화면에 보여줄 파티션 개수.
    pub partitions: u32,
}

/// 복사할 길이를 정하지 못하는 이유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// 판정에 필요한 만큼 읽지 못했다.
    TooShort { need: usize, have: usize },
    /// 0x55AA 서명이 없다. 파티션 테이블이 아니다.
    NoSignature,
    /// 테이블은 있는데 항목이 전부 비었다.
    NoPartitions,
    /// GPT 다.
    Gpt,
    /// 테이블이 장치 밖을 가리킨다. 손상된 테이블이다.
    BeyondDevice { need: u64, have: u64 },
}

/// GPT 보호용 MBR 이 쓰는 파티션 타입.
const GPT_PROTECTIVE: u8 = 0xEE;

/// 장치 앞부분에서 복사할 길이를 구한다.
///
/// `head` 는 오프셋 0 부터 읽은 바이트. GPT 헤더가 LBA 1 에 있으므로
/// **최소 2 섹터**가 필요하다.
pub fn parse(head: &[u8], device_bytes: u64, sector: u32) -> Result<Layout, LayoutError> {
    let ss = sector as usize;
    let need = ss * 2;
    if head.len() < need {
        return Err(LayoutError::TooShort {
            need,
            have: head.len(),
        });
    }

    // MBR 서명은 섹터 크기와 무관하게 항상 바이트 510 에 있다.
    if head[510] != 0x55 || head[511] != 0xAA {
        return Err(LayoutError::NoSignature);
    }

    // GPT 를 먼저 걸러낸다.
    //
    // 보호용 MBR 을 그냥 MBR 로 읽으면 0xEE 항목 하나가 장치 전체(또는
    // 0xFFFFFFFF)를 가리켜 엉뚱한 길이가 나온다. 그래서 감지는 어차피 필요하다.
    //
    // 지원하지 않는 이유: GPT 백업 헤더는 디스크 맨 끝에 있어서, 크기가 다른
    // USB 로 옮기면 헤더 위치와 AlternateLBA·LastUsableLBA·CRC 를 전부 다시
    // 계산해야 한다. 조용히 틀리면 부팅은 되는데 나중에 깨지는 종류의 버그가
    // 된다. m-shell·RR 로더는 MBR 이므로 실제로 막힐 일이 없다.
    if &head[ss..ss + 8] == b"EFI PART" {
        return Err(LayoutError::Gpt);
    }

    let mut end_lba: u64 = 0;
    let mut count: u32 = 0;

    for slot in 0..4 {
        let off = 446 + slot * 16;
        let kind = head[off + 4];
        if kind == 0 {
            continue;
        }
        if kind == GPT_PROTECTIVE {
            // 헤더는 못 읽었지만 보호용 MBR 이다. 위와 같은 이유로 거부한다.
            return Err(LayoutError::Gpt);
        }
        let start = u32::from_le_bytes(head[off + 8..off + 12].try_into().unwrap()) as u64;
        let sectors = u32::from_le_bytes(head[off + 12..off + 16].try_into().unwrap()) as u64;
        if sectors == 0 {
            continue;
        }
        count += 1;
        // u64 로 계산한다. u32 로 하면 장치 끝 근처 파티션에서 넘쳐서
        // 길이가 작게 나오고, 조용히 잘린 복제본이 만들어진다.
        end_lba = end_lba.max(start + sectors);
    }

    if count == 0 {
        return Err(LayoutError::NoPartitions);
    }

    let bytes = end_lba * sector as u64;
    if bytes > device_bytes {
        return Err(LayoutError::BeyondDevice {
            need: bytes,
            have: device_bytes,
        });
    }

    Ok(Layout {
        bytes,
        scheme: Scheme::Mbr,
        partitions: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 파티션 항목 하나를 MBR 바이트열에 써 넣는다.
    ///
    /// `slot` 은 0..4, `start_lba`/`sectors` 는 LBA 단위.
    fn put(mbr: &mut [u8], slot: usize, kind: u8, start_lba: u32, sectors: u32) {
        let off = 446 + slot * 16;
        mbr[off] = 0x00; // 부팅 표시. 계산에 쓰지 않는다.
        mbr[off + 4] = kind;
        mbr[off + 8..off + 12].copy_from_slice(&start_lba.to_le_bytes());
        mbr[off + 12..off + 16].copy_from_slice(&sectors.to_le_bytes());
    }

    /// 서명이 붙은 빈 MBR 디스크 앞부분.
    fn blank(sector: usize) -> Vec<u8> {
        let mut v = vec![0u8; sector * 4];
        v[510] = 0x55;
        v[511] = 0xAA;
        v
    }

    #[test]
    fn three_partitions_report_the_end_of_the_last() {
        let mut head = blank(512);
        put(&mut head, 0, 0x83, 2048, 61440); // 1MiB~31MiB
        put(&mut head, 1, 0x83, 63488, 102400); // ~31MiB~81MiB
        put(&mut head, 2, 0x83, 165888, 9611264); // 끝 = 9777152 LBA
        let l = parse(&head, 32 * 1024 * 1024 * 1024, 512).unwrap();
        assert_eq!(l.bytes, 9_777_152 * 512);
        assert_eq!(l.partitions, 3);
        assert_eq!(l.scheme, Scheme::Mbr);
    }

    #[test]
    fn a_disk_without_a_signature_is_not_a_loader() {
        let mut head = vec![0u8; 2048];
        put(&mut head, 0, 0x83, 2048, 1000);
        assert_eq!(parse(&head, 1 << 30, 512), Err(LayoutError::NoSignature));
    }

    #[test]
    fn an_empty_table_is_not_a_loader() {
        // 서명은 있지만 항목이 전부 비었다. 새로 포맷한 디스크가 이렇다.
        let head = blank(512);
        assert_eq!(parse(&head, 1 << 30, 512), Err(LayoutError::NoPartitions));
    }

    #[test]
    fn gpt_is_refused_by_its_header() {
        let mut head = blank(512);
        put(&mut head, 0, GPT_PROTECTIVE, 1, 0xFFFF_FFFF);
        head[512..520].copy_from_slice(b"EFI PART");
        assert_eq!(parse(&head, 1 << 30, 512), Err(LayoutError::Gpt));
    }

    #[test]
    fn a_protective_mbr_is_refused_even_without_the_header() {
        // 헤더가 손상돼 EFI PART 를 못 읽어도 0xEE 항목만으로 GPT 임을 안다.
        // 이걸 놓치면 0xFFFFFFFF 섹터를 복사하려 든다.
        let mut head = blank(512);
        put(&mut head, 0, GPT_PROTECTIVE, 1, 0xFFFF_FFFF);
        assert_eq!(parse(&head, 1 << 30, 512), Err(LayoutError::Gpt));
    }

    #[test]
    fn a_table_pointing_past_the_device_is_refused() {
        // 손상된 테이블이 장치보다 큰 길이를 선언하면, 그대로 믿고 읽다가
        // 장치 끝에서 실패한다. 시작 전에 잡는다.
        let mut head = blank(512);
        put(&mut head, 0, 0x83, 2048, 100_000);
        let device = 1024 * 1024; // 1MiB 밖에 안 되는 장치
        assert_eq!(
            parse(&head, device, 512),
            Err(LayoutError::BeyondDevice {
                need: 102_048 * 512,
                have: device
            })
        );
    }

    #[test]
    fn partition_order_in_the_table_does_not_matter() {
        // 테이블의 항목 순서와 디스크상의 순서는 무관하다.
        // 마지막 슬롯이 가장 앞의 파티션인 이미지가 실제로 있다.
        let mut head = blank(512);
        put(&mut head, 0, 0x83, 100_000, 1000); // 끝 101000
        put(&mut head, 1, 0x83, 2048, 1000);
        put(&mut head, 3, 0x83, 50_000, 1000);
        let l = parse(&head, 1 << 30, 512).unwrap();
        assert_eq!(l.bytes, 101_000 * 512);
        assert_eq!(l.partitions, 3);
    }

    #[test]
    fn four_kilobyte_sectors_scale_the_result() {
        // 4K 네이티브 장치. 섹터 크기를 512 로 가정하면 길이가 8배 작게 나온다.
        let mut head = blank(4096);
        put(&mut head, 0, 0x83, 256, 2048); // 끝 2304 LBA
        let l = parse(&head, 1 << 30, 4096).unwrap();
        assert_eq!(l.bytes, 2304 * 4096);
    }

    #[test]
    fn zero_length_entries_are_ignored() {
        let mut head = blank(512);
        put(&mut head, 0, 0x83, 2048, 1000);
        put(&mut head, 1, 0x83, 9000, 0); // 타입은 있는데 길이가 0
        let l = parse(&head, 1 << 30, 512).unwrap();
        assert_eq!(l.partitions, 1);
        assert_eq!(l.bytes, 3048 * 512);
    }

    #[test]
    fn a_head_shorter_than_two_sectors_is_refused() {
        // GPT 헤더는 LBA 1 에 있다. 1 섹터만 읽고 판정하면 GPT 를 MBR 로 오인한다.
        let head = blank(512);
        assert_eq!(
            parse(&head[..512], 1 << 30, 512),
            Err(LayoutError::TooShort {
                need: 1024,
                have: 512
            })
        );
    }

    #[test]
    fn a_partition_reaching_the_very_end_of_the_device_is_accepted() {
        // 경계값. 딱 맞는 것은 BeyondDevice 가 아니다.
        let mut head = blank(512);
        put(&mut head, 0, 0x83, 0, 2048);
        assert_eq!(parse(&head, 2048 * 512, 512).unwrap().bytes, 2048 * 512);
    }
}
