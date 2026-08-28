//! 媒体类型注册表：扩展名 → 素材类别与能力（导入 / 缩略图 / 粘贴派生）的唯一真相源。
//!
//! 目标（综合分析报告「扩展性缺口 #2」）：消除 catalog_loader、sample-library、
//! derive-thumbs、library 四处手写的「扩展名 → 类型/能力」映射。新增格式
//! （avif / heic / pdf / 音频…）只需要在本文件的 MEDIA_TYPES 注册表里加一行，
//! 各消费点自动获得一致的导入 / 缩略图 / 粘贴派生能力判定。
//!
//! 能力语义：
//! - importable：导入工序（sample-library）是否收集该扩展名；
//! - thumbnailable：derive-thumbs 是否为它派生浏览缩略图（无则瓦片回退占位色/图标）；
//! - paste_derivable：是否旁挂「上框用」paste.png（D20：图片全部派生，4096 cap 封顶）。
//!
//! 纯数据 / 零依赖运行时（const 表编进二进制）；AssetKind 本体在 domain（封闭世界枚举）。

use domain::AssetKind;
use std::path::Path;

/// 一种媒体类型的完整画像。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaType {
    /// 规范扩展名（小写、无点）。
    pub key: &'static str,
    pub kind: AssetKind,
    /// 能否作为素材导入。
    pub importable: bool,
    /// 能否派生出浏览缩略图。
    pub thumbnailable: bool,
    /// 是否旁挂「上框用」paste.png（D20 派生语义）。
    pub paste_derivable: bool,
}

/// 注册表。顺序即本文件维护顺序；查表按扩展名精确匹配，未知扩展 ⇒ 不适用。
pub static MEDIA_TYPES: &[MediaType] = &[
    // —— 图片：全部可导入、可缩略、可派生 paste.png（D20：PNG 原图同样派生封顶）——
    MediaType { key: "png", kind: AssetKind::Image, importable: true, thumbnailable: true, paste_derivable: true },
    MediaType { key: "jpg", kind: AssetKind::Image, importable: true, thumbnailable: true, paste_derivable: true },
    MediaType { key: "jpeg", kind: AssetKind::Image, importable: true, thumbnailable: true, paste_derivable: true },
    MediaType { key: "gif", kind: AssetKind::Image, importable: true, thumbnailable: true, paste_derivable: true },
    MediaType { key: "webp", kind: AssetKind::Image, importable: true, thumbnailable: true, paste_derivable: true },
    MediaType { key: "bmp", kind: AssetKind::Image, importable: true, thumbnailable: true, paste_derivable: true },
    // —— 视频：可导入、可缩略（worker 抽帧）；不上框派生（HDROP 交文件引用，D18）——
    MediaType { key: "mp4", kind: AssetKind::Video, importable: true, thumbnailable: true, paste_derivable: false },
    MediaType { key: "mov", kind: AssetKind::Video, importable: true, thumbnailable: true, paste_derivable: false },
    MediaType { key: "mkv", kind: AssetKind::Video, importable: true, thumbnailable: true, paste_derivable: false },
    MediaType { key: "avi", kind: AssetKind::Video, importable: true, thumbnailable: true, paste_derivable: false },
    MediaType { key: "webm", kind: AssetKind::Video, importable: true, thumbnailable: true, paste_derivable: false },
    // —— 文本：可导入、无缩略图（走文字卡片）、不派生 ——
    MediaType { key: "txt", kind: AssetKind::Text, importable: true, thumbnailable: false, paste_derivable: false },
    MediaType { key: "md", kind: AssetKind::Text, importable: true, thumbnailable: false, paste_derivable: false },
];

/// 按扩展名查注册表。ext 传小写、无点的扩展名；未知扩展返回 None。
pub fn by_extension(ext: &str) -> Option<&'static MediaType> {
    MEDIA_TYPES.iter().find(|t| t.key == ext)
}

/// 按文件路径判定素材类别；未知类型恒为 AssetKind::Other（不 panic，调用方据此降级）。
pub fn kind_of(path: &Path) -> AssetKind {
    ext_of(path)
        .and_then(|ext| by_extension(&ext))
        .map(|t| t.kind)
        .unwrap_or(AssetKind::Other)
}

/// 该路径是否可作为素材导入（导入工序收集判定）。
pub fn is_importable(path: &Path) -> bool {
    ext_of(path)
        .and_then(|ext| by_extension(&ext))
        .map(|t| t.importable)
        .unwrap_or(false)
}

/// 该扩展名是否可派生浏览缩略图。
pub fn is_thumbnailable(ext: &str) -> bool {
    by_extension(ext).map(|t| t.thumbnailable).unwrap_or(false)
}

/// 该扩展名是否需要旁挂「上框用」paste.png（D20）。
pub fn is_paste_derivable(ext: &str) -> bool {
    by_extension(ext).map(|t| t.paste_derivable).unwrap_or(false)
}

/// 全量可见列表（测试 / 诊断用）。
pub fn all() -> &'static [MediaType] {
    MEDIA_TYPES
}

/// 取路径扩展名（小写、无点）；无扩展名返回 None。
fn ext_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_the_four_kinds_and_importable_core() {
        let kinds: std::collections::HashSet<AssetKind> =
            MEDIA_TYPES.iter().map(|t| t.kind).collect();
        for kind in [AssetKind::Image, AssetKind::Video, AssetKind::Text] {
            assert!(kinds.contains(&kind), "注册表应覆盖 {kind:?}");
        }
        assert!(MEDIA_TYPES.iter().all(|t| t.importable));
    }

    #[test]
    fn by_extension_is_lowercase_contract() {
        assert_eq!(by_extension("jpg").unwrap().kind, AssetKind::Image);
        assert_eq!(by_extension("mp4").unwrap().kind, AssetKind::Video);
        assert_eq!(by_extension("txt").unwrap().kind, AssetKind::Text);
        // 大写进入注册表前已被调用方 lowercase（契约在 ext_of）
        assert!(by_extension("JPG").is_none());
        assert!(by_extension("avif").is_none());
    }

    #[test]
    fn kind_of_path_falls_back_to_other() {
        assert_eq!(kind_of(Path::new("a/b/photo.jpg")), AssetKind::Image);
        assert_eq!(kind_of(Path::new("clips/clip.mov")), AssetKind::Video);
        assert_eq!(kind_of(Path::new("notes/readme.md")), AssetKind::Text);
        assert_eq!(kind_of(Path::new("archive.zip")), AssetKind::Other);
        assert_eq!(kind_of(Path::new("noext")), AssetKind::Other);
    }

    #[test]
    fn capability_flags_match_import_thumbnail_paste_semantics() {
        assert!(is_thumbnailable("png"));
        assert!(is_thumbnailable("mp4"));
        assert!(!is_thumbnailable("txt"));
        assert!(is_paste_derivable("gif"));
        assert!(!is_paste_derivable("avi"));
        assert!(!is_paste_derivable("unknown"));
        assert!(is_importable(Path::new("x/photo.webp")));
        assert!(!is_importable(Path::new("x/pdf")));
    }
}
