//! 格式协商表：资产类型 × 目标 profile → 剪贴板载荷。纯函数、零 IO。
//!
//! 表驱动纪律（spec/pipeline Code Review 清单）：新增协商项走 (kind, profile)
//! 二维 match 行，而非 if-else 散落。

use std::path::PathBuf;

use platform::ClipboardPayload;

/// 资产类别（管线视角的最小分类）。
///
/// `Other` 承载 v1 未路由的资产类（归档/字体/音频等）：协商返回 `None`，
/// 由调用方降级为 Files 或提示不支持——这正是「未知组合」的一等表行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Image,
    Video,
    Text,
    Other,
}

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
pub fn negotiate(req: &AssetPayload<'_>, profile: TargetProfile) -> Option<ClipboardPayload> {
    match (req.kind, profile) {
        (AssetKind::Image, TargetProfile::ImGeneric) => {
            Some(ClipboardPayload::Png(req.png_bytes.to_vec()))
        }
        (AssetKind::Video, TargetProfile::ImGeneric) => {
            Some(ClipboardPayload::Files(vec![req.source_path.clone()]))
        }
        (AssetKind::Text, TargetProfile::ImGeneric) => {
            Some(ClipboardPayload::Text(req.text.clone()))
        }
        // 未路由组合（如 Other 类资产、未来新 profile）：显式 None 行。
        _ => None,
    }
}
