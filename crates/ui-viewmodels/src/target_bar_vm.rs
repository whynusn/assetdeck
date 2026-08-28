use pipeline::{AssetPayload, PasteConfig, PasteSession, TargetPasteOutcome, TargetPipelineDeps};
use targets::{
    matching_profile_windows, resolve_eligible_snapshot, Health, ProfileError, ProfileSet,
    TargetBinding, TargetId, TargetTracker, WindowSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetBarMode {
    Empty,
    Ready,
    NeedsConfirmation,
    ChooseTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetChoice {
    pub binding: TargetBinding,
    pub health: Health,
}

impl TargetChoice {
    /// UI 选择键必须包含窗口实例，不能只用 profile id；同一 IM 多开时 id 相同而 HWND 不同。
    pub fn selection_key(&self) -> String {
        let window = self
            .binding
            .hwnd
            .map_or_else(|| "dormant".to_string(), |hwnd| hwnd.0.to_string());
        format!("{}@{window}", self.binding.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetBarSnapshot {
    pub mode: TargetBarMode,
    pub label: String,
    pub health: Health,
    pub pinned: bool,
    pub choices: Vec<TargetChoice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetNoticeTone {
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPasteNotice {
    pub tone: TargetNoticeTone,
    pub text: String,
    /// 是否真的向目标注入了 Ctrl+V。`Injected` 两种 verified 值都是 true（
    /// verified 只影响文案）；仅复制 / 失败为 false。D45 连击合并据此判定
    /// 「刚注入过」，Warning tone 无法区分「已注入待确认」与「降级仅复制」。
    pub injected: bool,
}

/// 目标条的纯状态机。平台观察与窗口匹配由上游完成，本 VM 只处理呈现与用户选择。
#[derive(Debug, Default)]
pub struct TargetBarVm {
    current: Option<TargetChoice>,
    choices: Vec<TargetChoice>,
    picker: PickerState,
    pinned_id: Option<TargetId>,
    confirmed_fallbacks: Vec<TargetId>,
}

/// picker 的开启来源。用户手动点开的 picker 必须扛住后台刷新（前台事件驱动，
/// 事件源不可用时退路轮询）；只有用户自己的选择/关闭才能收起它。
/// 歧义自动展开的 picker 则允许在热目标重新明确后自行收起。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PickerState {
    #[default]
    Closed,
    /// 同画像多窗口并列时由 `set_ambiguous` 自动展开。
    Auto,
    /// 用户点击 chip 展开的冷目标选择器。
    User,
}

impl PickerState {
    fn is_open(self) -> bool {
        !matches!(self, PickerState::Closed)
    }
}

impl TargetBarVm {
    pub fn new() -> Self {
        Self::default()
    }

    /// 单一热目标自动进入 chip，无需用户点击选择。
    ///
    /// 该方法由后台刷新驱动，因此**不得**关闭用户手动展开的 picker，
    /// 否则冷目标选择器会在下一次刷新被静默收起（用户视角就是「点不开」）。
    pub fn set_hot_target(&mut self, target: Option<TargetChoice>) {
        if let Some(pinned_id) = self.pinned_id.as_ref() {
            if target
                .as_ref()
                .is_some_and(|choice| &choice.binding.id == pinned_id)
            {
                self.current = target;
            }
            return;
        }
        self.current = target;
        if self.picker == PickerState::Auto {
            self.picker = PickerState::Closed;
        }
    }

    /// 同一画像最高分并列时展开 picker，禁止静默取第一个。
    pub fn set_ambiguous(&mut self, choices: Vec<TargetChoice>) {
        if self.pinned_id.is_some() {
            return;
        }
        self.current = None;
        self.choices = choices;
        if self.picker == PickerState::Closed {
            self.picker = PickerState::Auto;
        }
    }

    /// 更新当前可选窗口。列表保留在 VM 内，使用户选择后仍可再次打开目标选择器。
    pub fn set_available_targets(&mut self, choices: Vec<TargetChoice>) {
        self.choices = choices;
        if self.choices.is_empty() {
            self.picker = PickerState::Closed;
        }
    }

    pub fn open_picker(&mut self) -> bool {
        if self.choices.is_empty() {
            return false;
        }
        self.picker = PickerState::User;
        true
    }

    pub fn close_picker(&mut self) {
        self.picker = PickerState::Closed;
    }

    /// chip 点击语义：展开/收起同一个选择器，避免用户第二次点击时无反馈。
    pub fn toggle_picker(&mut self) -> bool {
        if self.picker.is_open() {
            self.close_picker();
            return false;
        }
        self.open_picker()
    }

    pub fn choose(&mut self, selection_key: &str) -> bool {
        let Some(index) = self
            .choices
            .iter()
            .position(|choice| choice.selection_key() == selection_key)
        else {
            return false;
        };
        self.current = Some(self.choices[index].clone());
        if self.pinned_id.is_some() {
            self.pinned_id = Some(self.choices[index].binding.id.clone());
        }
        self.picker = PickerState::Closed;
        true
    }

    pub fn confirm_fallback(&mut self) -> bool {
        let Some(current) = self.current.as_ref() else {
            return false;
        };
        if !current.binding.fallback {
            return true;
        }
        if !self.confirmed_fallbacks.contains(&current.binding.id) {
            self.confirmed_fallbacks.push(current.binding.id.clone());
        }
        true
    }

    pub fn toggle_pin(&mut self) {
        if self.pinned_id.is_some() {
            self.pinned_id = None;
        } else if let Some(current) = self.current.as_ref() {
            self.pinned_id = Some(current.binding.id.clone());
        }
    }

    pub fn selected(&self) -> Option<&TargetBinding> {
        self.current.as_ref().map(|choice| &choice.binding)
    }

    pub fn snapshot(&self) -> TargetBarSnapshot {
        if self.picker.is_open() {
            return TargetBarSnapshot {
                mode: TargetBarMode::ChooseTarget,
                label: self.current.as_ref().map_or_else(
                    || "选择上框目标".to_string(),
                    |choice| choice.binding.label.clone(),
                ),
                health: self
                    .current
                    .as_ref()
                    .map_or(Health::Unknown, |choice| choice.health),
                pinned: self.pinned_id.is_some(),
                choices: self.choices.clone(),
            };
        }
        let Some(current) = self.current.as_ref() else {
            return TargetBarSnapshot {
                mode: TargetBarMode::Empty,
                label: "未识别目标".to_string(),
                health: Health::Unknown,
                pinned: false,
                choices: self.choices.clone(),
            };
        };
        let fallback_confirmed = self.confirmed_fallbacks.contains(&current.binding.id);
        TargetBarSnapshot {
            mode: if current.binding.fallback && !fallback_confirmed {
                TargetBarMode::NeedsConfirmation
            } else {
                TargetBarMode::Ready
            },
            label: current.binding.label.clone(),
            health: current.health,
            pinned: self.pinned_id.as_ref() == Some(&current.binding.id),
            choices: self.choices.clone(),
        }
    }
}

/// 多 IM 目标路由的界面状态门面。窗口采集和输入注入由壳层注入，目标决策留在纯 Rust。
pub struct TargetRoutingVm {
    profiles: ProfileSet,
    tracker: TargetTracker,
    bar: TargetBarVm,
}

impl TargetRoutingVm {
    pub fn from_profiles(builtin: &str, user: Option<&str>) -> Result<Self, ProfileError> {
        Ok(Self {
            profiles: targets::load_profiles(builtin, user)?,
            tracker: TargetTracker::new(),
            bar: TargetBarVm::new(),
        })
    }

    pub fn on_foreground(&mut self, snapshot: &WindowSnapshot) {
        let eligible = resolve_eligible_snapshot(&self.profiles, snapshot);
        self.tracker
            .on_foreground(eligible.map(|resolved| resolved.binding), false);
        self.sync_hot_target();
    }

    pub fn refresh_windows(&mut self, windows: &[WindowSnapshot]) {
        let mut choices = Vec::new();
        for profile in self.profiles.profiles() {
            let matches = matching_profile_windows(profile, windows);
            if matches.is_empty() {
                choices.push(TargetChoice {
                    binding: TargetBinding {
                        id: profile.id.clone(),
                        hwnd: None,
                        label: profile.label.clone(),
                        fallback: false,
                        minimized: false,
                        visible: false,
                        instance_id: String::new(),
                    },
                    health: Health::Unknown,
                });
            } else {
                choices.extend(matches.into_iter().map(|resolved| TargetChoice {
                    binding: resolved.binding,
                    health: Health::Unknown,
                }));
            }
        }

        if let Some(hot) = self.tracker.hot().cloned() {
            let bound_still_exists = hot.hwnd.is_some_and(|hwnd| {
                choices
                    .iter()
                    .any(|choice| choice.binding.hwnd == Some(hwnd))
            });
            if !bound_still_exists {
                if let Some(hwnd) = hot.hwnd {
                    // 真实低配机日志（2026-08-27）确认解绑→重绑的节奏直接决定上框
                    // 成败：这里必须留痕，否则现场只能看到「目标窗口已关闭」的
                    // 降级 headline，无法回溯是哪次枚举把还活着的窗口判没了。
                    log::info!(
                        "热目标解绑 target={} hwnd={}（枚举中未复现，进入休眠态等待重绑）",
                        hot.id.as_str(),
                        hwnd.0
                    );
                    self.tracker.on_window_gone(hwnd);
                }
                let replacements: Vec<TargetChoice> = choices
                    .iter()
                    .filter(|choice| {
                        choice.binding.id == hot.id
                            && !hot.instance_id.is_empty()
                            && choice.binding.instance_id == hot.instance_id
                            && choice.binding.hwnd.is_some()
                    })
                    .cloned()
                    .collect();
                match replacements.as_slice() {
                    [replacement] => {
                        log::info!(
                            "热目标重绑 target={} hwnd={}（同实例唯一窗口）",
                            hot.id.as_str(),
                            replacement.binding.hwnd.map_or(0, |h| h.0)
                        );
                        self.tracker.rebind(&hot.id, replacement.binding.clone());
                    }
                    [_, _, ..] => self.bar.set_ambiguous(replacements),
                    [] => {}
                }
            }
        }

        self.bar.set_available_targets(choices);
        if self.bar.snapshot().mode != TargetBarMode::ChooseTarget {
            self.sync_hot_target();
        }
    }

    pub fn open_picker(&mut self) -> bool {
        self.bar.open_picker()
    }

    /// chip 点击入口：展开或收起冷目标选择器。返回展开后的开启状态。
    pub fn toggle_picker(&mut self) -> bool {
        self.bar.toggle_picker()
    }

    pub fn choose(&mut self, selection_key: &str) -> bool {
        if !self.bar.choose(selection_key) {
            return false;
        }
        let Some(binding) = self.bar.selected().cloned() else {
            return false;
        };
        if self.tracker.pinned_id().is_some() {
            self.tracker.pin(binding);
        } else {
            self.tracker.select_explicit(binding);
        }
        true
    }

    pub fn toggle_pin(&mut self) {
        if self.tracker.pinned_id().is_some() {
            self.tracker.unpin();
            self.bar.toggle_pin();
        } else if let Some(target) = self.bar.selected().cloned() {
            self.tracker.pin(target);
            self.bar.toggle_pin();
        }
    }

    pub fn confirm_fallback(&mut self) -> bool {
        self.bar.confirm_fallback()
    }

    pub fn snapshot(&self) -> TargetBarSnapshot {
        self.bar.snapshot()
    }

    pub fn selected(&self) -> Option<&TargetBinding> {
        self.tracker.hot()
    }

    pub fn paste(
        &self,
        payload: &AssetPayload<'_>,
        deps: &mut TargetPipelineDeps<'_>,
    ) -> TargetPasteNotice {
        let target = self.tracker.hot();
        let profile = target
            .and_then(|binding| self.profiles.get(&binding.id))
            .unwrap_or_else(|| self.profiles.generic());
        let mut session = PasteSession::new(PasteConfig::default());
        session.begin_targeted(&self.tracker);
        match session.paste_targeted(payload, profile, deps) {
            TargetPasteOutcome::Injected { verified: true } => TargetPasteNotice {
                tone: TargetNoticeTone::Success,
                text: format!(
                    "已上框到 {}",
                    target.map_or(profile.label.as_str(), |binding| binding.label.as_str())
                ),
                injected: true,
            },
            TargetPasteOutcome::Injected { verified: false } => TargetPasteNotice {
                tone: TargetNoticeTone::Warning,
                text: format!(
                    "已粘贴到 {}，请确认输入框内容",
                    target.map_or(profile.label.as_str(), |binding| binding.label.as_str())
                ),
                injected: true,
            },
            TargetPasteOutcome::CopiedOnly { feedback }
            | TargetPasteOutcome::Failed { feedback } => TargetPasteNotice {
                tone: if feedback.severity == pipeline::FeedbackSeverity::Error {
                    TargetNoticeTone::Error
                } else {
                    TargetNoticeTone::Warning
                },
                text: format!("{} · {}", feedback.headline, feedback.hint),
                injected: false,
            },
        }
    }

    fn sync_hot_target(&mut self) {
        let target = self.tracker.hot().cloned().map(|binding| TargetChoice {
            binding,
            health: Health::Unknown,
        });
        self.bar.set_hot_target(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str, fallback: bool) -> TargetChoice {
        TargetChoice {
            binding: TargetBinding {
                id: TargetId::new(id),
                hwnd: None,
                label: id.to_string(),
                fallback,
                minimized: false,
                visible: true,
                instance_id: format!("pid-{id}"),
            },
            health: Health::Yellow,
        }
    }

    #[test]
    fn chip_shows_hot_target_without_user_click() {
        let mut vm = TargetBarVm::new();
        vm.set_hot_target(Some(choice("wechat", false)));
        let snapshot = vm.snapshot();
        assert_eq!(snapshot.mode, TargetBarMode::Ready);
        assert_eq!(snapshot.label, "wechat");
    }

    #[test]
    fn ambiguous_expands_picker() {
        let mut vm = TargetBarVm::new();
        vm.set_ambiguous(vec![choice("wechat-a", false), choice("wechat-b", false)]);
        let snapshot = vm.snapshot();
        assert_eq!(snapshot.mode, TargetBarMode::ChooseTarget);
        assert_eq!(snapshot.choices.len(), 2);
    }

    #[test]
    fn same_profile_windows_are_selected_by_unique_window_key() {
        let mut first = choice("wechat", false);
        first.binding.hwnd = Some(targets::WindowHandle(11));
        let mut second = choice("wechat", false);
        second.binding.hwnd = Some(targets::WindowHandle(22));
        let second_key = second.selection_key();

        let mut vm = TargetBarVm::new();
        vm.set_ambiguous(vec![first, second]);
        assert!(vm.choose(&second_key));
        assert_eq!(
            vm.selected().and_then(|binding| binding.hwnd),
            Some(targets::WindowHandle(22))
        );
    }

    #[test]
    fn picker_can_reopen_after_a_choice() {
        let first = choice("wechat", false);
        let second = choice("qq", false);
        let first_key = first.selection_key();
        let mut vm = TargetBarVm::new();
        vm.set_available_targets(vec![first, second]);
        assert!(vm.open_picker());
        assert!(vm.choose(&first_key));
        assert_eq!(vm.snapshot().mode, TargetBarMode::Ready);
        assert!(vm.open_picker());
        assert_eq!(vm.snapshot().mode, TargetBarMode::ChooseTarget);
    }

    #[test]
    fn fallback_target_requires_first_use_confirm() {
        let mut vm = TargetBarVm::new();
        vm.set_hot_target(Some(choice("custom-im", true)));
        assert_eq!(vm.snapshot().mode, TargetBarMode::NeedsConfirmation);
        assert!(vm.confirm_fallback());
        assert_eq!(vm.snapshot().mode, TargetBarMode::Ready);
    }

    #[test]
    fn pin_toggle_freezes_chip() {
        let mut vm = TargetBarVm::new();
        vm.set_hot_target(Some(choice("wechat", false)));
        vm.toggle_pin();
        vm.set_hot_target(Some(choice("qq", false)));
        assert_eq!(vm.snapshot().label, "wechat");
        assert!(vm.snapshot().pinned);

        vm.toggle_pin();
        vm.set_hot_target(Some(choice("qq", false)));
        assert_eq!(vm.snapshot().label, "qq");
        assert!(!vm.snapshot().pinned);
    }

    fn snapshot(hwnd: isize, exe: &str, title: &str) -> WindowSnapshot {
        snapshot_with_pid(hwnd, exe, title, 1)
    }

    fn snapshot_with_pid(hwnd: isize, exe: &str, title: &str, process_id: u32) -> WindowSnapshot {
        WindowSnapshot {
            hwnd: targets::WindowHandle(hwnd),
            exe_name: exe.to_string(),
            class_name: String::new(),
            title: title.to_string(),
            visible: true,
            minimized: false,
            rect: targets::WindowRect {
                left: 0,
                top: 0,
                right: 800,
                bottom: 600,
            },
            process_id,
        }
    }

    const ROUTING_PROFILES: &str = r#"
[[profiles]]
id = "wechat"
label = "微信"
exe_names = ["WeChat.exe"]

[[profiles]]
id = "telegram"
label = "Telegram"
exe_names = ["Telegram.exe"]
"#;

    #[test]
    fn unrelated_foreground_does_not_replace_routing_vm_hot_target() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.on_foreground(&snapshot(1, "WeChat.exe", "微信"));
        vm.on_foreground(&snapshot(2, "chrome.exe", "浏览器"));
        assert_eq!(vm.selected().unwrap().id.as_str(), "wechat");
    }

    #[test]
    fn refresh_rebinds_same_target_after_tray_reopen() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.on_foreground(&snapshot(1, "WeChat.exe", "微信"));
        vm.refresh_windows(&[snapshot(9, "WeChat.exe", "微信")]);
        assert_eq!(vm.selected().unwrap().hwnd, Some(targets::WindowHandle(9)));
    }

    #[test]
    fn refresh_does_not_rebind_other_wechat_process() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.on_foreground(&snapshot_with_pid(1, "WeChat.exe", "微信", 100));
        vm.refresh_windows(&[snapshot_with_pid(9, "WeChat.exe", "微信", 200)]);
        assert_eq!(vm.selected().unwrap().hwnd, None);
    }

    #[test]
    fn choosing_while_pinned_replaces_the_exact_pinned_window() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.refresh_windows(&[
            snapshot(1, "WeChat.exe", "微信 A"),
            snapshot(2, "WeChat.exe", "微信 B"),
        ]);
        let first_key = vm
            .snapshot()
            .choices
            .iter()
            .find(|choice| choice.binding.hwnd == Some(targets::WindowHandle(1)))
            .unwrap()
            .selection_key();
        let second_key = vm
            .snapshot()
            .choices
            .iter()
            .find(|choice| choice.binding.hwnd == Some(targets::WindowHandle(2)))
            .unwrap()
            .selection_key();

        assert!(vm.choose(&first_key));
        vm.toggle_pin();
        assert!(vm.choose(&second_key));

        assert_eq!(vm.selected().unwrap().hwnd, Some(targets::WindowHandle(2)));
        assert!(vm.snapshot().pinned);
    }

    #[test]
    fn pinned_chip_tracks_tray_rebind_without_accepting_other_targets() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.on_foreground(&snapshot(1, "WeChat.exe", "微信"));
        vm.toggle_pin();

        vm.on_foreground(&snapshot(2, "Telegram.exe", "Telegram"));
        assert_eq!(vm.selected().unwrap().hwnd, Some(targets::WindowHandle(1)));

        vm.refresh_windows(&[snapshot(9, "WeChat.exe", "微信")]);
        assert_eq!(vm.selected().unwrap().hwnd, Some(targets::WindowHandle(9)));
        assert_eq!(
            vm.bar.selected().and_then(|binding| binding.hwnd),
            Some(targets::WindowHandle(9))
        );
    }

    #[test]
    fn user_opened_picker_survives_background_polling() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        let windows = [
            snapshot_with_pid(1, "WeChat.exe", "微信 A", 100),
            snapshot_with_pid(2, "WeChat.exe", "微信 B", 200),
        ];
        vm.refresh_windows(&windows);
        assert!(vm.toggle_picker());
        assert_eq!(vm.snapshot().mode, TargetBarMode::ChooseTarget);

        // 后台 750ms 轮询：前台观察 + 窗口枚举都不得收起用户手动展开的选择器。
        for _ in 0..8 {
            vm.on_foreground(&windows[0]);
            vm.refresh_windows(&windows);
            assert_eq!(vm.snapshot().mode, TargetBarMode::ChooseTarget);
        }

        let key = vm
            .snapshot()
            .choices
            .iter()
            .find(|choice| choice.binding.hwnd == Some(targets::WindowHandle(2)))
            .unwrap()
            .selection_key();
        assert!(vm.choose(&key));
        assert_eq!(vm.snapshot().mode, TargetBarMode::Ready);
        assert_eq!(vm.selected().unwrap().hwnd, Some(targets::WindowHandle(2)));
    }

    #[test]
    fn chip_click_toggles_picker_closed_on_second_click() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.refresh_windows(&[snapshot(1, "WeChat.exe", "微信")]);
        assert!(vm.toggle_picker());
        assert!(!vm.toggle_picker());
        assert_ne!(vm.snapshot().mode, TargetBarMode::ChooseTarget);
    }

    #[test]
    fn ambiguous_picker_closes_once_hot_target_is_unambiguous_again() {
        let mut bar = TargetBarVm::new();
        bar.set_ambiguous(vec![choice("wechat-a", false), choice("wechat-b", false)]);
        assert_eq!(bar.snapshot().mode, TargetBarMode::ChooseTarget);
        bar.set_hot_target(Some(choice("wechat-a", false)));
        assert_eq!(bar.snapshot().mode, TargetBarMode::Ready);
    }
}
