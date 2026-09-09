//! 共享控制面 VM（D80-M1-a）：配对册 + 共享域 + 共享态的纯逻辑状态机。
//!
//! 职责边界：持有一个 [`ShareRegistry`]（持久化契约在 share crate），把
//! 「配对/建域/改成员/标记共享/撤销」等用户操作翻译成带校验的状态迁移，
//! 并维护 `dirty` 标记——app-ui 在每个动作后按 `take_dirty()` 决定是否
//! 原子落盘（事件驱动保存，零定时器）。信任语义（比 share 纯模型更严）：
//! - 域成员必须是已配对设备（控制面收口；纯模型的 validate 只管形状）；
//! - 解除配对 = 收回信任，级联把设备从所有域成员册移除；因此变空的域
//!   连同其共享事实一并删除（fail-closed：重新配对不隐式复活旧共享）；
//! - 拆域级联撤销该域全部共享事实（rev 单调推进，M1-b 增量通知依此）。
//!
//! 共享态只广播可获得性，不自动投递（D80-M1 不变量；VM 层不碰传输）。

use share::{
    is_device_id, is_safe_label, DomainError, DomainKind, PairedDevice, ShareDomain, ShareOffer,
    ShareRegistry,
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlError {
    /// 纯模型校验失败（域名/成员形状/成员数与域形态不符）。
    Domain(DomainError),
    /// 目标域不存在。
    UnknownDomain,
    /// 成员未配对：不能把不认识的设备写进域（index 指向首个违例）。
    UnpairedMember {
        index: usize,
    },
    BadDeviceId,
    BadDeviceName,
}

impl core::fmt::Display for ControlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ControlError::Domain(e) => write!(f, "{e}"),
            ControlError::UnknownDomain => write!(f, "共享域不存在"),
            ControlError::UnpairedMember { index } => {
                write!(f, "第 {index} 位成员尚未配对，不能加入共享域")
            }
            ControlError::BadDeviceId => write!(f, "设备标识不合法"),
            ControlError::BadDeviceName => write!(f, "设备名不合法"),
        }
    }
}

impl std::error::Error for ControlError {}

#[derive(Debug, Clone, Default)]
pub struct ShareControlVm {
    registry: ShareRegistry,
    dirty: bool,
}

impl ShareControlVm {
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动装载：从磁盘读出的注册表注入（dirty=false，静默态）。
    pub fn from_registry(registry: ShareRegistry) -> Self {
        Self {
            registry,
            dirty: false,
        }
    }

    pub fn registry(&self) -> &ShareRegistry {
        &self.registry
    }

    pub fn devices(&self) -> &share::PairedDevices {
        &self.registry.devices
    }

    pub fn domains(&self) -> &[ShareDomain] {
        &self.registry.domains
    }

    pub fn shared_entries(&self) -> &[share::SharingEntry] {
        self.registry.shared.entries()
    }

    pub fn rev(&self) -> u64 {
        self.registry.shared.rev()
    }

    pub fn digest(&self) -> u64 {
        self.registry.shared.digest()
    }

    /// 读取并清除落盘请求标记（读后即清，app-ui 据此决定是否原子写）。
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// 只读脏标记（测试与断言用；落盘决策走 take_dirty）。
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    // ---------- 配对册 ----------

    /// 配对（同 id 再配对 = 改备注名）；返回是否新增。
    pub fn pair(&mut self, id: &str, name: &str, paired_at: i64) -> Result<bool, ControlError> {
        if !is_device_id(id) {
            return Err(ControlError::BadDeviceId);
        }
        if !is_safe_label(name) {
            return Err(ControlError::BadDeviceName);
        }
        let added = self.registry.devices.pair(PairedDevice {
            id: id.to_string(),
            name: name.to_string(),
            paired_at,
        });
        if added {
            self.dirty = true;
        }
        Ok(added)
    }

    /// 解除配对（级联）：先从设备册移除，再把设备逐出所有域成员册。
    /// 因此变空的域：共享事实全部撤销（成员再出现时旧共享不得静默复活）；
    /// 个人域变空（恒单成员）连壳删除，群组域保留空壳（草稿态，零成员
    /// 零可见）。只动真正因本次解配而变空的域。返回是否真的解除了配对。
    pub fn unpair(&mut self, device_id: &str) -> bool {
        if !self.registry.devices.unpair(device_id) {
            return false;
        }
        let mut emptied: Vec<Uuid> = Vec::new();
        for domain in &mut self.registry.domains {
            let had_member = domain.members.iter().any(|m| m == device_id);
            domain.members.retain(|m| m != device_id);
            if had_member && domain.members.is_empty() {
                emptied.push(domain.id);
            }
        }
        self.registry.domains.retain(|d| {
            // 只有变空的个人域删壳；空群组壳保留（草稿态）。
            !(d.members.is_empty() && emptied.contains(&d.id) && d.kind == DomainKind::Personal)
        });
        for id in &emptied {
            self.registry.shared.revoke_domain(*id);
        }
        self.dirty = true;
        true
    }

    // ---------- 域册 ----------

    /// 建域：成员必须全部已配对，形状校验复用纯模型；返回新域 id。
    pub fn create_domain(
        &mut self,
        name: &str,
        kind: DomainKind,
        members: Vec<String>,
    ) -> Result<Uuid, ControlError> {
        self.check_members_paired(&members)?;
        let domain = ShareDomain {
            id: Uuid::new_v4(),
            name: name.to_string(),
            kind,
            members,
        };
        domain.validate().map_err(ControlError::Domain)?;
        let id = domain.id;
        self.registry.domains.push(domain);
        self.dirty = true;
        Ok(id)
    }

    pub fn rename_domain(&mut self, domain_id: Uuid, name: &str) -> Result<(), ControlError> {
        if !is_safe_label(name) {
            return Err(ControlError::Domain(DomainError::BadName));
        }
        self.domain_mut(domain_id)?.name = name.to_string();
        self.dirty = true;
        Ok(())
    }

    /// 整体替换域成员册（UI 一次提交完整勾选集）。先验后写：配对核对 +
    /// 域形态/形状校验全部通过才落状态。
    pub fn set_domain_members(
        &mut self,
        domain_id: Uuid,
        members: Vec<String>,
    ) -> Result<(), ControlError> {
        self.check_members_paired(&members)?;
        let (name, kind) = {
            let domain = self.domain(domain_id)?;
            (domain.name.clone(), domain.kind)
        };
        // 借临时域复用纯模型校验（成员数与形态、z32 形状、重复成员）。
        ShareDomain {
            id: domain_id,
            name,
            kind,
            members: members.clone(),
        }
        .validate()
        .map_err(ControlError::Domain)?;
        self.domain_mut(domain_id)?.members = members;
        self.dirty = true;
        Ok(())
    }

    /// 拆域：删除域定义并级联撤销其全部共享事实。返回是否真的删除了。
    pub fn remove_domain(&mut self, domain_id: Uuid) -> bool {
        let before = self.registry.domains.len();
        self.registry.domains.retain(|d| d.id != domain_id);
        if self.registry.domains.len() == before {
            return false;
        }
        self.registry.shared.revoke_domain(domain_id);
        self.dirty = true;
        true
    }

    // ---------- 共享态 ----------

    /// 标记共享（幂等）；域必须存在。返回是否新增了事实。
    pub fn mark_shared(
        &mut self,
        asset_uuid: Uuid,
        domain_id: Uuid,
        now: i64,
    ) -> Result<bool, ControlError> {
        if !self.registry.domains.iter().any(|d| d.id == domain_id) {
            return Err(ControlError::UnknownDomain);
        }
        let added = self.registry.shared.mark(asset_uuid, domain_id, now);
        if added {
            self.dirty = true;
        }
        Ok(added)
    }

    /// 撤销单条共享事实（幂等）；返回是否有变化。
    pub fn revoke_shared(&mut self, asset_uuid: Uuid, domain_id: Uuid) -> bool {
        let changed = self.registry.shared.revoke(asset_uuid, domain_id);
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// 撤销某素材的全部共享（删除/入回收站时调用）；返回撤销条数。
    pub fn revoke_asset(&mut self, asset_uuid: Uuid) -> usize {
        let removed = self.registry.shared.revoke_asset(asset_uuid);
        if removed > 0 {
            self.dirty = true;
        }
        removed
    }

    /// 某设备当前可见的素材集合（经域成员册过滤，去重升序）。
    pub fn visible_to(&self, device_id: &str) -> Vec<Uuid> {
        self.registry
            .shared
            .visible_to(device_id, &self.registry.domains)
    }

    /// 快照装配（M1-b SNAPSHOT 的载荷来源）：可见素材经 `lookup` 物化为
    /// 可得行；库内已不存在的素材自然缺席（不广播悬空可得）。返回
    /// (rev, offers)，offers 按素材 uuid 升序（visible_to 已保证）。
    pub fn snapshot_offers(
        &self,
        device_id: &str,
        lookup: impl Fn(Uuid) -> Option<ShareOffer>,
    ) -> (u64, Vec<ShareOffer>) {
        let offers = self
            .visible_to(device_id)
            .into_iter()
            .filter_map(lookup)
            .collect();
        (self.registry.shared.rev(), offers)
    }

    // ---------- 内部 ----------

    fn domain(&self, domain_id: Uuid) -> Result<&ShareDomain, ControlError> {
        self.registry
            .domains
            .iter()
            .find(|d| d.id == domain_id)
            .ok_or(ControlError::UnknownDomain)
    }

    fn domain_mut(&mut self, domain_id: Uuid) -> Result<&mut ShareDomain, ControlError> {
        self.registry
            .domains
            .iter_mut()
            .find(|d| d.id == domain_id)
            .ok_or(ControlError::UnknownDomain)
    }

    fn check_members_paired(&self, members: &[String]) -> Result<(), ControlError> {
        for (index, member) in members.iter().enumerate() {
            if !self.registry.devices.contains(member) {
                return Err(ControlError::UnpairedMember { index });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_id(seed: u8) -> String {
        let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
        (0..52)
            .map(|i| alphabet[(seed as usize + i) % 36] as char)
            .collect()
    }

    /// 两台已配对设备（手机、客厅机）+ 一个群组域（含两台）+ 个人域（仅手机）。
    fn vm_with_fixtures() -> (ShareControlVm, String, String, Uuid, Uuid) {
        let mut vm = ShareControlVm::new();
        vm.pair(&device_id(1), "手机", 100).unwrap();
        vm.pair(&device_id(2), "客厅机", 101).unwrap();
        let group = vm
            .create_domain(
                "家庭组",
                DomainKind::Group,
                vec![device_id(1), device_id(2)],
            )
            .unwrap();
        let personal = vm
            .create_domain("手机专属", DomainKind::Personal, vec![device_id(1)])
            .unwrap();
        vm.take_dirty(); // 清掉 fixture 期的 dirty
        (vm, device_id(1), device_id(2), group, personal)
    }

    #[test]
    fn pair_validates_shape_and_upserts() {
        let mut vm = ShareControlVm::new();
        assert_eq!(
            vm.pair("short-id", "手机", 1),
            Err(ControlError::BadDeviceId)
        );
        assert_eq!(
            vm.pair(&device_id(1), "bad\tname", 1),
            Err(ControlError::BadDeviceName)
        );

        assert!(vm.pair(&device_id(1), "手机", 100).unwrap());
        assert!(vm.dirty());
        assert!(
            !vm.pair(&device_id(1), "我的手机", 200).unwrap(),
            "同 id 再配对 = 改名"
        );
        assert_eq!(vm.devices().len(), 1);
        assert_eq!(vm.devices().get(&device_id(1)).unwrap().name, "我的手机");
        assert_eq!(vm.devices().get(&device_id(1)).unwrap().paired_at, 100);
        // 改名也置 dirty（备注名要落盘）。
        assert!(vm.take_dirty());
        assert!(!vm.take_dirty());
    }

    #[test]
    fn unpair_cascades_trust_removal() {
        let (mut vm, phone, living, group, personal) = vm_with_fixtures();
        // 空群组草稿（从未有成员）与手机无关，解配不得误删。
        let draft = vm
            .create_domain("空组草稿", DomainKind::Group, vec![])
            .unwrap();
        let asset = Uuid::new_v4();
        vm.mark_shared(asset, group, 1).unwrap();
        vm.mark_shared(asset, personal, 1).unwrap();
        vm.take_dirty();

        // 解除手机配对：群组域失去手机但客厅机还在（域保住）；个人域变空 →
        // 删除 + 其共享事实撤销。
        assert!(vm.unpair(&phone));
        assert!(vm.take_dirty());

        assert!(!vm.devices().contains(&phone));
        let group_domain = vm.domains().iter().find(|d| d.id == group).unwrap();
        assert_eq!(group_domain.members, vec![living.clone()]);
        assert!(
            !vm.domains().iter().any(|d| d.id == personal),
            "变空的个人域级联删除"
        );
        assert!(
            !vm.shared_entries().iter().any(|e| e.domain_id == personal),
            "被删域的共享事实级联撤销"
        );
        assert!(
            vm.shared_entries().iter().any(|e| e.domain_id == group),
            "群组域仍有成员，共享事实保留"
        );
        assert!(
            vm.domains().iter().any(|d| d.id == draft),
            "无关的空群组草稿不被解配级联误删"
        );
        // fail-closed：重新配对不复活旧共享。
        vm.pair(&phone, "手机", 300).unwrap();
        assert!(vm.shared_entries().iter().all(|e| e.domain_id != personal));
    }

    #[test]
    fn create_domain_requires_paired_and_valid_members() {
        let mut vm = ShareControlVm::new();
        vm.pair(&device_id(1), "手机", 1).unwrap();

        // 未配对成员拒之门外。
        assert_eq!(
            vm.create_domain(
                "家庭组",
                DomainKind::Group,
                vec![device_id(1), device_id(9)]
            ),
            Err(ControlError::UnpairedMember { index: 1 })
        );
        // 域形态违规（个人域两成员）经纯模型校验拒绝（成员数先于形状判定）。
        assert_eq!(
            vm.create_domain(
                "手机专属",
                DomainKind::Personal,
                vec![device_id(1), device_id(1)]
            ),
            Err(ControlError::Domain(DomainError::BadMembership {
                kind: DomainKind::Personal,
                count: 2
            }))
        );
        // 空群组 = 草稿态，直接可建（UI 先建组后勾成员的流）。
        let draft = vm
            .create_domain("新组草稿", DomainKind::Group, vec![])
            .unwrap();
        assert_eq!(vm.domains().len(), 1);
        assert_eq!(vm.domains()[0].id, draft);
        assert_eq!(vm.domains()[0].members.len(), 0);

        let id = vm
            .create_domain("家庭组", DomainKind::Group, vec![device_id(1)])
            .unwrap();
        assert_eq!(vm.domains().len(), 2);
        assert!(vm.domains().iter().any(|d| d.id == id));
    }

    #[test]
    fn set_domain_members_validates_then_writes() {
        let (mut vm, _phone, living, group, _personal) = vm_with_fixtures();
        vm.pair(&device_id(3), "书房机", 102).unwrap();
        vm.take_dirty();

        // 未配对成员拒绝。
        assert_eq!(
            vm.set_domain_members(group, vec![device_id(3), device_id(9)]),
            Err(ControlError::UnpairedMember { index: 1 })
        );
        // 合法替换生效。
        vm.set_domain_members(group, vec![living.clone(), device_id(3)])
            .unwrap();
        let domain = vm.domains().iter().find(|d| d.id == group).unwrap();
        assert_eq!(domain.members, vec![living.clone(), device_id(3)]);
        assert!(vm.take_dirty());

        // 未知域。
        assert_eq!(
            vm.set_domain_members(Uuid::new_v4(), vec![living.clone()]),
            Err(ControlError::UnknownDomain)
        );
    }

    #[test]
    fn remove_domain_cascades_revocation_and_bumps_rev() {
        let (mut vm, _phone, _living, group, _personal) = vm_with_fixtures();
        vm.mark_shared(Uuid::new_v4(), group, 1).unwrap();
        vm.mark_shared(Uuid::new_v4(), group, 1).unwrap();
        let rev = vm.rev();
        vm.take_dirty();

        assert!(vm.remove_domain(group));
        assert!(vm.domains().iter().all(|d| d.id != group));
        assert!(vm.shared_entries().is_empty());
        assert_eq!(vm.rev(), rev + 1, "级联撤销推进 rev（M1-b 增量通知依此）");
        assert!(vm.take_dirty());

        assert!(!vm.remove_domain(group), "重复拆域幂等");
        assert!(!vm.take_dirty());
    }

    #[test]
    fn mark_shared_requires_domain_and_is_idempotent() {
        let (mut vm, phone, _living, group, _personal) = vm_with_fixtures();
        let asset = Uuid::new_v4();

        assert_eq!(
            vm.mark_shared(asset, Uuid::new_v4(), 1),
            Err(ControlError::UnknownDomain)
        );

        assert!(vm.mark_shared(asset, group, 1).unwrap());
        assert!(vm.dirty());
        vm.take_dirty();
        // 幂等 mark：不加事实、不动 rev、不置 dirty（无虚假增量）。
        assert!(!vm.mark_shared(asset, group, 2).unwrap());
        assert!(!vm.dirty());
        assert_eq!(vm.rev(), 1, "一次真实标记 → rev=1");

        // 可见性：手机在群组域里 → 可见该素材。
        assert_eq!(vm.visible_to(&phone), vec![asset]);
    }

    #[test]
    fn revoke_shared_and_revoke_asset_are_idempotent() {
        let (mut vm, _phone, _living, group, personal) = vm_with_fixtures();
        let asset = Uuid::new_v4();
        vm.mark_shared(asset, group, 1).unwrap();
        vm.mark_shared(asset, personal, 1).unwrap();
        vm.take_dirty();

        assert!(vm.revoke_shared(asset, group));
        assert!(vm.take_dirty());
        assert_eq!(vm.shared_entries().len(), 1);

        assert!(!vm.revoke_shared(asset, group), "重复撤销幂等");
        assert!(!vm.dirty());

        assert_eq!(vm.revoke_asset(asset), 1);
        assert!(vm.shared_entries().is_empty());
        assert_eq!(vm.revoke_asset(asset), 0);
    }

    #[test]
    fn snapshot_offers_drop_missing_assets() {
        let (mut vm, phone, _living, _group, _personal) = vm_with_fixtures();
        let group = vm.domains()[0].id;
        let present = Uuid::new_v4();
        let gone = Uuid::new_v4();
        vm.mark_shared(present, group, 1).unwrap();
        vm.mark_shared(gone, group, 1).unwrap();
        let rev = vm.rev();

        let (got_rev, offers) = vm.snapshot_offers(&phone, |uuid| {
            if uuid == present {
                Some(ShareOffer {
                    asset_uuid: uuid,
                    file_name: "photo.png".to_string(),
                    kind: domain::AssetKind::Image,
                    size_bytes: 10,
                })
            } else {
                None // 库内已不存在 → 自然缺席
            }
        });
        assert_eq!(got_rev, rev);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].asset_uuid, present);

        // 未知设备拿不到任何东西（默认零共享）。
        let (_, none) = vm.snapshot_offers(&device_id(9), |_| None);
        assert!(none.is_empty());
    }

    #[test]
    fn from_registry_loads_silent_and_take_dirty_resets() {
        let (mut vm, _phone, _living, group, _personal) = vm_with_fixtures();
        vm.mark_shared(Uuid::new_v4(), group, 1).unwrap();
        let snapshot = vm.registry().clone();
        let rev = snapshot.shared.rev();

        let mut loaded = ShareControlVm::from_registry(snapshot);
        assert!(!loaded.take_dirty(), "装载态是静默态，不触发落盘");
        assert_eq!(loaded.rev(), rev);
        assert_eq!(loaded.shared_entries().len(), 1);
    }
}
