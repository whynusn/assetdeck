//! 共享空间 VM（D90）：spaces.json 的内存态与用户操作翻译。
//!
//! 职责边界：持有一个 [`share::SpaceRegistry`]（持久化契约在 share crate），
//! 把「创建/退出/旋转邀请码/身份回填」翻译成带校验的状态迁移，维护
//! `dirty` 标记——app-ui 在每个动作后按 [`Self::take_dirty`] 决定是否原子
//! 落盘（事件驱动保存，零定时器，同 ShareControlVm 模式）。
//!
//! P1（单机半场）不碰网络：本 VM 只管本地清单；P2 的 op log 同步状态
//! （已应用 seq / 等待源）在此 VM 上扩展，不另起炉灶。

use share::{SpaceError, SpaceMeta, SpaceRegistry};
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

    /// 侧栏空间行的副标签：成员数（P1 口径；P2 追加同步状态）。
    pub fn member_label(space: &SpaceMeta) -> String {
        format!("{} 名成员", space.members.len())
    }
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
}
