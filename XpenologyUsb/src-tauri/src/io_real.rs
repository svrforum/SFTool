//! 실제 네트워크와 압축 해제.
//!
//! [`crate::core::runner::Io`] 의 구현. 이 파일만 바깥 세상과 이야기한다.
//! 테스트에서는 통째로 가짜로 대체된다.

use crate::core::loader::Release;
use crate::core::runner::Io;
use std::io::Read;
use std::time::Duration;

/// GitHub 이 요구하는 User-Agent.
///
/// 없으면 API 가 403 을 돌려준다. 무엇이 호출하는지 알아볼 수 있게 적는다.
const USER_AGENT: &str = concat!("XpenologyUsb/", env!("CARGO_PKG_VERSION"));

pub struct RealIo {
    client: reqwest::blocking::Client,
}

impl RealIo {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            // **전체 타임아웃을 명시적으로 끈다.**
            //
            // reqwest 의 블로킹 클라이언트는 기본 30초 타임아웃을 갖고 있고,
            // `connect_timeout` 을 설정해도 그것은 지워지지 않는다. 게다가 그
            // 제한은 읽기 호출마다 적용돼서, 600MB~1.3GB 를 받는 도중 Wi-Fi 가
            // 잠깐 끊기거나 절전에서 깨어나는 것만으로 전송이 통째로 버려졌다.
            // 앞서 이 자리에 "연결 단계에만 제한을 둔다"고 적어뒀는데 사실이
            // 아니었다.
            .timeout(None)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

impl Io for RealIo {
    fn fetch_releases(&self, url: &str) -> Result<Vec<Release>, String> {
        let res = self.client.get(url).send().map_err(|e| e.to_string())?;

        // 60회/시간 제한은 IP 단위라, 공유 회선에서는 이미 소진된 상태로 도착할 수 있다.
        // 일반 네트워크 오류와 구분해 안내해야 사용자가 무엇을 해야 할지 안다.
        if res.status().as_u16() == 403 {
            let remaining = res
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if remaining == "0" {
                return Err(
                    "GitHub API 호출 한도를 초과했습니다. 잠시 후 다시 시도해 주세요.".into(),
                );
            }
        }
        if !res.status().is_success() {
            return Err(format!(
                "릴리스 목록을 가져오지 못했습니다 ({})",
                res.status()
            ));
        }

        res.json::<Vec<Release>>().map_err(|e| e.to_string())
    }

    /// 이미지를 내려받는다.
    ///
    /// 끊기면 **받은 지점부터 이어받는다.** 예전에는 한 번의 연결로 끝까지 받지
    /// 못하면 이미 받은 것을 통째로 버리고 처음부터 다시 받았다. 1.3GB 를
    /// 95% 까지 받은 뒤 잠깐 끊기는 것으로 전부 날아갔다.
    fn download(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
        should_stop: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, String> {
        const MAX_ATTEMPTS: u32 = 5;

        let mut out: Vec<u8> = Vec::new();
        let mut total: Option<u64> = None;
        let mut last_err = String::new();

        for attempt in 0..MAX_ATTEMPTS {
            if should_stop() {
                return Err("취소됨".into());
            }
            let mut req = self.client.get(url);
            if !out.is_empty() {
                // 이미 받은 만큼은 건너뛴다.
                req = req.header("Range", format!("bytes={}-", out.len()));
            }

            let mut res = match req.send() {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    continue;
                }
            };

            let status = res.status().as_u16();
            if status == 200 && !out.is_empty() {
                // 서버가 이어받기를 무시하고 처음부터 보낸다. 받은 것을 버리고
                // 새로 채운다 — 이어 붙이면 파일이 망가진다.
                out.clear();
            } else if status != 200 && status != 206 {
                return Err(format!("내려받기에 실패했습니다 (HTTP {status})"));
            }

            // 전체 크기. 이어받는 중이면 남은 양만 알려주므로 이미 받은 만큼을 더한다.
            if total.is_none() {
                total = res.content_length().map(|c| c + out.len() as u64);
            }

            let mut buf = vec![0u8; 256 * 1024];
            let mut stalled = false;
            loop {
                match res.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        out.extend_from_slice(&buf[..n]);
                        on_progress(out.len() as u64, total);
                        // 청크마다 확인한다. 256KB 단위라 취소가 즉시 반응한다.
                        if should_stop() {
                            return Err("취소됨".into());
                        }
                    }
                    Err(e) => {
                        last_err = e.to_string();
                        stalled = true;
                        break;
                    }
                }
            }

            if !stalled {
                // 전체 크기를 알고 있는데 모자라면 아직 안 끝난 것이다.
                match total {
                    Some(t) if (out.len() as u64) < t => {
                        last_err = format!("연결이 끊겼습니다 ({}/{} 바이트)", out.len(), t);
                    }
                    _ => return Ok(out),
                }
            }

            if attempt + 1 < MAX_ATTEMPTS {
                std::thread::sleep(Duration::from_secs(2));
            }
        }

        Err(format!(
            "내려받기를 {MAX_ATTEMPTS}회 시도했지만 완료하지 못했습니다: {last_err}"
        ))
    }

    fn open_decompressed(
        &self,
        data: Vec<u8>,
        name: &str,
    ) -> Result<(Box<dyn Read + Send>, Option<u64>), String> {
        let lower = name.to_ascii_lowercase();

        if lower.ends_with(".gz") {
            let size = gzip_expanded_size(&data, &lower);
            // MultiGzDecoder 를 쓴다. GzDecoder 는 **첫 번째 gzip 멤버에서 멈추고
            // EOF 를 보고한다.** 여러 멤버로 이어붙인 파일이면 이미지가 조용히
            // 잘리는데, 검증은 "쓴 것과 장치에 있는 것"만 비교하므로 잘린 채로도
            // 통과한다. 그러면 부팅되지 않는 USB 를 받아들고 원인을 가리킬
            // 단서가 하나도 남지 않는다.
            return Ok((
                Box::new(flate2::read::MultiGzDecoder::new(std::io::Cursor::new(
                    data,
                ))),
                size,
            ));
        }

        if lower.ends_with(".zip") {
            let cursor = std::io::Cursor::new(data);
            let mut archive = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;

            // 안에서 이미지 항목을 찾는다. RR 은 `rr.img` 라는 고정 이름을 쓰지만,
            // 이름을 단정하지 않고 확장자로 고른다 — 로더 저장소는 파일명을 바꾼 전례가 있다.
            let idx = (0..archive.len())
                .find(|i| {
                    archive
                        .by_index_raw(*i)
                        .map(|f| f.name().to_ascii_lowercase().ends_with(".img"))
                        .unwrap_or(false)
                })
                .ok_or_else(|| "압축 파일 안에 .img 가 없습니다".to_string())?;

            // ZipArchive 는 빌린 상태로 반환할 수 없어 통째로 풀어 담는다.
            // 이미지가 3GB 대라 메모리를 크게 쓰지만, 임시 파일을 만드는 것보다
            // 디스크 여유가 없는 기계에서 안전하다.
            let mut f = archive.by_index(idx).map_err(|e| e.to_string())?;
            // zip 중앙 디렉터리에는 푼 크기가 정확히 들어 있다.
            let size = f.size();
            let mut out = Vec::with_capacity(size as usize);
            f.read_to_end(&mut out).map_err(|e| e.to_string())?;
            return Ok((Box::new(std::io::Cursor::new(out)), Some(size)));
        }

        Err(format!("알 수 없는 압축 형식입니다: {name}"))
    }
}

/// gzip 파일의 압축 해제 후 크기.
///
/// gzip 은 마지막 4바이트(ISIZE)에 원본 크기를 담지만 **2^32 로 나머지 연산된
/// 값**이다. 4GiB 를 넘는 이미지는 실제보다 작게 나온다.
///
/// m-shell 표준판은 약 3.03GB 라 그대로 맞지만, 기본으로 고르는 `-5GB` 변형은
/// 약 4.98GB 여서 4GiB 만큼 작게 읽힌다. 에셋 이름으로 그 경우를 알아내 보정한다.
/// 진행률 표시에만 쓰는 값이라 어긋나도 치명적이지 않고, 실제로 넘어서면
/// 호출부가 불확정 표시로 되돌린다.
fn gzip_expanded_size(data: &[u8], lower_name: &str) -> Option<u64> {
    if data.len() < 4 {
        return None;
    }
    let tail = &data[data.len() - 4..];
    let isize_val = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as u64;
    if isize_val == 0 {
        return None;
    }
    // 4GiB 이상인 변형은 ISIZE 가 한 바퀴 돈다.
    const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;
    if lower_name.contains("-5gb") && isize_val < FOUR_GIB {
        return Some(isize_val + FOUR_GIB);
    }
    Some(isize_val)
}
