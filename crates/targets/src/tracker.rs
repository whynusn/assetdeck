use platform::WindowHandle;

use crate::{TargetBinding, TargetId};

/// 目标粘性状态机。只有 eligible 目标成为前台才能改写热目标。
#[derive(Debug, Default)]
pub struct TargetTracker {
    hot: Option<TargetBinding>,
    pinned: Option<TargetBinding>,
}

impl TargetTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 投递一次前台变化。`None` 代表浏览器、资源管理器等非目标窗口。
    /// 本应用面板由 `own_panel=true` 明确排除，避免自身污染热目标。
    pub fn on_foreground(&mut self, eligible: Option<TargetBinding>, own_panel: bool) {
        if own_panel {
            return;
        }
        let Some(target) = eligible else {
            return;
        };
        if self.pinned.is_some() {
            return;
        }
        self.hot = Some(target);
    }

    pub fn hot(&self) -> Option<&TargetBinding> {
        self.pinned.as_ref().or(self.hot.as_ref())
    }

    pub fn pin(&mut self, target: TargetBinding) {
        self.hot = Some(target.clone());
        self.pinned = Some(target);
    }

    /// 用户在冷目标选择器中明确选择一个窗口。未固定时，下一次 eligible 前台事件仍可
    /// 按热目标规则改写它。
    pub fn select_explicit(&mut self, target: TargetBinding) {
        if self.pinned.is_none() {
            self.hot = Some(target);
        }
    }

    /// 同一稳定身份的窗口实例变化只更新绑定，不改变热目标身份。
    pub fn rebind(&mut self, id: &TargetId, target: TargetBinding) -> bool {
        let mut rebound = false;
        if self.hot.as_ref().is_some_and(|hot| &hot.id == id) {
            self.hot = Some(target.clone());
            rebound = true;
        }
        if self.pinned.as_ref().is_some_and(|pinned| &pinned.id == id) {
            self.pinned = Some(target);
            rebound = true;
        }
        rebound
    }

    pub fn unpin(&mut self) {
        self.pinned = None;
    }

    pub fn pinned_id(&self) -> Option<&TargetId> {
        self.pinned.as_ref().map(|target| &target.id)
    }

    /// 窗口销毁只解绑 hwnd，稳定目标身份继续保留。
    pub fn on_window_gone(&mut self, hwnd: WindowHandle) {
        clear_handle(self.hot.as_mut(), hwnd);
        clear_handle(self.pinned.as_mut(), hwnd);
    }
}

fn clear_handle(target: Option<&mut TargetBinding>, hwnd: WindowHandle) {
    if let Some(target) = target {
        if target.hwnd == Some(hwnd) {
            target.hwnd = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn binding(id: &str, hwnd: isize) -> TargetBinding {
        let mut binding = TargetBinding::new(id.into(), WindowHandle(hwnd), id);
        binding.instance_id = format!("pid-{}", hwnd);
        binding
    }

    #[test]
    fn eligible_target_foreground_rewrites_hot_target() {
        let mut tracker = TargetTracker::new();
        tracker.on_foreground(Some(binding("wechat", 1)), false);
        tracker.on_foreground(Some(binding("qq", 2)), false);
        assert_eq!(tracker.hot().unwrap().id.as_str(), "qq");
    }

    #[test]
    fn unrelated_foreground_does_not_change_hot_target() {
        let mut tracker = TargetTracker::new();
        tracker.on_foreground(Some(binding("wechat", 1)), false);
        tracker.on_foreground(None, false);
        assert_eq!(tracker.hot().unwrap().id.as_str(), "wechat");
    }

    #[test]
    fn own_panel_foreground_is_ignored_by_tracker() {
        let mut tracker = TargetTracker::new();
        tracker.on_foreground(Some(binding("wechat", 1)), false);
        tracker.on_foreground(Some(binding("asset_manager", 9)), true);
        assert_eq!(tracker.hot().unwrap().id.as_str(), "wechat");
    }

    #[test]
    fn hot_target_has_no_ttl() {
        let mut tracker = TargetTracker::new();
        tracker.on_foreground(Some(binding("wechat", 1)), false);
        for _ in 0..10_000 {
            tracker.on_foreground(None, false);
        }
        assert_eq!(tracker.hot().unwrap().id.as_str(), "wechat");
    }

    #[test]
    fn pinned_target_not_overwritten() {
        let mut tracker = TargetTracker::new();
        tracker.pin(binding("wechat", 1));
        tracker.on_foreground(Some(binding("qq", 2)), false);
        assert_eq!(tracker.hot().unwrap().id.as_str(), "wechat");
        tracker.unpin();
        tracker.on_foreground(Some(binding("qq", 2)), false);
        assert_eq!(tracker.hot().unwrap().id.as_str(), "qq");
    }

    #[test]
    fn pinned_window_is_not_replaced_by_another_window_of_same_profile() {
        let mut tracker = TargetTracker::new();
        tracker.pin(binding("wechat", 1));

        tracker.on_foreground(Some(binding("wechat", 2)), false);
        assert_eq!(tracker.hot().unwrap().hwnd, Some(WindowHandle(1)));

        tracker.on_window_gone(WindowHandle(1));
        tracker.on_foreground(Some(binding("wechat", 2)), false);
        assert_eq!(tracker.hot().unwrap().hwnd, None);

        assert!(tracker.rebind(&"wechat".into(), binding("wechat", 2)));
        assert_eq!(tracker.hot().unwrap().hwnd, Some(WindowHandle(2)));
    }

    #[test]
    fn explicit_selection_becomes_hot_until_next_eligible_foreground() {
        let mut tracker = TargetTracker::new();
        tracker.on_foreground(Some(binding("wechat", 1)), false);
        tracker.select_explicit(binding("telegram", 2));
        assert_eq!(tracker.hot().unwrap().id.as_str(), "telegram");

        tracker.on_foreground(None, false);
        assert_eq!(tracker.hot().unwrap().id.as_str(), "telegram");

        tracker.on_foreground(Some(binding("qq", 3)), false);
        assert_eq!(tracker.hot().unwrap().id.as_str(), "qq");
    }

    #[test]
    fn rebind_updates_hwnd_without_changing_target_identity() {
        let mut tracker = TargetTracker::new();
        tracker.pin(binding("wechat", 1));
        tracker.on_window_gone(WindowHandle(1));

        assert!(tracker.rebind(&"wechat".into(), binding("wechat", 9)));
        assert_eq!(tracker.hot().unwrap().id.as_str(), "wechat");
        assert_eq!(tracker.hot().unwrap().hwnd, Some(WindowHandle(9)));
        assert_eq!(tracker.pinned_id().unwrap().as_str(), "wechat");
    }

    #[test]
    fn hot_target_survives_close_to_tray_and_reopen() {
        let mut tracker = TargetTracker::new();
        tracker.on_foreground(Some(binding("wechat", 1)), false);
        tracker.on_window_gone(WindowHandle(1));
        assert_eq!(tracker.hot().unwrap().hwnd, None);

        tracker.on_foreground(Some(binding("wechat", 2)), false);
        assert_eq!(tracker.hot().unwrap().hwnd, Some(WindowHandle(2)));
    }

    proptest! {
        #[test]
        fn arbitrary_non_eligible_events_preserve_hot_target(events in prop::collection::vec(any::<bool>(), 0..2048)) {
            let mut tracker = TargetTracker::new();
            tracker.on_foreground(Some(binding("wechat", 1)), false);
            for own_panel in events {
                tracker.on_foreground(None, own_panel);
            }
            prop_assert_eq!(tracker.hot().unwrap().id.as_str(), "wechat");
        }
    }
}
