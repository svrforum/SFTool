//! raw 디스크 쓰기.
//!
//! 순서를 바꾸면 동작하지 않는다. 각 단계가 왜 그 자리에 있는지는 주석에 남긴다.

use super::ioctl::{self, OwnedHandle};
use crate::core::model::DiskInfo;
use crate::device::{DeviceError, RawWriter, WriteSession};

/// 잠금 재시도 한도. Rufus 와 같은 15초.
const LOCK_RETRIES: u32 = 150;
const LOCK_RETRY_MS: u64 = 100;

pub struct WindowsRawWriter;

impl WindowsRawWriter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsRawWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl RawWriter for WindowsRawWriter {
    fn open(&self, disk: &DiskInfo) -> Result<Box<dyn WriteSession>, DeviceError> {
        let _ = (LOCK_RETRIES, LOCK_RETRY_MS);
        let _ = disk;
        Err(DeviceError::Io { code: 0, message: "미구현".into() })
    }
}

pub use ioctl::OwnedHandle as _Handle;
