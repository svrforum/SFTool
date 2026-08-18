//! 스트림을 장치에 쓴다.
//!
//! 로더를 굽는 흐름과 USB 를 복제하는 흐름이 이 코드를 공유한다. 앞의 것은
//! 네트워크에서 받아 gzip 을 푼 스트림을, 뒤의 것은 원본 USB 를 읽는 스트림을
//! 넘긴다. 장치 쪽에서 보면 둘은 구별되지 않는다.
//!
//! 여기 있는 규칙들은 전부 실물에서 실패해 본 뒤에 생긴 것이다. 복제 경로가
//! 같은 실패를 처음부터 다시 겪을 이유가 없어서 하나로 모았다.

use super::pipeline::{Cancel, ProgressEvent, ProgressReporter};
use crate::device::{DeviceError, WriteSession};
use sha2::{Digest, Sha256};
use std::io::Read;

/// 장치에 한 번에 보내는 크기. 섹터 배수로 맞춰 쓴다.
pub const CHUNK: usize = 8 * 1024 * 1024;

/// 맨 앞에서 보류했다가 마지막에 쓰는 크기.
///
/// 이미지의 첫 부분에는 **그 이미지 자신의 파티션 테이블**이 들어 있다.
/// 그걸 오프셋 0 에 쓰는 순간 장치에 유효한 파티션 테이블이 생기고, 윈도우가
/// 그것을 감지해 **쓰는 도중에 볼륨을 마운트한다.** 탐색기가 갑자기 열리는 것이
/// 그 증상이고, 새로 생긴 볼륨은 잠겨 있지 않으므로 그 섹터에 도달한 순간
/// 쓰기가 거부된다. 실제로 24MiB 지점에서 그렇게 실패했다.
///
/// 준비 단계에서 아무리 지워도 소용이 없다. 우리가 쓰는 내용 자체가 파티션
/// 테이블이기 때문이다. 그래서 앞부분을 보류했다가 **나머지를 다 쓴 뒤에**
/// 채운다. 윈도우가 유효한 테이블을 보는 시점에는 이미 끝나 있다.
///
/// 1MiB 면 MBR(섹터 0), GPT 헤더(섹터 1), GPT 항목(섹터 2~33)을 모두 덮는다.
pub const HOLDBACK: u64 = 1024 * 1024;

/// 장치 끝에서 지울 크기.
///
/// 이미지가 USB 보다 작으면 끝에 남은 옛 GPT 백업 헤더 때문에 Windows 가
/// 지워진 파티션 테이블을 되살린다.
pub const TAIL_ZERO: u64 = 1024 * 1024;

/// 검증 대조 단위.
///
/// 전체를 해시 하나로 비교하면 어긋났다는 사실만 알고 **어디가** 어긋났는지는
/// 알 수 없다. 그 상태로 실물 장애를 두 번 만났고, 두 번 다 추측으로 원인을
/// 골라야 했다. 블록 단위로 쪼개 두면 실패가 위치를 함께 말한다.
///
/// `HOLDBACK` 과 같은 크기인 것은 우연이 아니다. 그래야 0번 블록이 나중에 쓰는
/// 파티션 테이블 구간과 정확히 겹쳐서, "맨 앞만 틀렸다" 와 "본문이 틀렸다" 가
/// 블록 번호만으로 갈린다. 8GB 이미지라도 해시 목록은 256KB 다.
pub const BLOCK: u64 = HOLDBACK;

/// 전체 해시와 블록별 해시를 함께 쌓는다.
///
/// 먹이는 순서가 곧 장치에 놓이는 순서여야 한다. `stream` 이 홀드백을 먼저
/// 해시에 넣고 본문을 뒤에 넣는 것은 바로 그 때문이다 — 장치에서도 홀드백이
/// 0번 오프셋에 온다.
struct Tally {
    whole: Sha256,
    block: Sha256,
    filled: u64,
    blocks: Vec<[u8; 32]>,
}

impl Tally {
    fn new() -> Self {
        Self {
            whole: Sha256::new(),
            block: Sha256::new(),
            filled: 0,
            blocks: Vec::new(),
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.whole.update(data);
        while !data.is_empty() {
            let room = (BLOCK - self.filled) as usize;
            let take = room.min(data.len());
            self.block.update(&data[..take]);
            self.filled += take as u64;
            if self.filled == BLOCK {
                self.blocks.push(
                    std::mem::replace(&mut self.block, Sha256::new())
                        .finalize()
                        .into(),
                );
                self.filled = 0;
            }
            data = &data[take..];
        }
    }

    /// 마지막 자투리 블록까지 닫는다.
    fn finish(mut self) -> ([u8; 32], Vec<[u8; 32]>) {
        if self.filled > 0 {
            self.blocks.push(self.block.finalize().into());
        }
        (self.whole.finalize().into(), self.blocks)
    }
}

/// 쓰기 결과.
pub struct SinkOutcome {
    /// 실제로 쓴 바이트 수 (섹터 배수로 올림된 값).
    pub bytes: u64,
    /// 쓴 내용의 SHA-256. 검증할 때 되읽은 것과 대조한다.
    pub hash: [u8; 32],
    /// [`BLOCK`] 단위 해시. 어긋난 위치를 짚기 위한 것이다.
    pub blocks: Vec<[u8; 32]>,
}

/// 쓰기 실패 원인.
#[derive(Debug)]
pub enum SinkError {
    /// 원본 스트림을 읽지 못했다.
    Source(String),
    /// 원본이 한 바이트도 주지 않았다.
    ///
    /// 이것을 성공으로 처리하면 안 되는 이유는 [`stream`] 안에 적어 두었다.
    EmptySource,
    Device(DeviceError),
    TooSmall {
        need: u64,
        have: u64,
    },
    /// 되읽은 내용이 다르다. `at` 은 처음 어긋난 [`BLOCK`] 의 시작 오프셋.
    VerifyMismatch {
        at: u64,
    },
    Canceled,
}

impl From<DeviceError> for SinkError {
    fn from(e: DeviceError) -> Self {
        SinkError::Device(e)
    }
}

/// 스트림을 장치 앞에서부터 쓴다.
///
/// `expected_total` 은 진행률 표시용이다. 모르면 None — 불확정 막대가 된다.
pub fn stream<F: FnMut(ProgressEvent)>(
    src: &mut dyn Read,
    session: &mut dyn WriteSession,
    expected_total: Option<u64>,
    cancel: &dyn Cancel,
    rep: &mut ProgressReporter<F>,
) -> Result<SinkOutcome, SinkError> {
    let sector = session.sector_size() as usize;
    let capacity = session.total_bytes();

    let mut buf = vec![0u8; CHUNK];
    let mut offset: u64 = 0;
    let mut last_t = std::time::Instant::now();
    let mut tally = Tally::new();

    // 이미지 맨 앞을 담아 둘 곳. 마지막에 쓴다.
    let mut holdback: Vec<u8> = Vec::new();

    loop {
        if cancel.is_canceled() {
            return Err(SinkError::Canceled);
        }

        // 청크를 채운다. Read 는 요청보다 적게 줄 수 있으므로 반복해서 채운다.
        let mut filled = 0usize;
        while filled < buf.len() {
            match src.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(SinkError::Source(e.to_string())),
            }
        }
        if filled == 0 {
            break;
        }

        // 앞 HOLDBACK 바이트는 지금 쓰지 않고 들고 있는다.
        if (holdback.len() as u64) < HOLDBACK {
            let want = (HOLDBACK - holdback.len() as u64) as usize;
            let take = want.min(filled);
            holdback.extend_from_slice(&buf[..take]);
            tally.update(&buf[..take]);
            offset += take as u64;
            if take == filled {
                continue;
            }
            // 이 청크의 나머지는 정상적으로 쓴다.
            buf.copy_within(take..filled, 0);
            filled -= take;
        }

        // 장치에는 섹터 배수로만 쓸 수 있다. 마지막 조각은 올림하고
        // 남는 부분을 0 으로 채운다 — 할당된 그대로 두면 힙 내용이 USB 에 실린다.
        let padded = filled.div_ceil(sector) * sector;
        if padded > filled {
            buf[filled..padded].fill(0);
        }

        if offset + padded as u64 > capacity {
            return Err(SinkError::TooSmall {
                need: offset + padded as u64,
                have: capacity,
            });
        }

        session.write_at(offset, &buf[..padded])?;
        tally.update(&buf[..padded]);
        offset += padded as u64;

        let now = std::time::Instant::now();
        // 알고 있는 크기를 넘어서면 추정이 틀린 것이므로 불확정으로 되돌린다.
        // 100% 를 넘겨 표시하거나, 다 됐다고 보여준 뒤 계속 도는 것이
        // 아무 숫자도 없는 것보다 나쁘다 — 사용자가 USB 를 뽑는다.
        let total = expected_total.filter(|t| offset <= *t);
        rep.update(
            offset,
            total,
            now.duration_since(last_t).as_secs_f64(),
            padded as u64,
        );
        last_t = now;
    }

    // 한 바이트도 오지 않았다면 성공이 아니다.
    //
    // 여기까지 왔다는 것은 `RawWriter::open` 이 이미 대상의 파티션 테이블을
    // 지운 뒤라는 뜻이다. 그대로 Ok 를 돌려주면 `verify` 는 `while pos < 0` 이라
    // 한 바퀴도 돌지 않고 빈 해시끼리 비교해 통과하고, 사용자는 **USB 가 비워진
    // 채로** "성공했고 검증까지 됐다" 를 보게 된다. 빈 이미지는 어떤 경우에도
    // 정상이 아니므로 여기서 끊는다.
    //
    // 굽기 경로에서 실제로 도달할 수 있다: 서버가 Content-Length 없이 빈 본문을
    // 주거나, zip 안의 이미지 항목 크기가 0 이면 이 상태가 된다.
    if offset == 0 {
        return Err(SinkError::EmptySource);
    }

    // 보류해 둔 맨 앞을 이제 쓴다. 여기서 비로소 장치에 유효한 파티션 테이블이
    // 생기지만, 나머지는 이미 다 쓰인 뒤라 윈도우가 볼륨을 마운트해도 늦다.
    if !holdback.is_empty() {
        let raw = holdback.len();
        let padded = raw.div_ceil(sector) * sector;
        holdback.resize(padded, 0);
        if padded > raw {
            // 패딩까지 해시에 넣고 길이도 패딩 기준으로 맞춘다.
            //
            // 이 보정이 없으면 전체가 HOLDBACK 보다 작고 섹터 배수가 아닐 때
            // `bytes` 가 장치에 실제로 놓인 양보다 작아진다. 그 값이 검증 범위와
            // 꼬리 지우기 시작점을 함께 결정하기 때문에 두 군데가 동시에 어긋난다:
            // 검증은 마지막 부분 섹터를 읽지 못해 **멀쩡한 쓰기를 불량 USB 로
            // 보고**하고, 꼬리 지우기는 시작점이 앞으로 당겨져 **이미지의 그 섹터를
            // 지운다**. 검증이 기본으로 꺼져 있어서 뒤쪽은 조용히 일어난다.
            tally.update(&holdback[raw..]);
            offset = offset.max(padded as u64);
        }
        session.write_at(0, &holdback)?;
    }

    let (hash, blocks) = tally.finish();
    Ok(SinkOutcome {
        bytes: offset,
        hash,
        blocks,
    })
}

/// 장치 끝을 0 으로 덮는다.
///
/// 지우는 범위가 방금 쓴 내용을 침범해서는 안 된다. 무조건 마지막 1MiB 를
/// 지우면 내용이 장치 끝까지 닿는 경우 그것을 훼손한다.
pub fn zero_tail(session: &mut dyn WriteSession, written: u64) -> Result<(), SinkError> {
    let capacity = session.total_bytes();
    let tail_start = capacity.saturating_sub(TAIL_ZERO).max(written);
    if tail_start < capacity {
        session.zero_tail(capacity - tail_start)?;
    }
    Ok(())
}

/// 장치에서 되읽어 쓴 것과 같은지 확인한다.
///
/// 불량 USB 는 쓰기가 성공했다고 보고하고도 내용이 다른 경우가 있다.
/// 이 대조가 유일하게 그것을 잡는다.
pub fn verify<F: FnMut(ProgressEvent)>(
    session: &mut dyn WriteSession,
    bytes: u64,
    expected: &[u8; 32],
    blocks: &[[u8; 32]],
    cancel: &dyn Cancel,
    rep: &mut ProgressReporter<F>,
) -> Result<(), SinkError> {
    let sector = session.sector_size() as usize;
    let mut back = vec![0u8; BLOCK as usize];
    let mut whole = Sha256::new();
    let mut pos = 0u64;
    let mut index = 0usize;
    let mut last_v = std::time::Instant::now();

    while pos < bytes {
        if cancel.is_canceled() {
            return Err(SinkError::Canceled);
        }
        let n = BLOCK.min(bytes - pos) as usize;
        // 읽기도 섹터 배수여야 한다.
        let n = (n / sector) * sector;
        if n == 0 {
            break;
        }
        let at = pos;
        session.read_at(at, &mut back[..n])?;
        whole.update(&back[..n]);

        // 블록마다 그 자리에서 대조한다. 전체 해시 하나만 보면 어긋났다는
        // 사실만 남고 위치가 사라진다 — 그러면 원인을 추측으로 골라야 한다.
        let got: [u8; 32] = Sha256::digest(&back[..n]).into();
        match blocks.get(index) {
            Some(want) if *want == got => {}
            Some(_) => return Err(SinkError::VerifyMismatch { at }),
            // 기록된 블록 수보다 많이 읽고 있다면 길이 계산이 어긋난 것이다.
            None => return Err(SinkError::VerifyMismatch { at }),
        }
        index += 1;

        pos += n as u64;

        let now = std::time::Instant::now();
        rep.update(
            pos,
            Some(bytes),
            now.duration_since(last_v).as_secs_f64(),
            n as u64,
        );
        last_v = now;
    }

    // 블록이 전부 맞았는데 전체가 다르면 블록 경계 밖에서 어긋난 것이다.
    // 여기까지 오면 안 되지만, 조용히 통과시키느니 남은 범위를 짚어 준다.
    if whole.finalize().as_slice() != expected.as_slice() {
        return Err(SinkError::VerifyMismatch { at: pos });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::DiskInfo;
    use crate::core::pipeline::NeverCancel;
    use crate::device::fake::{FakeEnumerator, FakeWriter};
    use crate::device::{RawWriter, UsbEnumerator};
    use std::io::Cursor;

    fn disk() -> DiskInfo {
        FakeEnumerator::sample()
            .list_disks()
            .unwrap()
            .into_iter()
            .find(|d| d.number == 2)
            .expect("표본에 USB 가 있어야 한다")
    }

    #[test]
    fn an_empty_source_is_a_failure_not_a_silent_success() {
        // 여기서 Ok 를 돌려주면 사용자는 **비워진 USB 를 손에 쥔 채** "성공했고
        // 검증까지 됐다" 를 보게 된다. 이 지점에는 이미 준비 단계가 대상의
        // 파티션 테이블을 지운 뒤이고, verify 는 bytes == 0 이라 빈 해시끼리
        // 비교해 통과하기 때문이다.
        let w = FakeWriter::new(16 * 1024 * 1024, 512);
        let mut s = w.open(&disk()).unwrap();
        let mut rep = ProgressReporter::new(|_| {});
        let out = stream(
            &mut Cursor::new(Vec::new()),
            s.as_mut(),
            Some(0),
            &NeverCancel,
            &mut rep,
        );
        assert!(matches!(out, Err(SinkError::EmptySource)));
        assert!(w.write_offsets().is_empty());
    }

    #[test]
    fn a_stream_shorter_than_the_holdback_reports_what_the_device_actually_holds() {
        // 700 바이트를 512 섹터 장치에 쓰면 장치에는 1024 바이트가 놓인다.
        // 700 을 보고하면 검증 범위와 꼬리 지우기 시작점이 함께 어긋난다.
        let w = FakeWriter::new(16 * 1024 * 1024, 512);
        let mut s = w.open(&disk()).unwrap();
        let mut rep = ProgressReporter::new(|_| {});
        let out = stream(
            &mut Cursor::new(vec![0x5Au8; 700]),
            s.as_mut(),
            Some(700),
            &NeverCancel,
            &mut rep,
        )
        .unwrap();
        assert_eq!(out.bytes, 1024);
    }

    #[test]
    fn a_short_write_is_not_reported_as_a_lying_device() {
        // 길이만 패딩에 맞추고 해시에서 패딩을 빼면 되읽기 대조가 실패해서,
        // 멀쩡한 쓰기가 "이 USB 는 쓰기를 거짓 보고한다" 로 뜬다.
        let w = FakeWriter::new(16 * 1024 * 1024, 512);
        let mut s = w.open(&disk()).unwrap();
        let mut rep = ProgressReporter::new(|_| {});
        let out = stream(
            &mut Cursor::new(vec![0x5Au8; 700]),
            s.as_mut(),
            Some(700),
            &NeverCancel,
            &mut rep,
        )
        .unwrap();

        let mut rep2 = ProgressReporter::new(|_| {});
        verify(
            s.as_mut(),
            out.bytes,
            &out.hash,
            &out.blocks,
            &NeverCancel,
            &mut rep2,
        )
        .expect("멀쩡한 쓰기가 불량으로 보고됐다");
    }

    #[test]
    fn a_mismatch_says_which_block_went_wrong() {
        // 위치를 말하지 못하는 검증 때문에 원인을 두 번 추측해야 했다.
        // 본문 한가운데를 건드리면 그 블록을 짚어야 한다.
        let w = FakeWriter::new(16 * 1024 * 1024, 512);
        let mut s = w.open(&disk()).unwrap();
        let mut rep = ProgressReporter::new(|_| {});
        let payload: Vec<u8> = (0..(5 * BLOCK as usize)).map(|i| (i % 251) as u8).collect();
        let out = stream(
            &mut Cursor::new(payload),
            s.as_mut(),
            None,
            &NeverCancel,
            &mut rep,
        )
        .unwrap();

        // 3번 블록 안의 한 섹터를 뒤집는다.
        let corrupt_at = 3 * BLOCK + 4096;
        let mut sector = vec![0u8; 512];
        s.read_at(corrupt_at, &mut sector).unwrap();
        sector[0] ^= 0xFF;
        s.write_at(corrupt_at, &sector).unwrap();
        s.commit().unwrap();

        let mut rep2 = ProgressReporter::new(|_| {});
        let err = verify(
            s.as_mut(),
            out.bytes,
            &out.hash,
            &out.blocks,
            &NeverCancel,
            &mut rep2,
        )
        .unwrap_err();
        assert!(
            matches!(err, SinkError::VerifyMismatch { at } if at == 3 * BLOCK),
            "어긋난 블록을 짚지 못했다: {err:?}"
        );
    }

    #[test]
    fn a_mismatch_in_the_partition_table_points_at_offset_zero() {
        // 0번 블록은 홀드백이 마지막에 놓는 파티션 테이블 구간이다. 여기가
        // 어긋났다는 것은 장치 불량이 아니라 누군가 그 구간을 건드렸다는 뜻이고,
        // 그 구분이 지금 우리에게 필요한 정보다.
        let w = FakeWriter::new(16 * 1024 * 1024, 512);
        let mut s = w.open(&disk()).unwrap();
        let mut rep = ProgressReporter::new(|_| {});
        let payload: Vec<u8> = (0..(3 * BLOCK as usize)).map(|i| (i % 251) as u8).collect();
        let out = stream(
            &mut Cursor::new(payload),
            s.as_mut(),
            None,
            &NeverCancel,
            &mut rep,
        )
        .unwrap();

        // 윈도우가 MBR 디스크 서명 자리를 덮어쓴 상황을 흉내낸다.
        let mut first = vec![0u8; 512];
        s.read_at(0, &mut first).unwrap();
        first[440..444].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        s.write_at(0, &first).unwrap();
        s.commit().unwrap();

        let mut rep2 = ProgressReporter::new(|_| {});
        let err = verify(
            s.as_mut(),
            out.bytes,
            &out.hash,
            &out.blocks,
            &NeverCancel,
            &mut rep2,
        )
        .unwrap_err();
        assert!(
            matches!(err, SinkError::VerifyMismatch { at: 0 }),
            "파티션 테이블 훼손이 0 번 블록으로 보고되지 않았다: {err:?}"
        );
    }
}
