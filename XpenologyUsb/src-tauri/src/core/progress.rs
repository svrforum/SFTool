//! 진행률·속도·잔여 시간 계산.
//!
//! 순수 계산이라 전부 테스트된다. 표시 문구는 여기서 만들지 않는다 —
//! 숫자만 내보내고 문자열 조립은 i18n 계층이 맡는다.

use serde::{Deserialize, Serialize};

/// 작업 단계. UI 의 4단계 화면에서 진행 목록으로 표시된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// 최신 릴리스 확인.
    Resolving,
    /// 내려받기.
    Downloading,
    /// 압축 해제.
    Extracting,
    /// USB 준비 (잠금, 레이아웃 초기화).
    Preparing,
    /// 이미지 쓰기.
    Writing,
    /// 검증 (선택).
    Verifying,
    /// 마무리 (플러시, 꺼내기).
    Finishing,
}

/// 진행 상황 한 컷.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub stage: Stage,
    /// 0..=100. 총량을 모르면 None (불확정 진행 표시).
    pub percent: Option<u8>,
    pub done_bytes: u64,
    /// 총량을 모르면 None. 서버가 Content-Length 를 안 줄 수 있다.
    pub total_bytes: Option<u64>,
    /// 초당 바이트. 표본이 모자라면 None.
    pub bytes_per_sec: Option<u64>,
    /// 남은 초. 총량이나 속도를 모르면 None.
    pub eta_secs: Option<u64>,
}

/// 0..=100 으로 잘라낸 정수 퍼센트.
///
/// 총량이 0 이거나 없으면 None 을 돌려 불확정 진행으로 표시하게 한다.
/// 0 으로 나누는 실수를 호출부마다 반복하지 않기 위해 여기 가둔다.
pub fn percent(done: u64, total: Option<u64>) -> Option<u8> {
    let total = total?;
    if total == 0 {
        return None;
    }
    // u64 곱셈 넘침을 피하려고 u128 로 올린다. 3GB 정도면 안 넘치지만
    // 총량이 비정상적으로 크게 들어와도 무너지지 않게 한다.
    let p = (done as u128 * 100 / total as u128).min(100);
    Some(p as u8)
}

/// 남은 시간(초).
///
/// 속도가 0 이면 나눌 수 없으므로 None. 이미 다 됐으면 0.
pub fn eta_secs(done: u64, total: Option<u64>, bytes_per_sec: Option<u64>) -> Option<u64> {
    let total = total?;
    let rate = bytes_per_sec?;
    if rate == 0 {
        return None;
    }
    Some(total.saturating_sub(done) / rate)
}

/// 이동 평균 속도 계산기.
///
/// 순간 속도를 그대로 쓰면 표시가 심하게 튀어서 읽을 수 없다.
/// 지수 이동 평균으로 완만하게 만든다.
#[derive(Debug, Clone)]
pub struct RateEstimator {
    smoothed: Option<f64>,
    /// 0.0~1.0. 클수록 최근 값에 민감하다.
    alpha: f64,
}

impl RateEstimator {
    pub fn new() -> Self {
        Self {
            smoothed: None,
            alpha: 0.3,
        }
    }

    /// 표본 하나를 반영하고 현재 추정치를 돌려준다.
    ///
    /// `elapsed_secs` 가 0 이하면 무시한다 — 나눌 수 없고, 타이머 해상도 때문에
    /// 실제로 0 이 들어올 수 있다.
    pub fn sample(&mut self, bytes: u64, elapsed_secs: f64) -> Option<u64> {
        if elapsed_secs <= 0.0 {
            return self.smoothed.map(|v| v as u64);
        }
        let instant = bytes as f64 / elapsed_secs;
        self.smoothed = Some(match self.smoothed {
            None => instant,
            Some(prev) => self.alpha * instant + (1.0 - self.alpha) * prev,
        });
        self.smoothed.map(|v| v as u64)
    }

    pub fn current(&self) -> Option<u64> {
        self.smoothed.map(|v| v as u64)
    }
}

impl Default for RateEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// 사람이 읽는 용량 표기.
///
/// 저장장치 업계 관례대로 1000 단위를 쓴다. USB 에 "32GB" 라고 적혀 있으면
/// 사용자는 화면에서도 32GB 를 보길 기대하지, 29.8GiB 를 기대하지 않는다.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{:.0} {}", v, UNITS[i])
    } else if v >= 10.0 {
        format!("{:.1} {}", v, UNITS[i])
    } else {
        format!("{:.2} {}", v, UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_basic() {
        assert_eq!(percent(0, Some(100)), Some(0));
        assert_eq!(percent(50, Some(100)), Some(50));
        assert_eq!(percent(100, Some(100)), Some(100));
    }

    #[test]
    fn percent_clamps_overshoot() {
        // 서버가 알려준 크기보다 실제로 더 받는 경우가 있다.
        assert_eq!(percent(150, Some(100)), Some(100));
    }

    #[test]
    fn percent_unknown_total_is_indeterminate() {
        assert_eq!(percent(50, None), None);
        assert_eq!(percent(50, Some(0)), None);
    }

    #[test]
    fn percent_survives_huge_totals() {
        // u64 곱셈이었다면 done * 100 에서 넘쳐 엉뚱한 값이 나왔을 크기.
        assert_eq!(percent(1u64 << 62, Some(1u64 << 63)), Some(50));
        // 상한 근처에서도 무너지지 않는다 (절삭으로 49).
        assert_eq!(percent(u64::MAX / 2, Some(u64::MAX)), Some(49));
        assert_eq!(percent(u64::MAX, Some(u64::MAX)), Some(100));
    }

    #[test]
    fn eta_basic() {
        assert_eq!(eta_secs(0, Some(1000), Some(100)), Some(10));
        assert_eq!(eta_secs(500, Some(1000), Some(100)), Some(5));
        assert_eq!(eta_secs(1000, Some(1000), Some(100)), Some(0));
    }

    #[test]
    fn eta_handles_missing_inputs() {
        assert_eq!(eta_secs(0, None, Some(100)), None);
        assert_eq!(eta_secs(0, Some(1000), None), None);
        assert_eq!(eta_secs(0, Some(1000), Some(0)), None);
    }

    #[test]
    fn eta_does_not_underflow_past_completion() {
        // done > total 이어도 음수로 뒤집히지 않는다.
        assert_eq!(eta_secs(2000, Some(1000), Some(100)), Some(0));
    }

    #[test]
    fn rate_estimator_smooths() {
        let mut r = RateEstimator::new();
        assert_eq!(r.sample(100, 1.0), Some(100));
        // 순간값이 튀어도 표시는 완만하게 따라간다.
        let second = r.sample(1000, 1.0).unwrap();
        assert!(second > 100 && second < 1000, "실제값: {second}");
    }

    #[test]
    fn rate_estimator_ignores_zero_elapsed() {
        let mut r = RateEstimator::new();
        r.sample(100, 1.0);
        let before = r.current();
        assert_eq!(r.sample(999_999, 0.0), before);
    }

    #[test]
    fn rate_estimator_starts_empty() {
        assert_eq!(RateEstimator::new().current(), None);
    }

    #[test]
    fn format_bytes_uses_decimal_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1000), "1.00 KB");
        assert_eq!(format_bytes(30_752_000_000), "30.8 GB");
        assert_eq!(format_bytes(605_888_202), "606 MB");
    }

    #[test]
    fn format_bytes_matches_label_on_the_stick() {
        // 32GB 스틱의 실제 용량이 대략 이 정도. 화면에도 30GB 대로 보여야
        // 사용자가 자기 USB 를 알아본다.
        let s = format_bytes(30_752_000_000);
        assert!(s.ends_with(" GB"), "실제: {s}");
    }
}
