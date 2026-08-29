//! 目标实例别名册——`targets.json`（D13 第三层数据源）的别名部分。
//!
//! 键是窗口实例身份 `instance_id`（`exe:pid`）。诚实边界：pid 随目标进程重启
//! 变化，别名只保证**同一目标进程存活期内**稳定——微信双开区分（别名的主场景）
//! 发生在 IM 不重启的日常会话里，重启后重新命名一次是当前可观测身份（exe+pid）
//! 下的诚实上限；窗口 UIA 树不暴露账号昵称（2026-08-29 全树 dump 实证）。
//!
//! 文件路径选择、读取与原子保存归装配层（database-guidelines）；本类型只做
//! 纯模型：解析、查询、增删、序列化。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AliasMap {
    entries: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AliasDocument {
    version: u32,
    aliases: BTreeMap<String, String>,
}

impl AliasMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解析 targets.json 内容。空串与解析失败返回空册——别名是纯装饰性数据，
    /// 坏文件不得阻断目标功能，由装配层决定是否告警。
    pub fn parse(content: &str) -> Self {
        let Ok(doc) = serde_json::from_str::<AliasDocument>(content) else {
            return Self::default();
        };
        if doc.version != FORMAT_VERSION {
            return Self::default();
        }
        Self {
            entries: doc.aliases,
        }
    }

    pub fn get(&self, instance_id: &str) -> Option<&str> {
        self.entries.get(instance_id).map(String::as_str)
    }

    /// 设置别名；`None` = 清除。空串与去空格后空串视同清除——UI 侧「留空恢复
    /// 默认名」的语义在这里归一。
    pub fn set(&mut self, instance_id: &str, alias: Option<&str>) {
        let cleaned = alias.map(str::trim).filter(|value| !value.is_empty());
        match cleaned {
            Some(value) => {
                self.entries
                    .insert(instance_id.to_string(), value.to_string());
            }
            None => {
                self.entries.remove(instance_id);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 序列化为 targets.json。BTreeMap 保证键序稳定，重命名后的重写不产生无谓 diff。
    pub fn to_json(&self) -> String {
        let doc = AliasDocument {
            version: FORMAT_VERSION,
            aliases: self.entries.clone(),
        };
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_entries_and_key_order() {
        let mut map = AliasMap::new();
        map.set("Weixin.exe:12140", Some("主号"));
        map.set("Weixin.exe:13000", Some("工作号"));
        map.set("AliWorkbench.exe:22404", Some("接待"));
        let json = map.to_json();

        let parsed = AliasMap::parse(&json);
        assert_eq!(parsed, map);
        // BTreeMap 键序：AliWorkbench < Weixin，重写稳定。
        assert!(json.find("AliWorkbench.exe").unwrap() < json.find("Weixin.exe").unwrap());
    }

    #[test]
    fn empty_or_corrupt_content_yields_empty_map() {
        assert!(AliasMap::parse("").is_empty());
        assert!(AliasMap::parse("不是 json").is_empty());
        assert!(AliasMap::parse(r#"{"version": 99, "aliases": {"a": "b"}}"#).is_empty());
    }

    #[test]
    fn blank_alias_clears_instead_of_storing() {
        let mut map = AliasMap::new();
        map.set("Weixin.exe:1", Some("主号"));
        map.set("Weixin.exe:1", Some("   "));
        assert!(map.get("Weixin.exe:1").is_none(), "空串语义 = 恢复默认名");
        map.set("Weixin.exe:1", None);
        assert!(map.get("Weixin.exe:1").is_none());
    }

    #[test]
    fn set_then_get_trims_value() {
        let mut map = AliasMap::new();
        map.set("Weixin.exe:1", Some("  主号  "));
        assert_eq!(map.get("Weixin.exe:1"), Some("主号"));
    }
}
