use pipeline::{AssetPayload, TargetPipelineDeps};
use platform::{
    ClipboardSink, FileDialogs, FocusWatcher, ForegroundObserver, InputFocuser, KeyInjector,
    ReadinessProbe, WindowActivator, WindowEnumerator,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::{
    TargetBarSnapshot, TargetNoticeTone, TargetPasteNotice, TargetProfileError, TargetRoutingVm,
};

/// 连击合并窗口：OS 双击时值（默认 500ms）+ 一轮上框耗时（低配机 ≤250ms）。
/// 尾随点击的处理时刻 = max(点击时刻, 首次上框完成)，从首次注入完成起算
/// 最长 ≈ 双击时值，750ms 覆盖全部尾随半程；刻意的「再粘一份」节奏天然在 1s 以上。
const TRAILING_CLICK_COALESCE_MS: u64 = 750;

/// 平台能力集合。具体实现（真实桌面实现或测试替身）由二进制入口装配后注入，
/// 本 crate 只依赖 `platform` 的 trait 层，保证 VM 可纯 Rust 测试。
pub struct TargetRuntimeDeps {
    pub observer: Option<Box<dyn ForegroundObserver>>,
    pub enumerator: Box<dyn WindowEnumerator>,
    pub clipboard: Box<dyn ClipboardSink>,
    pub focus: Box<dyn FocusWatcher>,
    pub injector: Box<dyn KeyInjector>,
    pub activator: Box<dyn WindowActivator>,
    pub readiness: Box<dyn ReadinessProbe>,
    pub focuser: Box<dyn InputFocuser>,
    /// 原生文件对话框（导入选文件夹 / 导出选保存路径；平台具体实现在二进制入口装配）。
    pub dialogs: Box<dyn FileDialogs>,
}

/// 目标路由运行时门面。Slint 壳只消费状态快照和命令，不感知 HWND/WinEvent/SendInput。
pub struct TargetRoutingRuntime {
    routing: TargetRoutingVm,
    deps: TargetRuntimeDeps,
    /// 是否已进入事件驱动模式（`install_wakeup` 成功）。事件驱动下 `poll` 只在确有前台
    /// 事件到达时才全枚举；未进入（无观察器 / 观察器无法唤醒 / 退路 Timer）时每轮都枚举。
    event_driven: bool,
    /// 最近一次「实际注入成功」的上框（D45 连击合并）：素材路径 + 注入完成时刻。
    /// 降级 / 失败时清空——尾随点击必须放行为重试，而不是被误合并。
    last_injected: Option<(PathBuf, Instant)>,
}

impl TargetRoutingRuntime {
    pub fn new(
        builtin: &str,
        user: Option<&str>,
        deps: TargetRuntimeDeps,
    ) -> Result<Self, TargetProfileError> {
        let mut runtime = Self {
            routing: TargetRoutingVm::from_profiles(builtin, user)?,
            deps,
            event_driven: false,
            last_injected: None,
        };
        let _ = runtime.poll();
        Ok(runtime)
    }

    /// 消费 WinEvent，并按需做全窗口枚举。
    ///
    /// 全枚举（`refresh_windows` + `WindowEnumerator::windows`）过去每 750ms 无条件跑一次，
    /// 是 UI 线程上的常态开销。改造后只在「确有前台事件到达」或「事件源不可用（降级为
    /// 轮询）」时才做，其余轮次直接复用上次窗口快照——前台没变则目标条状态也不会变。
    /// `open_picker` 另有一次强制枚举，保证打开列表时健康度是最新的。
    pub fn poll(&mut self) -> Result<(), String> {
        let mut foreground_changed = false;
        if let Some(observer) = self.deps.observer.as_mut() {
            while let Some(snapshot) = observer
                .next_foreground()
                .map_err(|error| error.to_string())?
            {
                self.routing.on_foreground(&snapshot);
                foreground_changed = true;
            }
        }

        // 事件驱动模式下只在前台真的变了才全枚举；非事件驱动（退路轮询）时每轮都得枚举，
        // 否则窗口出现/消失无从察觉。
        if foreground_changed || !self.event_driven {
            self.refresh_windows_now()?;
        }
        Ok(())
    }

    /// 立即做一次全窗口枚举：刷新目标健康度，并把当前前台对齐到路由状态。
    fn refresh_windows_now(&mut self) -> Result<(), String> {
        let windows = self
            .deps
            .enumerator
            .windows()
            .map_err(|error| error.to_string())?;
        let foreground = self.deps.focus.foreground();
        if let Some(snapshot) = windows.iter().find(|snapshot| snapshot.hwnd == foreground) {
            self.routing.on_foreground(snapshot);
        }
        self.routing.refresh_windows(&windows);
        Ok(())
    }

    pub fn snapshot(&self) -> TargetBarSnapshot {
        self.routing.snapshot()
    }

    /// 原生文件对话框访问口（导入选文件夹 / 导出选保存路径）。
    pub fn dialogs(&mut self) -> &dyn FileDialogs {
        self.deps.dialogs.as_ref()
    }

    /// 把「前台可能变了」的唤醒回调装到观察器上，返回是否接管成功。
    ///
    /// 成功时事件到达会直接触发回调（由 app-ui 负责在其中唤醒 UI 事件循环并 `poll`），
    /// 不再依赖固定周期的 `Timer`。观察器不存在或事件源不可用时返回 false，
    /// 由调用方保留退路 `Timer`。
    pub fn install_wakeup(&mut self, wakeup: Box<dyn Fn() + Send + Sync>) -> bool {
        let installed = match self.deps.observer.as_mut() {
            Some(observer) => observer.set_wakeup(wakeup),
            None => false,
        };
        self.event_driven = installed;
        installed
    }

    pub fn open_picker(&mut self) -> bool {
        // 打开列表前强制刷新一次健康度，避免展示到已经消失或新登录的窗口的陈旧状态。
        let _ = self.refresh_windows_now();
        self.routing.open_picker()
    }

    /// 当前热目标绑定（只读快照）。供测试与「解绑自愈」判定使用；
    /// `hwnd == None` 表示目标身份存在但处于休眠（窗口暂时不可见/未复现）。
    pub fn selected(&self) -> Option<targets::TargetBinding> {
        self.routing.selected().cloned()
    }

    pub fn toggle_picker(&mut self) -> bool {
        self.routing.toggle_picker()
    }

    pub fn choose(&mut self, selection_key: &str) -> bool {
        self.routing.choose(selection_key)
    }

    pub fn toggle_pin(&mut self) {
        self.routing.toggle_pin();
    }

    pub fn paste(&mut self, payload: &AssetPayload<'_>) -> TargetPasteNotice {
        // 连击合并（D45）：Slint TouchArea 的 `clicked` 在双击的第二次抬笔同样触发
        // （i-slint-core 1.17.1 items.rs 的 Release 分支：先无条件发 `clicked`，
        // `click_count % 2 == 1` 再追加 `double_clicked`——两信号不互斥）。单击模式
        // 下用户的一次双击 = 两次完整上框请求；粘贴同步阻塞 UI 线程，尾随点击排队
        // 到首次完成后立即执行，形成低配机日志里同素材 ~300ms 的成对请求
        // （97,97 / 92,92 / 1931×3），且尾随点击的 mouse-down 在 win32k 输入路径上
        // 当场激活本应用、抢走首次注入前校验的前台。因此首次注入成功后的短窗内，
        // 同素材请求按连击尾随半程并入：不再重复上框。降级/失败清空记录，失败后的
        // 立即重试不受影响。
        if let Some((last_path, at)) = &self.last_injected {
            if *last_path == payload.source_path
                && at.elapsed() < Duration::from_millis(TRAILING_CLICK_COALESCE_MS)
            {
                let label = self
                    .routing
                    .selected()
                    .map_or_else(|| "目标".to_string(), |binding| binding.label.clone());
                log::info!(
                    "连击尾随点击并入刚完成的上框（间隔 {}ms），跳过重复请求",
                    at.elapsed().as_millis()
                );
                return TargetPasteNotice {
                    tone: TargetNoticeTone::Success,
                    text: format!("已上框到 {label}（连击已合并）"),
                    injected: true,
                };
            }
        }
        // 解绑自愈（D42）：真实低配机日志显示约 30% 的上框因「热目标暂时无窗口
        // 绑定」降级——枚举竞态（最小化/恢复动画中被跳过）会让绑定短暂解绑，
        // 恢复要等下一轮轮询（退路 Timer 下最长 2s），用户在用连续重试硬扛。
        // 上框请求本身就是最高优先级的重绑时机：发现热目标存在但解绑时，
        // 先强制一次全枚举，把「等轮询」变成「当次请求内自愈」。
        if matches!(self.routing.selected(), Some(binding) if binding.hwnd.is_none()) {
            log::info!("上框时热目标处于解绑态，先强制重绑（不再等轮询）");
            if let Err(error) = self.refresh_windows_now() {
                log::warn!("解绑自愈的全枚举失败: {error}");
            }
        }
        let mut deps = TargetPipelineDeps {
            clipboard: self.deps.clipboard.as_mut(),
            focus: self.deps.focus.as_ref(),
            injector: self.deps.injector.as_mut(),
            activator: self.deps.activator.as_ref(),
            readiness: self.deps.readiness.as_ref(),
            focuser: self.deps.focuser.as_ref(),
        };
        let notice = self.routing.paste(payload, &mut deps);
        if notice.injected {
            self.last_injected = Some((payload.source_path.clone(), Instant::now()));
        } else {
            self.last_injected = None;
        }
        notice
    }

    /// D48 右键「复制」入口：只写剪贴板，不激活、不注入、不进连击合并记录
    /// （它不是上框，last_injected 语义是「实际注入成功」）。
    pub fn copy_to_clipboard(&mut self, payload: &AssetPayload<'_>) -> Result<(), String> {
        let mut deps = TargetPipelineDeps {
            clipboard: self.deps.clipboard.as_mut(),
            focus: self.deps.focus.as_ref(),
            injector: self.deps.injector.as_mut(),
            activator: self.deps.activator.as_ref(),
            readiness: self.deps.readiness.as_ref(),
            focuser: self.deps.focuser.as_ref(),
        };
        self.routing.copy_to_clipboard(payload, &mut deps)
    }
}
