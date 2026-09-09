//! 共享态（D80-M1）：资源级「共享中」事实表，push/pull 混合同步的事实来源。
//!
//! 不变式（DECISIONS.md D80-M1 定稿）：共享态只广播**可获得性**，永远不
//! 自动投递——接收方据 Snapshot/Pull 结果看到的只是「对端有哪些素材可得」，
//! 拿到文件仍需走 D80 显式推送 + 双确认。
//!
//! - `rev` 单调递增，每次真实变更 +1（幂等 mark/unmark 不动 rev）；
//!   在线推送的 [`crate::sync::SyncMessage::Delta`] 以 (from_rev, to_rev]
//!   表达区间，上线拉取以 since_rev 表达断点。
//! - [`SharedState::digest`] 只绑定内容不绑定 rev：接收方拉完快照后比对
//!   摘要即可发现两端漂移（防丢包/防乱序的最终一致哨兵）。
//! - 可见性按域成员册过滤：[`SharedState::visible_to`] 是接收方视角的
//!   唯一投影函数，域定义本身不出现在任何同步载荷里。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::ShareDomain;

/// 一条共享事实：某素材在某域内处于共享态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingEntry {
    pub asset_uuid: Uuid,
    pub domain_id: Uuid,
    /// 进入共享态的 Unix 秒（展示用；排序与判定不依赖它）。
    pub since: i64,
}

/// 共享态事实表 + 单调 rev。序列化形态即 M1-a 批2 的持久化形态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedState {
    entries: Vec<SharingEntry>,
    rev: u64,
}

impl SharedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前修订号：0 = 从未共享过任何东西。
    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn entries(&self) -> &[SharingEntry] {
        &self.entries
    }

    /// 标记共享（幂等）：新增返回 true 且 rev+1；已存在同 (asset, domain)
    /// 事实则 no-op 返回 false——重复开关 UI 不会制造虚假增量。
    pub fn mark(&mut self, asset_uuid: Uuid, domain_id: Uuid, now: i64) -> bool {
        if self
            .entries
            .iter()
            .any(|e| e.asset_uuid == asset_uuid && e.domain_id == domain_id)
        {
            return false;
        }
        self.entries.push(SharingEntry {
            asset_uuid,
            domain_id,
            since: now,
        });
        self.rev += 1;
        true
    }

    /// 撤销单条共享事实；无此事实时不动 rev（撤销也要幂等）。
    pub fn revoke(&mut self, asset_uuid: Uuid, domain_id: Uuid) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|e| !(e.asset_uuid == asset_uuid && e.domain_id == domain_id));
        if self.entries.len() != before {
            self.rev += 1;
            true
        } else {
            false
        }
    }

    /// 撤销某素材在全部域的共享（素材删除/移动入回收站时调用）；
    /// 返回实际撤销的条数。
    pub fn revoke_asset(&mut self, asset_uuid: Uuid) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.asset_uuid != asset_uuid);
        let removed = before - self.entries.len();
        if removed > 0 {
            self.rev += 1;
        }
        removed
    }

    /// 接收方视角投影：该设备经域成员册过滤后可见的素材集合。
    /// 去重升序——同一素材经多个域可达时只出现一次，顺序稳定可比较。
    pub fn visible_to(&self, device_id: &str, domains: &[ShareDomain]) -> Vec<Uuid> {
        let mut visible: Vec<Uuid> = self
            .entries
            .iter()
            .filter(|entry| {
                domains
                    .iter()
                    .any(|d| d.id == entry.domain_id && d.has_member(device_id))
            })
            .map(|entry| entry.asset_uuid)
            .collect();
        visible.sort_unstable();
        visible.dedup();
        visible
    }

    /// 共享态内容摘要（rev 不参与）：对排序后的 (asset, domain) 事实对
    /// 做确定性哈希。用途是收发两端拉取后的漂移比对，不是安全校验和
    /// （SipHash 非密码学）；跨版本算法变更由 M1-b 协议版本字段兜底。
    pub fn digest(&self) -> u64 {
        let mut sorted: Vec<(Uuid, Uuid)> = self
            .entries
            .iter()
            .map(|e| (e.asset_uuid, e.domain_id))
            .collect();
        sorted.sort_unstable();
        let mut hasher = DefaultHasher::new();
        for (asset, domain) in &sorted {
            asset.hash(&mut hasher);
            domain.hash(&mut hasher);
        }
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DomainKind, ShareDomain};

    fn device_id(seed: u8) -> String {
        let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
        (0..52)
            .map(|i| alphabet[(seed as usize + i) % 36] as char)
            .collect()
    }

    fn domain(id: Uuid, members: Vec<String>) -> ShareDomain {
        ShareDomain {
            id,
            name: "域".to_string(),
            kind: if members.len() == 1 {
                DomainKind::Personal
            } else {
                DomainKind::Group
            },
            members,
        }
    }

    #[test]
    fn mark_is_idempotent_and_rev_only_moves_on_change() {
        let mut state = SharedState::new();
        assert_eq!(state.rev(), 0);

        let asset = Uuid::new_v4();
        let dom = Uuid::new_v4();
        assert!(state.mark(asset, dom, 100));
        assert_eq!(state.rev(), 1);

        // 同事实重复标记：no-op，rev 不动（不制造虚假增量）。
        assert!(!state.mark(asset, dom, 200));
        assert_eq!(state.rev(), 1);
        assert_eq!(state.entries()[0].since, 100);

        // 不同域再标：真变更。
        assert!(state.mark(asset, Uuid::new_v4(), 300));
        assert_eq!(state.rev(), 2);
    }

    #[test]
    fn revoke_is_idempotent_too() {
        let mut state = SharedState::new();
        let asset = Uuid::new_v4();
        let dom = Uuid::new_v4();
        state.mark(asset, dom, 1);
        let rev = state.rev();

        assert!(state.revoke(asset, dom));
        assert_eq!(state.rev(), rev + 1);
        assert!(state.entries().is_empty());

        assert!(!state.revoke(asset, dom));
        assert_eq!(state.rev(), rev + 1);
    }

    #[test]
    fn revoke_asset_sweeps_all_domains() {
        let mut state = SharedState::new();
        let asset = Uuid::new_v4();
        state.mark(asset, Uuid::new_v4(), 1);
        state.mark(asset, Uuid::new_v4(), 2);
        state.mark(Uuid::new_v4(), Uuid::new_v4(), 3);
        let rev = state.rev();

        assert_eq!(state.revoke_asset(asset), 2);
        assert_eq!(state.rev(), rev + 1);
        assert_eq!(state.entries().len(), 1);

        assert_eq!(state.revoke_asset(asset), 0);
        assert_eq!(state.rev(), rev + 1);
    }

    #[test]
    fn visible_to_filters_by_domain_membership() {
        let member = device_id(1);
        let outsider = device_id(2);
        let dom_a = Uuid::new_v4();
        let dom_b = Uuid::new_v4();
        let domains = vec![
            domain(dom_a, vec![member.clone()]),
            domain(dom_b, vec![member.clone(), outsider.clone()]),
        ];

        let mut state = SharedState::new();
        let in_a = Uuid::new_v4();
        let in_b = Uuid::new_v4();
        let in_both = Uuid::new_v4();
        let hidden = Uuid::new_v4();
        state.mark(in_a, dom_a, 1);
        state.mark(in_b, dom_b, 1);
        state.mark(in_both, dom_a, 1);
        state.mark(in_both, dom_b, 1);
        state.mark(hidden, Uuid::new_v4(), 1);

        let mut seen = state.visible_to(&member, &domains);
        seen.dedup();
        assert_eq!(seen, state.visible_to(&member, &domains));
        assert_eq!(seen.len(), 3, "成员看到 A、B、双域素材，看不到未共享域的");
        assert!(seen.contains(&in_a) && seen.contains(&in_b) && seen.contains(&in_both));
        assert!(!seen.contains(&hidden));

        // 局外人只在 dom_b 里 → 只见 in_b 与 in_both；in_both 双域可达只算一次。
        // visible_to 按素材 uuid 升序返回（随机 v4），期望侧同样排序后比较。
        let mut expected = vec![in_b, in_both];
        expected.sort_unstable();
        assert_eq!(state.visible_to(&outsider, &domains), expected);

        // 未知设备什么都看不到。
        assert!(state.visible_to(&device_id(9), &domains).is_empty());
    }

    #[test]
    fn revocation_hides_from_visibility() {
        let member = device_id(3);
        let dom = Uuid::new_v4();
        let domains = vec![domain(dom, vec![member.clone()])];
        let mut state = SharedState::new();
        let asset = Uuid::new_v4();
        state.mark(asset, dom, 1);
        assert_eq!(state.visible_to(&member, &domains), vec![asset]);

        state.revoke(asset, dom);
        assert!(state.visible_to(&member, &domains).is_empty());
    }

    #[test]
    fn digest_binds_content_not_order_or_rev() {
        let asset = Uuid::new_v4();
        let dom = Uuid::new_v4();

        let mut first = SharedState::new();
        first.mark(asset, dom, 1);

        let mut second = SharedState::new();
        second.mark(asset, dom, 999);

        assert_eq!(
            first.digest(),
            second.digest(),
            "同事实不同 since/时间 → 同摘要"
        );
        assert_eq!(first.digest(), first.digest(), "摘要纯函数稳定");

        second.mark(Uuid::new_v4(), dom, 1);
        assert_ne!(first.digest(), second.digest(), "新增事实 → 摘要变");

        second.revoke_asset(asset);
        assert_ne!(first.digest(), second.digest(), "撤销后事实集不同 → 摘要变");
    }

    #[test]
    fn shared_state_serde_roundtrip_is_persistence_shape() {
        let mut state = SharedState::new();
        state.mark(Uuid::new_v4(), Uuid::new_v4(), 42);
        state.mark(Uuid::new_v4(), Uuid::new_v4(), 43);

        let json = serde_json::to_string(&state).unwrap();
        let back: SharedState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
        assert_eq!(back.rev(), state.rev());
        assert_eq!(back.digest(), state.digest());
    }
}
