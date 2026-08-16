//! 작업 파이프라인과 진행 보고.
//!
//! 단계 전이와 진행률 계산을 여기 모아 둔다. 실제 입출력은 트레이트 뒤에 있어서
//! 가짜 구현으로 전체 흐름을 테스트할 수 있다.
//!
//! 진행 상황은 사용자가 "지금 뭐 하는 중인지" 알 수 있을 만큼 자세해야 한다.
//! 3GB 를 받는 동안 아무 표시가 없으면 멈춘 줄 알고 창을 닫는다.

use super::progress::{eta_secs, percent, RateEstimator, Stage};
use serde::{Deserialize, Serialize};

/// UI 로 보내는 진행 보고 한 건.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressEvent {
    /// 지금 수행 중인 단계.
    pub stage: Stage,
    /// 이 단계의 진행률. 총량을 모르면 None (불확정 표시).
    pub percent: Option<u8>,
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_sec: Option<u64>,
    pub eta_secs: Option<u64>,
    /// 전체 작업 중 완료된 단계들. UI 가 체크 표시를 그리는 데 쓴다.
    pub completed: Vec<Stage>,
    /// 이번 단계에 딸린 부가 정보 (받는 파일 이름 등).
    pub detail: Option<String>,
}

/// 전체 작업에서 거치는 단계 순서.
///
/// 검증은 선택이므로 실행 시점에 포함 여부가 정해진다.
pub fn planned_stages(verify: bool) -> Vec<Stage> {
    let mut v = vec![
        Stage::Resolving,
        Stage::Downloading,
        Stage::Extracting,
        Stage::Preparing,
        Stage::Writing,
    ];
    if verify {
        v.push(Stage::Verifying);
    }
    v.push(Stage::Finishing);
    v
}

/// 진행 보고를 만들어 내보내는 쪽.
///
/// 단계 전이를 여기로 모아서, 완료 목록을 갱신하는 것을 호출부마다
/// 반복하지 않게 한다. 빠뜨리면 UI 에서 체크 표시가 남지 않는다.
pub struct ProgressReporter<F: FnMut(ProgressEvent)> {
    emit: F,
    completed: Vec<Stage>,
    current: Stage,
    rate: RateEstimator,
    detail: Option<String>,
    /// 마지막으로 내보낸 퍼센트. 같은 값을 반복해서 보내지 않는다.
    last_percent: Option<u8>,
    /// 속도 계산용 누적치.
    ///
    /// 호출마다 순간 속도를 계산하면 안 된다. 내려받기 콜백은 256KB 읽을 때마다
    /// 불리는데, 소켓 버퍼에 쌓여 있던 것을 연달아 읽으면 간격이 0.1ms 도 안 되고
    /// 그러면 256KB / 0.0001s = 2.5GB/s 같은 값이 나온다. 실제 회선 속도와
    /// 무관한 숫자라 사용자에게는 그냥 잘못된 정보다.
    /// 최소 이 시간만큼 모은 뒤에 한 번 계산한다.
    window_bytes: u64,
    window_secs: f64,
}

/// 속도를 다시 계산하기까지 모으는 시간.
const RATE_WINDOW_SECS: f64 = 0.5;

impl<F: FnMut(ProgressEvent)> ProgressReporter<F> {
    pub fn new(emit: F) -> Self {
        Self {
            emit,
            completed: Vec::new(),
            current: Stage::Resolving,
            rate: RateEstimator::new(),
            detail: None,
            last_percent: None,
            window_bytes: 0,
            window_secs: 0.0,
        }
    }

    /// 새 단계로 넘어간다. 이전 단계는 완료로 기록된다.
    pub fn begin(&mut self, stage: Stage, detail: Option<String>) {
        if self.current != stage && !self.completed.contains(&self.current) {
            self.completed.push(self.current);
        }
        self.current = stage;
        self.detail = detail;
        self.rate = RateEstimator::new();
        self.last_percent = None;
        self.window_bytes = 0;
        self.window_secs = 0.0;
        self.emit_now(0, None);
    }

    /// 진행량을 갱신한다.
    ///
    /// 퍼센트가 바뀌지 않았으면 내보내지 않는다. 3GB 를 32KB 씩 읽으면
    /// 십만 번 넘게 호출되는데, 그대로 보내면 UI 스레드가 이벤트에 잠긴다.
    pub fn update(&mut self, done: u64, total: Option<u64>, elapsed_secs: f64, chunk: u64) {
        // 시간 창으로 묶어 계산한다. 호출마다 계산하면 간격이 너무 짧아
        // 회선 속도와 무관한 값이 나온다.
        self.window_bytes += chunk;
        self.window_secs += elapsed_secs.max(0.0);
        if self.window_secs >= RATE_WINDOW_SECS {
            self.rate.sample(self.window_bytes, self.window_secs);
            self.window_bytes = 0;
            self.window_secs = 0.0;
        }
        let p = percent(done, total);
        if p != self.last_percent {
            self.last_percent = p;
            self.emit_now(done, total);
        }
    }

    /// 마지막 단계까지 끝났음을 알린다.
    pub fn finish(&mut self) {
        if !self.completed.contains(&self.current) {
            self.completed.push(self.current);
        }
        self.emit_now(0, None);
    }

    fn emit_now(&mut self, done: u64, total: Option<u64>) {
        let rate = self.rate.current();
        let ev = ProgressEvent {
            stage: self.current,
            percent: percent(done, total),
            done_bytes: done,
            total_bytes: total,
            bytes_per_sec: rate,
            eta_secs: eta_secs(done, total, rate),
            completed: self.completed.clone(),
            detail: self.detail.clone(),
        };
        (self.emit)(ev);
    }
}

/// 취소 신호. 쓰기 도중에도 확인한다.
pub trait Cancel {
    fn is_canceled(&self) -> bool;
}

/// 취소를 지원하지 않는 기본 구현.
pub struct NeverCancel;
impl Cancel for NeverCancel {
    fn is_canceled(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn collector() -> (Rc<RefCell<Vec<ProgressEvent>>>, impl FnMut(ProgressEvent)) {
        let out = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&out);
        (out, move |e| sink.borrow_mut().push(e))
    }

    #[test]
    fn planned_stages_include_verify_only_when_asked() {
        assert!(!planned_stages(false).contains(&Stage::Verifying));
        assert!(planned_stages(true).contains(&Stage::Verifying));
        // 마무리는 항상 마지막이다.
        assert_eq!(*planned_stages(true).last().unwrap(), Stage::Finishing);
        assert_eq!(*planned_stages(false).last().unwrap(), Stage::Finishing);
    }

    #[test]
    fn beginning_a_stage_marks_the_previous_one_complete() {
        let (out, sink) = collector();
        let mut r = ProgressReporter::new(sink);
        r.begin(Stage::Downloading, None);
        r.begin(Stage::Extracting, None);
        let last = out.borrow().last().unwrap().clone();
        assert_eq!(last.stage, Stage::Extracting);
        assert!(last.completed.contains(&Stage::Resolving));
        assert!(last.completed.contains(&Stage::Downloading));
    }

    #[test]
    fn repeated_percentages_are_not_emitted() {
        // 3GB 를 잘게 읽으면 update 가 십만 번 넘게 불린다.
        // 퍼센트가 그대로면 이벤트를 보내지 않아야 UI 가 버틴다.
        let (out, sink) = collector();
        let mut r = ProgressReporter::new(sink);
        r.begin(Stage::Downloading, None);
        let before = out.borrow().len();
        for i in 0..1000u64 {
            // 총량 100000 에 대해 1 씩 늘리면 퍼센트는 1000 번 중 100 번만 바뀐다.
            r.update(i, Some(100_000), 0.01, 1);
        }
        let emitted = out.borrow().len() - before;
        assert!(emitted <= 100, "이벤트가 너무 많다: {emitted}");
        assert!(emitted > 0, "이벤트가 아예 없다");
    }

    /// 내려받기 콜백은 256KB 마다 불리고, 소켓 버퍼에 쌓인 것을 연달아 읽으면
    /// 간격이 0.1ms 도 안 된다. 그 순간값을 그대로 쓰면 2.5GB/s 같은 숫자가 나온다.
    /// 실제로 사용자 화면에 1GB/s 가 떴다.
    #[test]
    fn burst_reads_do_not_produce_absurd_speeds() {
        let (out, sink) = collector();
        let mut r = ProgressReporter::new(sink);
        r.begin(Stage::Downloading, None);

        // 40MB/s 회선에서 256KB 를 읽는 데 걸리는 시간은 약 6.4ms.
        // 그런데 버퍼에 쌓인 것을 연달아 읽으면 0.05ms 만에 돌아온다.
        // 두 경우를 섞어 실제 상황을 흉내낸다.
        let chunk = 256 * 1024u64;
        let mut done = 0u64;
        for i in 0..400 {
            done += chunk;
            let dt = if i % 8 == 0 { 0.0500 } else { 0.000_05 };
            r.update(done, Some(600_000_000), dt, chunk);
        }

        let speed = out
            .borrow()
            .iter()
            .filter_map(|e| e.bytes_per_sec)
            .max()
            .expect("속도가 보고돼야 한다");

        // 평균 실제 속도는 대략 40MB/s 근처다. 순간값을 쓰면 GB/s 대가 나온다.
        assert!(
            speed < 500_000_000,
            "말이 안 되는 속도가 보고됐다: {} B/s",
            speed
        );
    }

    #[test]
    fn update_reports_percent_and_eta() {
        let (out, sink) = collector();
        let mut r = ProgressReporter::new(sink);
        r.begin(Stage::Downloading, Some("m-shell v1.4.2.8".into()));
        r.update(500, Some(1000), 1.0, 500);
        let e = out.borrow().last().unwrap().clone();
        assert_eq!(e.percent, Some(50));
        assert_eq!(e.total_bytes, Some(1000));
        assert!(e.bytes_per_sec.is_some());
        assert!(e.eta_secs.is_some());
        assert_eq!(e.detail.as_deref(), Some("m-shell v1.4.2.8"));
    }

    #[test]
    fn unknown_total_produces_indeterminate_progress() {
        // 서버가 Content-Length 를 주지 않는 경우.
        let (out, sink) = collector();
        let mut r = ProgressReporter::new(sink);
        r.begin(Stage::Downloading, None);
        r.update(1234, None, 1.0, 1234);
        let e = out.borrow().last().unwrap().clone();
        assert_eq!(e.percent, None);
        assert_eq!(e.eta_secs, None);
    }

    #[test]
    fn finish_marks_the_last_stage_complete() {
        let (out, sink) = collector();
        let mut r = ProgressReporter::new(sink);
        r.begin(Stage::Finishing, None);
        r.finish();
        assert!(out
            .borrow()
            .last()
            .unwrap()
            .completed
            .contains(&Stage::Finishing));
    }

    #[test]
    fn detail_is_cleared_between_stages() {
        let (out, sink) = collector();
        let mut r = ProgressReporter::new(sink);
        r.begin(Stage::Downloading, Some("파일명".into()));
        r.begin(Stage::Writing, None);
        assert_eq!(out.borrow().last().unwrap().detail, None);
    }
}
