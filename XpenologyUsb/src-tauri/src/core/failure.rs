use super::{cloner::CloneError, runner::RunError};
use crate::device::DeviceError;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Failure {
    pub code: &'static str,
    pub detail: String,
    pub target_erased: bool,
}

impl Failure {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            target_erased: false,
        }
    }

    fn erased(mut self) -> Self {
        self.target_erased = true;
        self
    }
}

impl From<DeviceError> for Failure {
    fn from(error: DeviceError) -> Self {
        let code = match &error {
            DeviceError::NeedsElevation => "needs_elevation",
            DeviceError::NotFound { .. } | DeviceError::MediaChanged { .. } => "media_changed",
            DeviceError::IdentityChanged { .. } => "identity_changed",
            DeviceError::Locked { .. } => "locked",
            DeviceError::WriteDenied { .. } => "write_denied",
            DeviceError::BadSectorSize(_) => "image_invalid",
            // 대상을 지운 뒤 장치가 쓰기를 거부했으면 쓰기 거부 안내로 보낸다.
            // 원인 안내(제어된 폴더 액세스·백신)가 필요한 것은 그쪽이고, USB 가
            // 비워졌다는 사실은 `erased` 가 따로 싣는다. 코드 5 로 가리지 않는다 —
            // 플러시 실패나 안전 제거의 CONFIGRET 5 까지 쓰기 거부로 보이게 된다.
            DeviceError::TargetErased { refused: true, .. } => "write_denied",
            DeviceError::TargetErased { .. } => "target_erased",
            DeviceError::Io { .. } => "generic",
        };
        let erased = matches!(&error, DeviceError::TargetErased { .. });
        let detail = match error {
            DeviceError::Io { code, message } | DeviceError::TargetErased { code, message, .. } => {
                format!("{message}\nOS code: {code}")
            }
            DeviceError::IdentityChanged { message } => message,
            other => format!("{other:?}"),
        };
        Self {
            code,
            detail,
            target_erased: erased,
        }
    }
}

impl From<RunError> for Failure {
    fn from(error: RunError) -> Self {
        match error {
            RunError::Resolve(s) | RunError::Download(s) => Self::new("network", s),
            RunError::Extract(s) => Self::new("image_invalid", s),
            RunError::Rejected(r) => Self::new("rejected", format!("{r:?}")),
            RunError::IdentityChanged(s) => Self::new("identity_changed", s),
            RunError::Device(e) => e.into(),
            RunError::VerifyMismatch { at, reread } => Self::new(
                "verify_mismatch",
                format!("VerifyMismatch {{ at: {at}, reread: {reread:?} }}"),
            )
            .erased(),
            RunError::TargetErased { cause } => Self::from(*cause).erased(),
            RunError::Canceled => Self::new("canceled", ""),
        }
    }
}

impl From<CloneError> for Failure {
    fn from(error: CloneError) -> Self {
        match error {
            CloneError::Layout(super::layout::LayoutError::Gpt) => Self::new("layout_gpt", ""),
            CloneError::Layout(e) => Self::new("layout_nosig", format!("{e:?}")),
            CloneError::Rejected(r) => Self::new("rejected", format!("{r:?}")),
            CloneError::SourceIdentityChanged(s) | CloneError::TargetIdentityChanged(s) => {
                Self::new("identity_changed", s)
            }
            CloneError::Device(e) => e.into(),
            CloneError::Source(s) => Self::new("image_invalid", s),
            CloneError::VerifyMismatch { at, reread } => Self::new(
                "verify_mismatch",
                format!("VerifyMismatch {{ at: {at}, reread: {reread:?} }}"),
            )
            .erased(),
            CloneError::TargetErased { cause } => Self::from(*cause).erased(),
            CloneError::Canceled => Self::new("canceled", ""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destructive_cancellation_preserves_both_cause_and_device_state() {
        let failure = Failure::from(RunError::TargetErased {
            cause: Box::new(RunError::Canceled),
        });
        assert_eq!(failure.code, "canceled");
        assert!(failure.target_erased);
        assert_eq!(
            Failure::from(RunError::Download("timeout".into())).code,
            "network"
        );
    }

    /// 쓰는 도중 장치가 거부하면(Win32 5) 쓰기 거부 안내가 나가야 한다.
    ///
    /// 0.5.0 에서 오류가 코드로 넘어가기 시작하면서, `with_write_context` 가
    /// 만든 `Io { code: 5 }` 가 `generic` 으로 떨어졌다. 화면에는 "실패했습니다"
    /// 만 남고 "장치가 쓰기를 거부했습니다" 는 접힌 상세 안으로 숨었다.
    #[test]
    fn a_write_refused_mid_write_keeps_the_write_denied_guidance() {
        let refused = crate::device::prep::with_write_context(
            DeviceError::WriteDenied {
                op: crate::device::prep::DEVICE_WRITE,
            },
            "쓰기",
            Some(24 * 1024 * 1024),
            "준비 단계: 볼륨 1개 잠금, 실패 기록 없음",
        );
        let failure = Failure::from(RunError::TargetErased {
            cause: Box::new(RunError::Device(refused)),
        });
        assert_eq!(failure.code, "write_denied");
        assert!(
            failure.target_erased,
            "USB 가 이미 비워졌다는 사실을 잃었다"
        );
        assert!(failure.detail.contains("쓰기를 거부"), "{}", failure.detail);
    }

    /// 파티션 테이블을 지운 뒤 준비 단계에서 섹터 쓰기가 거부된 경우도 같다.
    #[test]
    fn a_write_refused_after_the_layout_was_erased_is_still_a_refusal() {
        let mut prep = crate::device::prep::Prep::new();
        prep.reached(crate::device::prep::Touched::Layout);
        let failure = Failure::from(prep.explain(DeviceError::WriteDenied {
            op: crate::device::prep::DEVICE_WRITE,
        }));
        assert_eq!(failure.code, "write_denied");
        assert!(failure.target_erased);
    }

    /// 거부가 아닌 이유로 지워진 뒤 멈춘 것은 지금처럼 `target_erased` 다.
    #[test]
    fn other_failures_after_erasure_stay_target_erased() {
        let mut prep = crate::device::prep::Prep::new();
        prep.reached(crate::device::prep::Touched::Layout);
        let failure = Failure::from(prep.explain(DeviceError::Locked {
            op: "볼륨 잠금"
        }));
        assert_eq!(failure.code, "target_erased");
        assert!(failure.target_erased);
    }

    /// 코드 5 라는 것만으로 쓰기 거부가 되지는 않는다. 이미지를 다 쓴 뒤
    /// 플러시만 실패했을 때는 지워졌다는 안내가 맞다.
    #[test]
    fn a_code_five_that_was_not_a_refusal_is_not_shown_as_one() {
        // `ioctl::flush` 는 Win32 5 를 `WriteDenied { op: "캐시 플러시" }` 로 올린다.
        let flush = crate::device::prep::with_write_context(
            DeviceError::WriteDenied {
                op: "캐시 플러시"
            },
            "마무리(캐시 플러시)",
            None,
            "준비 단계: 볼륨 1개 잠금, 실패 기록 없음",
        );
        assert_eq!(Failure::from(flush).code, "target_erased");
        // 안전 제거는 Win32 코드가 아니라 CONFIGRET 를 싣는다. 5 는
        // CR_INVALID_DEVNODE 다.
        let eject = DeviceError::Io {
            code: 5,
            message: "제거가 거부되었습니다 (사유 코드 0)".into(),
        };
        assert_eq!(Failure::from(eject).code, "generic");
    }
}
