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
            // 이미지가 3GB 대라 전체 타임아웃을 걸면 느린 회선에서 끊긴다.
            // 연결 단계에만 제한을 둔다.
            .connect_timeout(Duration::from_secs(20))
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

    fn download(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Vec<u8>, String> {
        let mut res = self.client.get(url).send().map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("내려받기에 실패했습니다 ({})", res.status()));
        }

        // Content-Length 가 없을 수 있다. 그 경우 진행률은 불확정으로 표시된다.
        let total = res.content_length();
        let mut out = Vec::with_capacity(total.unwrap_or(0) as usize);
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;

        loop {
            let n = res.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            done += n as u64;
            on_progress(done, total);
        }
        Ok(out)
    }

    fn open_decompressed(&self, data: Vec<u8>, name: &str) -> Result<Box<dyn Read + Send>, String> {
        let lower = name.to_ascii_lowercase();

        if lower.ends_with(".gz") {
            return Ok(Box::new(flate2::read::GzDecoder::new(
                std::io::Cursor::new(data),
            )));
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
            let mut out = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut out).map_err(|e| e.to_string())?;
            return Ok(Box::new(std::io::Cursor::new(out)));
        }

        Err(format!("알 수 없는 압축 형식입니다: {name}"))
    }
}
