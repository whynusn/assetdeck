//! 传输清单（D80）：推送前先送达接收方的批次描述。
//!
//! 接收方据此完成首连信任确认（显示发送方与批规模）、SHA-256 预去重（D61/D65 口径）
//! 与归类弹窗预填（D50）。清单是「当次显式选中集」的描述，不是库的索引快照——
//! 条数上限（[`MANIFEST_MAX_ITEMS`]）从结构上排除「全库倾倒」路径（D80 红线 1）。

use core::fmt;
use domain::AssetKind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 单批推送条数上限。上限来自 D80 红线 1（默认零共享、无静默全局同步），
/// 防的是清单被构造成本库索引快照；一次显式多选远达不到这个量级。
pub const MANIFEST_MAX_ITEMS: usize = 1000;

/// 展示文件名的长度上限（字符数），与 Windows 文件名约束同一量级。
pub const FILE_NAME_MAX_CHARS: usize = 200;

/// 一批推送的清单。`batch_id` 关联接收确认、传输进度与共享记录（同一批次同一 id）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferManifest {
    pub batch_id: Uuid,
    /// 发送方设备显示名（mDNS 宣告名），只用于确认弹窗与记录面。
    pub sender_name: String,
    pub items: Vec<ManifestItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestItem {
    pub asset_uuid: Uuid,
    /// 展示用原始文件名。接收端把对象物化为库内 canonical 名（raw.<ext>），
    /// 此名不作为落盘路径成分，但仍按不可信输入校验（防御纵深 + 记录面展示安全）。
    pub file_name: String,
    pub kind: AssetKind,
    pub size_bytes: u64,
    /// 发送库内对象文件的 SHA-256 十六进制（大小写不敏感；[`Self::sha256_lower`] 取规范形）。
    pub sha256_hex: String,
    /// 发送方分类名，仅作归类弹窗预填建议（D50），接收方裁决。
    pub category_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// 空清单（无显式选中集）。
    Empty,
    /// 超过单批上限。
    TooMany { count: usize },
    /// 文件名含路径成分、非法字符或越界（index 为 items 下标）。
    BadFileName { index: usize },
    /// 摘要不是 64 位十六进制（index 为 items 下标）。
    BadSha256 { index: usize },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Empty => write!(f, "清单为空"),
            ManifestError::TooMany { count } => {
                write!(f, "单批最多 {} 项（实际 {} 项）", MANIFEST_MAX_ITEMS, count)
            }
            ManifestError::BadFileName { index } => write!(f, "第 {} 项文件名非法", index + 1),
            ManifestError::BadSha256 { index } => write!(f, "第 {} 项内容摘要非法", index + 1),
        }
    }
}

impl TransferManifest {
    /// 结构校验：发送端在组清单后调用，接收端在信任确认前调用——两侧同守一份规则。
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.items.is_empty() {
            return Err(ManifestError::Empty);
        }
        if self.items.len() > MANIFEST_MAX_ITEMS {
            return Err(ManifestError::TooMany {
                count: self.items.len(),
            });
        }
        for (index, item) in self.items.iter().enumerate() {
            if !is_safe_display_name(&item.file_name) {
                return Err(ManifestError::BadFileName { index });
            }
            if !is_sha256_hex(&item.sha256_hex) {
                return Err(ManifestError::BadSha256 { index });
            }
        }
        Ok(())
    }
}

impl ManifestItem {
    /// 规范形小写摘要——接收端查重前统一折叠（对齐 D70 SUMS 解析的大小写归一）。
    pub fn sha256_lower(&self) -> String {
        self.sha256_hex.to_ascii_lowercase()
    }
}

/// 展示文件名安全校验：拒绝路径分隔符、盘符、Windows 保留字符、控制字符
/// 与尾随点/空格（Windows shell 语义会把尾随点/空格静默剥掉）。
fn is_safe_display_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > FILE_NAME_MAX_CHARS {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    name.chars().all(|c| match c {
        '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*' => false,
        c if c.is_control() => false,
        _ => true,
    })
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn item(file_name: &str, sha256_hex: &str) -> ManifestItem {
        ManifestItem {
            asset_uuid: Uuid::new_v4(),
            file_name: file_name.to_string(),
            kind: AssetKind::Image,
            size_bytes: 1,
            sha256_hex: sha256_hex.to_string(),
            category_hint: None,
        }
    }

    fn manifest(items: Vec<ManifestItem>) -> TransferManifest {
        TransferManifest {
            batch_id: Uuid::new_v4(),
            sender_name: "测试机".to_string(),
            items,
        }
    }

    #[test]
    fn valid_manifest_passes() {
        manifest(vec![item("wallpaper.jpg", SHA)])
            .validate()
            .expect("合法清单应通过");
    }

    #[test]
    fn empty_manifest_is_rejected() {
        assert_eq!(manifest(vec![]).validate(), Err(ManifestError::Empty));
    }

    #[test]
    fn oversized_manifest_is_rejected() {
        let items = vec![item("a.png", SHA); MANIFEST_MAX_ITEMS + 1];
        assert_eq!(
            manifest(items).validate(),
            Err(ManifestError::TooMany {
                count: MANIFEST_MAX_ITEMS + 1
            })
        );
    }

    #[test]
    fn path_like_names_are_rejected() {
        for bad in [
            "../x.png",
            "a/b.png",
            "a\\b.png",
            "C:evil.png",
            "a<b.png",
            "a|b.png",
            ".",
            "..",
        ] {
            assert_eq!(
                manifest(vec![item(bad, SHA)]).validate(),
                Err(ManifestError::BadFileName { index: 0 }),
                "应拒绝 {bad:?}"
            );
        }
    }

    #[test]
    fn trailing_dot_and_space_are_rejected() {
        for bad in ["a.png.", "a.png "] {
            assert_eq!(
                manifest(vec![item(bad, SHA)]).validate(),
                Err(ManifestError::BadFileName { index: 0 }),
                "应拒绝 {bad:?}"
            );
        }
    }

    #[test]
    fn control_chars_and_oversized_names_are_rejected() {
        assert_eq!(
            manifest(vec![item("a\u{7}b.png", SHA)]).validate(),
            Err(ManifestError::BadFileName { index: 0 })
        );
        let long: String = "名".repeat(FILE_NAME_MAX_CHARS + 1);
        assert_eq!(
            manifest(vec![item(&long, SHA)]).validate(),
            Err(ManifestError::BadFileName { index: 0 })
        );
    }

    #[test]
    fn malformed_sha256_is_rejected() {
        for bad in ["", "a", &"g".repeat(64), &"0".repeat(63), &"0".repeat(65)] {
            assert_eq!(
                manifest(vec![item("a.png", bad)]).validate(),
                Err(ManifestError::BadSha256 { index: 0 }),
                "应拒绝 {bad:?}"
            );
        }
    }

    #[test]
    fn uppercase_sha_is_valid_and_normalizes() {
        let upper: String = SHA.to_ascii_uppercase();
        manifest(vec![item("a.png", &upper)])
            .validate()
            .expect("大写十六进制是合法摘要");
        assert_eq!(item("a.png", &upper).sha256_lower(), SHA);
    }

    #[test]
    fn manifest_serde_roundtrip() {
        let m = manifest(vec![
            ManifestItem {
                category_hint: Some("旅行".to_string()),
                ..item("photo.jpg", SHA)
            },
            item("notes.txt", SHA),
        ]);
        let json = serde_json::to_string(&m).expect("序列化");
        let back: TransferManifest = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(back, m);
    }
}
