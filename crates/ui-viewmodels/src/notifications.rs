//! Bounded, UI-independent notification policy. Severity never implies persistence.
use std::collections::VecDeque;
use std::time::Duration;

pub const HISTORY_LIMIT: usize = 20;
const TEXT_LIMIT: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Success,
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub severity: Severity,
    pub text: String,
}

impl Notice {
    pub fn new(severity: Severity, text: impl Into<String>) -> Self {
        let text = text.into();
        let mut bounded: String = text.chars().take(TEXT_LIMIT).collect();
        if text.chars().count() > TEXT_LIMIT {
            bounded.push('…');
        }
        Self {
            severity,
            text: bounded,
        }
    }

    /// Keep the floating surface compact; full bounded text remains in history.
    pub fn preview(&self) -> String {
        let mut chars = self.text.chars();
        let mut text: String = chars.by_ref().take(160).collect();
        if chars.next().is_some() {
            text.push_str("…（详见记录）");
        }
        text
    }

    fn duration(&self) -> Duration {
        Duration::from_millis(match self.severity {
            Severity::Success | Severity::Info => 3200,
            Severity::Warning => 6500,
            Severity::Error => 9000,
        })
    }
}

#[derive(Default)]
pub struct Notifications {
    toast: Option<Notice>,
    banner: Option<(&'static str, Notice)>,
    history: VecDeque<Notice>,
    remaining: Duration,
}

impl Notifications {
    pub fn toast(&self) -> Option<&Notice> {
        self.toast.as_ref()
    }
    pub fn banner(&self) -> Option<&Notice> {
        self.banner.as_ref().map(|(_, notice)| notice)
    }
    pub fn history(&self) -> impl Iterator<Item = &Notice> {
        self.history.iter()
    }

    /// Duplicate events do not extend a toast indefinitely. Lower priority events
    /// remain available in history without replacing a warning/error being read.
    pub fn push(&mut self, notice: Notice) {
        if notice.text.trim().is_empty() {
            return;
        }
        if self.history.front() != Some(&notice) {
            self.history.push_front(notice.clone());
            self.history.truncate(HISTORY_LIMIT);
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|current| current == &notice || current.severity > notice.severity)
        {
            return;
        }
        self.remaining = notice.duration();
        self.toast = Some(notice);
    }

    pub fn advance(&mut self, elapsed: Duration, paused: bool) {
        if paused {
            return;
        }
        self.remaining = self.remaining.saturating_sub(elapsed);
        if self.remaining.is_zero() {
            self.dismiss();
        }
    }

    pub fn dismiss(&mut self) {
        self.toast = None;
        self.remaining = Duration::ZERO;
    }

    /// Explicit persistent state, resolved by its owner rather than a timer.
    pub fn set_problem(&mut self, key: &'static str, notice: Notice) {
        self.banner = Some((key, notice));
    }

    pub fn resolve(&mut self, key: &str) {
        if self
            .banner
            .as_ref()
            .is_some_and(|(active, _)| *active == key)
        {
            self.banner = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_expire_but_remain_retrievable() {
        let mut vm = Notifications::default();
        vm.push(Notice::new(Severity::Warning, "已复制，请手动粘贴"));
        vm.advance(Duration::from_secs(20), true);
        assert!(vm.toast.is_some());
        vm.advance(Duration::from_secs(7), false);
        assert!(vm.toast.is_none());
        assert_eq!(vm.history.len(), 1);
    }

    #[test]
    fn repeated_events_do_not_pin_toast_and_success_cannot_hide_error() {
        let mut vm = Notifications::default();
        let error = Notice::new(Severity::Error, "保存失败");
        vm.push(error.clone());
        vm.advance(Duration::from_secs(8), false);
        vm.push(error.clone());
        vm.push(Notice::new(Severity::Success, "复制成功"));
        assert_eq!(vm.toast, Some(error));
        vm.advance(Duration::from_secs(1), false);
        assert!(vm.toast.is_none());
        assert_eq!(vm.history.len(), 2);
    }

    #[test]
    fn persistent_problem_is_independent_and_only_owner_resolves_it() {
        let mut vm = Notifications::default();
        vm.set_problem("library", Notice::new(Severity::Error, "库不可用"));
        vm.push(Notice::new(Severity::Success, "已复制"));
        vm.advance(Duration::from_secs(100), false);
        vm.resolve("network");
        assert!(vm.banner.is_some());
        vm.resolve("library");
        assert!(vm.banner.is_none());
    }

    #[test]
    fn history_and_messages_are_bounded() {
        let mut vm = Notifications::default();
        for i in 0..100 {
            vm.push(Notice::new(Severity::Info, format!("提示 {i}")));
        }
        assert_eq!(vm.history.len(), HISTORY_LIMIT);
        let long = Notice::new(Severity::Error, "文".repeat(10000));
        assert_eq!(long.text.chars().count(), TEXT_LIMIT + 1);
        assert!(long.preview().chars().count() < 180);
    }
}
