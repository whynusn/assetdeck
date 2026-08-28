use platform::KEY_UP;

use crate::Health;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthLevel {
    L0Static,
    L1Window,
    L2Activation,
    L3SelfTest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfTestReport {
    pub sentinel: String,
    pub read_back: Option<String>,
    pub injected_keys: Vec<u16>,
    pub cleaned_up: bool,
}

impl SelfTestReport {
    pub fn passed(&self) -> bool {
        self.read_back.as_deref() == Some(self.sentinel.as_str())
            && self.cleaned_up
            && !contains_enter(&self.injected_keys)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthCheckInput {
    pub custom_target: bool,
    pub l0_valid: bool,
    pub l1_window_found: bool,
    pub l2_activated: bool,
    pub readiness_probeable: bool,
    pub l3: Option<SelfTestReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    pub health: Health,
    pub highest_level: Option<HealthLevel>,
    pub enabled: bool,
}

pub fn evaluate_health(input: &HealthCheckInput) -> HealthReport {
    if !input.l0_valid {
        return HealthReport {
            health: Health::Red,
            highest_level: None,
            enabled: false,
        };
    }
    if !input.l1_window_found {
        return HealthReport {
            health: Health::Unknown,
            highest_level: Some(HealthLevel::L0Static),
            enabled: false,
        };
    }
    if !input.l2_activated {
        return HealthReport {
            health: Health::Red,
            highest_level: Some(HealthLevel::L1Window),
            enabled: false,
        };
    }

    let enabled =
        !input.custom_target || input.l0_valid && input.l1_window_found && input.l2_activated;
    let l3_passed = input.l3.as_ref().is_some_and(SelfTestReport::passed);
    HealthReport {
        health: if l3_passed {
            Health::Green
        } else {
            Health::Yellow
        },
        highest_level: Some(if l3_passed {
            HealthLevel::L3SelfTest
        } else {
            HealthLevel::L2Activation
        }),
        enabled,
    }
}

fn contains_enter(keys: &[u16]) -> bool {
    const VK_RETURN: u16 = 0x0D;
    keys.iter().any(|key| key & !KEY_UP == VK_RETURN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_l3() -> SelfTestReport {
        SelfTestReport {
            sentinel: "__asset_manager_probe__".to_string(),
            read_back: Some("__asset_manager_probe__".to_string()),
            injected_keys: vec![0x11, 0x56, 0x8056, 0x8011],
            cleaned_up: true,
        }
    }

    #[test]
    fn l3_selftest_sequence_contains_no_enter() {
        assert!(passing_l3().passed());
        let mut report = passing_l3();
        report.injected_keys.extend([0x0D, 0x800D]);
        assert!(!report.passed());
    }

    #[test]
    fn l3_selftest_reads_back_sentinel_and_cleans_up() {
        assert!(passing_l3().passed());
        let mut not_cleaned = passing_l3();
        not_cleaned.cleaned_up = false;
        assert!(!not_cleaned.passed());
        let mut wrong_readback = passing_l3();
        wrong_readback.read_back = Some("other".to_string());
        assert!(!wrong_readback.passed());
    }

    #[test]
    fn custom_target_requires_l0_l2_before_enabling() {
        let report = evaluate_health(&HealthCheckInput {
            custom_target: true,
            l0_valid: true,
            l1_window_found: true,
            l2_activated: false,
            readiness_probeable: true,
            l3: None,
        });
        assert!(!report.enabled);
        assert_eq!(report.health, Health::Red);
    }

    #[test]
    fn window_not_running_is_unknown_not_red() {
        let report = evaluate_health(&HealthCheckInput {
            custom_target: true,
            l0_valid: true,
            l1_window_found: false,
            l2_activated: false,
            readiness_probeable: false,
            l3: None,
        });
        assert!(!report.enabled);
        assert_eq!(report.health, Health::Unknown);
        assert_eq!(report.highest_level, Some(HealthLevel::L0Static));
    }

    #[test]
    fn health_grade_downgrades_to_yellow_when_readiness_unprobeable() {
        let report = evaluate_health(&HealthCheckInput {
            custom_target: true,
            l0_valid: true,
            l1_window_found: true,
            l2_activated: true,
            readiness_probeable: false,
            l3: None,
        });
        assert!(report.enabled);
        assert_eq!(report.health, Health::Yellow);
        assert_eq!(report.highest_level, Some(HealthLevel::L2Activation));
    }

    #[test]
    fn only_l3_selftest_can_grade_green() {
        let report = evaluate_health(&HealthCheckInput {
            custom_target: false,
            l0_valid: true,
            l1_window_found: true,
            l2_activated: true,
            readiness_probeable: true,
            l3: Some(passing_l3()),
        });
        assert_eq!(report.health, Health::Green);
        assert_eq!(report.highest_level, Some(HealthLevel::L3SelfTest));
    }
}
