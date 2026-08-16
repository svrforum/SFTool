//! 플랫폼에 의존하지 않는 핵심 로직.
//!
//! 이 모듈 안의 코드는 Windows API 를 부르지 않는다. 덕분에 개발 환경(Linux)과
//! CI 양쪽에서 전부 단위 테스트되며, 실제 USB 없이도 안전 규칙을 검증할 수 있다.
//!
//! Windows 전용 코드는 `crate::device` 에 트레이트 뒤로 격리한다.

pub mod layout;
pub mod loader;
pub mod model;
pub mod pipeline;
pub mod progress;
pub mod runner;
pub mod safety;
pub mod sink;
pub mod source;
