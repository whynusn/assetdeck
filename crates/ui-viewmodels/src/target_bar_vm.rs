use pipeline::{AssetPayload, PasteConfig, PasteSession, TargetPasteOutcome, TargetPipelineDeps};
use targets::{
    matching_profile_windows, resolve_eligible_snapshot, AliasMap, Health, ProfileError,
    ProfileSet, TargetBinding, TargetId, TargetTracker, WindowSnapshot,
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
    /// 别名覆盖前的默认标签（`profile.label · 窗口标题`）。清除别名时据此恢复，
    /// 不必重新枚举窗口。别名只改 `binding.label`，base 恒为构造时的原貌。
    pub base_label: String,
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
                    || "选择聊天窗口".to_string(),
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
                // 空态文案即下一步动作（点 chip 弹出选择列表），不再是内部状态名
                // 「未识别目标」——它与右侧提示「未选择目标」曾三词一义互相打架。
                label: "点击选择聊天窗口".to_string(),
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

/// 热目标的一行摘要（切换日志用）：id@hwnd 标签 会话窗 最小化。
fn describe_hot(binding: &Option<TargetBinding>) -> String {
    match binding {
        None => "无".to_string(),
        Some(b) => format!(
            "{}@{:?} {:?} 会话窗={} 最小化={}",
            b.id.as_str(),
            b.hwnd.map(|h| h.0),
            b.label,
            b.session_window,
            b.minimized,
        ),
    }
}

/// 多 IM 目标路由的界面状态门面。窗口采集和输入注入由壳层注入，目标决策留在纯 Rust。
pub struct TargetRoutingVm {
    profiles: ProfileSet,
    tracker: TargetTracker,
    bar: TargetBarVm,
    /// 实例别名册（targets.json）。键 `exe:pid`，只影响展示标签，不影响匹配。
    aliases: AliasMap,
}

impl TargetRoutingVm {
    pub fn from_profiles(builtin: &str, user: Option<&str>) -> Result<Self, ProfileError> {
        Ok(Self {
            profiles: targets::load_profiles(builtin, user)?,
            tracker: TargetTracker::new(),
            bar: TargetBarVm::new(),
            aliases: AliasMap::new(),
        })
    }

    /// 装配层在启动时注入已加载的别名册；对已存在的候选立即重放。
    pub fn set_aliases(&mut self, aliases: AliasMap) {
        self.aliases = aliases;
        for choice in &mut self.bar.choices {
            if let Some(alias) = self.aliases.get(&choice.binding.instance_id) {
                choice.binding.label = alias.to_string();
            } else {
                choice.binding.label = choice.base_label.clone();
            }
        }
    }

    pub fn aliases(&self) -> &AliasMap {
        &self.aliases
    }

    /// 重命名（或清除，`None`/空白 = 恢复默认名）一个窗口实例。返回是否命中候选。
    ///
    /// 改名同步三处：候选列表、热/钉住绑定（chip 标签）、别名册本身——持久化由
    /// 装配层在调用后取 [`Self::aliases`] 完成。
    pub fn rename_target(&mut self, selection_key: &str, alias: Option<&str>) -> bool {
        let Some(instance_id) = self
            .bar
            .choices
            .iter()
            .find(|choice| choice.selection_key() == selection_key)
            .map(|choice| choice.binding.instance_id.clone())
        else {
            return false;
        };
        self.aliases.set(&instance_id, alias);
        let label = self.aliases.get(&instance_id).map(str::to_string);
        for choice in &mut self.bar.choices {
            if choice.binding.instance_id == instance_id {
                choice.binding.label = label.clone().unwrap_or_else(|| choice.base_label.clone());
            }
        }
        // chip / 钉住绑定只换标签，hwnd 与身份不动（rebind 的既有语义）。
        let mut relabelled: Option<TargetBinding> = None;
        if let Some(hot) = self.tracker.hot() {
            if hot.instance_id == instance_id {
                let mut updated = hot.clone();
                updated.label = label.clone().unwrap_or_else(|| {
                    self.bar
                        .choices
                        .iter()
                        .find(|choice| choice.binding.instance_id == instance_id)
                        .map(|choice| choice.base_label.clone())
                        .unwrap_or_else(|| updated.label.clone())
                });
                relabelled = Some(updated);
            }
        }
        if let Some(updated) = relabelled {
            let id = updated.id.clone();
            self.tracker.rebind(&id, updated);
        }
        true
    }

    /// 按实例身份把别名写进绑定标签（匹配后、进 tracker 前统一过这里）。
    fn apply_alias(&self, binding: &mut TargetBinding) {
        if let Some(alias) = self.aliases.get(&binding.instance_id) {
            binding.label = alias.to_string();
        }
    }

    pub fn on_foreground(&mut self, snapshot: &WindowSnapshot) {
        let eligible = resolve_eligible_snapshot(&self.profiles, snapshot).map(|mut resolved| {
            self.apply_alias(&mut resolved.binding);
            resolved.binding
        });
        let before = self.tracker.hot().cloned();
        self.tracker.on_foreground(eligible, false);
        // 热目标切换打点（2026-08-29）：此前前台漂移改写完全静默——千牛优惠弹窗
        // 借类名命中顶替热目标后，现场无从回溯「目标什么时候变成弹窗的」。
        // 会话窗标志一眼区分正常跟随与「类名命中但标题不合会话特征」的可疑顶替。
        let after = self.tracker.hot().cloned();
        if describe_hot(&before) != describe_hot(&after) {
            log::info!(
                "热目标切换 {} -> {}",
                describe_hot(&before),
                describe_hot(&after)
            );
        }
        self.sync_hot_target();
    }

    pub fn refresh_windows(&mut self, windows: &[WindowSnapshot]) {
        let mut choices = Vec::new();
        for profile in self.profiles.profiles() {
            let matches = matching_profile_windows(profile, windows);
            if matches.is_empty() {
                // D17 收窄（2026-08-29 用户裁定）：「未运行·选择后仅复制」态只
                // 属于用户捕捉过的目标——热/钉住目标休眠时置灰保留（D13）。从未
                // 上过框的内置画像不入列，否则没装 Telegram 的机器上 picker 永远
                // 躺着一个灰色 Telegram，内置名单成了硬编码广告位。
                let dormant_hot = self.tracker.hot().is_some_and(|hot| hot.id == profile.id);
                if !dormant_hot {
                    continue;
                }
                choices.push(TargetChoice {
                    binding: TargetBinding {
                        id: profile.id.clone(),
                        hwnd: None,
                        label: profile.label.clone(),
                        fallback: false,
                        minimized: false,
                        visible: false,
                        instance_id: String::new(),
                        session_window: false,
                    },
                    health: Health::Unknown,
                    base_label: profile.label.clone(),
                });
            } else {
                choices.extend(matches.into_iter().map(|resolved| {
                    let mut binding = resolved.binding;
                    let base_label = binding.label.clone();
                    if let Some(alias) = self.aliases.get(&binding.instance_id) {
                        binding.label = alias.to_string();
                    }
                    TargetChoice {
                        binding,
                        health: Health::Unknown,
                        base_label,
                    }
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

    /// D48 右键菜单「复制」：只写剪贴板，绝不激活目标/注入 Ctrl+V——
    /// 与上框链路共享格式协商（negotiate_detailed），语义止步「素材进剪贴板」。
    /// 「粘贴即发送」格式（千牛类）照写：本路径不注入，用户自行 Ctrl+V 时
    /// 的发送与否是目标语义，不是本应用的注入行为。
    pub fn copy_to_clipboard(
        &self,
        payload: &AssetPayload<'_>,
        deps: &mut TargetPipelineDeps<'_>,
    ) -> Result<(), String> {
        let profile = self
            .tracker
            .hot()
            .and_then(|binding| self.profiles.get(&binding.id))
            .unwrap_or_else(|| self.profiles.generic());
        let clipboard_payload = match pipeline::negotiate_detailed(payload, profile) {
            pipeline::Negotiated::Safe { payload, .. }
            | pipeline::Negotiated::WouldSend { payload, .. } => payload,
            pipeline::Negotiated::Unsupported => return Err("素材类型无可用剪贴板格式".to_string()),
        };
        deps.clipboard
            .write(&clipboard_payload)
            .map_err(|e| e.to_string())
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
        let target = self.tracker.hot().cloned().map(|binding| {
            let base_label = binding.label.clone();
            TargetChoice {
                binding,
                health: Health::Unknown,
                base_label,
            }
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
                session_window: false,
            },
            health: Health::Yellow,
            base_label: id.to_string(),
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

    const STRICT_QIANNIU_PROFILES: &str = r#"
[[profiles]]
id = "qianniu"
label = "千牛"
exe_names = ["AliWorkbench.exe"]
class_names = ["Qt5152QWindowIcon"]
title_regexes = ["接待(中心|台)$", "千牛工作台"]
require_title = true
"#;

    fn snapshot_with_class(hwnd: isize, exe: &str, class: &str, title: &str) -> WindowSnapshot {
        WindowSnapshot {
            class_name: class.to_string(),
            ..snapshot(hwnd, exe, title)
        }
    }

    /// 严格档回归（2026-08-29 用户裁定）：优惠弹窗类窗口（类名命中、标题不合
    /// 会话特征）抢前台不得顶替会话窗口热目标——严格档下它根本不命中画像。
    /// 「接待台」标题变体（部分用户如此）照常跟随。
    #[test]
    fn promo_popup_foreground_never_takes_over_session_hot_target() {
        let mut vm = TargetRoutingVm::from_profiles(STRICT_QIANNIU_PROFILES, None).unwrap();
        vm.on_foreground(&snapshot_with_class(
            1,
            "AliWorkbench.exe",
            "Qt5152QWindowIcon",
            "易软坊-接待中心",
        ));
        let hot = vm.selected().expect("会话窗口应成为热目标");
        assert_eq!(hot.id.as_str(), "qianniu");
        assert!(hot.session_window, "标题命中的绑定必须携带会话窗标志");

        // 弹窗抢前台：同 exe 同类名、标题无关。
        vm.on_foreground(&snapshot_with_class(
            2,
            "AliWorkbench.exe",
            "Qt5152QWindowIcon",
            "限时特惠",
        ));
        let hot = vm.selected().unwrap();
        assert_eq!(
            hot.hwnd,
            Some(targets::WindowHandle(1)),
            "弹窗不得顶替会话窗口热目标"
        );

        // 「接待台」变体照常成为热目标。
        vm.on_foreground(&snapshot_with_class(
            3,
            "AliWorkbench.exe",
            "Qt5152QWindowIcon",
            "易软坊-接待台",
        ));
        assert_eq!(vm.selected().unwrap().hwnd, Some(targets::WindowHandle(3)));
    }

    /// 别名全链路：rename 改候选+热绑定标签，清除恢复 base，set_aliases 对既有
    /// 候选立即重放。持久化（targets.json）由装配层取 `aliases()` 完成。
    #[test]
    fn rename_target_applies_alias_and_clear_restores_base() {
        let mut vm = TargetRoutingVm::from_profiles(STRICT_QIANNIU_PROFILES, None).unwrap();
        vm.refresh_windows(&[snapshot_with_class(
            1,
            "AliWorkbench.exe",
            "Qt5152QWindowIcon",
            "易软坊-接待中心",
        )]);
        let key = vm.snapshot().choices[0].selection_key();
        let base = vm.snapshot().choices[0].base_label.clone();

        assert!(vm.rename_target(&key, Some("主接待")));
        assert_eq!(vm.snapshot().choices[0].binding.label, "主接待");
        assert_eq!(
            vm.snapshot().choices[0].base_label,
            base,
            "base 不得被别名污染"
        );

        // 前台跟随产生的热目标（chip 标签来源）同样带别名。
        vm.on_foreground(&snapshot_with_class(
            1,
            "AliWorkbench.exe",
            "Qt5152QWindowIcon",
            "易软坊-接待中心",
        ));
        assert_eq!(vm.selected().unwrap().label, "主接待");

        // 空白 = 清除：候选与热绑定都恢复默认标签。
        assert!(vm.rename_target(&key, Some("   ")));
        assert_eq!(vm.snapshot().choices[0].binding.label, base);
        assert_eq!(vm.selected().unwrap().label, base);
        assert!(vm.aliases().is_empty());
        assert!(
            !vm.rename_target("不存在的键@0", None),
            "未命中候选必须返回 false"
        );
    }

    #[test]
    fn set_aliases_replays_on_existing_choices() {
        let mut vm = TargetRoutingVm::from_profiles(STRICT_QIANNIU_PROFILES, None).unwrap();
        vm.refresh_windows(&[snapshot_with_class(
            1,
            "AliWorkbench.exe",
            "Qt5152QWindowIcon",
            "易软坊-接待中心",
        )]);
        let instance_id = vm.snapshot().choices[0].binding.instance_id.clone();

        let mut aliases = targets::AliasMap::new();
        aliases.set(&instance_id, Some("主接待"));
        vm.set_aliases(aliases);

        assert_eq!(vm.snapshot().choices[0].binding.label, "主接待");
    }

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

    /// 从未捕捉过的内置画像不得出现在候选列表：没装 Telegram 的机器上，
    /// picker 不能永远躺着一个灰色 Telegram。
    #[test]
    fn never_used_profiles_do_not_appear_in_candidates() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.refresh_windows(&[snapshot(1, "WeChat.exe", "微信")]);
        let choices = vm.snapshot().choices;
        assert_eq!(choices.len(), 1, "无窗口的画像必须整条淘汰");
        assert_eq!(choices[0].binding.id.as_str(), "wechat");
        assert_eq!(choices[0].binding.hwnd, Some(targets::WindowHandle(1)));
    }

    /// D13「休眠置灰保留」只属于捕捉过的目标：热目标窗口消失后，候选里保留
    /// 一个灰色占位，chip 也继续显示它，等窗口回来重绑。
    #[test]
    fn dormant_hot_target_stays_in_candidates_as_greyed_placeholder() {
        let mut vm = TargetRoutingVm::from_profiles(ROUTING_PROFILES, None).unwrap();
        vm.on_foreground(&snapshot(1, "Telegram.exe", "Telegram"));
        vm.refresh_windows(&[]);
        let choices = vm.snapshot().choices;
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].binding.id.as_str(), "telegram");
        assert_eq!(choices[0].binding.hwnd, None, "休眠占位必须无窗口绑定");
        assert_eq!(vm.selected().unwrap().id.as_str(), "telegram");
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
