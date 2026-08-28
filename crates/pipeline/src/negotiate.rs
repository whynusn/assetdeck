//! 格式协商表：资产类型 × 目标 profile → 剪贴板载荷。纯函数、零 IO。
//!
//! 表驱动纪律（spec/pipeline Code Review 清单）：新增协商项走 (kind, profile)
//! 二维 match 行，而非 if-else 散落。

use std::borrow::Cow;
use std::path::PathBuf;

use platform::ClipboardPayload;
use targets::{ClipboardFormat, FormatKind, Profile};

/// 资产类别（管线视角的最小分类，本体收敛在 domain::AssetKind）。
///
/// `Other` 承载 v1 未路由的资产类（归档/字体/音频等）：协商返回 `None`，
/// 由调用方降级为 Files 或提示不支持——这正是「未知组合」的一等表行。
pub use domain::AssetKind;

/// 待粘贴资产的运行时载荷快照（UI 侧取数后传入，管线内不做任何解码）。
#[derive(Debug, Clone)]
pub struct AssetPayload<'a> {
    pub kind: AssetKind,
    /// Image 行使用：上游已编码的 PNG 字节（「UI 进程不解码」红线）。
    pub png_bytes: &'a [u8],
    pub source_path: PathBuf,
    pub text: String,
}

/// 目标应用画像。v1 只有通用 IM profile，枚举留扩展位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetProfile {
    ImGeneric,
}

/// 协商所需的画像视图。保留 M6 的枚举入口，同时让 M8 使用数据化 Profile。
pub trait NegotiationProfile {
    fn formats(&self, kind: AssetKind) -> &[ClipboardFormat];

    /// 该 (类别, 格式) 组合粘进这个目标是否等于「直接发送」。默认否。
    ///
    /// 带上 kind 是实测要求：千牛把视频的 CF_HDROP 当场发出，图片的 CF_HDROP
    /// 却停在输入框，同一格式在同一目标上结论相反。
    fn paste_sends(&self, _kind: AssetKind, _format: ClipboardFormat) -> bool {
        false
    }
}

impl NegotiationProfile for TargetProfile {
    fn formats(&self, kind: AssetKind) -> &[ClipboardFormat] {
        const IMAGE: &[ClipboardFormat] = &[ClipboardFormat::Png, ClipboardFormat::Files];
        const VIDEO: &[ClipboardFormat] = &[ClipboardFormat::Files];
        const TEXT: &[ClipboardFormat] = &[ClipboardFormat::Text];
        const NONE: &[ClipboardFormat] = &[];
        match (self, kind) {
            (TargetProfile::ImGeneric, AssetKind::Image) => IMAGE,
            (TargetProfile::ImGeneric, AssetKind::Video) => VIDEO,
            (TargetProfile::ImGeneric, AssetKind::Text) => TEXT,
            (TargetProfile::ImGeneric, AssetKind::Other) => NONE,
        }
    }
}

impl NegotiationProfile for &Profile {
    fn formats(&self, kind: AssetKind) -> &[ClipboardFormat] {
        self.formats.for_kind(format_kind(kind))
    }

    fn paste_sends(&self, kind: AssetKind, format: ClipboardFormat) -> bool {
        self.paste_sends_format(format_kind(kind), format)
    }
}

fn format_kind(kind: AssetKind) -> FormatKind {
    match kind {
        AssetKind::Image => FormatKind::Image,
        AssetKind::Video => FormatKind::Video,
        AssetKind::Text => FormatKind::Text,
        AssetKind::Other => FormatKind::Other,
    }
}

/// 协商结果。「只上框不发送」是红线，所以「能承载」和「粘进去安全」是两件事，
/// 必须分开表达：前者决定写什么剪贴板，后者决定敢不敢注入 Ctrl+V。
///
/// 载荷与 AssetPayload 同生命周期（借用不拷贝，见 platform::ClipboardPayload）。
#[derive(Debug, Clone)]
pub enum Negotiated<'a> {
    /// 找到既能承载素材、又不会触发发送的格式：可以写剪贴板并注入。
    Safe {
        format: ClipboardFormat,
        payload: ClipboardPayload<'a>,
    },
    /// 只剩下「粘贴即发送」的格式（如千牛 × 视频 → CF_HDROP）。
    /// 仍返回载荷以便复制到剪贴板，但调用方**不得注入**。
    WouldSend {
        format: ClipboardFormat,
        payload: ClipboardPayload<'a>,
    },
    /// 画像声明的格式都无法承载该素材。
    Unsupported,
}

// 跨生命周期比较（与 platform::ClipboardPayload 的手工实现同理）：不同借用源的
// 协商结果在测试断言里需要直接相等判定，派生只能生成同生命周期实现。
impl<'a, 'b> PartialEq<Negotiated<'b>> for Negotiated<'a> {
    fn eq(&self, other: &Negotiated<'b>) -> bool {
        match (self, other) {
            (
                Negotiated::Safe {
                    format: af,
                    payload: ap,
                },
                Negotiated::Safe {
                    format: bf,
                    payload: bp,
                },
            ) => af == bf && ap == bp,
            (
                Negotiated::WouldSend {
                    format: af,
                    payload: ap,
                },
                Negotiated::WouldSend {
                    format: bf,
                    payload: bp,
                },
            ) => af == bf && ap == bp,
            (Negotiated::Unsupported, Negotiated::Unsupported) => true,
            _ => false,
        }
    }
}
impl<'a> Eq for Negotiated<'a> {}

/// 完整协商：按画像顺序取首个可承载格式，并区分它是否「粘贴即发送」。
///
/// 优先返回安全格式；只有当所有可承载格式都会触发发送时，才返回
/// [`Negotiated::WouldSend`]（携带首个可承载格式，供仅复制路径使用）。
pub fn negotiate_detailed<'a, P: NegotiationProfile>(
    req: &'a AssetPayload<'a>,
    profile: P,
) -> Negotiated<'a> {
    let mut would_send: Option<(ClipboardFormat, ClipboardPayload<'a>)> = None;
    for &format in profile.formats(req.kind) {
        let Some(payload) = carry(req, format) else {
            continue;
        };
        if profile.paste_sends(req.kind, format) {
            would_send.get_or_insert((format, payload));
            continue;
        }
        return Negotiated::Safe { format, payload };
    }
    match would_send {
        Some((format, payload)) => Negotiated::WouldSend { format, payload },
        None => Negotiated::Unsupported,
    }
}

/// 单格式承载判定：该格式能否装下这份素材（纯查表，无重编码）。
///
/// DIB 行存在但不默认路由：当前 [`AssetPayload`] 不承载 DIB 字节，
/// 画像声明 DIB 时返回 `None` 让协商继续尝试下一格式——UI 进程永不做位图解码（红线）。
fn carry<'a>(req: &'a AssetPayload<'a>, format: ClipboardFormat) -> Option<ClipboardPayload<'a>> {
    match format {
        ClipboardFormat::Png if !req.png_bytes.is_empty() => {
            // 借用源切片，零拷贝；写入端再单次搬进剪贴板块。
            Some(ClipboardPayload::Png(Cow::Borrowed(req.png_bytes)))
        }
        ClipboardFormat::Files if !req.source_path.as_os_str().is_empty() => {
            Some(ClipboardPayload::Files(vec![req.source_path.clone()]))
        }
        ClipboardFormat::Text if !req.text.is_empty() => {
            Some(ClipboardPayload::Text(Cow::Borrowed(req.text.as_str())))
        }
        ClipboardFormat::Dib
        | ClipboardFormat::Png
        | ClipboardFormat::Files
        | ClipboardFormat::Text => None,
    }
}

/// 查表：(kind × profile) → 剪贴板载荷。
///
/// - Image → PNG 字节透传（不重编码）；
/// - Video → 源文件 HDROP（视频无帧级粘贴语义，交文件引用）；
/// - Text  → Unicode 文本；
/// - 无映射（未知组合）→ `None`，「查不到」是合法查询结果而非错误，
///   调用方据此降级为 Files 或报不支持。
///
/// DIB 行存在但不默认路由：仅当上游已持有编码字节时由专用路径构造
/// [`ClipboardPayload::Dib`]——UI 进程永不做位图解码（红线）。
///
/// 本函数只回答「安全格式」，会跳过画像标记为「粘贴即发送」的格式；
/// 需要区分 `Unsupported` 与 `WouldSend` 的调用方请用 [`negotiate_detailed`]。
pub fn negotiate<'a, P: NegotiationProfile>(
    req: &'a AssetPayload<'a>,
    profile: P,
) -> Option<ClipboardPayload<'a>> {
    match negotiate_detailed(req, profile) {
        Negotiated::Safe { payload, .. } => Some(payload),
        Negotiated::WouldSend { .. } | Negotiated::Unsupported => None,
    }
}
