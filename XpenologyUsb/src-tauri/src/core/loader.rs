//! 로더(부트로더) 릴리스 해석.
//!
//! ## 왜 파일명을 조립하지 않는가
//!
//! 직관적인 구현은 "최신 태그를 알아내서 `<repo>/releases/download/<tag>/<고정패턴>` 을
//! 만든다"이다. 실제로 기존 `pve_xpenol_install.sh` 가 그렇게 한다. 그런데 이 방식은
//! 조사에서 두 가지로 깨지는 것이 확인됐다.
//!
//! 1. **파일명 접두사가 바뀐다.** PeterSuh-Q3/tinycore-redpill 은 2026-07-16 에
//!    `tinycore-redpill.` → `alpine-redpill.` 로 접두사를 바꿨다. 최근 80개 릴리스 중
//!    `alpine-redpill.<tag>.m-shell.img.gz` 로 맞는 것은 13개뿐이다.
//!    저장소도 작성자도 에셋의 역할도 그대로인데 이름만 바뀌었다.
//!
//! 2. **최신 태그에 에셋이 없을 수 있다.** RROrg/rr 은 최근 60개 릴리스 중 7개가
//!    에셋 없이 게시됐다. `releases/latest` 는 그런 릴리스도 태연히 가리키며,
//!    실제로 26.8.0 이 약 8시간 동안 최신이었고 그동안 조립한 URL 은 404 였다.
//!
//! 그래서 **태그로 이름을 추측하지 않는다.** 릴리스 목록을 받아 에셋을 직접 찾고,
//! API 가 준 `browser_download_url` 을 그대로 쓴다.

use serde::{Deserialize, Serialize};

/// 지원하는 부트로더.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Loader {
    /// PeterSuh-Q3/tinycore-redpill 의 m-shell 이미지. 기본 추천.
    MShell,
    /// RROrg/rr.
    Rr,
}

impl Loader {
    pub fn repo(self) -> &'static str {
        match self {
            Loader::MShell => "PeterSuh-Q3/tinycore-redpill",
            Loader::Rr => "RROrg/rr",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Loader::MShell => "m-shell",
            Loader::Rr => "RR",
        }
    }

    /// 사용자에게 기본 추천하는가.
    pub fn is_recommended(self) -> bool {
        matches!(self, Loader::MShell)
    }

    /// 릴리스 목록 API 주소.
    ///
    /// `latest` 가 아니라 목록을 받는다. 최신 릴리스에 에셋이 없을 수 있어서
    /// 뒤로 거슬러 올라가며 찾아야 하기 때문이다.
    pub fn releases_api_url(self) -> String {
        format!(
            "https://api.github.com/repos/{}/releases?per_page=10",
            self.repo()
        )
    }
}

/// GitHub 릴리스 에셋 (필요한 필드만).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    pub browser_download_url: String,
}

/// GitHub 릴리스 (필요한 필드만).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// 해석 결과 — 실제로 내려받을 대상.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedImage {
    pub loader: Loader,
    pub tag: String,
    /// 이미지 에셋 이름.
    pub asset_name: String,
    /// API 가 준 URL 을 그대로 사용한다. 조립하지 않는다.
    pub download_url: String,
    /// 압축된 상태의 크기.
    pub compressed_size: u64,
    /// sha256sum 에셋 주소. 없으면 None.
    ///
    /// RR 은 대부분의 릴리스에 sha256sum 을 올리지만 m-shell 은 전혀 올리지 않는다.
    /// 따라서 없는 것이 정상이며, 없다고 실패시키면 안 된다.
    pub checksum_url: Option<String>,
}

/// 로더 해석 실패 사유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// 최근 릴리스를 다 뒤졌는데 쓸 수 있는 이미지가 없다.
    NoUsableAsset { searched: usize },
    /// 릴리스 목록이 비어 있다.
    NoReleases,
}

/// 이 에셋이 USB 에 구울 수 있는 raw 디스크 이미지인가.
///
/// 확실히 걸러야 하는 것들:
/// - `.vmdk.gz` — VMware 가상 디스크다. USB 에 구우면 부팅되지 않는다.
/// - `.ova.zip` / `.vhd.zip` — 가상화용.
/// - `updateall-*.zip` — 기존 설치를 갱신하는 꾸러미이지 부팅 이미지가 아니다.
///   이걸 USB 에 구우면 부팅되지 않으면서 원인도 알기 어렵다.
/// - `xtcrp` 계열 — m-shell 을 요청했을 때 섞이면 안 된다.
fn is_target_image(loader: Loader, name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match loader {
        Loader::MShell => {
            // 접두사는 바뀌므로 조건에 넣지 않는다. 역할을 나타내는 부분만 본다.
            lower.ends_with(".img.gz") && lower.contains("m-shell") && !lower.contains("vmdk")
        }
        Loader::Rr => {
            // rr-<tag>.img.zip. ova/vhd/updateall 은 제외된다.
            lower.starts_with("rr-") && lower.ends_with(".img.zip")
        }
    }
}

/// 같은 릴리스 안에 후보가 여러 개일 때의 우선순위. 숫자가 작을수록 먼저다.
///
/// m-shell 은 릴리스마다 용량 변형을 함께 올린다. 실사용 통계상 `-5GB` 쪽이
/// 표준판보다 더 많이 받아지고 있어 이쪽을 기본으로 삼는다.
/// 구버전 릴리스에는 변형이 없거나 `-4GB` 이므로 없으면 표준판으로 내려온다.
///
/// `-5GB` 는 압축 해제 시 약 4.98GB 라서 8GB 이상 USB 가 필요하다.
/// 최소 용량 요구가 이미 8GB(`safety::MIN_USB_BYTES`)이므로 추가 제약은 생기지 않는다.
fn preference(loader: Loader, name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    match loader {
        Loader::MShell => {
            if lower.contains("-5gb") {
                0
            } else if lower.contains("-4gb") {
                2
            } else {
                1
            }
        }
        Loader::Rr => 0,
    }
}

/// 릴리스 하나에서 쓸 이미지를 고른다. 후보가 없으면 None.
fn select_asset(loader: Loader, assets: &[Asset]) -> Option<&Asset> {
    assets
        .iter()
        .filter(|a| is_target_image(loader, &a.name))
        .min_by_key(|a| (preference(loader, &a.name), a.name.clone()))
}

/// 체크섬 에셋인가.
fn is_checksum(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "sha256sum" || lower.ends_with(".sha256")
}

/// 릴리스 목록에서 실제로 쓸 수 있는 이미지를 찾는다.
///
/// 목록은 최신순으로 들어온다고 가정하고 앞에서부터 훑는다.
/// draft / prerelease 는 건너뛰고, **에셋이 실제로 있는 첫 릴리스**를 채택한다.
pub fn resolve(loader: Loader, releases: &[Release]) -> Result<ResolvedImage, ResolveError> {
    if releases.is_empty() {
        return Err(ResolveError::NoReleases);
    }

    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        let Some(image) = select_asset(loader, &release.assets) else {
            // 에셋 없는 릴리스. 다음 것으로 넘어간다.
            continue;
        };

        let checksum_url = release
            .assets
            .iter()
            .find(|a| is_checksum(&a.name))
            .map(|a| a.browser_download_url.clone());

        return Ok(ResolvedImage {
            loader,
            tag: release.tag_name.clone(),
            asset_name: image.name.clone(),
            download_url: image.browser_download_url.clone(),
            compressed_size: image.size,
            checksum_url,
        });
    }

    Err(ResolveError::NoUsableAsset {
        searched: releases.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, size: u64) -> Asset {
        Asset {
            name: name.into(),
            size,
            browser_download_url: format!("https://example.invalid/{name}"),
        }
    }

    fn release(tag: &str, assets: Vec<Asset>) -> Release {
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    /// 조사에서 확인된 실제 m-shell 릴리스 구성 (릴리스당 8개 에셋).
    fn mshell_assets(tag: &str, prefix: &str) -> Vec<Asset> {
        vec![
            asset(&format!("{prefix}.{tag}.m-shell.img.gz"), 605_888_202),
            asset(&format!("{prefix}.{tag}.m-shell-5GB.img.gz"), 607_744_000),
            asset(&format!("{prefix}.{tag}.m-shell.vmdk.gz"), 600_000_000),
            asset(&format!("{prefix}.{tag}.m-shell-5GB.vmdk.gz"), 601_000_000),
            asset(&format!("{prefix}.{tag}.xtcrp.img.gz"), 590_000_000),
            asset(&format!("{prefix}.{tag}.xtcrp-4GB.img.gz"), 591_000_000),
            asset(&format!("{prefix}.{tag}.xtcrp.vmdk.gz"), 592_000_000),
            asset(&format!("{prefix}.{tag}.xtcrp-4GB.vmdk.gz"), 593_000_000),
        ]
    }

    /// 조사에서 확인된 실제 RR 릴리스 구성.
    fn rr_assets(tag: &str) -> Vec<Asset> {
        vec![
            asset(&format!("rr-{tag}.img.zip"), 1_300_283_351),
            asset(&format!("rr-{tag}.ova.zip"), 1_310_000_000),
            asset(&format!("rr-{tag}.vhd.zip"), 1_320_000_000),
            asset("sha256sum", 512),
            asset(&format!("updateall-{tag}.zip"), 200_000_000),
        ]
    }

    // --- 접두사 변경에 견디는가 (이 프로그램의 존재 이유 중 하나) ---

    // 이 두 테스트의 목적은 **접두사가 바뀌어도 찾아내는가**이지 변형 선택이 아니다.
    // 변형 우선순위는 별도 테스트가 담당한다.

    #[test]
    fn resolves_new_alpine_prefix() {
        let rs = vec![release(
            "v1.4.2.8",
            mshell_assets("v1.4.2.8", "alpine-redpill"),
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert!(got.asset_name.starts_with("alpine-redpill."));
        assert!(got.asset_name.contains("m-shell"));
        assert!(got.asset_name.ends_with(".img.gz"));
    }

    #[test]
    fn resolves_old_tinycore_prefix_too() {
        // 2026-07-16 이전 릴리스. 하드코딩이었다면 여기서 404 가 났다.
        let rs = vec![release(
            "v1.3.1.1",
            mshell_assets("v1.3.1.1", "tinycore-redpill"),
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert!(got.asset_name.starts_with("tinycore-redpill."));
        assert!(got.asset_name.contains("m-shell"));
        assert!(got.asset_name.ends_with(".img.gz"));
    }

    #[test]
    fn resolves_hypothetical_future_prefix() {
        // 접두사가 또 바뀌어도 동작해야 한다.
        let rs = vec![release("v2.0.0", mshell_assets("v2.0.0", "something-new"))];
        assert!(resolve(Loader::MShell, &rs).is_ok());
    }

    // --- 잘못된 에셋을 고르지 않는가 ---

    #[test]
    fn never_picks_vmdk() {
        // vmdk 를 USB 에 구우면 부팅되지 않는다.
        let rs = vec![release(
            "v1.4.2.8",
            mshell_assets("v1.4.2.8", "alpine-redpill"),
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert!(!got.asset_name.contains("vmdk"));
        assert!(got.asset_name.ends_with(".img.gz"));
    }

    #[test]
    fn prefers_5gb_variant_when_available() {
        // 실사용 통계상 -5GB 쪽이 더 많이 받아진다 (v1.4.2.8 기준 85 vs 58).
        let rs = vec![release(
            "v1.4.2.8",
            mshell_assets("v1.4.2.8", "alpine-redpill"),
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert_eq!(got.asset_name, "alpine-redpill.v1.4.2.8.m-shell-5GB.img.gz");
    }

    #[test]
    fn falls_back_to_standard_when_no_5gb_variant() {
        // 변형을 올리지 않던 구버전 릴리스.
        let rs = vec![release(
            "v1.2.5.0",
            vec![
                asset("tinycore-redpill.v1.2.5.0.m-shell.img.gz", 500_000_000),
                asset("tinycore-redpill.v1.2.5.0.m-shell.vmdk.gz", 500_000_000),
            ],
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert_eq!(got.asset_name, "tinycore-redpill.v1.2.5.0.m-shell.img.gz");
    }

    #[test]
    fn prefers_standard_over_old_4gb_naming() {
        // 용량 접미사가 -4GB 이던 시절. 5GB 가 없으면 표준판이 4GB 판보다 낫다.
        let rs = vec![release(
            "v1.3.0.0",
            vec![
                asset("tinycore-redpill.v1.3.0.0.m-shell-4GB.img.gz", 500_000_000),
                asset("tinycore-redpill.v1.3.0.0.m-shell.img.gz", 500_000_000),
            ],
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert_eq!(got.asset_name, "tinycore-redpill.v1.3.0.0.m-shell.img.gz");
    }

    #[test]
    fn never_picks_xtcrp_when_mshell_requested() {
        let rs = vec![release(
            "v1.4.2.8",
            mshell_assets("v1.4.2.8", "alpine-redpill"),
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert!(got.asset_name.contains("m-shell"));
    }

    #[test]
    fn rr_picks_img_zip_not_ova_or_vhd() {
        let rs = vec![release("26.8.1", rr_assets("26.8.1"))];
        let got = resolve(Loader::Rr, &rs).unwrap();
        assert_eq!(got.asset_name, "rr-26.8.1.img.zip");
    }

    #[test]
    fn rr_ignores_updateall_bundle() {
        let rs = vec![release(
            "26.8.1",
            vec![
                asset("updateall-26.8.1.zip", 200_000_000),
                asset("rr-26.8.1.img.zip", 1_300_283_351),
            ],
        )];
        let got = resolve(Loader::Rr, &rs).unwrap();
        assert_eq!(got.asset_name, "rr-26.8.1.img.zip");
    }

    // --- 에셋 없는 릴리스를 건너뛰는가 ---

    #[test]
    fn skips_assetless_release_and_takes_previous() {
        // 실제로 있었던 상황: 26.8.0 이 에셋 없이 8시간 동안 최신이었다.
        let rs = vec![
            release("26.8.0", vec![]),
            release("26.7.9", rr_assets("26.7.9")),
        ];
        let got = resolve(Loader::Rr, &rs).unwrap();
        assert_eq!(got.tag, "26.7.9");
    }

    #[test]
    fn skips_multiple_consecutive_assetless_releases() {
        // 25.5.0 ~ 25.5.3 처럼 연속으로 비어 있던 사례.
        let rs = vec![
            release("25.5.3", vec![]),
            release("25.5.2", vec![]),
            release("25.5.1", vec![]),
            release("25.5.0", vec![]),
            release("25.4.9", rr_assets("25.4.9")),
        ];
        assert_eq!(resolve(Loader::Rr, &rs).unwrap().tag, "25.4.9");
    }

    #[test]
    fn skips_release_with_only_irrelevant_assets() {
        let rs = vec![
            release("26.8.0", vec![asset("sha256sum", 512)]),
            release("26.7.9", rr_assets("26.7.9")),
        ];
        assert_eq!(resolve(Loader::Rr, &rs).unwrap().tag, "26.7.9");
    }

    #[test]
    fn skips_draft_and_prerelease() {
        let mut draft = release("26.9.0", rr_assets("26.9.0"));
        draft.draft = true;
        let mut pre = release("26.8.9", rr_assets("26.8.9"));
        pre.prerelease = true;
        let rs = vec![draft, pre, release("26.8.1", rr_assets("26.8.1"))];
        assert_eq!(resolve(Loader::Rr, &rs).unwrap().tag, "26.8.1");
    }

    // --- 체크섬 ---

    #[test]
    fn rr_exposes_checksum_url() {
        let rs = vec![release("26.8.1", rr_assets("26.8.1"))];
        assert!(resolve(Loader::Rr, &rs).unwrap().checksum_url.is_some());
    }

    #[test]
    fn mshell_has_no_checksum_and_that_is_ok() {
        // m-shell 은 체크섬을 전혀 제공하지 않는다. 없다고 실패하면 안 된다.
        let rs = vec![release(
            "v1.4.2.8",
            mshell_assets("v1.4.2.8", "alpine-redpill"),
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert_eq!(got.checksum_url, None);
    }

    // --- URL 을 조립하지 않는다 ---

    #[test]
    fn uses_api_provided_url_verbatim() {
        let rs = vec![release(
            "v1.4.2.8",
            vec![Asset {
                name: "alpine-redpill.v1.4.2.8.m-shell.img.gz".into(),
                size: 1,
                browser_download_url: "https://cdn.example.invalid/redirected/path".into(),
            }],
        )];
        let got = resolve(Loader::MShell, &rs).unwrap();
        assert_eq!(
            got.download_url,
            "https://cdn.example.invalid/redirected/path"
        );
    }

    // --- 실패 경로 ---

    #[test]
    fn empty_release_list_is_an_error() {
        assert_eq!(resolve(Loader::Rr, &[]), Err(ResolveError::NoReleases));
    }

    #[test]
    fn all_assetless_reports_how_many_were_searched() {
        let rs = vec![release("a", vec![]), release("b", vec![])];
        assert_eq!(
            resolve(Loader::Rr, &rs),
            Err(ResolveError::NoUsableAsset { searched: 2 })
        );
    }

    #[test]
    fn loader_repos_are_correct() {
        assert_eq!(Loader::MShell.repo(), "PeterSuh-Q3/tinycore-redpill");
        assert_eq!(Loader::Rr.repo(), "RROrg/rr");
        assert!(Loader::MShell.is_recommended());
        assert!(!Loader::Rr.is_recommended());
    }
}
