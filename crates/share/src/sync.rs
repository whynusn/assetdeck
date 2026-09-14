//! 同步消息契约（D80-M1）：push/pull 混合同步的三种报文 + 被拉方的
//! 「我共享给谁什么」状态册。
//!
//! 载体 = M1-b 落地的独立 QUIC 同步通道（ALPN [`crate::request::ALPN_SYNC`]，
//! 单 bi 流 + `\n` 行 JSON），与推送通道互不干扰。铁律：
//! - 任何报文都**不含域定义**（成员册不出本机）；接收方视角只有
//!   [`ShareOffer`]（谁有哪些素材可得），连域 id 都不上线——Delta/Snapshot
//!   都是发送方**按接收方预先过滤**的视角报文，过滤逻辑留在本机。
//! - [`SyncMessage::Delta`] 表达区间 (from_rev, to_rev] 内**接收方视角**的
//!   变更（added = 新增可得、removed = 不再可得，按 asset_uuid 计），由
//!   在线推送通道投递；[`SyncMessage::Pull`] 是上线方的拉取请求
//!   （since_rev = 本地缓存 rev，0 = 全量）；应答只有
//!   [`SyncMessage::Snapshot`]——已按拉取方过滤的可得清单 + 服务端 rev。
//! - 报文乱序/漏收的防御（rev 为发送方全局单调计数，逐动作 +1）：
//!   Delta 的 to_rev ≤ 本地缓存 rev 是重放，直接丢弃；from_rev > 本地
//!   缓存 rev 是断档（漏收过推送），同样丢弃并触发补拉——只有
//!   from_rev == 本地缓存 rev 的区间才原位套用。Push 丢失由补拉自愈，
//!   冷启动由 Pull 全量覆盖。
//!
//! 服务端应答 PULL 的数据来自 [`SyncBookState`]：UI 主进程在共享态每次
//! 变化后整册推送，worker 只读替换、逐请求按拉取方取视角（册里没有的
//! 对端 = 零可得，fail-closed）。

use std::collections::BTreeMap;

use domain::AssetKind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::manifest::is_safe_display_name;
use crate::request::is_device_id;

/// 接收方视角的一行「可得」：只含元数据，不含域、不含路径——对象路径
/// 只在后续显式推送的清单（manifest）里出现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareOffer {
    pub asset_uuid: Uuid,
    pub file_name: String,
    pub kind: AssetKind,
    pub size_bytes: u64,
}

/// 被拉方给单个对端备好的视角快照：自己的 rev + 该对端可见的可得清单。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerSyncState {
    pub rev: u64,
    pub offers: Vec<ShareOffer>,
}

/// 整册：键 = 对端设备标识（z32）。序列化即落线形态（SYNC_STATE 命令载荷
/// 与 PULL 应答查表共用同一形状，改字段就是破协议）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBookState {
    pub peers: BTreeMap<String, PeerSyncState>,
}

impl SyncBookState {
    pub fn set(&mut self, peer_id: impl Into<String>, state: PeerSyncState) {
        self.peers.insert(peer_id.into(), state);
    }

    pub fn get(&self, peer_id: &str) -> Option<&PeerSyncState> {
        self.peers.get(peer_id)
    }

    pub fn validate(&self) -> Result<(), SyncError> {
        for (peer, state) in &self.peers {
            if !is_device_id(peer) {
                return Err(SyncError::BadPeerId(peer.clone()));
            }
            check_offers(&state.offers)?;
        }
        Ok(())
    }
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
    /// 册/报文里的对端设备标识不是合法 z32 形状。
    BadPeerId(String),
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
            SyncError::BadPeerId(id) => write!(f, "对端设备标识不合法：{id}"),
        }
    }
}

impl std::error::Error for SyncError {}

fn check_offers(offers: &[ShareOffer]) -> Result<(), SyncError> {
    for (index, offer) in offers.iter().enumerate() {
        if !is_safe_display_name(&offer.file_name) {
            return Err(SyncError::BadFileName { index });
        }
    }
    Ok(())
}

impl SyncMessage {
    /// 结构性守卫：Delta 区间必须非空且携带变更（含新增项文件名检查）；
    /// Snapshot 的可得项文件名复用清单同名规则；册复用同一规则。Pull 恒合法。
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
                check_offers(added)
            }
            SyncMessage::Pull { .. } => Ok(()),
            SyncMessage::Snapshot { offers, .. } => check_offers(offers),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncMessage {
    /// 在线增量：区间 (from_rev, to_rev] 内**接收方视角**的新增与撤销。
    Delta {
        from_rev: u64,
        to_rev: u64,
        added: Vec<ShareOffer>,
        removed: Vec<Uuid>,
    },
    /// 上线拉取请求：since_rev = 拉取方本地缓存的最后 rev（0 = 冷启动全量）。
    /// 服务端恒以全量 Snapshot 应答（不做增量续传：册里没有历史）。
    Pull { since_rev: u64 },
    /// 快照应答：服务端当前 rev + 已按拉取方过滤的可得清单（可为空）。
    Snapshot { rev: u64, offers: Vec<ShareOffer> },
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
            added: vec![offer("photo.png")],
            removed: vec![Uuid::new_v4()],
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
            removed: vec![Uuid::new_v4()],
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
            added: vec![offer("a.png")],
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

        // 新增项文件名不安全同样拒绝（与 Snapshot 同规则）。
        let bad_name = SyncMessage::Delta {
            from_rev: 4,
            to_rev: 5,
            added: vec![offer("ok.png"), offer("..\\evil.png")],
            removed: vec![],
        };
        assert_eq!(
            bad_name.validate(),
            Err(SyncError::BadFileName { index: 1 })
        );
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

    #[test]
    fn book_state_roundtrip_and_validate() {
        let mut book = SyncBookState::default();
        let peer = "a".repeat(52);
        book.set(
            peer.clone(),
            PeerSyncState {
                rev: 4,
                offers: vec![offer("b.png")],
            },
        );
        let json = serde_json::to_string(&book).unwrap();
        assert!(json.contains(r#""rev":4"#));
        let back: SyncBookState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, book);
        assert_eq!(back.get(&peer).unwrap().rev, 4);
        assert!(back.get(&"b".repeat(52)).is_none());
        book.validate().unwrap();

        // 键不是 z32 设备标识 → 拒。
        book.set("short-id", PeerSyncState::default());
        assert!(matches!(
            book.validate(),
            Err(SyncError::BadPeerId(id) ) if id == "short-id"
        ));

        // 可得项文件名不安全 → 拒。
        let mut book = SyncBookState::default();
        book.set(
            peer,
            PeerSyncState {
                rev: 1,
                offers: vec![offer("bad/../name.png")],
            },
        );
        assert!(matches!(
            book.validate(),
            Err(SyncError::BadFileName { index: 0 })
        ));
    }
}
