//! 전체 작업 흐름.
//!
//! 해석 → 내려받기 → 압축 해제 → 준비 → 쓰기 → (검증) → 마무리.
//!
//! 입출력은 전부 트레이트나 주입된 함수 뒤에 있어서, 가짜 구현으로
//! 이 흐름 전체를 실제 USB 없이 완주시킬 수 있다.

use super::loader::{Loader, Release, ResolvedImage};
use super::pipeline::ProgressReporter;
// 바깥(lib.rs, 테스트)이 계속 `runner::Cancel` 로 쓸 수 있게 재노출한다.
pub use super::pipeline::{Cancel, NeverCancel};
use super::progress::Stage;
use super::safety::{self, Rejection};
use super::sink::{self, SinkError};
use crate::core::model::DiskInfo;
use crate::device::{DeviceError, RawWriter, WriteSession};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;

/// 32바이트를 16진수 문자열로. 오류 메시지에 두 값을 나란히 보여주기 위한 것이다.
fn hex32(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 작업 실패 원인.
#[derive(Debug)]
pub enum RunError {
    /// 릴리스를 해석하지 못했다.
    Resolve(String),
    /// 내려받기 실패.
    Download(String),
    /// 압축 해제 실패.
    Extract(String),
    /// 안전 규칙에 걸렸다.
    Rejected(Rejection),
    /// 쓰기 직전 확인에서 다른 장치로 판명됐다. 무엇이 어긋났는지 함께 담는다.
    ///
    /// 예전에는 이름뿐이었다. 그러면 사용자는 엉뚱한 USB 를 집은 것인지,
    /// 용량이 30초 만에 달라진 것인지(가짜 용량 USB 의 대표 증상이다)
    /// 구별할 수 없다 — 판정한 쪽은 알고 있었는데도.
    IdentityChanged(String),
    /// 장치 오류.
    Device(DeviceError),
    /// 검증 불일치.
    ///
    /// `at` 은 **처음 어긋난 [`sink::BLOCK`] 의 시작 오프셋**이다. 실제로
    /// 계산된 값이며, 예전처럼 자리를 채우려고 넣은 0 이 아니다.
    ///
    /// 이 값이 있어야 원인을 좁힐 수 있다. `at` 이 0 이면 어긋난 곳은 맨 앞
    /// 1MiB — 즉 홀드백이 마지막에 놓는 파티션 테이블 구간이고, 그건 장치
    /// 불량이 아니라 윈도우가 그 구간을 건드렸다는 뜻이 된다. `at` 이 그보다
    /// 뒤면 본문이 어긋난 것이라 장치 쪽을 의심하는 게 맞다. 둘을 구분하지
    /// 못하는 동안 원인을 추측으로 골라야 했다.
    VerifyMismatch { at: u64 },
    /// **대상이 이미 지워진 뒤에** 실패했다.
    ///
    /// [`crate::device::RawWriter::open`] 은 그 안에서 마운트 지점을 떼고
    /// 파티션 테이블을 지운다. 그래서 그 뒤의 실패는 — 사용자가 누른 취소까지
    /// 포함해 — 원래 내용이 이미 사라진 USB 를 남긴다. 원인만 올리면 화면에는
    /// "취소됨" 이나 "장치가 바뀌었습니다" 만 뜨고, 정작 사용자가 알아야 할
    /// USB 의 상태는 어디에도 나오지 않는다.
    TargetErased { cause: Box<RunError> },
    /// 사용자가 취소했다.
    Canceled,
}

impl RunError {
    /// 대상이 지워진 뒤에 난 실패로 표시한다. 원인은 그대로 안에 남는다.
    fn already_erased(cause: RunError) -> RunError {
        RunError::TargetErased {
            cause: Box::new(cause),
        }
    }
}

impl From<DeviceError> for RunError {
    fn from(e: DeviceError) -> Self {
        RunError::Device(e)
    }
}

impl From<SinkError> for RunError {
    fn from(e: SinkError) -> Self {
        match e {
            SinkError::Source(s) => RunError::Extract(s),
            // 압축을 푼 결과가 비었다는 뜻이다. 내려받기가 빈 본문을 받았거나
            // zip 안의 이미지 항목 크기가 0 인 경우다.
            SinkError::EmptySource => {
                RunError::Extract("압축을 푼 이미지가 비어 있습니다 (0 바이트)".into())
            }
            SinkError::Device(d) => RunError::Device(d),
            SinkError::TooSmall { need, have } => {
                RunError::Rejected(Rejection::TooSmall { need, have })
            }
            SinkError::VerifyMismatch { at } => RunError::VerifyMismatch { at },
            SinkError::Canceled => RunError::Canceled,
        }
    }
}

/// 바깥에서 주입하는 입출력.
///
/// 네트워크와 파일 접근을 트레이트로 빼서, 테스트에서는 고정된 응답과
/// 메모리 버퍼로 대체한다.
pub trait Io {
    /// 릴리스 목록을 가져온다.
    fn fetch_releases(&self, url: &str) -> Result<Vec<Release>, String>;

    /// 이미지를 내려받는다.
    ///
    /// 진행 콜백은 (누적 바이트, 전체 바이트) 를 받는다.
    ///
    /// `should_stop` 이 참을 돌려주면 즉시 중단해야 한다. 이 인자가 없던 시절에는
    /// 취소가 **내려받기 전체가 끝난 뒤에야** 확인돼서, 1.3GB 를 받는 도중
    /// 취소를 눌러도 30분을 기다려야 했다. 사용자는 그냥 강제 종료한다.
    fn download(
        &self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
        should_stop: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, String>;

    /// 압축을 푼 스트림을 연다.
    ///
    /// gz 와 zip 을 모두 다뤄야 한다. zip 은 내부 항목 중 이미지 하나를 고른다.
    ///
    /// 두 번째 값은 **압축을 푼 뒤의 크기**다. 알 수 있으면 돌려준다.
    /// 이게 없으면 쓰기 단계 내내 진행률이 불확정으로 표시돼서, 사용자는
    /// 몇 분 동안 아무 숫자도 없는 막대만 보게 된다.
    fn open_decompressed(
        &self,
        data: Vec<u8>,
        name: &str,
    ) -> Result<(Box<dyn Read + Send>, Option<u64>), String>;

    /// 작은 텍스트 파일 하나를 받는다. 체크섬 파일용이다.
    ///
    /// 이미지 내려받기와 나누는 이유는 진행률·이어받기·취소가 전혀 필요 없기
    /// 때문이다. RR 이 올리는 sha256sum 은 400바이트대다.
    fn fetch_text(&self, url: &str) -> Result<String, String>;
}

/// sha256sum 파일에서 이 에셋의 해시를 찾는다.
///
/// 형식은 `<64자리 16진수>  <파일 이름>` 이 줄마다 하나씩이다. 이름이 맞는 줄을
/// 찾고, 줄이 딱 하나뿐이면 이름을 따지지 않는다 (에셋 이름만 담은 `.sha256`
/// 파일이 그렇다).
///
/// **못 찾으면 None 이고, None 은 검사를 건너뛴다는 뜻이다.** 형식이 낯설다는
/// 이유로 굽기를 막으면, 저장소가 파일 모양을 바꾸는 날 프로그램이 통째로
/// 쓸모없어진다. 우리가 막으려는 것은 망가진 전송이지 낯선 형식이 아니다.
fn checksum_for(body: &str, asset_name: &str) -> Option<[u8; 32]> {
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    let pick = lines
        .iter()
        .find(|l| {
            l.split_whitespace()
                .nth(1)
                .is_some_and(|n| n.trim_start_matches('*').ends_with(asset_name))
        })
        .or(if lines.len() == 1 {
            lines.first()
        } else {
            None
        })?;

    let hex = pick.split_whitespace().next()?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// 작업이 끝난 뒤의 요약.
///
/// 완료 화면에서 "무엇이 얼마나 쓰였는지" 를 보여주기 위한 것이다. 이것이 없으면
/// 사용자는 성공했다는 말만 듣고, 윈도우 탐색기에서 USB 내용이 보이지 않는 것을
/// 보고 실패했다고 판단하게 된다.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunSummary {
    pub loader: String,
    pub tag: String,
    pub asset_name: String,
    pub bytes_written: u64,
    pub verified: bool,
}

/// 작업 설정.
pub struct RunConfig {
    pub loader: Loader,
    pub verify: bool,
}

/// 전체 작업을 수행한다.
#[allow(clippy::too_many_arguments)]
pub fn run<F: FnMut(super::pipeline::ProgressEvent)>(
    cfg: RunConfig,
    disk: &DiskInfo,
    protected: &HashSet<u32>,
    io: &dyn Io,
    writer: &dyn RawWriter,
    cancel: &dyn Cancel,
    emit: F,
) -> Result<RunSummary, RunError> {
    let mut rep = ProgressReporter::new(emit);

    // --- 1. 어떤 이미지를 받을지 정한다 -------------------------------------
    rep.begin(Stage::Resolving, None);
    let releases = io
        .fetch_releases(&cfg.loader.releases_api_url())
        .map_err(RunError::Resolve)?;
    let image: ResolvedImage = super::loader::resolve(cfg.loader, &releases)
        .map_err(|e| RunError::Resolve(format!("{e:?}")))?;

    // 용량 확인은 압축을 푼 크기로 해야 맞다. 여기서는 아직 모르므로
    // 최소 용량 규칙만 먼저 적용하고, 실제 크기 확인은 쓰기 단계에서 한다.
    safety::can_write(disk, protected, 0, None).map_err(RunError::Rejected)?;

    if cancel.is_canceled() {
        return Err(RunError::Canceled);
    }

    // --- 2. 내려받기 --------------------------------------------------------
    rep.begin(
        Stage::Downloading,
        Some(format!("{} {}", cfg.loader.display_name(), image.tag)),
    );
    let mut last = std::time::Instant::now();
    let mut prev_done = 0u64;
    let compressed = io
        .download(
            &image.download_url,
            &mut |done, total| {
                let now = std::time::Instant::now();
                let dt = now.duration_since(last).as_secs_f64();
                rep.update(done, total, dt, done.saturating_sub(prev_done));
                last = now;
                prev_done = done;
            },
            &|| cancel.is_canceled(),
        )
        .map_err(|e| {
            if cancel.is_canceled() {
                RunError::Canceled
            } else {
                RunError::Download(e)
            }
        })?;

    if cancel.is_canceled() {
        return Err(RunError::Canceled);
    }

    // 받은 양이 릴리스가 알려준 크기와 같은가. **장치를 열기 전에** 본다.
    //
    // 이 한 줄이 없어서, Range 를 잘못 다루는 중간 장비(투명 캐시, HTTPS 를
    // 가로채는 백신)가 이어받기 응답에 파일 전체를 실어 보내면 몸통이 140%
    // 크기로 만들어진 채 그대로 통과했다. gzip CRC 가 결국 걸러내기는 하는데,
    // 그건 `writer.open()` 이 파티션 테이블을 지운 뒤라서 사용자는 멀쩡했던
    // USB 를 잃고 "압축 해제 실패" 만 보게 된다. 여기서 끊으면 USB 는 무사하다.
    //
    // 크기가 0 이면 릴리스가 알려주지 않은 것이다 (`Asset::size` 는 없으면 0).
    // 모르는 값으로 대조하면 안 되므로 그때는 넘어간다.
    if image.compressed_size > 0 && compressed.len() as u64 != image.compressed_size {
        return Err(RunError::Download(format!(
            "내려받은 크기가 다릅니다: {} 바이트를 받았는데 {} 바이트여야 합니다. \
             네트워크 중간 장비가 응답을 건드렸을 수 있습니다. 다시 시도해 주세요.",
            compressed.len(),
            image.compressed_size
        )));
    }

    // 발행자가 올린 sha256 과도 대조한다. 역시 **장치를 열기 전에** 한다.
    //
    // 길이만 맞고 내용이 어긋나는 경우까지 여기서 걸러진다. m-shell 은 체크섬을
    // 올리지 않으므로 없는 것이 정상이고, 없다고 실패시키면 안 된다.
    //
    // 체크섬 파일을 못 받았거나 형식을 못 읽은 경우도 통과시킨다. 우리가 막으려는
    // 것은 망가진 전송이지 낯선 형식이 아니고, 400바이트짜리 곁다리 파일이 안
    // 받아졌다고 굽기 전체를 막으면 그게 더 나쁘다. **어긋난 것이 확인됐을 때만**
    // 멈춘다.
    if let Some(url) = &image.checksum_url {
        if let Ok(body) = io.fetch_text(url) {
            if let Some(want) = checksum_for(&body, &image.asset_name) {
                let got: [u8; 32] = Sha256::digest(&compressed).into();
                if got != want {
                    return Err(RunError::Download(format!(
                        "내려받은 파일이 발행자가 올린 체크섬과 다릅니다.\n                         받은 값: {}\n기대한 값: {}\n네트워크 중간 장비가 응답을 \
                         건드렸을 수 있습니다. 다시 시도해 주세요.",
                        hex32(&got),
                        hex32(&want)
                    )));
                }
            }
        }
    }

    // --- 3. 압축 해제 + 쓰기 ------------------------------------------------
    // 푼 내용을 파일로 떨구지 않고 장치로 바로 흘려보낸다.
    // 3GB 짜리 임시 파일을 만들지 않으므로 디스크 여유가 없는 기계에서도 동작한다.
    rep.begin(Stage::Extracting, None);
    let (mut stream, expanded_size) = io
        .open_decompressed(compressed, &image.asset_name)
        .map_err(RunError::Extract)?;

    rep.begin(Stage::Preparing, None);
    let session = writer.open(disk)?;

    // --- 4. 여기서부터 대상은 되돌릴 수 없다 --------------------------------
    //
    // `open()` 은 그 안에서 마운트 지점을 떼고 파티션 테이블을 지운 뒤 앞
    // 1MiB 를 0 으로 덮는다. 그러므로 이 아래의 실패는 하나도 빠짐없이
    // "USB 는 이미 비워졌다" 를 뜻한다 — 사용자가 누른 취소도 마찬가지다.
    // 감싸는 자리를 한 곳으로 모은 이유는, 단계를 하나 더 넣는 사람이
    // `?` 하나를 빠뜨리는 것만으로 이 사실이 다시 새어나가기 때문이다.
    let bytes_written = write_to_target(
        &cfg,
        disk,
        session,
        stream.as_mut(),
        expanded_size,
        cancel,
        &mut rep,
    )
    .map_err(RunError::already_erased)?;

    rep.finish();
    Ok(RunSummary {
        loader: cfg.loader.display_name().to_string(),
        tag: image.tag.clone(),
        asset_name: image.asset_name.clone(),
        bytes_written,
        verified: cfg.verify,
    })
}

/// 대상을 연 뒤의 모든 단계. 쓴 바이트 수를 돌려준다.
///
/// [`run`] 에서 떼어낸 이유는 오직 하나, **되돌릴 수 없는 지점 이후의 실패를
/// 한 자리에서 감싸기 위해서**다. 흐름 자체는 바뀌지 않았다.
#[allow(clippy::too_many_arguments)]
fn write_to_target<F: FnMut(super::pipeline::ProgressEvent)>(
    cfg: &RunConfig,
    disk: &DiskInfo,
    mut session: Box<dyn WriteSession>,
    stream: &mut dyn Read,
    expanded_size: Option<u64>,
    cancel: &dyn Cancel,
    rep: &mut ProgressReporter<F>,
) -> Result<u64, RunError> {
    // 쓰기 직전 신원 확인. 목록을 만든 뒤 USB 가 바뀌었을 수 있다.
    safety::confirm_identity(disk, session.observed())
        .map_err(|m| RunError::IdentityChanged(m.describe()))?;

    rep.begin(Stage::Writing, None);
    let out = sink::stream(stream, session.as_mut(), expanded_size, cancel, rep)?;
    sink::zero_tail(session.as_mut(), out.bytes)?;

    if cfg.verify {
        // 되읽기 전에 캐시를 매체로 내려보낸다. 이 한 줄이 없으면 검증은
        // 방금 쓴 캐시를 되읽어 자기 자신과 비교하거나, 홀드백 때문에 막
        // 붙기 시작한 볼륨이 캐시를 버리면 쓰기 이전 내용을 읽는다.
        // 후자가 실물에서 났다 — 멀쩡히 써진 USB 가 불량으로 보고됐다.
        session.commit()?;
        rep.begin(Stage::Verifying, None);
        sink::verify(
            session.as_mut(),
            out.bytes,
            &out.hash,
            &out.blocks,
            cancel,
            rep,
        )?;
    }

    rep.begin(Stage::Finishing, None);
    session.finish()?;
    Ok(out.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    // 쓰기 규칙은 sink 로 옮겼지만, 그 규칙이 run() 을 통해 지켜지는지는
    // 여기서 계속 확인한다. 경계값을 손으로 베끼면 상수가 바뀔 때 어긋난다.
    use super::sink::{HOLDBACK, TAIL_ZERO};
    use crate::core::loader::Asset;
    use crate::device::fake::{FakeEnumerator, FakeWriter};
    use crate::device::UsbEnumerator;

    /// 고정된 응답을 돌려주는 가짜 입출력.
    struct FakeIo {
        payload: Vec<u8>,
        /// 릴리스가 알려주는 에셋 크기. 기본은 실제로 주는 양과 같다.
        /// 다르게 두면 중간 장비가 몸통을 잘라먹거나 덧붙인 상황이 된다.
        declared: u64,
        /// 릴리스가 함께 올린 sha256sum 파일의 내용. None 이면 안 올린 것.
        checksum: Option<String>,
    }

    impl FakeIo {
        fn new(payload: Vec<u8>) -> Self {
            let declared = payload.len() as u64;
            Self {
                payload,
                declared,
                checksum: None,
            }
        }

        /// 릴리스에 sha256sum 을 함께 올린 상태로 만든다. 본문은 그대로 쓰인다.
        fn with_checksum(mut self, body: &str) -> Self {
            self.checksum = Some(body.to_string());
            self
        }

        /// 릴리스가 알려주는 크기만 바꾼다. 실제로 주는 몸통은 그대로다.
        fn declaring(mut self, size: u64) -> Self {
            self.declared = size;
            self
        }
    }

    impl Io for FakeIo {
        fn fetch_releases(&self, _url: &str) -> Result<Vec<Release>, String> {
            Ok(vec![Release {
                tag_name: "v1.4.2.8".into(),
                draft: false,
                prerelease: false,
                assets: {
                    let mut a = vec![Asset {
                        name: "alpine-redpill.v1.4.2.8.m-shell-5GB.img.gz".into(),
                        size: self.declared,
                        browser_download_url: "https://example.invalid/img".into(),
                    }];
                    if self.checksum.is_some() {
                        a.push(Asset {
                            name: "sha256sum".into(),
                            size: 64,
                            browser_download_url: "https://example.invalid/sha".into(),
                        });
                    }
                    a
                },
            }])
        }

        fn fetch_text(&self, _url: &str) -> Result<String, String> {
            self.checksum
                .clone()
                .ok_or_else(|| "체크섬 파일이 없습니다".to_string())
        }

        fn download(
            &self,
            _url: &str,
            on_progress: &mut dyn FnMut(u64, Option<u64>),
            _should_stop: &dyn Fn() -> bool,
        ) -> Result<Vec<u8>, String> {
            let total = self.payload.len() as u64;
            on_progress(total / 2, Some(total));
            on_progress(total, Some(total));
            Ok(self.payload.clone())
        }

        fn open_decompressed(
            &self,
            data: Vec<u8>,
            _name: &str,
        ) -> Result<(Box<dyn Read + Send>, Option<u64>), String> {
            let n = data.len() as u64;
            Ok((Box::new(std::io::Cursor::new(data)), Some(n)))
        }
    }

    fn usb() -> DiskInfo {
        FakeEnumerator::sample().list_disks().unwrap()[2].clone()
    }

    /// 대상이 지워진 뒤의 실패는 원인을 한 겹 안에 담는다. 알맹이만 꺼낸다.
    ///
    /// 감싸는 것 자체는 [`RunError::TargetErased`] 의 주석에 적힌 이유로 옳다.
    /// 원인별 동작을 확인하는 테스트는 그 겉면을 넘어가야 한다.
    fn cause(e: &RunError) -> &RunError {
        match e {
            RunError::TargetErased { cause } => cause,
            other => other,
        }
    }

    #[test]
    fn full_run_writes_the_image_to_the_device() {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let io = FakeIo::new(payload.clone());
        let writer = FakeWriter::new(1024 * 1024, 512);
        let mut events = Vec::new();

        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |e| events.push(e),
        )
        .expect("작업이 성공해야 한다");

        // 이미지가 장치 앞부분에 그대로 쓰여야 한다.
        let written = writer.contents();
        assert_eq!(&written[..payload.len()], &payload[..]);
        // 마무리가 호출돼야 한다. 빠뜨리면 플러시 없이 끝난다.
        assert!(writer.was_finished(), "finish 가 호출되지 않았다");
    }

    /// 이미지가 보류 크기보다 클 때, 앞부분을 나중에 써도 내용이 정확한지.
    ///
    /// 실제 이미지는 GB 단위라 항상 이 경로를 탄다. 다른 테스트들은 전부
    /// 1MiB 보다 작아서 "전체가 보류되는" 경로만 검증한다.
    #[test]
    fn image_larger_than_the_holdback_is_written_in_the_right_order() {
        // 보류 크기의 세 배 남짓. 경계를 넘나드는 값으로 고른다.
        let size = (HOLDBACK as usize) * 3 + 12345;
        let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let io = FakeIo::new(payload.clone());
        let writer = FakeWriter::new(8 * 1024 * 1024, 512);

        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("작업이 성공해야 한다");

        // 보류했던 앞부분이 제자리에 있어야 한다.
        let written = writer.contents();
        assert_eq!(
            &written[..HOLDBACK as usize],
            &payload[..HOLDBACK as usize],
            "보류한 앞부분이 잘못 쓰였다"
        );
        // 그 뒤도 이어서 정확해야 한다 — 오프셋이 어긋나면 여기서 걸린다.
        assert_eq!(
            &written[HOLDBACK as usize..payload.len()],
            &payload[HOLDBACK as usize..],
            "보류 이후 데이터의 위치가 어긋났다"
        );
    }

    /// 보류한 앞부분은 **나머지를 다 쓴 뒤에** 쓰여야 한다.
    ///
    /// 순서가 뒤바뀌면 윈도우가 쓰는 도중에 파티션을 인식해 볼륨을 마운트하고,
    /// 그 볼륨이 잠기지 않았으므로 이후 쓰기가 거부된다. 실제로 24MiB 지점에서
    /// 그렇게 실패했다.
    #[test]
    fn the_partition_table_is_written_last() {
        let size = (HOLDBACK as usize) * 2;
        let payload: Vec<u8> = (0..size).map(|i| (i % 241) as u8).collect();
        let io = FakeIo::new(payload.clone());
        let writer = FakeWriter::new(8 * 1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        let order = writer.write_offsets();
        let first_write_at_zero = order
            .iter()
            .position(|o| *o == 0)
            .expect("오프셋 0 에 쓴 적이 없다");
        assert!(
            first_write_at_zero > 0,
            "오프셋 0 을 가장 먼저 썼다 — 파티션 테이블이 마지막에 쓰여야 한다"
        );
    }

    #[test]
    fn every_stage_is_reported_in_order() {
        let io = FakeIo::new(vec![7u8; 2048]);
        let writer = FakeWriter::new(1024 * 1024, 512);
        let mut events = Vec::new();
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |e| events.push(e),
        )
        .unwrap();

        let seen: Vec<Stage> = events.iter().map(|e| e.stage).collect();
        for s in [
            Stage::Resolving,
            Stage::Downloading,
            Stage::Extracting,
            Stage::Preparing,
            Stage::Writing,
            Stage::Finishing,
        ] {
            assert!(seen.contains(&s), "{s:?} 단계가 보고되지 않았다");
        }
        // 마지막 이벤트에는 모든 단계가 완료로 찍혀야 한다.
        assert!(events.last().unwrap().completed.contains(&Stage::Finishing));
    }

    #[test]
    fn tail_is_zeroed_so_an_old_gpt_backup_cannot_survive() {
        let io = FakeIo::new(vec![1u8; 1024]);
        // 0xAA 로 채워진 장치. 꼬리가 0 이 돼야 한다.
        let writer = FakeWriter::new(4 * 1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        let c = writer.contents();
        let tail = &c[c.len() - TAIL_ZERO as usize..];
        assert!(tail.iter().all(|b| *b == 0), "장치 끝이 지워지지 않았다");
    }

    #[test]
    fn padding_is_zeroed_not_leaked_heap() {
        // 섹터 배수가 아닌 크기. 남는 부분이 0 이어야 한다.
        let payload = vec![9u8; 700];
        let io = FakeIo::new(payload.clone());
        let writer = FakeWriter::new(1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();

        let c = writer.contents();
        assert_eq!(&c[..700], &payload[..]);
        // 700 -> 1024 로 올림되며 채워진 부분.
        assert!(c[700..1024].iter().all(|b| *b == 0), "패딩이 0 이 아니다");
    }

    #[test]
    fn refuses_when_the_device_is_a_different_one() {
        // 목록을 만든 뒤 USB 가 바뀐 상황.
        let disks = FakeEnumerator::sample().list_disks().unwrap();
        let selected = disks[2].clone();
        let mut swapped = disks[3].clone();
        swapped.number = selected.number;

        let io = FakeIo::new(vec![0u8; 512]);
        let writer = FakeWriter::new(1024 * 1024, 512).with_observed(swapped);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &selected,
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        // 이 확인은 대상을 연 **뒤에** 일어난다. 즉 그 USB 는 이미 지워졌다.
        // 예전에는 안내가 "안전을 위해 중단했습니다" 였는데, 그 말이 맞으려면
        // 아무것도 건드리지 않았어야 한다. 이제 겉면이 그 사실을 실어 나른다.
        assert!(
            matches!(err, RunError::TargetErased { .. }),
            "실제: {err:?}"
        );
        assert!(
            matches!(cause(&err), RunError::IdentityChanged(_)),
            "실제: {err:?}"
        );
        // 어긋난 항목과 두 값이 남아 있어야 한다. 목록의 32GB 와 실제 64GB —
        // 가짜 용량 USB 는 이 숫자 둘로만 알아볼 수 있다.
        let text = format!("{err:?}");
        assert!(text.contains("용량이 다릅니다"), "실제: {text}");
        assert!(text.contains("30752000000"), "실제: {text}");
        assert!(text.contains("64055500800"), "실제: {text}");
    }

    #[test]
    fn refuses_a_protected_disk() {
        let io = FakeIo::new(vec![0u8; 512]);
        let writer = FakeWriter::new(1024 * 1024, 512);
        let protected: HashSet<u32> = [2].into_iter().collect();
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &protected,
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, RunError::Rejected(_)), "실제: {err:?}");
    }

    #[test]
    fn stops_when_the_image_exceeds_the_device() {
        let io = FakeIo::new(vec![3u8; 8192]);
        // 장치가 이미지보다 작다.
        let writer = FakeWriter::new(4096, 512);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(cause(&err), RunError::Rejected(_) | RunError::Device(_)),
            "실제: {err:?}"
        );
    }

    #[test]
    fn verify_passes_on_a_healthy_device() {
        let io = FakeIo::new((0..8192u32).map(|i| (i % 253) as u8).collect());
        let writer = FakeWriter::new(1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: true,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("정상 장치에서는 검증이 통과해야 한다");
    }

    #[test]
    fn verify_reads_the_medium_and_not_a_cache_that_has_not_landed_yet() {
        // 0.4.0 을 실물에서 돌렸을 때 멀쩡히 써진 USB 가 VerifyMismatch 로
        // 실패했다. 원인은 장치가 아니라 순서였다 — 검증이 플러시보다 먼저
        // 돌았다. 핸들은 캐시를 우회하지 않으므로 그 시점의 되읽기는 매체가
        // 아니라 캐시를 확인하는 것이고, 홀드백이 방금 오프셋 0 에 놓이면서
        // 윈도우가 새 볼륨을 붙이기 시작한 참이라 캐시가 버려질 수 있다.
        //
        // 가짜가 이 현실을 흉내내지 않았기 때문에 테스트는 전부 초록불이었다.
        let io = FakeIo::new((0..8192u32).map(|i| (i % 253) as u8).collect());
        let writer = FakeWriter::new(1024 * 1024, 512).buffering();
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: true,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("검증 전에 매체로 내려보내지 않아 멀쩡한 쓰기를 불량으로 보고했다");
        assert!(writer.was_committed(), "검증 전에 commit 이 불리지 않았다");
    }

    #[test]
    fn verify_catches_a_device_that_lies_about_writing() {
        // 불량 USB 는 쓰기가 성공했다고 보고하고도 내용이 다를 수 있다.
        // 검증이 실제로 의미를 가지려면 이 경우를 잡아야 한다.
        let io = FakeIo::new((0..8192u32).map(|i| (i % 253) as u8).collect());
        let writer = FakeWriter::new(1024 * 1024, 512).corrupting_after(4096);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: true,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(cause(&err), RunError::VerifyMismatch { .. }),
            "손상을 잡지 못했다: {err:?}"
        );
    }

    #[test]
    fn verify_is_skipped_when_not_requested() {
        // 검증을 끄면 손상된 장치라도 통과한다. 기본값이 꺼짐이므로
        // 이 동작을 명시적으로 기록해 둔다.
        let io = FakeIo::new(vec![1u8; 8192]);
        let writer = FakeWriter::new(1024 * 1024, 512).corrupting_after(4096);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("검증을 끄면 통과해야 한다");
    }

    struct AlwaysCancel;
    impl Cancel for AlwaysCancel {
        fn is_canceled(&self) -> bool {
            true
        }
    }

    /// 대상이 열린 뒤부터 취소로 답하는 취소기.
    ///
    /// `open()` 안에서 이미 파티션 테이블이 지워지므로, 그 뒤의 취소는
    /// "아직 아무 일도 없었다" 가 아니다. 호출 횟수를 세는 대신 실제
    /// 되돌릴 수 없는 지점을 기준으로 삼아, 흐름에 검사가 하나 늘어도
    /// 이 테스트가 엉뚱한 곳을 겨누지 않게 한다.
    struct CancelOnceOpened<'a>(&'a FakeWriter);
    impl Cancel for CancelOnceOpened<'_> {
        fn is_canceled(&self) -> bool {
            self.0.was_opened()
        }
    }

    /// 검증기는 어디서 어긋났는지 계산하지 않는다. 모르는 것을 아는 척하면 안 된다.
    ///
    /// `offset: 0` 은 하필 파티션 테이블 자리를 가리켜서, 보고를 받은 사람이
    /// 실제로 손상된 곳이 아니라 맨 앞 쓰기 경로를 뒤지게 만든다.
    #[test]
    fn a_verify_mismatch_does_not_claim_a_position_it_never_computed() {
        let io = FakeIo::new((0..8192u32).map(|i| (i % 253) as u8).collect());
        let writer = FakeWriter::new(1024 * 1024, 512).corrupting_after(4096);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: true,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        let text = format!("{err:?}");
        assert!(
            matches!(cause(&err), RunError::VerifyMismatch { .. }),
            "실제: {text}"
        );
        assert!(
            !text.contains("offset"),
            "검증은 어긋난 위치를 구하지 않는데 오류가 위치를 주장한다: {text}"
        );
    }

    /// 릴리스가 알려준 크기와 받은 양이 다르면 **장치를 열기 전에** 멈춰야 한다.
    ///
    /// Range 를 잘못 다루는 중간 장비는 이어받기 응답에 파일 전체를 실어 보낸다.
    /// 그러면 받은 양이 실제보다 많아지는데, 지금까지는 그걸 그대로 압축 해제로
    /// 넘겼다. gzip CRC 가 결국 걸러내지만 그건 `open()` 이 파티션 테이블을
    /// 지운 **뒤**라, 사용자는 멀쩡한 USB 를 잃고 "압축 해제 실패" 를 본다.
    #[test]
    fn a_download_that_does_not_match_the_published_size_never_reaches_the_device() {
        let io = FakeIo::new(vec![7u8; 4096]).declaring(100);
        let writer = FakeWriter::new(1024 * 1024, 512);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(err, RunError::Download(_)), "실제: {err:?}");
        assert!(
            !writer.was_opened(),
            "대상을 이미 열었다 — 이 시점이면 파티션 테이블은 지워진 뒤다"
        );
    }

    /// 크기를 안 알려주는 릴리스도 있다. 그때는 대조하지 않고 그대로 진행한다.
    #[test]
    fn a_release_without_a_published_size_still_burns() {
        let io = FakeIo::new(vec![7u8; 4096]).declaring(0);
        let writer = FakeWriter::new(1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("크기를 모른다고 굽기를 막으면 안 된다");
    }

    /// 대상이 열린 뒤에 실패하면 **대상이 이미 지워졌다는 사실**을 실어야 한다.
    ///
    /// 이 시점의 USB 는 파티션 테이블이 없고 이미지가 일부만 들어 있다.
    /// 그냥 `MediaChanged` 만 올리면 UI 는 "다시 꽂고 처음부터" 라고만 하고,
    /// 사용자는 자기 USB 가 이미 비워졌다는 것을 어디서도 듣지 못한다.
    #[test]
    fn a_failure_after_the_target_is_opened_says_the_target_is_already_erased() {
        let payload: Vec<u8> = (0..(HOLDBACK as usize * 2))
            .map(|i| (i % 251) as u8)
            .collect();
        let io = FakeIo::new(payload);
        let writer = FakeWriter::new(8 * 1024 * 1024, 512).failing_at(HOLDBACK + 4096);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(
            writer.was_opened(),
            "이 테스트는 열린 뒤의 실패를 재현해야 한다"
        );
        assert!(
            matches!(err, RunError::TargetErased { .. }),
            "대상이 지워진 뒤의 실패인데 그대로 올렸다: {err:?}"
        );
        // 원인을 잃어버리면 안 된다. 지워졌다는 사실이 원인을 대신하지 못한다.
        assert!(
            format!("{err:?}").contains("MediaChanged"),
            "원래 원인이 사라졌다: {err:?}"
        );
    }

    /// 취소도 마찬가지다. 사용자가 눌렀다고 해서 USB 가 멀쩡한 것은 아니다.
    #[test]
    fn canceling_after_the_target_is_opened_still_says_it_was_erased() {
        let io = FakeIo::new(vec![5u8; 4096]);
        let writer = FakeWriter::new(1024 * 1024, 512);
        let cancel = CancelOnceOpened(&writer);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &cancel,
            |_| {},
        )
        .unwrap_err();

        assert!(writer.was_opened());
        assert!(
            matches!(err, RunError::TargetErased { .. }),
            "취소를 그대로 올렸다 — USB 가 지워진 사실이 전달되지 않는다: {err:?}"
        );
        assert!(format!("{err:?}").contains("Canceled"), "실제: {err:?}");
    }

    /// 발행자가 올린 sha256 과 다르면 **장치를 열기 전에** 멈춰야 한다.
    ///
    /// 릴리스가 체크섬을 올려두고도 아무도 읽지 않던 자리다. 길이는 맞는데
    /// 내용이 어긋나는 전송을 걸러낼 수 있는 유일한 수단이고, 그것도 USB 가
    /// 아직 멀쩡할 때 할 수 있다.
    #[test]
    fn a_download_that_fails_the_published_checksum_never_reaches_the_device() {
        let io = FakeIo::new(vec![7u8; 4096]).with_checksum(
            "0000000000000000000000000000000000000000000000000000000000000000  \
             alpine-redpill.v1.4.2.8.m-shell-5GB.img.gz\n",
        );
        let writer = FakeWriter::new(1024 * 1024, 512);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(err, RunError::Download(_)), "실제: {err:?}");
        assert!(
            !writer.was_opened(),
            "대상을 이미 열었다 — 이 시점이면 파티션 테이블은 지워진 뒤다"
        );
    }

    #[test]
    fn a_download_that_matches_the_published_checksum_burns() {
        let payload = vec![7u8; 4096];
        let digest: [u8; 32] = Sha256::digest(&payload).into();
        let io = FakeIo::new(payload).with_checksum(&format!(
            "{}  alpine-redpill.v1.4.2.8.m-shell-5GB.img.gz\n",
            hex32(&digest)
        ));
        let writer = FakeWriter::new(1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("체크섬이 맞는데 막았다");
    }

    /// 형식을 못 읽는 체크섬 파일 때문에 굽기가 막히면 안 된다.
    ///
    /// 저장소가 파일 모양을 바꾸는 날 프로그램이 통째로 쓸모없어진다.
    /// 우리가 막으려는 것은 망가진 전송이지 낯선 형식이 아니다.
    #[test]
    fn an_unreadable_checksum_file_does_not_block_the_burn() {
        let io = FakeIo::new(vec![7u8; 4096]).with_checksum("# 여기에 해시는 없습니다\n");
        let writer = FakeWriter::new(1024 * 1024, 512);
        run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &NeverCancel,
            |_| {},
        )
        .expect("읽을 수 없는 체크섬 파일이 굽기를 막았다");
    }

    #[test]
    fn a_checksum_line_is_matched_by_asset_name() {
        let body = "aa  other.img.gz\n                    bb  wanted.img.gz\n";
        // 64자리가 아니면 해시로 인정하지 않는다.
        assert_eq!(checksum_for(body, "wanted.img.gz"), None);

        let good = format!(
            "{}  wanted.img.gz\n{}  other.img.gz\n",
            "ab".repeat(32),
            "cd".repeat(32)
        );
        assert_eq!(checksum_for(&good, "wanted.img.gz"), Some([0xab; 32]));
        assert_eq!(checksum_for(&good, "other.img.gz"), Some([0xcd; 32]));
        // 이름이 목록에 없으면 건너뛴다 — 막지 않는다.
        assert_eq!(checksum_for(&good, "nowhere.img.gz"), None);
    }

    #[test]
    fn cancel_is_honoured_before_anything_is_written() {
        let io = FakeIo::new(vec![5u8; 4096]);
        let writer = FakeWriter::new(1024 * 1024, 512);
        let err = run(
            RunConfig {
                loader: Loader::MShell,
                verify: false,
            },
            &usb(),
            &HashSet::new(),
            &io,
            &writer,
            &AlwaysCancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, RunError::Canceled));
        // 장치는 손대지 않은 상태여야 한다.
        assert!(writer.contents().iter().all(|b| *b == 0xAA));
    }
}
