//! 粘贴管线：格式协商 → 剪贴板 → 焦点校验 → 注入 → auto-send 开关（默认关）。
//!
//! D8 红线：「双击 = 素材进输入框」止步于此；回车直发（auto_send）是管线
//! 末端的独立布尔开关且**默认关**，任何重构不得把 Enter 合成并入主路径。
//!
//! 失败语义是降级而非中断：除剪贴板写入失败（唯一硬失败 → [`PasteOutcome::Failed`]）
//! 外，其余一律返回可呈现的降级结果，交由调用方 UI 弹 toast。

pub mod negotiate;

use serde::{Deserialize, Serialize};

use platform::{ClipboardPayload, ClipboardSink, FocusWatcher, KeyInjector};

pub use negotiate::{negotiate, AssetKind, AssetPayload, TargetProfile};

/// 虚拟键码：Ctrl（与 Win32 VK_CONTROL 同值；D12 平台事实）。
pub const VK_CONTROL: u16 = 0x11;
/// 虚拟键码：字母 V。Win32 头文件对字母键无常量，惯例取 ASCII 大写码。
pub const VK_V: u16 = 0x56;
/// 虚拟键码：回车。auto_send 关闭时注入序列绝不出现
/// （守卫测试 auto_send_off_never_synththesizes_enter 锁定）。
pub const VK_RETURN: u16 = 0x0D;

/// Ctrl+V 和弦序列：Ctrl↓ V↓ V↑ Ctrl↑。
///
/// 按下/释放相位以 [`platform::KEY_UP`] 位标记；顺序不可重排——
/// Ctrl 必须在 V 之后才释放，否则和弦断裂。
fn chord_paste() -> Vec<u16> {
    vec![
        VK_CONTROL,
        VK_V,
        VK_V | platform::KEY_UP,
        VK_CONTROL | platform::KEY_UP,
    ]
}

/// 回车序列：Enter↓ Enter↑。仅在 auto_send 显式开启时追加到注入序列末端。
fn chord_enter() -> Vec<u16> {
    vec![VK_RETURN, VK_RETURN | platform::KEY_UP]
}

/// 管线配置。D8 红线：`auto_send` 默认值必须为 false
/// （快照测试 auto_send_flag_defaults_off 锁定序列化形态）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteConfig {
    pub auto_send: bool,
}

/// 管线结果。降级不是 Err——用枚举结果表达，让调用方 UI 呈现 toast。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteOutcome {
    /// 已写剪贴板并完成注入（Ctrl+V，及开启时的 Enter）。
    Injected,
    /// 仅复制成功。reason 说明未注入的原因，供 toast 文案。
    CopiedOnly { reason: String },
    /// 硬失败。唯一来源：剪贴板写入失败。
    Failed(String),
}

/// 管线的平台依赖集合：三个 trait 对象由装配层（app-ui）注入；
/// 测试以带共享操作日志的 mock 实现替换。
pub struct PipelineDeps<'a> {
    pub clipboard: &'a mut dyn ClipboardSink,
    pub focus: &'a dyn FocusWatcher,
    pub injector: &'a mut dyn KeyInjector,
}

/// 一次「唤起面板 → 选素材 → 粘贴」的会话状态。
///
/// 编排顺序（D8 锁定，操作日志断言对象）：
/// `write_clipboard → is_alive(previous) → inject Ctrl+V → [auto_send] inject Enter`
pub struct PasteSession {
    config: PasteConfig,
    /// 唤起面板时刻的前一前台窗口（焦点校验锚点）。
    previous_foreground: Option<platform::WindowHandle>,
}

impl PasteSession {
    pub fn new(config: PasteConfig) -> Self {
        Self {
            config,
            previous_foreground: None,
        }
    }

    /// 面板唤起时调用：记录此刻的前一前台窗口（D8 红线——后续焦点校验以此为锚，
    /// 宁可少发、不可误发到错误窗口）。
    pub fn begin_panel(&mut self, focus: &dyn FocusWatcher) {
        self.previous_foreground = Some(focus.foreground());
    }

    /// 只读测试钩子：上一记录的前台窗口（仿 library::set_paused 先例）。
    pub fn previous_foreground(&self) -> Option<platform::WindowHandle> {
        self.previous_foreground
    }

    pub fn paste(&mut self, req: &AssetPayload<'_>, deps: &mut PipelineDeps<'_>) -> PasteOutcome {
        // 1. 格式协商：无映射即降级（尚未触碰剪贴板，无副作用）。
        let payload: ClipboardPayload = match negotiate(req, TargetProfile::ImGeneric) {
            Some(p) => p,
            None => {
                return PasteOutcome::CopiedOnly {
                    reason: "素材类型无可用剪贴板格式".to_string(),
                }
            }
        };
        // 2. 红线：先写剪贴板，后切焦点/注入（顺序颠倒会粘出旧内容）。
        //    写入失败是整条管线唯一的硬失败。
        if let Err(e) = deps.clipboard.write(&payload) {
            return PasteOutcome::Failed(e.to_string());
        }
        // 3. 红线：焦点校验目标是唤起面板时记录的「前一前台窗口」；
        //    失败/无记录一律降级仅复制，禁止重试后强注。
        let Some(target) = self.previous_foreground else {
            return PasteOutcome::CopiedOnly {
                reason: "唤起面板前未记录前台窗口".to_string(),
            };
        };
        if !deps.focus.is_alive(target) {
            return PasteOutcome::CopiedOnly {
                reason: "前一前台窗口已失活".to_string(),
            };
        }
        // 4. 注入 Ctrl+V；auto_send 是独立末端开关（默认关），开启才追加 Enter。
        let mut keys = chord_paste();
        if self.config.auto_send {
            keys.extend_from_slice(&chord_enter());
        }
        if let Err(e) = deps.injector.inject(&keys) {
            // 注入失败但复制已完成：降级呈现，不算硬失败。
            return PasteOutcome::CopiedOnly {
                reason: format!("注入失败: {e}"),
            };
        }
        PasteOutcome::Injected
    }
}
