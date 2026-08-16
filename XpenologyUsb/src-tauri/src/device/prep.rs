//! 준비 단계가 대상에 어디까지 손을 댔는지 기록하고, 그것을 사용자에게 말한다.
//!
//! [`crate::device::RawWriter::open`] 은 한 바이트도 쓰기 전에 대상의 드라이브
//! 문자를 떼고 파티션 테이블을 지운다. 그 뒤로 무엇이 실패하든 사용자의 USB 는
//! 이미 변한 뒤다. 그런데 실패는 `Locked` 같은 이름 하나로 올라갔고, 화면에는
//! "탐색기 창을 닫고 다시 시도해 주세요" 가 떴다 — USB 가 멀쩡하다는 전제에서만
//! 맞는 안내다. 사용자는 자기 USB 가 이미 비워졌다는 것을 어디서도 듣지 못하고,
//! 탐색기에서 사라진 USB 를 보며 이 프로그램이 망가뜨렸다고 판단한다.
//!
//! 세션이 만들어진 뒤라면 `Drop` 이 정리를 맡는다. 문제는 **세션이 만들어지기
//! 전**의 창이다. 거기서 나가는 오류는 `Drop` 도, 세션에 실린 준비 기록도
//! 만나지 못한 채 스택과 함께 사라진다.
//!
//! 이 모듈이 Windows 밖에서도 컴파일되는 이유는, 여기 있는 판단 — 어디까지
//! 손댔는가, 무엇을 문구로 내보낼 것인가 — 이 Win32 와 무관하고 그래야 실제
//! USB 없이 시험할 수 있기 때문이다. Win32 호출은 `device::windows::raw` 에 남는다.

use super::DeviceError;

/// 파티션 테이블까지 지운 뒤에 붙이는 상태 설명.
const ERASED: &str = "이 USB 는 파티션 테이블이 이미 지워진 상태입니다. \
     안에 있던 내용은 돌아오지 않고, 탐색기에는 빈 장치로 보이거나 \
     \"포맷하시겠습니까\" 를 묻습니다. 다시 굽기를 실행하면 정상적으로 끝납니다.";

/// 드라이브 문자만 뗀 뒤에 붙이는 상태 설명. 내용은 그대로다.
const UNMOUNTED: &str = "이 USB 의 드라이브 문자가 떨어진 상태입니다. \
     뽑았다 다시 꽂으면 윈도우가 문자를 새로 붙여줍니다. 안의 내용은 그대로입니다.";

/// 쓰기 세션에서 실패했을 때 붙이는 상태 설명.
///
/// 세션이 존재한다는 것 자체가 준비 단계를 다 지났다는 뜻이므로, 여기서는
/// "지워졌을 수도 있다" 가 아니라 지워졌다고 단정해도 된다.
const PARTIAL: &str = "이 USB 는 파티션 테이블이 지워지고 이미지가 일부만 쓰인 \
     상태입니다. 지금 상태로는 부팅되지 않습니다. 다시 굽기를 실행해 주세요.";

/// 준비 단계가 대상에 어디까지 손을 댔는가. 뒤로 갈수록 되돌리기 어렵다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Touched {
    /// 아직 아무것도 건드리지 않았다. 여기서 실패하면 USB 는 그대로다.
    #[default]
    Nothing,
    /// 드라이브 문자를 뗐다. 다시 꽂으면 윈도우가 새로 붙여준다.
    Mounts,
    /// 파티션 테이블을 지웠다. **다시 꽂아도 돌아오지 않는다.**
    Layout,
}

/// 준비 단계에서 일어난 일.
#[derive(Debug, Default)]
pub struct Prep {
    touched: Touched,
    notes: Vec<String>,
}

impl Prep {
    pub fn new() -> Self {
        Self::default()
    }

    /// 무엇이 안 됐는지 남긴다.
    pub fn note(&mut self, what: impl Into<String>) {
        self.notes.push(what.into());
    }

    /// 여기까지 손댔다고 표시한다. 한 번 나아간 단계는 물러나지 않는다.
    pub fn reached(&mut self, t: Touched) {
        self.touched = self.touched.max(t);
    }

    pub fn touched(&self) -> Touched {
        self.touched
    }

    /// 세션이 들고 다닐 준비 상태 문구.
    ///
    /// `enumerated` 는 볼륨 목록을 **훑을 수 있었는가**다. 이걸 따로 받는 이유는,
    /// 열거 자체가 실패해도 "잠글 볼륨이 하나도 없었다" 와 똑같이 볼륨 0개 ·
    /// 기록 0개로 보이기 때문이다. 예전에는 그 상태가 "준비 완료 (볼륨 0 잠금)"
    /// 으로 나갔다. 아무것도 확인하지 못했는데 다 됐다고 말한 셈이고, 그 줄이
    /// 쓰기 거부 안내 바로 밑에 붙어서 두 거짓말이 서로를 보증했다.
    pub fn summary(&self, locked: usize, enumerated: bool) -> String {
        let head = if enumerated {
            format!("볼륨 {locked}개 잠금")
        } else {
            "볼륨 목록을 읽지 못해 무엇이 마운트돼 있는지 확인하지 못함".to_string()
        };
        if self.notes.is_empty() {
            // "준비 완료" 라고 쓰지 않는다. 기록이 없다는 것은 실패를 보지 못했다는
            // 뜻이지 다 됐다는 뜻이 아니다.
            format!("준비 단계: {head}, 실패 기록 없음")
        } else {
            format!("준비 단계: {head}\n문제:\n  {}", self.notes.join("\n  "))
        }
    }

    /// 준비 도중 난 오류에 **지금 대상이 어떤 상태인지**를 실어 올린다.
    ///
    /// 아직 아무것도 안 건드렸으면 원래 오류가 가장 정확하므로 그대로 둔다.
    pub fn explain(&self, e: DeviceError) -> DeviceError {
        match self.touched {
            Touched::Nothing => e,
            Touched::Mounts => DeviceError::Io {
                code: code_of(&e),
                message: format!("{UNMOUNTED}\n원인: {}\n{}", describe(&e), self.trail()),
            },
            Touched::Layout => DeviceError::TargetErased {
                code: code_of(&e),
                message: format!("{ERASED}\n원인: {}\n{}", describe(&e), self.trail()),
            },
        }
    }

    fn trail(&self) -> String {
        if self.notes.is_empty() {
            "준비 단계에 남은 실패 기록 없음".to_string()
        } else {
            format!("준비 단계 기록:\n  {}", self.notes.join("\n  "))
        }
    }
}

/// 쓰기 세션에서 나온 오류에 위치와 준비 단계 상태를 실어 올린다.
///
/// 세션이 살아 있다는 것은 준비 단계가 끝났다는 뜻이고, 곧 대상이 이미 지워진
/// 뒤라는 뜻이다. 예전에는 이 정보가 `WriteDenied` 한 갈래에만 붙었고 나머지는
/// 변형 이름만 올라가서, 이미지를 다 쓴 뒤 플러시만 실패한 경우조차 화면에는
/// "장치가 쓰기를 거부했습니다 / 백신을 확인하세요" 가 떴다.
///
/// `at` 은 실패한 위치. 플러시처럼 위치가 없는 작업은 None 이다.
pub fn with_write_context(
    e: DeviceError,
    what: &str,
    at: Option<u64>,
    prep_notes: &str,
) -> DeviceError {
    // 쓰기 거부만은 "쓰기를 거부했습니다" 라는 문구를 유지한다. 프런트엔드가
    // 그 문구를 보고 전용 안내를 고르고, 그 안내는 실물에서 여러 번 다듬어진
    // 것이다. 다만 대상 상태는 여기에도 붙인다 — 그 안내만으로는 "다시 시도"
    // 라는 말이 USB 가 아직 멀쩡하다는 뜻으로 읽힌다.
    if let DeviceError::WriteDenied { op } = &e {
        return DeviceError::Io {
            code: 5,
            message: format!(
                "장치가 쓰기를 거부했습니다 ({op}, 오프셋 {}).\n{PARTIAL}\n{prep_notes}",
                at.unwrap_or(0)
            ),
        };
    }
    let at = match at {
        Some(o) => format!("{what} 중 오프셋 {o} 에서 실패했습니다."),
        None => format!("{what} 에서 실패했습니다."),
    };
    DeviceError::TargetErased {
        code: code_of(&e),
        message: format!("{PARTIAL}\n{at}\n원인: {}\n{prep_notes}", describe(&e)),
    }
}

/// 오류에 딸린 Win32 코드. 없으면 0.
pub fn code_of(e: &DeviceError) -> i32 {
    match e {
        DeviceError::WriteDenied { .. } => 5,
        DeviceError::Locked { .. } => 32,
        DeviceError::MediaChanged { .. } => 1110,
        DeviceError::Io { code, .. } | DeviceError::TargetErased { code, .. } => *code,
        _ => 0,
    }
}

/// 오류를 사람이 읽는 한 줄로.
///
/// **변형 이름(Locked, WriteDenied …)은 넣지 않는다.** 프런트엔드는 오류 문자열
/// **전체**에서 그 이름들을 찾아 어떤 화면을 띄울지 고른다. 설명하려고 적어 넣은
/// 이름 하나가 "USB 를 잠글 수 없습니다 / 탐색기를 닫으세요" 화면을 불러오면,
/// 정작 알려야 할 상태는 그 밑에 묻힌다.
pub fn describe(e: &DeviceError) -> String {
    match e {
        DeviceError::NeedsElevation => "관리자 권한이 없습니다".into(),
        DeviceError::NotFound { disk_number } => {
            format!("디스크 {disk_number} 를 찾을 수 없습니다")
        }
        DeviceError::Locked { op } => {
            format!("{op}: 다른 프로그램이 붙잡고 있어 잠그지 못했습니다 (Win32 32)")
        }
        DeviceError::WriteDenied { op } => {
            format!("{op}: 장치가 접근을 거부했습니다 (Win32 5)")
        }
        DeviceError::MediaChanged { op } => {
            format!("{op}: 도중에 장치가 빠졌거나 바뀌었습니다 (Win32 1110)")
        }
        DeviceError::BadSectorSize(n) => {
            format!("장치가 알려준 섹터 크기가 비정상입니다 ({n} 바이트)")
        }
        DeviceError::IdentityChanged { message } => message.clone(),
        DeviceError::TargetErased { message, .. } | DeviceError::Io { message, .. } => {
            message.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 프런트엔드가 오류 문자열에서 찾는 이름들. 이 이름이 설명문에 섞여 들어가면
    /// 화면이 엉뚱하게 골라진다.
    const UI_KEYS: [&str; 5] = [
        "NeedsElevation",
        "Locked",
        "WriteDenied",
        "MediaChanged",
        "IdentityChanged",
    ];

    fn every_error() -> Vec<DeviceError> {
        vec![
            DeviceError::NeedsElevation,
            DeviceError::NotFound { disk_number: 3 },
            DeviceError::Locked {
                op: "볼륨 잠금"
            },
            DeviceError::WriteDenied {
                op: "장치 쓰기"
            },
            DeviceError::MediaChanged {
                op: "장치 읽기"
            },
            DeviceError::BadSectorSize(0),
            DeviceError::IdentityChanged {
                message: "용량이 다릅니다".into(),
            },
            DeviceError::Io {
                code: 21,
                message: "장치 용량 조회 실패".into(),
            },
        ]
    }

    #[test]
    fn descriptions_never_carry_the_names_the_ui_switches_on() {
        for e in every_error() {
            let d = describe(&e);
            for k in UI_KEYS {
                assert!(
                    !d.contains(k),
                    "설명문에 화면 선택용 이름이 섞였다: {d} (금지: {k})"
                );
            }
        }
    }

    #[test]
    fn the_operation_name_survives_into_the_description() {
        // 어느 작업에서 났는지 모르면 보고를 받아도 원인을 좁힐 수 없다.
        let d = describe(&DeviceError::WriteDenied {
            op: "캐시 플러시"
        });
        assert!(d.contains("캐시 플러시"), "실제: {d}");
        assert!(d.contains("5"), "Win32 코드가 사라졌다: {d}");
    }

    #[test]
    fn nothing_touched_leaves_the_error_exactly_as_it_was() {
        let p = Prep::new();
        let e = DeviceError::Locked {
            op: "볼륨 잠금"
        };
        assert_eq!(p.explain(e.clone()), e);
    }

    /// 파티션 테이블을 지운 뒤의 실패는 **지워졌다는 사실**과 준비 기록을
    /// 함께 실어야 한다. 둘 다 세션이 만들어져야만 전달되던 것들이다.
    #[test]
    fn an_erased_target_is_reported_as_erased_and_keeps_the_notes() {
        let mut p = Prep::new();
        p.reached(Touched::Mounts);
        p.note("파티션 테이블 삭제 실패 (Win32 5)");
        p.reached(Touched::Layout);
        p.note("파티션 테이블 재인식 실패");

        let out = p.explain(DeviceError::Locked {
            op: "물리 디스크 열기(쓰기)",
        });
        let DeviceError::TargetErased { code, message } = out else {
            panic!("지워진 뒤의 실패가 그대로 올라갔다: {out:?}");
        };
        assert_eq!(code, 32, "Win32 코드가 사라졌다");
        assert!(message.contains("지워진 상태"), "실제: {message}");
        assert!(
            message.contains("파티션 테이블 삭제 실패"),
            "실제: {message}"
        );
        assert!(
            message.contains("파티션 테이블 재인식 실패"),
            "실제: {message}"
        );
        assert!(
            message.contains("물리 디스크 열기(쓰기)"),
            "실제: {message}"
        );
    }

    /// 드라이브 문자만 뗀 상태를 "지워졌다" 고 말하면 그것도 거짓말이다.
    #[test]
    fn removing_only_the_drive_letter_is_not_reported_as_an_erased_disk() {
        let mut p = Prep::new();
        p.reached(Touched::Mounts);

        let out = p.explain(DeviceError::Io {
            code: 2,
            message: "물리 디스크 열기(쓰기) 실패: Win32 오류 2".into(),
        });
        assert!(
            !matches!(out, DeviceError::TargetErased { .. }),
            "내용이 멀쩡한데 지워졌다고 했다: {out:?}"
        );
        let DeviceError::Io { message, .. } = out else {
            panic!("실제: {out:?}");
        };
        assert!(message.contains("드라이브 문자"), "실제: {message}");
        assert!(message.contains("내용은 그대로"), "실제: {message}");
    }

    /// 한 번 지운 뒤에는 어떤 표시가 더 와도 되돌아가지 않는다.
    #[test]
    fn the_recorded_damage_never_walks_backwards() {
        let mut p = Prep::new();
        p.reached(Touched::Layout);
        p.reached(Touched::Mounts);
        assert_eq!(p.touched(), Touched::Layout);
    }

    /// 볼륨 목록을 훑지도 못한 상태를 "준비 완료" 로 말하면 안 된다.
    #[test]
    fn a_failed_volume_scan_is_not_reported_as_a_finished_preparation() {
        let p = Prep::new();
        let s = p.summary(0, false);
        assert!(
            !s.contains("준비 완료"),
            "확인한 적 없는 것을 다 됐다고 했다: {s}"
        );
        assert!(s.contains("확인하지 못함"), "실제: {s}");
    }

    #[test]
    fn a_clean_preparation_says_what_it_actually_knows() {
        let p = Prep::new();
        let s = p.summary(2, true);
        assert!(s.contains("볼륨 2개 잠금"), "실제: {s}");
        assert!(s.contains("실패 기록 없음"), "실제: {s}");
        assert!(!s.contains("준비 완료"), "실제: {s}");
    }

    /// 쓰기 도중의 실패는 위치와 준비 기록을 함께 올려야 한다.
    /// 예전에는 이것이 쓰기 거부 한 갈래에만 붙어 있었다.
    #[test]
    fn a_write_failure_carries_where_it_happened_and_what_preparation_saw() {
        let notes = "준비 단계: 볼륨 1개 잠금\n문제:\n  \\\\?\\Volume{x}: 잠금 실패";
        let out = with_write_context(
            DeviceError::Locked {
                op: "장치 쓰기"
            },
            "이미지 쓰기",
            Some(25_165_824),
            notes,
        );
        let DeviceError::TargetErased { code, message } = out else {
            panic!("실제: {out:?}");
        };
        assert_eq!(code, 32);
        assert!(message.contains("25165824"), "위치가 없다: {message}");
        assert!(
            message.contains("잠금 실패"),
            "준비 기록이 버려졌다: {message}"
        );
        assert!(message.contains("일부만 쓰인"), "실제: {message}");
    }

    /// 위치가 없는 작업(플러시)도 삼켜지지 않아야 한다.
    #[test]
    fn a_flush_failure_is_not_dressed_up_as_a_refused_write() {
        let out = with_write_context(
            DeviceError::Io {
                code: 5,
                message: "캐시 플러시 실패: Win32 오류 5".into(),
            },
            "마무리(캐시 플러시)",
            None,
            "준비 단계: 볼륨 1개 잠금, 실패 기록 없음",
        );
        let DeviceError::TargetErased { message, .. } = out else {
            panic!("실제: {out:?}");
        };
        assert!(message.contains("마무리(캐시 플러시)"), "실제: {message}");
        assert!(!message.contains("쓰기를 거부"), "실제: {message}");
    }

    /// 쓰기 거부는 프런트엔드가 문구로 알아본다. 그 문구를 바꾸면 전용 안내가
    /// 사라지고 일반 실패 화면으로 떨어진다.
    #[test]
    fn a_refused_write_keeps_the_wording_the_ui_recognises() {
        let out = with_write_context(
            DeviceError::WriteDenied {
                op: "장치 쓰기"
            },
            "이미지 쓰기",
            Some(8_388_608),
            "준비 단계: 볼륨 0개 잠금, 실패 기록 없음",
        );
        let DeviceError::Io { code, message } = out else {
            panic!("실제: {out:?}");
        };
        assert_eq!(code, 5);
        assert!(message.contains("쓰기를 거부"), "실제: {message}");
        assert!(message.contains("8388608"), "실제: {message}");
        assert!(message.contains("준비 단계"), "실제: {message}");
        // 전용 안내를 유지하면서도 대상 상태는 말해야 한다. "다시 시도" 라는
        // 말만으로는 USB 가 아직 멀쩡하다는 뜻으로 읽힌다.
        assert!(message.contains("일부만 쓰인"), "실제: {message}");
    }
}
