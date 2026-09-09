//! 同步消息契约（D80-M1）：push/pull 混合同步的三种报文。
//!
//! 载体与编码在 M1-b 落地（复用 D80 的 QUIC 单 bi 流 + 行协议）——本文件
//! 只定契约本身：字段、不变式、校验。铁律：
//! - 任何报文都**不含域定义**（成员册不出本机）；接收方视角只有
//!   [`ShareOffer`]（谁有哪些素材可得）。
//! - [`SyncMessage::Delta`] 表达区间 (from_rev, to_rev] 的变更集，由
//!   在线推送通道投递；[`SyncMessage::Pull`] 是上线方的拉取请求
//!   （since_rev = 本地缓存 rev，0 = 全量）；应答只有
//!   [`SyncMessage::Snapshot`]——已按拉取方过滤的可得清单 + 服务端 rev。
//! - 报文无序到达时的防御：Delta 的 to_rev ≤ 本地缓存 rev 直接丢弃
//!   （重放），那部分一致性由拉取后 Snapshot 摘要比对兜底。

use domain::AssetKind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::manifest::is_safe_display_name;
use crate::sharing::SharingEntry;

/// 接收方视角的一行「可得」：只含元数据，不含域、不含路径——对象路径
/// 只在后续显式推送的清单（manifest）里出现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareOffer {
    pub asset_uuid: Uuid,
    pub file_name: String,
    pub kind: AssetKind,
    pub size_bytes: u64,
}

/// 被撤销的共享事实（Delta 的 removed 侧）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedEntry {
    pub asset_uuid: Uuid,
    pub domain_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncMessage {
    /// 在线增量：区间 (from_rev, to_rev] 内的新增与撤销。
    Delta {
        from_rev: u64,
        to_rev: u64,
        added: Vec<SharingEntry>,
        removed: Vec<RevokedEntry>,
    },
    /// 上线拉取请求：since_rev = 拉取方本地缓存的最后 rev（0 = 冷启动全量）。
    Pull { since_rev: u64 },
    /// 快照应答：服务端当前 rev + 已按拉取方过滤的可得清单（可为空）。
    Snapshot { rev: u64, offers: Vec<ShareOffer> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// Delta 区间倒挂或空区间（to_rev ≤ from_rev）。
    BadRevOrder {
        from_rev: u64,
        to_rev: u64,
    },
    /// Delta 无任何变更（无信息量，属协议噪声）。
    EmptyDelta,
    BadFileName {
        index: usize,
    },
}

impl core::fmt::Display for SyncError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SyncError::BadRevOrder { from_rev, to_rev } => {
                write!(f, "Delta 区间非法：from_rev={from_rev} to_rev={to_rev}")
            }
            SyncError::EmptyDelta => write!(f, "Delta 不含任何变更"),
            SyncError::BadFileName { index } => {
                write!(f, "第 {index} 条可得项文件名不安全")
            }
        }
    }
}

impl std::error::Error for SyncError {}

impl SyncMessage {
    /// 结构性守卫：Delta 区间必须非空且携带变更；Snapshot 的可得项文件名
    /// 复用清单同名规则。Pull 恒合法。
    pub fn validate(&self) -> Result<(), SyncError> {
        match self {
            SyncMessage::Delta {
                from_rev,
                to_rev,
                added,
                removed,
            } => {
                if to_rev <= from_rev {
                    return Err(SyncError::BadRevOrder {
                        from_rev: *from_rev,
                        to_rev: *to_rev,
                    });
                }
                if added.is_empty() && removed.is_empty() {
                    return Err(SyncError::EmptyDelta);
                }
                Ok(())
            }
            SyncMessage::Pull { .. } => Ok(()),
            SyncMessage::Snapshot { offers, .. } => {
                for (index, offer) in offers.iter().enumerate() {
                    if !is_safe_display_name(&offer.file_name) {
                        return Err(SyncError::BadFileName { index });
                    }
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(name: &str) -> ShareOffer {
        ShareOffer {
            asset_uuid: Uuid::new_v4(),
            file_name: name.to_string(),
            kind: AssetKind::Image,
            size_bytes: 64,
        }
    }

    #[test]
    fn serde_roundtrip_with_kind_tag() {
        let delta = SyncMessage::Delta {
            from_rev: 3,
            to_rev: 5,
            added: vec![SharingEntry {
                asset_uuid: Uuid::new_v4(),
                domain_id: Uuid::new_v4(),
                since: 42,
            }],
            removed: vec![RevokedEntry {
                asset_uuid: Uuid::new_v4(),
                domain_id: Uuid::new_v4(),
            }],
        };
        let pull = SyncMessage::Pull { since_rev: 7 };
        let snapshot = SyncMessage::Snapshot {
            rev: 9,
            offers: vec![offer("photo.png")],
        };

        for message in [&delta, &pull, &snapshot] {
            let json = serde_json::to_string(message).unwrap();
            let back: SyncMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, message);
        }

        // 行协议可读性锚点：tag 字段名与 snake_case 值锁定，改了就是破协议。
        assert!(serde_json::to_string(&delta)
            .unwrap()
            .contains(r#""kind":"delta""#));
        assert!(serde_json::to_string(&pull)
            .unwrap()
            .contains(r#""kind":"pull""#));
        assert!(serde_json::to_string(&snapshot)
            .unwrap()
            .contains(r#""kind":"snapshot""#));
    }

    #[test]
    fn delta_validation() {
        SyncMessage::Delta {
            from_rev: 3,
            to_rev: 5,
            added: vec![],
            removed: vec![RevokedEntry {
                asset_uuid: Uuid::new_v4(),
                domain_id: Uuid::new_v4(),
            }],
        }
        .validate()
        .unwrap();

        // 区间倒挂。
        let reversed = SyncMessage::Delta {
            from_rev: 5,
            to_rev: 3,
            added: vec![],
            removed: vec![],
        };
        assert_eq!(
            reversed.validate(),
            Err(SyncError::BadRevOrder {
                from_rev: 5,
                to_rev: 3
            })
        );

        // 空区间（to == from）即使带变更也拒：契约是 (from, to] 非空。
        let empty_range = SyncMessage::Delta {
            from_rev: 4,
            to_rev: 4,
            added: vec![SharingEntry {
                asset_uuid: Uuid::new_v4(),
                domain_id: Uuid::new_v4(),
                since: 1,
            }],
            removed: vec![],
        };
        assert!(matches!(
            empty_range.validate(),
            Err(SyncError::BadRevOrder { .. })
        ));

        // 有区间无变更。
        let no_changes = SyncMessage::Delta {
            from_rev: 4,
            to_rev: 5,
            added: vec![],
            removed: vec![],
        };
        assert_eq!(no_changes.validate(), Err(SyncError::EmptyDelta));
    }

    #[test]
    fn snapshot_validation() {
        SyncMessage::Snapshot {
            rev: 0,
            offers: vec![],
        }
        .validate()
        .unwrap();

        let bad = SyncMessage::Snapshot {
            rev: 1,
            offers: vec![offer("ok.png"), offer("..\\evil.png")],
        };
        assert_eq!(bad.validate(), Err(SyncError::BadFileName { index: 1 }));
    }

    #[test]
    fn pull_is_always_valid() {
        SyncMessage::Pull { since_rev: 0 }.validate().unwrap();
        SyncMessage::Pull {
            since_rev: u64::MAX,
        }
        .validate()
        .unwrap();
    }
}
