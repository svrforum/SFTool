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

/// 재시도 사이에 쉬는 시간.
///
/// 테스트에서는 짧게 둔다. 쉬는 **길이**는 검증 대상이 아니고, 쉬기는 하는지가
/// 대상이다. 진짜로 2초씩 쉬면 시험 한 번이 수십 초가 된다.
#[cfg(not(test))]
const RETRY_BACKOFF: Duration = Duration::from_secs(2);
#[cfg(test)]
const RETRY_BACKOFF: Duration = Duration::from_millis(150);

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
        /// 연속으로 한 발짝도 못 나간 횟수의 한도.
        ///
        /// 예전에는 이것이 **전송 전체**의 예산이었다. 그래서 200MB 마다 끊기는
        /// 회선에서는 매 시도가 제대로 이어받아 앞으로 나아가는데도 다섯 번째에
        /// 포기하고 이미 받은 1GB 를 버렸다. 세어야 하는 것은 끊긴 횟수가 아니라
        /// **소득 없는** 시도의 횟수다.
        const MAX_FRUITLESS: u32 = 5;
        /// 전체 시도 횟수의 한도. 진척이 있으면 위 예산이 되살아나므로,
        /// 서버가 계속 이상하게 굴 때 무한히 도는 것만 막는 뒷문이다.
        const MAX_ATTEMPTS: u32 = 60;

        let mut out: Vec<u8> = Vec::new();
        let mut last_err = String::new();
        // 지금까지 도달해 본 최대 길이. 쓸 수 없는 응답을 버리고 다시 받는 일이
        // 있어서, 현재 길이만 보면 같은 자리를 오가는 것도 진척으로 세게 된다.
        let mut best = 0usize;
        let mut fruitless = 0u32;

        for attempt in 0..MAX_ATTEMPTS {
            if fruitless >= MAX_FRUITLESS {
                break;
            }
            if should_stop() {
                return Err("취소됨".into());
            }
            if attempt > 0 {
                // **연결 자체가 실패한 경우에도** 쉰다. 예전에는 이 잠이 반복문
                // 맨 끝에 있었고 send() 실패는 `continue` 로 건너뛰어서, 다섯 번의
                // 시도가 수십 밀리초 만에 다 타버렸다. Wi-Fi 가 1초 끊기는 것만으로
                // 이미 받아둔 1.2GB 가 날아간 이유가 이것이다.
                std::thread::sleep(RETRY_BACKOFF);
            }

            'attempt: {
                let mut req = self.client.get(url);
                if !out.is_empty() {
                    // 이미 받은 만큼은 건너뛴다.
                    req = req.header("Range", format!("bytes={}-", out.len()));
                }

                let mut res = match req.send() {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = e.to_string();
                        break 'attempt;
                    }
                };

                let status = res.status().as_u16();
                if status != 200 && status != 206 {
                    return Err(format!("내려받기에 실패했습니다 (HTTP {status})"));
                }

                // 206 이 **무엇을** 담고 있는지 Content-Range 로 확인한다.
                //
                // 예전에는 206 이면 무조건 "요청한 지점부터의 나머지" 라고 믿고
                // 이어 붙였다. Range 를 잘못 다루는 중간 장비(투명 캐시, HTTPS 를
                // 가로채는 백신)는 파일 전체나 엉뚱한 구간을 206 으로 돌려준다.
                // 그걸 붙이면 몸통이 조용히 망가지는데, 완성 검사가 "모자라지
                // 않은가" 만 보기 때문에 길어진 몸통은 그대로 통과해 굽기까지 갔다.
                // 전체 크기는 이번 응답에서 매번 새로 읽는다. 시도마다 서버가
                // 다른 말을 할 수 있으므로 앞선 시도의 값을 들고 있지 않는다.
                let total: Option<u64> = if status == 200 {
                    // 서버가 이어받기를 무시하고 처음부터 보낸다. 받은 것을 버리고
                    // 새로 채운다 — 이어 붙이면 파일이 망가진다.
                    out.clear();
                    res.content_length()
                } else {
                    let Some((start, instance)) = res
                        .headers()
                        .get("content-range")
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_content_range)
                    else {
                        // 어디서부터인지 말해주지 않는 206 은 붙일 자리를 알 수 없다.
                        last_err = "서버가 Content-Range 없이 206 을 보냈습니다".into();
                        break 'attempt;
                    };
                    if start != out.len() as u64 {
                        last_err = format!(
                            "서버가 {start} 바이트부터 보냈습니다 (요청한 자리: {})",
                            out.len()
                        );
                        // 이 응답은 쓸 수 없다. 붙이지 말고 처음부터 다시 받는다.
                        out.clear();
                        break 'attempt;
                    }
                    // 전체 크기는 Content-Range 의 뒷부분이 말해준다. 이어받는
                    // 중이면 Content-Length 는 **남은 양**이라 전체가 아니다.
                    instance.or_else(|| res.content_length().map(|c| c + start))
                };

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
                if stalled {
                    break 'attempt;
                }

                match total {
                    // 아직 안 끝났다. 다음 시도에서 이어받는다.
                    Some(t) if (out.len() as u64) < t => {
                        last_err = format!("연결이 끊겼습니다 ({}/{t} 바이트)", out.len());
                    }
                    // 알려준 크기보다 많이 왔다 — 붙이면 안 될 것을 붙였다는 뜻이다.
                    // 그대로 돌려주면 압축 해제가 깨지는데, 그때는 이미 USB 를
                    // 지운 뒤라 사용자는 멀쩡한 USB 를 잃고 "압축 해제 실패" 를 본다.
                    Some(t) if (out.len() as u64) > t => {
                        last_err = format!(
                            "받은 양이 알려준 크기보다 많습니다 ({}/{t} 바이트)",
                            out.len()
                        );
                        out.clear();
                    }
                    Some(_) => return Ok(out),
                    // **전체 크기를 모르면 다 받았는지 알 수 없다.**
                    //
                    // 깨끗하게 끊긴 연결과 정상 종료가 구별되지 않는다. 이걸
                    // 성공으로 처리하면, 멤버 경계에서 잘린 multi-member gzip 은
                    // 오류 없이 풀려서 **부팅되지 않는 이미지가 "검증 완료" 로
                    // 나간다.** 여기서 다루는 에셋들이 정확히 그 모양이다.
                    None => {
                        last_err =
                            "서버가 전체 크기를 알려주지 않아 다 받았는지 확인할 수 없습니다"
                                .into();
                    }
                }
            }

            // 이번 시도가 실제로 앞으로 나아갔는가.
            if out.len() > best {
                best = out.len();
                fruitless = 0;
            } else {
                fruitless += 1;
            }
        }

        Err(format!(
            "내려받기를 여러 번 시도했지만 완료하지 못했습니다: {last_err}"
        ))
    }

    /// 체크섬 파일 하나를 받는다.
    ///
    /// 이어받기도 진행률도 없다. RR 이 올리는 sha256sum 은 400바이트대라
    /// 한 번에 받지 못하면 그냥 실패로 두는 편이 낫다 — 호출부가 이 실패를
    /// 굽기를 막는 이유로 쓰지 않기 때문이다.
    fn fetch_text(&self, url: &str) -> Result<String, String> {
        let res = self.client.get(url).send().map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("체크섬을 가져오지 못했습니다 ({})", res.status()));
        }
        res.text().map_err(|e| e.to_string())
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
            //
            // **앞에서 첫 번째가 아니라 가장 큰 것을 고른다.** 앞에서부터 고르면
            // macOS 가 끼워 넣는 `__MACOSX/._rr.img` 같은 4KB 짜리 껍데기나
            // 함께 실린 작은 grub 이미지가 뽑힌다. 그 항목도 CRC 는 멀쩡해서
            // 어디에서도 오류가 나지 않고, 4KB 를 구운 뒤 나머지를 0 으로 지우고
            // **"성공, 검증 완료"까지 보여준다.** 지금 에셋에 `.img` 가 하나뿐이라
            // 드러나지 않을 뿐, 항목이 하나 늘어나는 순간 그렇게 된다.
            let idx = (0..archive.len())
                .filter_map(|i| {
                    let f = archive.by_index_raw(i).ok()?;
                    let name = f.name().to_ascii_lowercase();
                    if f.is_dir() || name.starts_with("__macosx/") || !name.ends_with(".img") {
                        return None;
                    }
                    Some((f.size(), i))
                })
                .max()
                .map(|(_, i)| i)
                .ok_or_else(|| "압축 파일 안에 .img 가 없습니다".to_string())?;

            // ZipArchive 는 빌린 상태로 반환할 수 없어 통째로 풀어 담는다.
            // 이미지가 3GB 대라 메모리를 크게 쓰지만, 임시 파일을 만드는 것보다
            // 디스크 여유가 없는 기계에서 안전하다.
            let mut f = archive.by_index(idx).map_err(|e| e.to_string())?;
            // zip 중앙 디렉터리에는 푼 크기가 정확히 들어 있다.
            let size = f.size();
            // 다만 그 값은 **아직 CRC 로 확인되지 않은 남의 말**이다. 그대로
            // `with_capacity` 에 넘기면 말도 안 되는 크기를 요구했을 때 러스트가
            // 할당 실패로 프로세스를 죽인다 — 창이 오류 한 줄 없이 사라진다.
            let mut out = Vec::new();
            out.try_reserve_exact(size as usize).map_err(|_| {
                format!("이미지를 담을 메모리가 모자랍니다 ({size} 바이트가 필요합니다)")
            })?;
            f.read_to_end(&mut out).map_err(|e| e.to_string())?;
            return Ok((Box::new(std::io::Cursor::new(out)), Some(size)));
        }

        Err(format!("알 수 없는 압축 형식입니다: {name}"))
    }
}

/// `Content-Range: bytes 40000-99999/100000` 에서 (시작 위치, 전체 크기)를 꺼낸다.
///
/// 전체 크기를 `*` 로 적는 서버가 있어서 그쪽은 None 으로 둔다. 시작 위치는
/// 없으면 응답을 쓸 수 없으므로 전체를 None 으로 돌려 거절하게 한다.
fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, len) = rest.split_once('/')?;
    let start = range.split('-').next()?.trim().parse().ok()?;
    Some((start, len.trim().parse().ok()))
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
    //
    // `isize_val < FOUR_GIB` 를 함께 보던 시절이 있었는데, ISIZE 는 u32 라
    // 그 조건은 언제나 참이었다. 검사하는 척만 하는 조건은 읽는 사람을 속인다.
    const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;
    if lower_name.contains("-5gb") {
        return Some(isize_val + FOUR_GIB);
    }
    Some(isize_val)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// 요청 하나마다 대본대로 답하고 연결을 닫는 소켓 서버.
    ///
    /// 실제 소켓을 쓰는 이유는, 여기서 잡으려는 것이 전부 **서버나 중간 장비가
    /// 잘못 굴었을 때 우리가 무엇을 믿어버리는가** 이기 때문이다. reqwest 를
    /// 흉내낸 가짜로는 Range·Content-Range·본문 길이의 어긋남이 재현되지 않는다.
    ///
    /// 대본은 (몇 번째 요청인가, Range 로 요청한 시작 위치)를 받아 소켓에 그대로
    /// 실을 바이트를 돌려준다. 헤더까지 직접 쓰게 두어야 잘못된 응답을 만들 수 있다.
    fn serve(script: impl Fn(u32, Option<u64>) -> Vec<u8> + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(mut s) = stream else { break };
                // 요청 헤더를 빈 줄까지 읽어 Range 시작 위치만 뽑는다.
                let mut range = None;
                {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut line = String::new();
                    while r.read_line(&mut line).unwrap_or(0) > 0 {
                        if line == "\r\n" || line == "\n" {
                            break;
                        }
                        let lower = line.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("range: bytes=") {
                            range = v.split('-').next().and_then(|x| x.trim().parse().ok());
                        }
                        line.clear();
                    }
                }
                let _ = s.write_all(&script(n as u32, range));
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Write);
            }
        });
        format!("http://{addr}/asset")
    }

    fn head(extra: &str) -> Vec<u8> {
        format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{extra}\r\n\r\n").into_bytes()
    }

    fn body(from: usize, to: usize) -> Vec<u8> {
        (from..to).map(|i| (i % 251) as u8).collect()
    }

    fn get(url: &str) -> Result<Vec<u8>, String> {
        RealIo::new()
            .unwrap()
            .download(url, &mut |_, _| {}, &|| false)
    }

    /// 정상 서버에서 이어받기가 계속 동작하는지. 아래 검사들이 이 길을
    /// 막아버리면 1.3GB 를 95% 까지 받고 끊겼을 때 처음부터 다시 받게 된다.
    #[test]
    fn a_well_behaved_resume_still_completes() {
        let url = serve(|n, range| {
            let start = range.unwrap_or(0) as usize;
            if n == 0 {
                let mut v = head("Content-Length: 100000\r\nAccept-Ranges: bytes");
                v.extend_from_slice(&body(0, 40000));
                return v;
            }
            let mut v = format!(
                "HTTP/1.1 206 Partial Content\r\nConnection: close\r\n\
                 Content-Range: bytes {start}-99999/100000\r\nContent-Length: {}\r\n\r\n",
                100000 - start
            )
            .into_bytes();
            v.extend_from_slice(&body(start, 100000));
            v
        });

        let got = get(&url).expect("정상적인 이어받기가 실패했다");
        assert_eq!(got, body(0, 100000), "이어붙인 내용이 원본과 다르다");
    }

    /// 이어받기 응답이 요청한 위치에서 시작하지 않으면 **이어 붙이면 안 된다.**
    ///
    /// Range 를 잘못 다루는 중간 장비는 206 에 파일 전체를 실어 보낸다. 그걸
    /// 그대로 붙이면 몸통이 140% 가 되고, 길이 검사가 "모자라지 않은가" 만
    /// 보기 때문에 그 상태로 성공 처리된다.
    #[test]
    fn a_partial_response_that_restarts_from_zero_is_not_spliced_on() {
        let url = serve(|n, _| {
            if n == 0 {
                let mut v = head("Content-Length: 100000\r\nAccept-Ranges: bytes");
                v.extend_from_slice(&body(0, 40000));
                return v;
            }
            if n == 1 {
                // Range 를 무시하고 처음부터 전부 보낸다.
                let mut v = b"HTTP/1.1 206 Partial Content\r\nConnection: close\r\n\
                     Content-Range: bytes 0-99999/100000\r\nContent-Length: 100000\r\n\r\n"
                    .to_vec();
                v.extend_from_slice(&body(0, 100000));
                return v;
            }
            let mut v = head("Content-Length: 100000");
            v.extend_from_slice(&body(0, 100000));
            v
        });

        let got = get(&url).expect("결국은 받아내야 한다");
        assert_eq!(got, body(0, 100000), "겹쳐 붙인 몸통을 그대로 돌려줬다");
    }

    /// 어긋난 206 의 몸통이 **남은 양과 정확히 같으면** 총량이 맞아떨어진다.
    ///
    /// 위 검사만으로는 시작 위치를 실제로 보는지 알 수 없다. 거기서는 이어붙인
    /// 몸통이 140% 가 되어 **총량 초과 검사**가 먼저 걸러내기 때문에, 시작 위치를
    /// 아예 보지 않아도 통과한다. 실제로 리버트해 보니 그랬다 — 시작 위치 검사를
    /// 통째로 들어내도 그 검사는 초록불이었다.
    ///
    /// 총량이 딱 맞는 이 모양에서만 드러난다. 받은 것은 40000 바이트 뒤에 다시
    /// 0 부터 시작하는 몸통이라 내용은 엉망인데, 크기는 정확히 100000 이다.
    /// 크기만 보는 코드는 이것을 완성본으로 넘긴다.
    #[test]
    fn a_resume_that_starts_somewhere_else_is_not_spliced_on() {
        let url = serve(|n, range| {
            let start = range.unwrap_or(0) as usize;
            if n == 0 {
                let mut v = head("Content-Length: 100000\r\nAccept-Ranges: bytes");
                v.extend_from_slice(&body(0, 40000));
                return v;
            }
            if n == 1 {
                // 40000 부터 달라고 했는데 0 부터 보낸다. 길이는 남은 양과 같다.
                let mut v = b"HTTP/1.1 206 Partial Content\r\nConnection: close\r\n\
                     Content-Range: bytes 0-59999/100000\r\nContent-Length: 60000\r\n\r\n"
                    .to_vec();
                v.extend_from_slice(&body(0, 60000));
                return v;
            }
            let mut v = format!(
                "HTTP/1.1 206 Partial Content\r\nConnection: close\r\n\
                 Content-Range: bytes {start}-99999/100000\r\nContent-Length: {}\r\n\r\n",
                100000 - start
            )
            .into_bytes();
            v.extend_from_slice(&body(start, 100000));
            v
        });

        let got = get(&url).expect("결국은 받아내야 한다");
        assert_eq!(
            got,
            body(0, 100000),
            "요청한 위치에서 시작하지 않는 몸통을 이어 붙였다"
        );
    }

    /// Range 를 보내지 않았는데 206 이 오면, 그 몸통은 파일 전체가 아니다.
    ///
    /// 전체 크기를 그 응답의 Content-Length 에서 가져오면 "받은 만큼이 전부" 가
    /// 되어 40% 짜리 파일이 완성본으로 통과한다. Content-Range 가 진짜 크기를
    /// 말해주고 있는데도 읽지 않았다.
    #[test]
    fn an_unsolicited_partial_response_is_not_treated_as_the_whole_file() {
        let url = serve(|_, _| {
            let mut v = b"HTTP/1.1 206 Partial Content\r\nConnection: close\r\n\
                 Content-Range: bytes 0-39999/100000\r\nContent-Length: 40000\r\n\r\n"
                .to_vec();
            v.extend_from_slice(&body(0, 40000));
            v
        });

        let err = get(&url).unwrap_err();
        assert!(!err.is_empty(), "40% 짜리 몸통을 완성본으로 받아들였다");
    }

    /// 전체 크기를 모르면 다 받았는지 알 수 없다. 알 수 없는 것을 성공으로
    /// 처리하면, 멤버 경계에서 잘린 multi-member gzip 은 오류 없이 풀려서
    /// **부팅되지 않는 USB 가 "성공"으로 보고된다.**
    #[test]
    fn a_body_of_unknown_length_is_never_reported_as_complete() {
        let url = serve(|_, _| {
            let mut v = head("Content-Type: application/octet-stream");
            v.extend_from_slice(&body(0, 40000));
            v
        });

        let err = get(&url).unwrap_err();
        assert!(!err.is_empty(), "길이를 모르는 몸통을 완성본으로 돌려줬다");
    }

    /// 연결 자체가 안 될 때도 재시도 사이에 쉬어야 한다.
    ///
    /// send() 실패에서 곧장 `continue` 하면 다섯 번의 시도가 수십 밀리초 만에
    /// 다 타버린다. Wi-Fi 가 1초만 끊겨도 1.2GB 를 이미 받아둔 전송이 통째로
    /// 버려지는 이유가 이것이었다.
    #[test]
    fn a_connect_failure_still_waits_between_attempts() {
        // 열자마자 닫아 반드시 거부되는 포트를 만든다.
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = dead.local_addr().unwrap();
        drop(dead);

        let t = std::time::Instant::now();
        let _ = get(&format!("http://{addr}/asset"));
        let waited = t.elapsed();
        assert!(
            waited >= RETRY_BACKOFF * 4,
            "재시도 사이에 쉬지 않았다: {waited:?}"
        );
    }

    /// 매번 앞으로 나아가는 전송은 몇 번 끊기든 끝까지 가야 한다.
    ///
    /// 시도 횟수를 전송 전체의 예산으로 쓰면, 200MB 마다 끊기는 회선에서
    /// 1.3GB 는 영원히 완성되지 않는다 — 매 시도가 제대로 이어받았는데도.
    #[test]
    fn a_transfer_that_advances_every_time_is_not_given_up_on() {
        const TOTAL: usize = 500_000;
        const STEP: usize = 60_000;
        let url = serve(|_, range| {
            let start = range.unwrap_or(0) as usize;
            let end = (start + STEP).min(TOTAL);
            let mut v = if start == 0 {
                head(&format!("Content-Length: {TOTAL}\r\nAccept-Ranges: bytes"))
            } else {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\n\
                     Content-Range: bytes {start}-{}/{TOTAL}\r\nContent-Length: {}\r\n\r\n",
                    TOTAL - 1,
                    TOTAL - start
                )
                .into_bytes()
            };
            v.extend_from_slice(&body(start, end));
            v
        });

        let got = get(&url).expect("매번 진척이 있었는데 포기했다");
        assert_eq!(got, body(0, TOTAL));
    }

    /// zip 안에 `.img` 가 여럿이면 **큰 쪽**이 이미지다.
    ///
    /// 앞에서부터 첫 번째를 고르면, macOS 가 끼워 넣는 `__MACOSX/._rr.img`
    /// 같은 4KB 짜리 껍데기를 이미지로 착각한다. 그 항목도 CRC 는 멀쩡하므로
    /// 어디서도 오류가 나지 않는다 — 4KB 를 굽고 나머지를 0 으로 지운 뒤
    /// "성공, 검증 완료" 를 보여준다.
    #[test]
    fn the_zip_entry_is_chosen_by_size_not_by_position() {
        use zip::write::SimpleFileOptions;

        let real = body(0, 1_000_000);
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opt = SimpleFileOptions::default();
        w.start_file("__MACOSX/._rr.img", opt).unwrap();
        w.write_all(&[0u8; 4096]).unwrap();
        w.start_file("rr.img", opt).unwrap();
        w.write_all(&real).unwrap();
        let archive = w.finish().unwrap().into_inner();

        let (mut r, size) = RealIo::new()
            .unwrap()
            .open_decompressed(archive, "rr-26.8.1.img.zip")
            .unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();

        assert_eq!(size, Some(real.len() as u64), "고른 항목의 크기가 다르다");
        assert_eq!(out, real, "껍데기 항목을 이미지로 골랐다");
    }
}
