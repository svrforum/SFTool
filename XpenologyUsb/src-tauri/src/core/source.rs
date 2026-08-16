//! 원본 장치를 `Read` 로 감싼다.
//!
//! 이 어댑터 하나 덕분에 USB 복제가 별도의 쓰기 경로를 갖지 않는다.
//! `sink` 입장에서 원본 USB 는 gzip 스트림과 구별되지 않으므로,
//! 홀드백·정렬·해시·취소·진행률이 그대로 따라온다.

use crate::device::ReadSession;
use std::io::{self, Read};

/// 원본 세션을 앞에서부터 `limit` 바이트까지 흘려보낸다.
pub struct SessionReader {
    session: Box<dyn ReadSession>,
    /// 장치에서 다음에 읽을 위치. 항상 섹터 배수다.
    pos: u64,
    /// 여기까지만 흘려보낸다.
    limit: u64,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

impl SessionReader {
    /// `limit` 과 `chunk` 는 세션의 섹터 크기의 배수여야 한다.
    ///
    /// `limit` 은 파티션 끝(LBA × 섹터 크기)이라 항상 배수이고, `chunk` 는
    /// 호출자가 상수로 넘긴다. 배수가 아니면 장치가 정렬 오류로 거부한다.
    pub fn new(session: Box<dyn ReadSession>, limit: u64, chunk: usize) -> Self {
        Self {
            session,
            pos: 0,
            limit,
            buf: vec![0u8; chunk],
            start: 0,
            end: 0,
        }
    }

    /// 다 쓰고 나서 원본 세션을 돌려받는다. 닫으려면 이게 필요하다.
    pub fn into_session(self) -> Box<dyn ReadSession> {
        self.session
    }
}

impl Read for SessionReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.start == self.end {
            if self.pos >= self.limit {
                return Ok(0);
            }
            // 남은 만큼만 읽는다. limit 이 섹터 배수이므로 이 값도 배수다.
            let n = (self.buf.len() as u64).min(self.limit - self.pos) as usize;
            self.session
                .read_at(self.pos, &mut self.buf[..n])
                .map_err(|e| io::Error::other(format!("{e:?}")))?;
            self.pos += n as u64;
            self.start = 0;
            self.end = n;
        }
        let n = out.len().min(self.end - self.start);
        out[..n].copy_from_slice(&self.buf[self.start..self.start + n]);
        self.start += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::DiskInfo;
    use crate::device::fake::FakeReader;
    use crate::device::RawReader;

    fn disk() -> DiskInfo {
        use crate::device::fake::FakeEnumerator;
        use crate::device::UsbEnumerator;
        FakeEnumerator::sample().list_disks().unwrap()[1].clone()
    }

    fn reader(data: Vec<u8>, limit: u64, chunk: usize) -> SessionReader {
        let s = FakeReader::new(data, 512).open(&disk()).unwrap();
        SessionReader::new(s, limit, chunk)
    }

    #[test]
    fn it_yields_exactly_the_limit_and_then_stops() {
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        let mut r = reader(data.clone(), 2048, 1024);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, data[..2048]);
    }

    #[test]
    fn a_caller_asking_for_less_than_a_chunk_still_gets_every_byte_in_order() {
        // sink 는 8MiB 를 요청하지만 Read 구현이 그보다 적게 줄 수 있다.
        // 반대로 여기서는 호출자가 아주 조금씩 가져가는 경우를 본다.
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let mut r = reader(data.clone(), 4096, 1024);
        let mut out = Vec::new();
        let mut small = [0u8; 7];
        loop {
            let n = r.read(&mut small).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&small[..n]);
        }
        assert_eq!(out, data);
    }

    #[test]
    fn a_limit_equal_to_the_chunk_size_ends_cleanly() {
        // 경계값. 버퍼를 정확히 한 번 채우고 끝나는 경우.
        let data: Vec<u8> = (0..2048u32).map(|i| (i % 251) as u8).collect();
        let mut r = reader(data.clone(), 1024, 1024);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, data[..1024]);
    }

    #[test]
    fn it_never_reads_past_the_limit_even_when_the_device_is_bigger() {
        // 32GB USB 의 5GB 로더를 복사할 때, 나머지 27GB 를 건드리면 안 된다.
        // 장치보다 작은 limit 을 넘겨 확인한다.
        let mut r = reader(vec![0x77u8; 8192], 1536, 1024);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 1536);
    }

    #[test]
    fn a_read_failure_surfaces_instead_of_being_reported_as_end_of_stream() {
        // 도중에 USB 를 뽑으면 읽기가 실패한다. 그걸 EOF 로 처리하면
        // 잘린 복제본이 "성공" 으로 만들어진다.
        let s = FakeReader::new(vec![0u8; 8192], 512)
            .failing_at(2048)
            .open(&disk())
            .unwrap();
        let mut r = SessionReader::new(s, 8192, 1024);
        let mut out = Vec::new();
        assert!(r.read_to_end(&mut out).is_err());
    }

    #[test]
    fn the_session_comes_back_so_the_source_can_be_closed() {
        let r = reader(vec![0u8; 1024], 1024, 1024);
        let s = r.into_session();
        assert!(s.finish().is_ok());
    }
}
