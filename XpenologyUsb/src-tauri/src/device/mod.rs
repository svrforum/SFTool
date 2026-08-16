//! 장치 계층 — Windows API 를 부르는 유일한 곳.
//!
//! 여기 있는 트레이트가 이 프로그램의 검증 전략을 지탱한다. 실제 구현은
//! Windows 에서만 동작하고 실제 USB 가 있어야 의미가 있지만, 트레이트로 갈라두면
//! 가짜 구현을 끼워 넣어 다음을 하드웨어 없이 확인할 수 있다:
//!
//! - 내장 디스크가 목록에 절대 나타나지 않는다는 안전 규칙
//! - 내려받기 → 압축 해제 → 쓰기 → 검증 전체 파이프라인 (쓰기 대상만 임시 파일)
//! - UI 전체 흐름 (개발 환경인 Linux 에서 앱을 그대로 띄운다)

use crate::core::model::DiskInfo;
use std::collections::HashSet;

pub mod fake;

#[cfg(windows)]
pub mod windows;

/// 장치 계층에서 발생하는 오류.
///
/// 사용자에게 다른 안내를 해야 하는 것들을 구분해 둔다. 원인을 뭉뚱그리면
/// "알 수 없는 오류"밖에 보여줄 수 없다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    /// 관리자 권한이 없다.
    NeedsElevation,
    /// 장치를 찾을 수 없다. 목록을 만든 뒤 뽑혔을 가능성이 높다.
    NotFound { disk_number: u32 },
    /// 다른 프로그램이 붙잡고 있어 잠글 수 없다.
    ///
    /// 재시도 한도까지 기다린 뒤에도 실패한 경우다. 누가 잡고 있는지는
    /// 알려주지 않는다 — raw 핸들 보유자는 Restart Manager 로 식별되지 않는다.
    Locked,
    /// 모든 잠금이 성공했는데도 쓰기가 거부됐다.
    ///
    /// Defender 의 Controlled Folder Access 가 원인일 수 있으나 확증된 바는 없다.
    /// 안내에서는 가능성으로만 제시한다.
    WriteDenied,
    /// 쓰는 도중 장치가 사라졌거나 교체됐다.
    MediaChanged,
    /// 섹터 크기가 비정상이다.
    BadSectorSize(u32),
    /// 쓰기 직전 확인에서 다른 장치로 판명됐다.
    IdentityChanged,
    /// 그 외 입출력 오류. OS 오류 코드와 설명을 담는다.
    Io { code: i32, message: String },
}

/// 연결된 디스크를 열거한다.
pub trait UsbEnumerator: Send + Sync {
    /// 시스템의 모든 디스크. 여기서 거르지 않는다 —
    /// 안전 판정은 [`crate::core::safety`] 가 담당한다.
    ///
    /// 열거 자체가 계층을 나누는 이유는, 안전 규칙을 순수 함수로 유지해
    /// 하드웨어 없이 테스트하기 위해서다.
    fn list_disks(&self) -> Result<Vec<DiskInfo>, DeviceError>;

    /// 절대 건드리면 안 되는 디스크 번호 집합.
    ///
    /// 시스템 드라이브, 윈도우 폴더, 실행 중인 프로그램, 페이지파일이 올라간
    /// 디스크를 커널에 직접 물어 구한다. WMI 정보와 독립적이어야 의미가 있다.
    ///
    /// 볼륨이 여러 extent 에 걸쳐 있을 수 있으므로(미러링된 C:, 저장소 공간)
    /// **모든 extent 의 디스크 번호를 합집합으로** 모은다. 단일 extent 만
    /// 가정하면 그런 시스템에서 보호가 통째로 비어버린다.
    fn protected_disk_numbers(&self) -> Result<HashSet<u32>, DeviceError>;
}

/// 쓰기 세션 — 열려 있는 대상 장치.
///
/// 획득(잠금)과 준비(레이아웃 초기화)를 마친 상태로 넘어온다.
pub trait WriteSession: Send {
    /// 열린 핸들에서 직접 읽은 장치 정보.
    ///
    /// 사용자가 고른 것과 대조해 TOCTOU 를 막는다. 디스크 번호는 재사용되므로
    /// 번호만으로는 같은 장치임을 보장하지 못한다.
    fn observed(&self) -> &DiskInfo;

    /// 장치가 보고한 논리 섹터 크기. 512 를 가정하지 않는다.
    fn sector_size(&self) -> u32;

    /// 장치의 정확한 바이트 크기.
    fn total_bytes(&self) -> u64;

    /// 한 덩어리를 쓴다.
    ///
    /// `offset` 과 `data.len()` 은 모두 섹터 크기의 배수여야 한다.
    /// 마지막 덩어리를 패딩할 때는 남는 부분을 **명시적으로 0으로 채운다** —
    /// 할당된 그대로 넘기면 힙 내용이 그대로 USB 에 실린다.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DeviceError>;

    /// 장치 끝의 지정 바이트를 0으로 덮는다.
    ///
    /// 이미지가 USB 보다 작을 때 이전 GPT 백업 헤더가 장치 끝에 남아 있으면
    /// Windows 가 옛 파티션 테이블을 되살린다.
    fn zero_tail(&mut self, bytes: u64) -> Result<(), DeviceError>;

    /// 되읽기 (검증용).
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError>;

    /// 마무리 — 플러시, 파티션 테이블 재인식, 잠금 해제, 꺼내기.
    fn finish(self: Box<Self>) -> Result<(), DeviceError>;
}

/// 대상 장치를 열어 쓰기 세션을 만든다.
pub trait RawWriter: Send + Sync {
    /// 지정한 디스크를 쓰기용으로 연다.
    ///
    /// 구현은 다음을 이 순서로 수행해야 한다:
    /// 마운트 지점 제거 → 논리 볼륨 잠금(최소 하나) → 준비용 물리 핸들로
    /// RAW 레이아웃 적용 후 닫기 → 쓰기용 물리 핸들 새로 열기.
    ///
    /// 준비용 핸들을 그대로 들고 쓰기로 넘어가면 재열거 때문에
    /// [`DeviceError::MediaChanged`] 가 난다.
    fn open(&self, disk: &DiskInfo) -> Result<Box<dyn WriteSession>, DeviceError>;
}

/// 읽기 전용 세션 — 열려 있는 **원본** 장치.
///
/// 쓰기 세션과 트레이트를 나눈 이유는 타입만 봐도 방향이 드러나게 하기 위해서다.
/// 복제에서 원본과 대상을 뒤바꾸는 실수는 사용자의 데이터를 지우는 결과로
/// 이어지므로, 원본 쪽에는 쓰는 수단이 아예 없어야 한다.
pub trait ReadSession: Send {
    /// 열린 핸들에서 직접 읽은 장치 정보. TOCTOU 확인용.
    fn observed(&self) -> &DiskInfo;
    fn sector_size(&self) -> u32;
    fn total_bytes(&self) -> u64;
    /// 한 덩어리를 읽는다. `offset` 과 `buf.len()` 은 섹터 크기의 배수여야 한다.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError>;
    fn finish(self: Box<Self>) -> Result<(), DeviceError>;
}

/// 원본 장치를 읽기용으로 연다.
///
/// 구현은 **잠그지 않고, 마운트를 해제하지 않고, 레이아웃을 지우지 않는다.**
/// 원본은 사용자가 이미 잘 쓰고 있는 USB 이므로 복제가 그것을 건드려서는 안 된다.
pub trait RawReader: Send + Sync {
    fn open(&self, disk: &DiskInfo) -> Result<Box<dyn ReadSession>, DeviceError>;
}
