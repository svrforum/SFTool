//! 쓰기 전 점검 — 대상을 지우기 **전에** 알 수 있는 위험.
//!
//! 쓰기 도중 거부되면 USB 는 파티션 테이블이 지워진 채로 남고, 사용자는 USB 가
//! 죽었다고 읽는다. 원인을 미리 알 수 있는 것은 여기서 확인 화면으로 올린다.
//! 판정만 여기 두고, 설정을 읽는 Win32 호출은 `device::windows` 에 있다.

/// 확인 화면에 띄울 경고 코드. 프런트엔드가 문구로 번역한다.
pub const CONTROLLED_FOLDER_ACCESS: &str = "controlled_folder_access";

/// 제어된 폴더 액세스(CFA)가 부트 레코드 섹터 쓰기를 막는 설정인가.
///
/// Microsoft 문서의 모드 표를 따른다. 1(Enabled/Block)은 "Untrusted apps can't
/// ... write to disk sectors", 3(Block disk modification only)은 부트 레코드
/// 섹터 쓰기만 막는다. 이 프로그램은 서명이 없으므로 허용 목록에 넣지 않은
/// 한 신뢰되지 않은 앱이다. 막히는 것은 언제나 대상을 바꾸기 시작한 **뒤**라서
/// 그 USB 는 그대로 쓸 수 없게 된다. 준비 단계의 앞부분 지우기 실패는 기록만 하고
/// 넘어가므로, 실제로 실패하는 자리는 첫 이미지 쓰기이거나(파티션 부트 섹터까지
/// 막는 경우) 마지막 파티션 테이블 쓰기다. 어느 쪽인지는 실물로만 확인된다.
///
/// 그룹 정책 값이 있으면 로컬 값을 이긴다. 읽지 못했거나 문서에 없는 값은
/// "모른다" 로 두고 경고하지 않는다 — 허용 목록에 넣었는지는 알 수 없으므로
/// 이 판정은 막는 데 쓰지 않고 확인을 한 번 더 받는 데만 쓴다.
pub fn cfa_blocks_boot_sector(policy: Option<u32>, local: Option<u32>) -> bool {
    matches!(policy.or(local), Some(1 | 3))
}

/// 확인 화면에 띄울 경고들.
pub fn warnings(cfa_policy: Option<u32>, cfa_local: Option<u32>) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cfa_blocks_boot_sector(cfa_policy, cfa_local) {
        out.push(CONTROLLED_FOLDER_ACCESS);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Microsoft 문서의 모드 표: 1 Enabled(Block) 과 3 Block disk modification
    /// only 만 섹터 쓰기를 막는다. 0 Disabled, 2 Audit, 4 Audit disk modification
    /// only 는 기록만 하고 막지 않는다.
    #[test]
    fn only_the_two_blocking_modes_warn() {
        for (value, blocks) in [(0, false), (1, true), (2, false), (3, true), (4, false)] {
            assert_eq!(
                cfa_blocks_boot_sector(None, Some(value)),
                blocks,
                "모드 {value}"
            );
        }
    }

    /// 그룹 정책 값이 있으면 그것이 로컬 설정을 이긴다.
    #[test]
    fn group_policy_overrides_the_local_setting() {
        assert!(!cfa_blocks_boot_sector(Some(0), Some(1)));
        assert!(cfa_blocks_boot_sector(Some(3), Some(0)));
    }

    /// 읽지 못했거나 문서에 없는 값이면 경고하지 않는다. 판정 실패를 경고로
    /// 바꾸면 모든 사용자가 이유 없는 확인란을 하나 더 누르게 된다.
    #[test]
    fn unknown_or_unreadable_settings_do_not_warn() {
        assert!(!cfa_blocks_boot_sector(None, None));
        assert!(!cfa_blocks_boot_sector(None, Some(7)));
    }

    #[test]
    fn warnings_carry_a_stable_code_for_the_ui() {
        assert_eq!(warnings(None, Some(1)), vec![CONTROLLED_FOLDER_ACCESS]);
        assert!(warnings(None, Some(0)).is_empty());
    }
}
