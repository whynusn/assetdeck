//! 共享空间同步协议（D90-P2）：抗熵交换、字节索取与贴码加入的纯模型。
//!
//! 载体 = 既有 QUIC 同步通道（[`crate::request::ALPN_SYNC`]，单 bi 流 + 行
//! JSON），空间报文作为 [`crate::sync::SyncMessage::Space`] 变体复用同一
//! 解析面（M1 的域模型报文保留但 D90 后不再产生）。
//!
//! 协议形态（一问一答，双方各发起一次即双向收敛）：
//! - [`SpaceMessage::Summary`]：我在这 spaces 里、手上有的 op 键全集
//!   （断档全量比对——v1 不做区间压缩，空间 op 量级远低于阈值）；
//! - [`SpaceMessage::Ops`]：应答方按差量补齐请求方缺失的 op（跨空间合并，
//!   截断到 [`MAX_OPS_PER_MESSAGE`]；截断无害——下一轮 Summary 自愈）；
//! - [`SpaceMessage::Fetch`]：向持有者索取指定 content_hash 的字节。持有者
//!   随后走**推送通道**投递（引擎的清单/哈希校验/暂存原样复用），索取方
//!   对 space 清单免确认（入空间 = 同意接收，D90 的确认移位）；
//! - [`SpaceMessage::Join`] / [`JoinOk`] / [`JoinErr`]：贴码加入（新成员 →
//!   空间主）。加入者身份取 QUIC 握手（同 M1-b 铁律：报文不自报身份）。
//!
//! 可用性语义：同步只在「任意两成员连接」时发生，字节可向任一副本持有者
//! 拉——不要求素材添加者在线。全员离线 = 无同步，各副本独立演进，下次
//! 连接由 Summary 全量比对收敛（零 Timer 轮询，断档自愈沿用 M1-b 结论）。
//!
//! [`SpaceBookState`] 是 UI 推送给 worker 的服务册（worker 应答 Summary/
//! Join、执行 Fetch 的唯一数据源，同 M1-b 的 SyncBook 整册替换模式）。
//! 册里没有的空间 = 不应答不泄漏（D90：连库的存在性都不出线）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::manifest::is_safe_display_name;
use crate::request::{is_device_id, is_safe_label};
use crate::space::{SpaceAssetMeta, SpaceError, SpaceMeta, SpaceOp};

/// 单条抗熵摘要最多覆盖的空间数（防御性上限；本地册通常 ≤ 个位数）。
pub const MAX_SUMMARY_SPACES: usize = 32;
/// 单个空间的摘要键上限（op 超过此数的空间在 v1 属异常形态，拒收防滥用）。
pub const MAX_SUMMARY_KEYS: usize = 4096;
/// 单条 Ops 应答的 op 数上限（截断无害：下一轮 Summary 自愈续传）。
pub const MAX_OPS_PER_MESSAGE: usize = 256;
/// 单条 Fetch 的素材数上限（UI 侧分批索取）。
pub const MAX_FETCH_ITEMS: usize = 64;

/// op 全序键 (lamport, author)——抗熵摘要的最小单元。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpKey {
    pub lamport: u64,
    pub author: String,
}

impl OpKey {
    pub fn of(op: &SpaceOp) -> Self {
        Self {
            lamport: op.lamport,
            author: op.author.clone(),
        }
    }
}

/// Summary 的一行：我在该空间持有的 op 键全集（canonical 序）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceSummaryEntry {
    pub space_id: Uuid,
    pub keys: Vec<OpKey>,
}

/// 空间同步报文（`SyncMessage::Space` 的载荷；serde tag 改名即破协议）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpaceMessage {
    /// 抗熵请求：发起方持有的 op 键全集（应答 = Ops 差量）。
    Summary { entries: Vec<SpaceSummaryEntry> },
    /// Summary 应答：应答方补齐请求方缺失的 op（截断见 [`MAX_OPS_PER_MESSAGE`]）。
    Ops { ops: Vec<SpaceOp> },
    /// 字节索取：向持有者要这些文件（持有者经推送通道投递，身份 QUIC 背书）。
    Fetch {
        space_id: Uuid,
        items: Vec<SpaceAssetMeta>,
    },
    /// 贴码加入（新成员 → 空间主）：加入者身份 = QUIC remote_id，不自报。
    Join { code: String, joiner_name: String },
    /// Join 应答：空间元数据（名册已含新成员）+ 全量 op log（冷启动一次拉齐）。
    JoinOk { meta: SpaceMeta, ops: Vec<SpaceOp> },
    /// Join 拒绝（码过期/空间不存在/非空间主受理等，给用户看的短语）。
    JoinErr { reason: String },
}

/// worker 服务册的一行：空间的元数据 + 本地全量 op log + 字节服务索引。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceBookEntry {
    pub meta: SpaceMeta,
    pub ops: Vec<SpaceOp>,
    /// content_hash(32B) → 本地文件绝对路径（空间库 objects 下的落位文件）。
    pub serve: Vec<ServeItem>,
}

/// 字节服务索引的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServeItem {
    pub content_hash: Vec<u8>,
    pub path: String,
}

/// UI → worker 整册（SPACE_STATE 命令载荷；worker 只读，应答唯一数据源）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceBookState {
    pub spaces: BTreeMap<Uuid, SpaceBookEntry>,
}

impl SpaceBookState {
    pub fn set(&mut self, id: Uuid, entry: SpaceBookEntry) {
        self.spaces.insert(id, entry);
    }

    pub fn validate(&self) -> Result<(), SpaceSyncError> {
        for entry in self.spaces.values() {
            entry.meta.validate().map_err(SpaceSyncError::BadMeta)?;
            validate_ops(&entry.ops)?;
            for item in &entry.serve {
                if item.content_hash.len() != 32 || !is_serve_path(&item.path) {
                    return Err(SpaceSyncError::BadServePath);
                }
            }
        }
        Ok(())
    }

    /// Summary 差量：请求方缺的 op（本册范围内；不在我册里的空间静默跳过
    /// ——不泄漏存在性）。确定性序 = (space_id, lamport, author)，截断取前
    /// [`MAX_OPS_PER_MESSAGE`] 条，重复发起自愈续传。
    pub fn diff_against(&self, entries: &[SpaceSummaryEntry]) -> Vec<SpaceOp> {
        let mut missing: Vec<SpaceOp> = Vec::new();
        for entry in entries {
            let Some(mine) = self.spaces.get(&entry.space_id) else {
                continue;
            };
            for op in &mine.ops {
                let have = entry.keys.contains(&OpKey::of(op));
                if !have {
                    missing.push(op.clone());
                }
            }
        }
        missing.sort_by(|a, b| {
            a.space_id
                .cmp(&b.space_id)
                .then(a.lamport.cmp(&b.lamport))
                .then(a.author.cmp(&b.author))
        });
        missing.dedup_by(|a, b| {
            a.space_id == b.space_id && a.lamport == b.lamport && a.author == b.author
        });
        missing.truncate(MAX_OPS_PER_MESSAGE);
        missing
    }

    /// Fetch 的本地落位查询：hash → 服务路径。
    pub fn serve_path(&self, space_id: Uuid, content_hash: &[u8]) -> Option<&str> {
        let entry = self.spaces.get(&space_id)?;
        entry
            .serve
            .iter()
            .find(|item| item.content_hash == content_hash)
            .map(|item| item.path.as_str())
    }
}

/// 贴码加入受理（空间主侧）：验码入册 + 追加 Roster op。
///
/// 返回 true = 新入册（名册已扩、Roster op 已追加）；false = 幂等重入
///（已在册，无变更）。码的 secret 段与当前世代比对——旋转后旧码即刻失效。
pub fn admit_join(
    entry: &mut SpaceBookEntry,
    code: &str,
    joiner_id: &str,
    joiner_name: &str,
    now: i64,
) -> Result<bool, SpaceJoinError> {
    let parsed = crate::space::parse_space_invite_code(code).map_err(SpaceJoinError::BadCode)?;
    if !is_device_id(joiner_id) {
        return Err(SpaceJoinError::BadJoiner);
    }
    if !is_safe_display_name(joiner_name) {
        return Err(SpaceJoinError::BadJoiner);
    }
    if parsed.owner_id != entry.meta.owner_id || parsed.space_id != entry.meta.id {
        // 码指向别的空间：不泄漏我册里有没有这个空间。
        return Err(SpaceJoinError::NoSuchSpace);
    }
    if parsed.secret != entry.meta.invite_secret {
        return Err(SpaceJoinError::StaleSecret);
    }
    if entry.meta.is_member(joiner_id) {
        return Ok(false);
    }
    entry.meta.members.push(crate::invite::RosterMember {
        id: joiner_id.to_string(),
        name: joiner_name.to_string(),
    });
    entry.meta.validate().map_err(SpaceJoinError::BadMeta)?;
    let lamport = entry.ops.iter().map(|op| op.lamport).max().unwrap_or(0) + 1;
    let roster = SpaceOp {
        space_id: entry.meta.id,
        lamport,
        author: entry.meta.owner_id.clone(),
        ts: now,
        op: crate::space::SpaceOpKind::Roster {
            space: entry.meta.clone(),
        },
    };
    roster.validate().map_err(SpaceJoinError::BadMeta)?;
    entry.ops.push(roster);
    Ok(true)
}

/// 加入侧落册：JoinOk/Roster 的 meta 进本机清单。重入/名册更新 = 原位
/// 覆盖但保留本地首次加入时间（joined_at = 我何时加入，不是名册几时刷新）。
pub fn apply_join(
    registry: &mut crate::space::SpaceRegistry,
    meta: SpaceMeta,
    now: i64,
) -> Result<(), SpaceError> {
    meta.validate()?;
    let joined = SpaceMeta {
        joined_at: now,
        ..meta
    };
    match registry.find_mut(joined.id) {
        Some(slot) => {
            let mut next = joined;
            next.joined_at = slot.joined_at;
            *slot = next;
        }
        None => registry.spaces.push(joined),
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceSyncError {
    /// 摘要超上限（空间数 / 键数）。
    SummaryTooLarge,
    /// Ops/Fetch/JoinOk 载荷超上限或含非法 op。
    BadOps,
    BadFetchItems,
    BadJoinShape,
    BadMeta(SpaceError),
    /// 服务索引的路径不合法（非绝对路径 / 控制字符）。
    BadServePath,
}

impl core::fmt::Display for SpaceSyncError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpaceSyncError::SummaryTooLarge => write!(f, "空间摘要超上限"),
            SpaceSyncError::BadOps => write!(f, "空间 op 载荷不合法"),
            SpaceSyncError::BadFetchItems => write!(f, "字节索取载荷不合法"),
            SpaceSyncError::BadJoinShape => write!(f, "加入报文形状不合法"),
            SpaceSyncError::BadMeta(error) => write!(f, "空间元数据不合法: {error}"),
            SpaceSyncError::BadServePath => write!(f, "服务索引路径不合法"),
        }
    }
}

impl std::error::Error for SpaceSyncError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceJoinError {
    BadCode(SpaceError),
    BadJoiner,
    NoSuchSpace,
    StaleSecret,
    BadMeta(SpaceError),
}

impl core::fmt::Display for SpaceJoinError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpaceJoinError::BadCode(error) => write!(f, "邀请码不合法: {error}"),
            SpaceJoinError::BadJoiner => write!(f, "加入者标识或名称不合法"),
            SpaceJoinError::NoSuchSpace => write!(f, "邀请码与该空间不匹配"),
            SpaceJoinError::StaleSecret => write!(f, "邀请码已失效（空间主可能已重置）"),
            SpaceJoinError::BadMeta(error) => write!(f, "空间元数据不合法: {error}"),
        }
    }
}

impl std::error::Error for SpaceJoinError {}

/// 服务索引路径守卫：绝对路径（盘符或根开头）且不含行协议致命字符。
fn is_serve_path(path: &str) -> bool {
    if path.is_empty() || path.contains(|c: char| c.is_control()) {
        return false;
    }
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || path.starts_with('\\')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'));
    absolute && (path.contains('\\') || path.contains('/'))
}

fn validate_ops(ops: &[SpaceOp]) -> Result<(), SpaceSyncError> {
    if ops.len() > MAX_OPS_PER_MESSAGE {
        return Err(SpaceSyncError::BadOps);
    }
    for op in ops {
        op.validate().map_err(|_| SpaceSyncError::BadOps)?;
    }
    Ok(())
}

impl SpaceMessage {
    /// 结构性守卫（同 [`crate::sync::SyncMessage::validate`] 的职责面）。
    pub fn validate(&self) -> Result<(), SpaceSyncError> {
        match self {
            SpaceMessage::Summary { entries } => {
                if entries.len() > MAX_SUMMARY_SPACES {
                    return Err(SpaceSyncError::SummaryTooLarge);
                }
                for entry in entries {
                    if entry.keys.len() > MAX_SUMMARY_KEYS {
                        return Err(SpaceSyncError::SummaryTooLarge);
                    }
                    if entry.keys.iter().any(|key| !is_device_id(&key.author)) {
                        return Err(SpaceSyncError::SummaryTooLarge);
                    }
                }
                Ok(())
            }
            SpaceMessage::Ops { ops } => validate_ops(ops),
            SpaceMessage::Fetch { items, .. } => {
                if items.len() > MAX_FETCH_ITEMS || items.is_empty() {
                    return Err(SpaceSyncError::BadFetchItems);
                }
                for item in items {
                    item.validate().map_err(|_| SpaceSyncError::BadFetchItems)?;
                }
                Ok(())
            }
            SpaceMessage::Join { code, joiner_name } => {
                if crate::space::parse_space_invite_code(code).is_err()
                    || !is_safe_label(joiner_name)
                {
                    Err(SpaceSyncError::BadJoinShape)
                } else {
                    Ok(())
                }
            }
            SpaceMessage::JoinOk { meta, ops } => {
                meta.validate().map_err(SpaceSyncError::BadMeta)?;
                validate_ops(ops)
            }
            SpaceMessage::JoinErr { reason } => {
                if reason.is_empty()
                    || reason.len() > 120
                    || reason.contains(|c: char| c.is_control())
                {
                    Err(SpaceSyncError::BadJoinShape)
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invite::RosterMember;
    use crate::space::{SpaceOpKind, SpaceRegistry};

    fn device_id(seed: u8) -> String {
        (0..52)
            .map(|i| crate::pairing::ALPHABET[(seed as usize + i) % 36] as char)
            .collect()
    }

    fn owner_entry() -> SpaceBookEntry {
        let mut registry = SpaceRegistry::new();
        registry.self_id = Some(device_id(1));
        registry.self_name = "店长".into();
        let meta = registry.create_space("团队素材", 1000).unwrap();
        SpaceBookEntry {
            meta,
            ops: Vec::new(),
            serve: Vec::new(),
        }
    }

    fn add_op(space_id: Uuid, lamport: u64, author: &str) -> SpaceOp {
        SpaceOp {
            space_id,
            lamport,
            author: author.to_string(),
            ts: 0,
            op: SpaceOpKind::Add {
                asset: SpaceAssetMeta {
                    uuid: Uuid::new_v4(),
                    file_name: "a.png".into(),
                    kind: crate::space::SpaceAssetKind::Image,
                    size_bytes: 10,
                    content_hash: vec![0u8; 32],
                },
            },
        }
    }

    #[test]
    fn serde_roundtrip_with_kind_tags() {
        let entry = owner_entry();
        let sid = entry.meta.id;
        let summary = SpaceMessage::Summary {
            entries: vec![SpaceSummaryEntry {
                space_id: sid,
                keys: vec![OpKey {
                    lamport: 3,
                    author: device_id(1),
                }],
            }],
        };
        let ops = SpaceMessage::Ops {
            ops: vec![add_op(sid, 1, &device_id(1))],
        };
        let fetch = SpaceMessage::Fetch {
            space_id: sid,
            items: vec![SpaceAssetMeta {
                uuid: Uuid::new_v4(),
                file_name: "a.png".into(),
                kind: crate::space::SpaceAssetKind::Image,
                size_bytes: 8,
                content_hash: vec![1u8; 32],
            }],
        };
        let join = SpaceMessage::Join {
            code: crate::space::space_invite_code(&entry.meta),
            joiner_name: "小李的电脑".into(),
        };
        let join_ok = SpaceMessage::JoinOk {
            meta: entry.meta.clone(),
            ops: Vec::new(),
        };
        let join_err = SpaceMessage::JoinErr {
            reason: "邀请码已失效".into(),
        };
        for message in [&summary, &ops, &fetch, &join, &join_ok, &join_err] {
            let json = serde_json::to_string(message).unwrap();
            let back: SpaceMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, message);
        }
        assert!(serde_json::to_string(&summary)
            .unwrap()
            .contains(r#""kind":"summary""#));
        assert!(serde_json::to_string(&ops)
            .unwrap()
            .contains(r#""kind":"ops""#));
        assert!(serde_json::to_string(&fetch)
            .unwrap()
            .contains(r#""kind":"fetch""#));
        assert!(serde_json::to_string(&join)
            .unwrap()
            .contains(r#""kind":"join""#));
        assert!(serde_json::to_string(&join_ok)
            .unwrap()
            .contains(r#""kind":"join_ok""#));
        assert!(serde_json::to_string(&join_err)
            .unwrap()
            .contains(r#""kind":"join_err""#));
    }

    #[test]
    fn summary_validation_enforces_caps_and_shapes() {
        let entry = owner_entry();
        let ok = SpaceMessage::Summary {
            entries: vec![SpaceSummaryEntry {
                space_id: entry.meta.id,
                keys: Vec::new(),
            }],
        };
        ok.validate().unwrap();

        // 键数超上限 / 作者 id 坏形状 / 空间数超上限。
        let too_many_keys = SpaceMessage::Summary {
            entries: vec![SpaceSummaryEntry {
                space_id: entry.meta.id,
                keys: vec![
                    OpKey {
                        lamport: 0,
                        author: device_id(1),
                    };
                    MAX_SUMMARY_KEYS + 1
                ],
            }],
        };
        assert_eq!(
            too_many_keys.validate(),
            Err(SpaceSyncError::SummaryTooLarge)
        );
        let bad_author = SpaceMessage::Summary {
            entries: vec![SpaceSummaryEntry {
                space_id: entry.meta.id,
                keys: vec![OpKey {
                    lamport: 0,
                    author: "short".into(),
                }],
            }],
        };
        assert_eq!(bad_author.validate(), Err(SpaceSyncError::SummaryTooLarge));
        let too_many_spaces = SpaceMessage::Summary {
            entries: vec![
                SpaceSummaryEntry {
                    space_id: Uuid::new_v4(),
                    keys: Vec::new(),
                };
                MAX_SUMMARY_SPACES + 1
            ],
        };
        assert_eq!(
            too_many_spaces.validate(),
            Err(SpaceSyncError::SummaryTooLarge)
        );
    }

    #[test]
    fn diff_against_returns_missing_in_canonical_order() {
        let mut book = SpaceBookState::default();
        let mut entry = owner_entry();
        let sid = entry.meta.id;
        let op1 = add_op(sid, 1, &device_id(1));
        let op2 = add_op(sid, 2, &device_id(2));
        let op3 = add_op(sid, 3, &device_id(1));
        entry.ops = vec![op3.clone(), op1.clone(), op2.clone()];
        let id = entry.meta.id;
        book.set(id, entry);

        // 请求方持有 op3 → 补 op1/op2（canonical 序 = lamport 升序）。
        let diff = book.diff_against(&[SpaceSummaryEntry {
            space_id: id,
            keys: vec![OpKey::of(&op3)],
        }]);
        assert_eq!(diff, vec![op1, op2]);

        // 请求方持有全部 → 空。
        let keys = vec![
            OpKey {
                lamport: 1,
                author: device_id(1),
            },
            OpKey {
                lamport: 2,
                author: device_id(2),
            },
            OpKey {
                lamport: 3,
                author: device_id(1),
            },
        ];
        assert!(book
            .diff_against(&[SpaceSummaryEntry { space_id: id, keys }])
            .is_empty());

        // 不在我册里的空间：静默跳过（不泄漏存在性）。
        assert!(book
            .diff_against(&[SpaceSummaryEntry {
                space_id: Uuid::new_v4(),
                keys: Vec::new(),
            }])
            .is_empty());
    }

    #[test]
    fn diff_against_truncates_at_cap() {
        let mut book = SpaceBookState::default();
        let mut entry = owner_entry();
        let sid = entry.meta.id;
        entry.ops = (1..=MAX_OPS_PER_MESSAGE as u64 + 10)
            .map(|lamport| add_op(sid, lamport, &device_id(1)))
            .collect::<Vec<_>>();
        let id = entry.meta.id;
        book.set(id, entry);
        let diff = book.diff_against(&[SpaceSummaryEntry {
            space_id: id,
            keys: Vec::new(),
        }]);
        assert_eq!(diff.len(), MAX_OPS_PER_MESSAGE);
        // 截断取键序最小的前 N 条。
        assert_eq!(diff.first().unwrap().lamport, 1);
    }

    #[test]
    fn fetch_and_join_payload_validation() {
        let entry = owner_entry();
        let item = |name: &str| SpaceAssetMeta {
            uuid: Uuid::new_v4(),
            file_name: name.into(),
            kind: crate::space::SpaceAssetKind::Text,
            size_bytes: 8,
            content_hash: vec![2u8; 32],
        };
        SpaceMessage::Fetch {
            space_id: entry.meta.id,
            items: vec![item("a.txt")],
        }
        .validate()
        .unwrap();
        // 空 / 超上限 / 坏文件名 / 坏摘要。
        assert_eq!(
            SpaceMessage::Fetch {
                space_id: entry.meta.id,
                items: Vec::new(),
            }
            .validate(),
            Err(SpaceSyncError::BadFetchItems)
        );
        assert_eq!(
            SpaceMessage::Fetch {
                space_id: entry.meta.id,
                items: vec![item("ok.txt"); MAX_FETCH_ITEMS + 1],
            }
            .validate(),
            Err(SpaceSyncError::BadFetchItems)
        );
        assert_eq!(
            SpaceMessage::Fetch {
                space_id: entry.meta.id,
                items: vec![item("..\\evil.txt")],
            }
            .validate(),
            Err(SpaceSyncError::BadFetchItems)
        );

        // Join：好码好名通过；坏码 / 坏名拒。
        SpaceMessage::Join {
            code: crate::space::space_invite_code(&entry.meta),
            joiner_name: "小李".into(),
        }
        .validate()
        .unwrap();
        assert_eq!(
            SpaceMessage::Join {
                code: "junk".into(),
                joiner_name: "小李".into(),
            }
            .validate(),
            Err(SpaceSyncError::BadJoinShape)
        );
        assert_eq!(
            SpaceMessage::Join {
                code: crate::space::space_invite_code(&entry.meta),
                joiner_name: "bad\tname".into(),
            }
            .validate(),
            Err(SpaceSyncError::BadJoinShape)
        );

        // JoinErr 短语边界。
        SpaceMessage::JoinErr {
            reason: "邀请码已失效".into(),
        }
        .validate()
        .unwrap();
        assert_eq!(
            SpaceMessage::JoinErr {
                reason: String::new(),
            }
            .validate(),
            Err(SpaceSyncError::BadJoinShape)
        );
        assert_eq!(
            SpaceMessage::JoinErr {
                reason: "x".repeat(121),
            }
            .validate(),
            Err(SpaceSyncError::BadJoinShape)
        );
    }

    #[test]
    fn admit_join_happy_path_appends_roster_op() {
        let mut entry = owner_entry();
        entry.ops.push(add_op(entry.meta.id, 1, &device_id(1)));
        let code = crate::space::space_invite_code(&entry.meta);
        let joiner = device_id(7);

        let admitted = admit_join(&mut entry, &code, &joiner, "客服小美", 2000).unwrap();
        assert!(admitted);
        assert_eq!(entry.meta.members.len(), 2);
        assert!(entry.meta.is_member(&joiner));
        // Roster op：lamport = max+1，作者 = 空间主，名册为最新 meta。
        let roster = entry.ops.last().unwrap();
        assert_eq!(roster.lamport, 2);
        assert_eq!(roster.author, device_id(1));
        match &roster.op {
            SpaceOpKind::Roster { space } => {
                assert_eq!(space.members.len(), 2);
                assert!(space.is_member(&joiner));
            }
            other => panic!("期望 Roster op，得到 {other:?}"),
        }

        // 幂等重入：已在册 → false，无新 op。
        let again = admit_join(&mut entry, &code, &joiner, "客服小美", 3000).unwrap();
        assert!(!again);
        assert_eq!(entry.ops.len(), 2);
    }

    #[test]
    fn admit_join_rejects_stale_and_foreign_codes() {
        let mut entry = owner_entry();
        let code = crate::space::space_invite_code(&entry.meta);
        let joiner = device_id(7);

        // 坏码形状。
        assert!(matches!(
            admit_join(&mut entry, "junk", &joiner, "小美", 1),
            Err(SpaceJoinError::BadCode(_))
        ));
        // 旋转后的旧码 = StaleSecret。
        let old_code = code.clone();
        entry.meta.invite_secret = crate::invite::z32_encode(Uuid::new_v4().as_bytes());
        assert_eq!(
            admit_join(&mut entry, &old_code, &joiner, "小美", 1),
            Err(SpaceJoinError::StaleSecret)
        );
        // 指向别的空间（好形状、secret 对不上）= NoSuchSpace（不泄漏存在性）。
        let mut other = owner_entry();
        other.meta.owner_id = device_id(9);
        other.meta.members = vec![RosterMember {
            id: device_id(9),
            name: "别人".into(),
        }];
        let foreign = crate::space::space_invite_code(&other.meta);
        let mut mine = owner_entry();
        assert_eq!(
            admit_join(&mut mine, &foreign, &joiner, "小美", 1),
            Err(SpaceJoinError::NoSuchSpace)
        );
        // 加入者 id / 名不合法。
        let code = crate::space::space_invite_code(&mine.meta);
        assert_eq!(
            admit_join(&mut mine, &code, "short", "小美", 1),
            Err(SpaceJoinError::BadJoiner)
        );
        assert_eq!(
            admit_join(&mut mine, &code, &joiner, "bad\t名", 1),
            Err(SpaceJoinError::BadJoiner)
        );
    }

    #[test]
    fn apply_join_inserts_and_overrides() {
        let mut registry = SpaceRegistry::new();
        registry.self_id = Some(device_id(7));
        registry.self_name = "客服小美".into();
        let owner_entry = owner_entry();
        let mut meta = owner_entry.meta.clone();
        meta.members.push(RosterMember {
            id: device_id(7),
            name: "客服小美".into(),
        });
        meta.validate().unwrap();

        apply_join(&mut registry, meta.clone(), 5000).unwrap();
        assert_eq!(registry.spaces.len(), 1);
        assert_eq!(registry.spaces[0].joined_at, 5000);

        // 重入/名册更新 = 原位覆盖，joined_at 保留首次加入值。
        apply_join(&mut registry, meta, 6000).unwrap();
        assert_eq!(registry.spaces.len(), 1);
        assert_eq!(registry.spaces[0].joined_at, 5000);
    }

    #[test]
    fn book_validate_and_serve_lookup() {
        let mut book = SpaceBookState::default();
        let entry = owner_entry();
        let id = entry.meta.id;
        book.set(
            id,
            SpaceBookEntry {
                meta: entry.meta.clone(),
                ops: vec![add_op(entry.meta.id, 1, &device_id(1))],
                serve: vec![ServeItem {
                    content_hash: vec![9u8; 32],
                    path: r"C:\spaces\x\objects\ab\file.png".into(),
                }],
            },
        );
        book.validate().unwrap();
        assert_eq!(
            book.serve_path(id, &[9u8; 32]),
            Some(r"C:\spaces\x\objects\ab\file.png")
        );
        assert_eq!(book.serve_path(id, &[8u8; 32]), None);
        assert_eq!(book.serve_path(Uuid::new_v4(), &[9u8; 32]), None);

        // 坏服务路径（相对路径）与坏摘要 → 拒。
        let mut bad = book.clone();
        if let Some(entry) = bad.spaces.get_mut(&id) {
            entry.serve[0].path = r"relative\file.png".into();
        }
        assert_eq!(bad.validate(), Err(SpaceSyncError::BadServePath));
        let mut bad_hash = book;
        if let Some(entry) = bad_hash.spaces.get_mut(&id) {
            entry.serve[0].content_hash = vec![9u8; 16];
        }
        assert_eq!(bad_hash.validate(), Err(SpaceSyncError::BadServePath));
    }
}
