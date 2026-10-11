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
            DeviceError::TargetErased { .. } => "target_erased",
            DeviceError::Io { .. } => "generic",
        };
        let erased = matches!(&error, DeviceError::TargetErased { .. });
        let detail = match error {
            DeviceError::Io { code, message } | DeviceError::TargetErased { code, message } => {
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
}
