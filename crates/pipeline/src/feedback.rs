use targets::NotReadyReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackSeverity {
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackAction {
    None,
    Retry,
    OpenTarget,
    SignIn,
    SelectConversation,
    CloseModal,
    ChooseTarget,
    /// 素材已在剪贴板，但自动注入会触发发送：请用户自己在目标里 Ctrl+V。
    PasteManually,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteFeedback {
    pub severity: FeedbackSeverity,
    pub target_label: String,
    pub headline: String,
    pub hint: String,
    pub action: FeedbackAction,
    pub diagnostic: Option<String>,
}

impl PasteFeedback {
    pub fn copied(
        target_label: impl Into<String>,
        hint: impl Into<String>,
        action: FeedbackAction,
    ) -> Self {
        let target_label = target_label.into();
        Self {
            severity: FeedbackSeverity::Warning,
            headline: format!("已复制，未能上框到 {target_label}"),
            target_label,
            hint: hint.into(),
            action,
            diagnostic: None,
        }
    }

    pub fn not_ready(target_label: impl Into<String>, reason: NotReadyReason) -> Self {
        let (hint, action) = match reason {
            NotReadyReason::NotLoggedIn => ("请先登录目标应用", FeedbackAction::SignIn),
            NotReadyReason::NoConversation => (
                "请先选择一个会话并让输入框可见",
                FeedbackAction::SelectConversation,
            ),
            NotReadyReason::ReadOnly => (
                "当前会话不可输入，请切换到可发送的会话",
                FeedbackAction::SelectConversation,
            ),
            NotReadyReason::ModalBlocking => {
                ("请先关闭遮挡输入框的弹窗", FeedbackAction::CloseModal)
            }
            NotReadyReason::Starting => ("目标应用仍在启动，请稍后重试", FeedbackAction::Retry),
            NotReadyReason::ForegroundLost => (
                "目标窗口不在前台，素材已复制；切回目标窗口后 Ctrl+V 即可粘贴",
                FeedbackAction::Retry,
            ),
            NotReadyReason::WindowGone => (
                "目标窗口已关闭，请重新打开后重试",
                FeedbackAction::OpenTarget,
            ),
            NotReadyReason::NoTarget => ("请选择一个上框目标", FeedbackAction::ChooseTarget),
            NotReadyReason::Ambiguous => (
                "检测到多个可用窗口，请明确选择一个",
                FeedbackAction::ChooseTarget,
            ),
            NotReadyReason::ProbeUnavailable => (
                "无法确认输入框状态，请在目标应用中手动粘贴",
                FeedbackAction::Retry,
            ),
            NotReadyReason::PasteWouldSend => (
                "该应用会把粘贴的文件直接发出而不进输入框；素材已复制，确认要发送时再手动 Ctrl+V",
                FeedbackAction::PasteManually,
            ),
        };
        Self::copied(target_label, hint, action)
    }

    pub fn failed(target_label: impl Into<String>, diagnostic: impl Into<String>) -> Self {
        let target_label = target_label.into();
        Self {
            severity: FeedbackSeverity::Error,
            headline: format!("未能复制素材到 {target_label}"),
            target_label,
            hint: "请检查素材后重试".to_string(),
            action: FeedbackAction::Retry,
            diagnostic: Some(diagnostic.into()),
        }
    }

    pub fn with_diagnostic(mut self, diagnostic: impl Into<String>) -> Self {
        self.diagnostic = Some(diagnostic.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_not_ready_reason_maps_to_nonempty_feedback() {
        let reasons = [
            NotReadyReason::NotLoggedIn,
            NotReadyReason::NoConversation,
            NotReadyReason::ReadOnly,
            NotReadyReason::ModalBlocking,
            NotReadyReason::Starting,
            NotReadyReason::ForegroundLost,
            NotReadyReason::WindowGone,
            NotReadyReason::NoTarget,
            NotReadyReason::Ambiguous,
            NotReadyReason::ProbeUnavailable,
            NotReadyReason::PasteWouldSend,
        ];
        for reason in reasons {
            let feedback = PasteFeedback::not_ready("微信", reason);
            assert!(!feedback.headline.is_empty());
            assert!(!feedback.hint.is_empty());
        }
    }

    #[test]
    fn feedback_headline_contains_target_label() {
        let feedback = PasteFeedback::not_ready("Telegram", NotReadyReason::NoConversation);
        assert!(feedback.headline.contains("Telegram"));
    }

    #[test]
    fn all_degraded_feedback_mentions_clipboard_copied() {
        let feedback = PasteFeedback::not_ready("QQ", NotReadyReason::ModalBlocking);
        assert!(feedback.headline.contains("已复制"));
    }
}
