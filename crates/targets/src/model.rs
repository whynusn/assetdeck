use std::fmt;

use platform::WindowHandle;
use serde::{Deserialize, Serialize};

/// 配置中的稳定目标身份。窗口销毁或托盘往返不会改变它。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TargetId(String);

impl TargetId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<&str> for TargetId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetScope {
    #[default]
    App,
    Conversation,
}

/// 稳定身份与当前窗口实例的绑定。`hwnd=None` 表示目标仍被记住但暂不可达。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetBinding {
    pub id: TargetId,
    pub hwnd: Option<WindowHandle>,
    pub label: String,
    pub fallback: bool,
    pub minimized: bool,
    pub visible: bool,
    pub instance_id: String,
    /// 匹配时标题正则是否命中（「会话窗口」证据）。千牛优惠弹窗这类
    /// 「类名命中但标题不合会话特征」的窗口为 false，热目标切换日志据此
    /// 区分正常跟随与可疑顶替。
    pub session_window: bool,
}

impl TargetBinding {
    pub fn new(id: TargetId, hwnd: WindowHandle, label: impl Into<String>) -> Self {
        Self {
            id,
            hwnd: Some(hwnd),
            label: label.into(),
            fallback: false,
            minimized: false,
            visible: true,
            instance_id: format!("handle-{}", hwnd.0),
            session_window: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Green,
    Yellow,
    Red,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotReadyReason {
    NotLoggedIn,
    NoConversation,
    ReadOnly,
    ModalBlocking,
    Starting,
    /// 目标窗口进程还活着，但注入前前台已不在目标窗口上（D44 拆分自 WindowGone：
    /// 两者给用户的指引完全不同——已关要重开目标，漂移要切回窗口）。
    ForegroundLost,
    WindowGone,
    NoTarget,
    Ambiguous,
    ProbeUnavailable,
    /// 目标应用把该剪贴板格式的粘贴当作「立即发送」（千牛 × CF_HDROP 实测）。
    /// 「只上框、不发送」是红线，故这类格式不注入：只复制并提示用户手动粘贴。
    PasteWouldSend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    Unknown,
    NotReady(NotReadyReason),
}
