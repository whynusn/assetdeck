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
    MediaType {
        key: "png",
        kind: AssetKind::Image,
        importable: true,
        thumbnailable: true,
        paste_derivable: true,
    },
    MediaType {
        key: "jpg",
        kind: AssetKind::Image,
        importable: true,
        thumbnailable: true,
        paste_derivable: true,
    },
    MediaType {
        key: "jpeg",
        kind: AssetKind::Image,
        importable: true,
        thumbnailable: true,
        paste_derivable: true,
    },
    MediaType {
        key: "gif",
        kind: AssetKind::Image,
        importable: true,
        thumbnailable: true,
        paste_derivable: true,
    },
    MediaType {
        key: "webp",
        kind: AssetKind::Image,
        importable: true,
        thumbnailable: true,
        paste_derivable: true,
    },
    MediaType {
        key: "bmp",
        kind: AssetKind::Image,
        importable: true,
        thumbnailable: true,
        paste_derivable: true,
    },
    // —— 视频：可导入、可缩略（worker 抽帧）；不上框派生（HDROP 交文件引用，D18）——
    MediaType {
        key: "mp4",
        kind: AssetKind::Video,
        importable: true,
        thumbnailable: true,
        paste_derivable: false,
    },
    MediaType {
        key: "mov",
        kind: AssetKind::Video,
        importable: true,
        thumbnailable: true,
        paste_derivable: false,
    },
    MediaType {
        key: "mkv",
        kind: AssetKind::Video,
        importable: true,
        thumbnailable: true,
        paste_derivable: false,
    },
    MediaType {
        key: "avi",
        kind: AssetKind::Video,
        importable: true,
        thumbnailable: true,
        paste_derivable: false,
    },
    MediaType {
        key: "webm",
        kind: AssetKind::Video,
        importable: true,
        thumbnailable: true,
        paste_derivable: false,
    },
    // —— 文本：可导入、无缩略图（走文字卡片）、不派生 ——
    MediaType {
        key: "txt",
        kind: AssetKind::Text,
        importable: true,
        thumbnailable: false,
        paste_derivable: false,
    },
    MediaType {
        key: "md",
        kind: AssetKind::Text,
        importable: true,
        thumbnailable: false,
        paste_derivable: false,
    },
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
    by_extension(ext)
        .map(|t| t.paste_derivable)
        .unwrap_or(false)
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

/// 库内文本不变量（D60）：所有入库文本统一为 UTF-8（无 BOM）。
///
/// 判定顺序（确定性，不做内容嗅探）：UTF-8 BOM → 剥壳（主体须仍合法，病态
/// 文件的主体再按 GBK）；UTF-16 BOM → 交给 encoding_rs 按 BOM 精确解码；
/// 无 BOM 且合法 UTF-8 → 原样通过（最常见路径，零拷贝）；其余一律按 GBK
/// 转码——中文 Windows 的 ANSI 事实标准。GBK 解码全域可映射不会失败，异种
/// 编码会得到替换字符：宁可如此也不把非 UTF-8 字节放进库。
///
/// 只作用于入库副本，原始文件不动。纯内存计算，无 IO。
pub fn normalize_text_to_utf8(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return match std::str::from_utf8(rest) {
            Ok(_) => std::borrow::Cow::Borrowed(rest),
            Err(_) => std::borrow::Cow::Owned(gbk_to_utf8(rest)),
        };
    }
    // UTF-16 BOM（LE/BE）分支：for_bom 按 BOM 选编码解码（此分支必然命中，
    // UTF-8 BOM 已在上面剥离）。注意 encoding_rs 没有自由函数 decode()。
    if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, _) = encoding.decode(bytes);
        return std::borrow::Cow::Owned(text.into_owned().into_bytes());
    }
    if std::str::from_utf8(bytes).is_ok() {
        return std::borrow::Cow::Borrowed(bytes);
    }
    std::borrow::Cow::Owned(gbk_to_utf8(bytes))
}

fn gbk_to_utf8(bytes: &[u8]) -> Vec<u8> {
    let (text, _, _) = encoding_rs::GBK.decode(bytes);
    text.into_owned().into_bytes()
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

    /// D60 库内文本不变量的四条输入路径：UTF-8 直通、GBK 转码、BOM 剥壳、
    /// UTF-16 解码——产出一律是合法 UTF-8（严格解码验证）。
    #[test]
    fn text_normalization_covers_utf8_gbk_bom_and_utf16() {
        // 无 BOM 合法 UTF-8：原样通过（零拷贝借用）。
        let utf8 = "你好，world".as_bytes().to_vec();
        assert!(matches!(
            normalize_text_to_utf8(&utf8),
            std::borrow::Cow::Borrowed(_)
        ));

        // GBK（中文 Windows ANSI）：你好 = C4 E3 BA C3。
        let gbk = [0xC4u8, 0xE3, 0xBA, 0xC3, b'a', b'b'];
        assert_eq!(normalize_text_to_utf8(&gbk).as_ref(), "你好ab".as_bytes());

        // UTF-8 BOM：剥壳。
        let mut bom_utf8 = vec![0xEFu8, 0xBB, 0xBF];
        bom_utf8.extend_from_slice("hi".as_bytes());
        assert_eq!(normalize_text_to_utf8(&bom_utf8).as_ref(), b"hi");

        // UTF-16LE BOM：解码为 UTF-8。
        let mut utf16 = vec![0xFFu8, 0xFE];
        for unit in "你好".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(normalize_text_to_utf8(&utf16).as_ref(), "你好".as_bytes());

        // 归一化产物必须是严格合法 UTF-8。
        for input in [gbk.as_slice(), bom_utf8.as_slice(), utf16.as_slice()] {
            let out = normalize_text_to_utf8(input);
            assert!(
                std::str::from_utf8(out.as_ref()).is_ok(),
                "归一化输出必须是合法 UTF-8"
            );
        }
    }
}
