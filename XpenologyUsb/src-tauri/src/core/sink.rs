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

/// 쓰기 결과.
pub struct SinkOutcome {
    /// 실제로 쓴 바이트 수 (섹터 배수로 올림된 값).
    pub bytes: u64,
    /// 쓴 내용의 SHA-256. 검증할 때 되읽은 것과 대조한다.
    pub hash: [u8; 32],
}

/// 쓰기 실패 원인.
#[derive(Debug)]
pub enum SinkError {
    /// 원본 스트림을 읽지 못했다.
    Source(String),
    Device(DeviceError),
    TooSmall {
        need: u64,
        have: u64,
    },
    VerifyMismatch,
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
    let mut write_hasher = Sha256::new();

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
            write_hasher.update(&buf[..take]);
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
        write_hasher.update(&buf[..padded]);
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

    // 보류해 둔 맨 앞을 이제 쓴다. 여기서 비로소 장치에 유효한 파티션 테이블이
    // 생기지만, 나머지는 이미 다 쓰인 뒤라 윈도우가 볼륨을 마운트해도 늦다.
    if !holdback.is_empty() {
        let padded = holdback.len().div_ceil(sector) * sector;
        holdback.resize(padded, 0);
        session.write_at(0, &holdback)?;
    }

    Ok(SinkOutcome {
        bytes: offset,
        hash: write_hasher.finalize().into(),
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
    cancel: &dyn Cancel,
    rep: &mut ProgressReporter<F>,
) -> Result<(), SinkError> {
    let sector = session.sector_size() as usize;
    let mut back = vec![0u8; CHUNK];
    let mut hasher = Sha256::new();
    let mut pos = 0u64;
    let mut last_v = std::time::Instant::now();

    while pos < bytes {
        if cancel.is_canceled() {
            return Err(SinkError::Canceled);
        }
        let n = (CHUNK as u64).min(bytes - pos) as usize;
        // 읽기도 섹터 배수여야 한다.
        let n = (n / sector) * sector;
        if n == 0 {
            break;
        }
        session.read_at(pos, &mut back[..n])?;
        hasher.update(&back[..n]);
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

    if hasher.finalize().as_slice() != expected.as_slice() {
        return Err(SinkError::VerifyMismatch);
    }
    Ok(())
}
