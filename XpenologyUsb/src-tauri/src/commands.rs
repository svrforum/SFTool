//! 프런트엔드 경계.
//!
//! 여기서 도메인 타입을 UI 가 쓰기 좋은 형태로 바꾼다. 안전 판정 결과를
//! 문자열이 아니라 구조로 넘겨서, 표시 문구는 프런트엔드의 i18n 이 정하게 한다.

use crate::core::model::DiskInfo;
use crate::core::progress::format_bytes;
use crate::core::safety::{self, Availability, Rejection};
use crate::device::UsbEnumerator;
use serde::Serialize;

/// 목록에 표시할 디스크 하나.
#[derive(Debug, Clone, Serialize)]
pub struct DiskEntry {
    pub number: u32,
    pub name: String,
    pub size_bytes: u64,
    /// 사람이 읽는 용량. 계산을 프런트엔드에 중복 구현하지 않기 위해 여기서 만든다.
    pub size_label: String,
    /// 현재 붙어 있는 드라이브 문자들. 사용자가 자기 USB 를 알아보는 단서다.
    pub drive_letters: Vec<String>,
    /// 선택 가능한가.
    pub ready: bool,
    /// 선택할 수 없다면 그 사유 코드. 프런트엔드가 문구로 번역한다.
    pub blocked_reason: Option<String>,
    /// 사유에 딸린 수치 (용량 부족일 때 최소 요구 용량 등).
    pub blocked_detail: Option<String>,
}

/// 원본 분석 결과. 확인 화면이 "복사할 양" 을 보여주는 데 쓴다.
#[derive(Debug, Clone, Serialize)]
pub struct SourcePlan {
    pub bytes: u64,
    /// 사람이 읽는 용량. 계산을 프런트엔드에 중복 구현하지 않는다.
    pub size_label: String,
    pub partitions: u32,
    pub scheme: String,
}

impl From<crate::core::layout::Layout> for SourcePlan {
    fn from(l: crate::core::layout::Layout) -> Self {
        Self {
            bytes: l.bytes,
            size_label: format_bytes(l.bytes),
            partitions: l.partitions,
            scheme: match l.scheme {
                crate::core::layout::Scheme::Mbr => "MBR".to_string(),
            },
        }
    }
}

/// 사유를 UI 가 번역할 수 있는 안정적인 코드로 바꾼다.
///
/// 문자열 자체를 넘기지 않는 이유는 언어 전환 때문이다. 백엔드가 한국어 문장을
/// 만들어 넘기면 영어로 바꿀 수 없다.
fn reason_code(r: &Rejection) -> (&'static str, Option<String>) {
    match r {
        Rejection::ReadOnly => ("read_only", None),
        Rejection::BelowMinimumCapacity { minimum, .. } => {
            ("too_small_for_any_image", Some(format_bytes(*minimum)))
        }
        Rejection::TooSmall { need, .. } => ("image_too_large", Some(format_bytes(*need))),
        Rejection::NoMedia => ("no_media", None),
        Rejection::SpannedVolume => ("spanned_volume", None),
        Rejection::SourceOnTarget => ("source_on_target", None),
        Rejection::SameDisk => ("same_disk", None),
        // 아래는 감춰지는 사유라 UI 에 도달하지 않는다.
        Rejection::NotUsb(_) => ("not_usb", None),
        Rejection::DiskZero => ("disk_zero", None),
        Rejection::SystemDisk => ("system_disk", None),
        Rejection::Protected => ("protected", None),
    }
}

fn to_entry(disk: &DiskInfo, availability: Availability) -> DiskEntry {
    let (ready, blocked_reason, blocked_detail) = match &availability {
        Availability::Ready => (true, None, None),
        Availability::Disabled(r) | Availability::Hidden(r) => {
            let (code, detail) = reason_code(r);
            (false, Some(code.to_string()), detail)
        }
    };
    DiskEntry {
        number: disk.number,
        name: disk.friendly_name.clone(),
        size_bytes: disk.size_bytes,
        size_label: format_bytes(disk.size_bytes),
        drive_letters: disk
            .volumes
            .iter()
            .filter_map(|v| v.drive_letter)
            .map(|c| format!("{c}:"))
            .collect(),
        ready,
        blocked_reason,
        blocked_detail,
    }
}

/// 목록 화면에 필요한 전부.
///
/// 목록만 돌려주던 시절에는 열거에서 **빠진** 장치의 사유를 담을 자리가
/// 아예 없었다. 그래서 사용자의 USB 하나만 조회에 실패하면 — 다른 디스크가
/// 하나라도 읽히는 한 — 그 USB 는 아무 설명 없이 목록에서 사라졌고, 화면에는
/// "USB를 찾지 못했습니다" 만 남았다. 백엔드는 그 장치의 Win32 오류 코드를
/// 손에 쥔 채였다.
#[derive(Debug, Clone, Serialize)]
pub struct DiskList {
    pub disks: Vec<DiskEntry>,
    /// 열거에서 빠진 장치와 그 사유. 목록 밑에 그대로 보여준다.
    pub notes: Vec<String>,
}

/// 목록에 보여줄 디스크들.
///
/// 감춰야 할 것은 여기서 이미 빠진다. 프런트엔드는 걸러내는 책임을 지지 않는다 —
/// UI 버그가 내장 디스크를 노출시키는 경로를 아예 만들지 않기 위해서다.
pub fn list_disks_with(enumerator: &dyn UsbEnumerator) -> Result<DiskList, String> {
    let protected = enumerator
        .protected_disk_numbers()
        .map_err(|e| format!("{e:?}"))?;
    let disks = enumerator.list_disks().map_err(|e| format!("{e:?}"))?;

    Ok(DiskList {
        disks: disks
            .iter()
            .filter_map(|d| {
                let a = safety::availability(d, &protected);
                if a.is_visible() {
                    Some(to_entry(d, a))
                } else {
                    None
                }
            })
            .collect(),
        notes: enumerator.skipped(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::fake::FakeEnumerator;

    #[test]
    fn listing_hides_internal_disks_entirely() {
        let entries = list_disks_with(&FakeEnumerator::sample()).unwrap().disks;
        // 표본에는 시스템 NVMe 와 SATA HDD 가 들어 있다. 하나도 나오면 안 된다.
        assert!(!entries.iter().any(|e| e.name.contains("990 PRO")));
        assert!(!entries.iter().any(|e| e.name.contains("WDC")));
        assert!(!entries.iter().any(|e| e.number == 0));
    }

    #[test]
    fn listing_keeps_unusable_sticks_visible_with_a_reason() {
        let entries = list_disks_with(&FakeEnumerator::sample()).unwrap().disks;
        let small = entries.iter().find(|e| e.number == 4).unwrap();
        assert!(!small.ready);
        assert_eq!(
            small.blocked_reason.as_deref(),
            Some("too_small_for_any_image")
        );
        // 최소 요구 용량을 함께 넘겨 "8GB 이상이 필요합니다" 를 만들 수 있게 한다.
        assert!(small.blocked_detail.is_some());
    }

    #[test]
    fn empty_card_reader_is_not_listed() {
        let entries = list_disks_with(&FakeEnumerator::sample()).unwrap().disks;
        assert!(!entries.iter().any(|e| e.size_bytes == 0));
    }

    #[test]
    fn ready_entries_carry_display_fields() {
        let entries = list_disks_with(&FakeEnumerator::sample()).unwrap().disks;
        let ok = entries.iter().find(|e| e.ready).unwrap();
        assert!(ok.size_label.ends_with(" GB"), "실제: {}", ok.size_label);
        assert!(ok.blocked_reason.is_none());
    }

    /// 일부만 열거된 경우에도 **빠진 장치의 사유가 화면까지 간다.**
    ///
    /// 예전에는 목록이 통째로 비었을 때만 사유가 올라갔다. 내장 SSD 가 하나만
    /// 읽혀도 목록은 비어 있지 않으므로, 정작 사용자가 찾는 USB 가 조회 실패로
    /// 빠지면 아무 설명 없이 사라졌다. 사용자가 할 수 있는 일은 USB 를 다시
    /// 꽂아보는 것뿐이었고, 원인은 백엔드 안에만 있었다.
    #[test]
    fn a_disk_that_was_skipped_is_explained_even_when_others_enumerated() {
        let e = FakeEnumerator::sample().with_skipped(vec![
            "디스크 2: 용량 조회 실패 (Win32 21 — 장치가 준비되지 않음)".into(),
        ]);
        let out = list_disks_with(&e).unwrap();
        assert!(
            !out.disks.is_empty(),
            "이 상황은 일부가 열거된 경우여야 한다"
        );
        assert_eq!(out.notes.len(), 1, "빠진 사유가 사라졌다");
        assert!(out.notes[0].contains("21"), "실제: {}", out.notes[0]);
    }

    #[test]
    fn drive_letters_are_exposed_for_recognition() {
        let entries = list_disks_with(&FakeEnumerator::sample()).unwrap().disks;
        let sandisk = entries.iter().find(|e| e.number == 2).unwrap();
        assert_eq!(sandisk.drive_letters, vec!["E:"]);
    }
}
