//! 共享空间 VM（D90）：spaces.json 的内存态与用户操作翻译。
//!
//! 职责边界：持有一个 [`share::SpaceRegistry`]（持久化契约在 share crate），
//! 把「创建/退出/旋转邀请码/身份回填」翻译成带校验的状态迁移，维护
//! `dirty` 标记——app-ui 在每个动作后按 [`Self::take_dirty`] 决定是否原子
//! 落盘（事件驱动保存，零定时器，同 ShareControlVm 模式）。
//!
//! P1（单机半场）不碰网络：本 VM 只管本地清单；P2 的 op log 同步状态
//! （已应用 seq / 等待源）在此 VM 上扩展，不另起炉灶。

use share::{SpaceBookState, SpaceMessage};
use share::{SpaceError, SpaceMeta, SpaceOp, SpaceRegistry};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct SpaceVm {
    registry: SpaceRegistry,
    dirty: bool,
}

impl SpaceVm {
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动装载：从磁盘读出的注册表注入（dirty=false，静默态）。
    pub fn from_registry(registry: SpaceRegistry) -> Self {
        Self {
            registry,
            dirty: false,
        }
    }

    pub fn registry(&self) -> &SpaceRegistry {
        &self.registry
    }

    pub fn spaces(&self) -> &[SpaceMeta] {
        &self.registry.spaces
    }

    /// 序列化为落盘文本（壳层只管原子写）。
    pub fn to_json(&self) -> String {
        self.registry.to_json()
    }

    /// 读后即清：返回自上次落盘以来是否变更过（app-ui 据此决定写盘）。
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// worker READY 回填身份（52 位标识 + 展示名）。变化才置 dirty——READY
    /// 每次启动都会来一发，静默重放不得刷脏。
    pub fn set_identity(&mut self, self_id: &str, self_name: &str) -> bool {
        let changed = self.registry.self_id.as_deref() != Some(self_id)
            || self.registry.self_name != self_name;
        if changed {
            self.registry.self_id = Some(self_id.to_string());
            self.registry.self_name = self_name.to_string();
            self.dirty = true;
        }
        changed
    }

    /// 身份是否就绪（创建/加入空间的前置）。
    pub fn is_ready(&self) -> bool {
        self.registry.self_id.is_some()
    }

    /// 创建空间（本机即空间主）。返回新空间 id 供壳层建目录/切视图。
    pub fn create_space(&mut self, name: &str, now: i64) -> Result<Uuid, SpaceError> {
        let space = self.registry.create_space(name, now)?;
        self.dirty = true;
        Ok(space.id)
    }

    /// 退出空间（空间主侧 UI 先引导解散/转让，见 D90）。返回是否真的移除。
    pub fn leave_space(&mut self, id: Uuid) -> bool {
        let left = self.registry.leave_space(id);
        if left {
            self.dirty = true;
        }
        left
    }

    /// 旋转邀请码（空间主动作，拍板③：UI 上只是「重置邀请码」一个按钮，
    /// gen/secret 细节不暴露）。返回新邀请码。
    pub fn rotate_secret(&mut self, id: Uuid) -> Option<String> {
        let rotated = self.registry.rotate_secret(id);
        if rotated.is_some() {
            self.dirty = true;
        }
        rotated.and_then(|_| self.invite_code(id))
    }

    /// 当前邀请码（空间主侧展示/复制；成员侧看到的是最近一次 Roster op 的）。
    pub fn invite_code(&self, id: Uuid) -> Option<String> {
        self.registry.find(id).map(share::space_invite_code)
    }

    /// 空间库根目录名（uuid 连字符形态，文件系统安全）。
    pub fn space_root_name(id: Uuid) -> String {
        id.hyphenated().to_string()
    }

    /// 名册原位覆盖（Roster op / JoinOk 应用）：joined_at 保留本地首次
    /// 加入时间（share::apply_join 语义），未知空间 = 新行插入。
    pub fn apply_join(&mut self, meta: SpaceMeta, now: i64) -> Result<(), SpaceError> {
        share::apply_join(&mut self.registry, meta, now)?;
        self.dirty = true;
        Ok(())
    }

    /// 本机身份（worker READY 回填后可用；创建/加入的前置）。
    pub fn identity(&self) -> Option<(&str, &str)> {
        self.registry
            .self_id
            .as_deref()
            .map(|id| (id, self.registry.self_name.as_str()))
    }

    /// 侧栏空间行的副标签：成员数（P1 口径；P2 追加同步状态）。
    pub fn member_label(space: &SpaceMeta) -> String {
        format!("{} 名成员", space.members.len())
    }
}

// —— D90-P2 行协议/持久化辅助（serde_json 只在本 crate；app-ui 依赖
// 白名单不含 serde 系，一切 JSON 收发经此转字符串）——

/// ops.jsonl 文本 → 排序去重后的 op log（坏行跳过；键 = (lamport, author)，
/// 与 share crate 的全序同源）。只保留属于 `space_id` 的行。
pub fn space_ops_from_lines(text: &str, space_id: Uuid) -> Vec<SpaceOp> {
    let mut ops: Vec<SpaceOp> = text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_end_matches(['\r', ' ']);
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<SpaceOp>(trimmed)
                .ok()
                .filter(|op| op.space_id == space_id && op.validate().is_ok())
        })
        .collect();
    sort_and_dedupe_ops(&mut ops);
    ops
}

/// op log → ops.jsonl 文本（每行一条 JSON；追加与整写共用）。
pub fn space_ops_to_lines(ops: &[SpaceOp]) -> String {
    let mut text = String::new();
    for op in ops {
        if let Ok(json) = serde_json::to_string(op) {
            text.push_str(&json);
            text.push('\n');
        }
    }
    text
}

/// 排序 (lamport, author) + 按键去重（装载与合并共用；接收序无关收敛）。
pub fn sort_and_dedupe_ops(ops: &mut Vec<SpaceOp>) {
    ops.sort_by(|a, b| a.lamport.cmp(&b.lamport).then(a.author.cmp(&b.author)));
    ops.dedup_by(|a, b| a.lamport == b.lamport && a.author == b.author);
}

/// 服务册 → SPACE_STATE 行载荷 JSON。
pub fn space_book_json(book: &SpaceBookState) -> String {
    serde_json::to_string(book).unwrap_or_else(|_| "{}".to_string())
}

/// 空间报文 → SYNC_SEND 行（对端 id + 地址段 + SyncMessage::Space JSON）。
pub fn space_send_line(peer_id: &str, addrs: &[String], message: SpaceMessage) -> Option<String> {
    let message = share::SyncMessage::Space { message };
    let json = serde_json::to_string(&message).ok()?;
    Some(format!("SYNC_SEND\t{peer_id}\t{}\t{json}", addrs.join(";")))
}

/// INCOMING 载荷解析（auto-accept 判定用：是否空间投递 + 目标空间）。
pub fn parse_transfer_manifest(json: &str) -> Option<share::TransferManifest> {
    serde_json::from_str(json).ok()
}

/// STAGED 载荷解析：(batch_id, 暂存路径表)。
pub fn parse_staged_payload(json: &str) -> Option<(Uuid, Vec<std::path::PathBuf>)> {
    serde_json::from_str::<(Uuid, Vec<std::path::PathBuf>)>(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_id(seed: u8) -> String {
        const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        (0..52)
            .map(|i| ALPHABET[(seed as usize + i) % 36] as char)
            .collect()
    }

    #[test]
    fn identity_is_idempotent_after_first_write() {
        let mut vm = SpaceVm::new();
        assert!(!vm.is_ready());
        assert!(vm.set_identity(&device_id(1), "店长的电脑"));
        assert!(vm.take_dirty());
        // 同值重放不置脏。
        assert!(!vm.set_identity(&device_id(1), "店长的电脑"));
        assert!(!vm.take_dirty());
        assert!(vm.is_ready());
    }

    #[test]
    fn create_leave_rotate_roundtrip() {
        let mut vm = SpaceVm::new();
        vm.set_identity(&device_id(1), "店长的电脑");
        let id = vm.create_space("团队素材", 1000).unwrap();
        assert!(vm.take_dirty());

        // 邀请码可解析且旋转后变化。
        let code = vm.invite_code(id).unwrap();
        assert!(share::parse_space_invite_code(&code).is_ok());
        let rotated = vm.rotate_secret(id).unwrap();
        assert_ne!(code, rotated);
        assert!(vm.take_dirty());

        // 退出后清单为空、邀请码取不到。
        assert!(vm.leave_space(id));
        assert!(vm.take_dirty());
        assert!(vm.invite_code(id).is_none());
    }

    #[test]
    fn json_roundtrip_preserves_identity_and_spaces() {
        let mut vm = SpaceVm::new();
        vm.set_identity(&device_id(2), "客服小李");
        vm.create_space("售前组", 1).unwrap();
        let json = vm.to_json();
        let registry = SpaceRegistry::from_json(&json).unwrap();
        let mut restored = SpaceVm::from_registry(registry);
        assert_eq!(restored.spaces().len(), 1);
        assert_eq!(restored.registry().self_name, "客服小李");
        assert!(!restored.take_dirty());
    }

    #[test]
    fn root_name_is_hyphenated_uuid() {
        let id = Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        assert_eq!(
            SpaceVm::space_root_name(id),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
    }

    #[test]
    fn apply_join_preserves_local_joined_at_and_covers_roster() {
        let mut vm = SpaceVm::new();
        vm.set_identity(&device_id(1), "客服小美");
        // 合法 secret 只能源自 create_space（z32 表手拼必错）。
        let mut meta = {
            let mut owner_registry = SpaceRegistry::new();
            owner_registry.self_id = Some(device_id(2));
            owner_registry.self_name = "店长".into();
            owner_registry.create_space("团队素材", 1).unwrap()
        };
        meta.members.push(share::RosterMember {
            id: device_id(1),
            name: "客服小美".into(),
        });
        vm.apply_join(meta.clone(), 5000).unwrap();
        assert!(vm.take_dirty());
        assert_eq!(vm.spaces()[0].joined_at, 5000);
        // Roster 更新（成员变化）原位覆盖，joined_at 保留本地首入值。
        meta.members.pop();
        meta.secret_gen = 2;
        vm.apply_join(meta, 9000).unwrap();
        assert_eq!(vm.spaces().len(), 1);
        assert_eq!(vm.spaces()[0].joined_at, 5000);
        assert_eq!(vm.spaces()[0].secret_gen, 2);
        assert_eq!(vm.spaces()[0].members.len(), 1);
    }

    #[test]
    fn ops_lines_roundtrip_sorts_and_dedupes() {
        let space_id = Uuid::new_v4();
        let author = "a".repeat(52);
        let op = |lamport: u64| SpaceOp {
            space_id,
            lamport,
            author: author.clone(),
            ts: 0,
            op: share::SpaceOpKind::CategoryCreate {
                name: "新品".into(),
            },
        };
        // 乱序 + 重放 + 坏行 + 他人空间的行 → 装载后 = 排序去重后的本空间 log。
        let other = SpaceOp {
            space_id: Uuid::new_v4(),
            lamport: 1,
            author: author.clone(),
            ts: 0,
            op: share::SpaceOpKind::CategoryCreate {
                name: "别人".into(),
            },
        };
        let text = format!(
            "{}\n{}\n{}\nnot-json\n{}\n",
            serde_json::to_string(&op(2)).unwrap(),
            serde_json::to_string(&op(1)).unwrap(),
            serde_json::to_string(&op(1)).unwrap(),
            serde_json::to_string(&other).unwrap(),
        );
        let loaded = space_ops_from_lines(&text, space_id);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].lamport, 1);
        assert_eq!(loaded[1].lamport, 2);

        // 整写 → 再装载闭环。
        let rewritten = space_ops_to_lines(&loaded);
        assert_eq!(space_ops_from_lines(&rewritten, space_id), loaded);
    }

    #[test]
    fn send_line_and_payload_parsers() {
        let peer = "a".repeat(52);
        let fetch = SpaceMessage::Fetch {
            space_id: Uuid::new_v4(),
            items: vec![share::SpaceAssetMeta {
                uuid: Uuid::new_v4(),
                file_name: "a.png".into(),
                kind: share::SpaceAssetKind::Image,
                size_bytes: 8,
                content_hash: vec![1u8; 32],
            }],
        };
        let line = space_send_line(&peer, &["1.2.3.4:5".into()], fetch).unwrap();
        assert!(line.starts_with(&format!("SYNC_SEND\t{peer}\t1.2.3.4:5\t")));
        assert!(line.contains(r#""kind":"space""#));
        assert!(line.contains(r#""kind":"fetch""#));

        // INCOMING / STAGED 载荷解析。
        let manifest = share::TransferManifest {
            batch_id: Uuid::new_v4(),
            sender_name: "对方".into(),
            space_id: Some(Uuid::new_v4()),
            items: vec![],
        };
        let parsed = parse_transfer_manifest(&serde_json::to_string(&manifest).unwrap()).unwrap();
        assert!(parsed.space_id.is_some());
        let staged = parse_staged_payload(
            &serde_json::to_string(&(Uuid::new_v4(), vec!["C:\\s\\a.png"])).unwrap(),
        )
        .unwrap();
        assert_eq!(staged.1.len(), 1);
    }

    #[test]
    fn book_json_roundtrips_state() {
        use share::SpaceBookEntry;
        let entry = SpaceBookEntry {
            meta: {
                let mut registry = SpaceRegistry::new();
                registry.self_id = Some("a".repeat(52));
                registry.self_name = "店长".into();
                registry.create_space("团队素材", 1).unwrap()
            },
            ops: Vec::new(),
            serve: Vec::new(),
        };
        let mut book = SpaceBookState::default();
        book.set(entry.meta.id, entry);
        let json = space_book_json(&book);
        assert!(json.contains(r#""spaces""#));
        let back: SpaceBookState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.spaces.len(), 1);
    }
}
