//! 共享域控制面（D80-M1）：把「共享给谁」从每一次推送指令里解耦出来。
//!
//! 三类模型各司其职（DECISIONS.md D80-M1 定稿）：
//! - [`ShareDomain`]：共享范围定义。个人域 = 我对某台设备的专属共享；
//!   群组域 = 多设备同享。成员册只在本机保存，**永不出现在同步载荷里**
//!   ——接收方只能感知「来自谁、哪些素材可得」，感知不到任何域定义。
//! - [`PairedDevice`]：已配对设备册（信任的最小单元）。配对 = 互知
//!   EndpointId 并由用户命名；未入册的设备对任何域都不可见。
//! - 校验全部复用 [`crate::request`] 的形状助手（z32 / 安全标签），
//!   坏数据不进控制面。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::request::{is_device_id, is_safe_label};

/// 域的形态：个人域恒单成员，群组域多成员。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DomainKind {
    Personal,
    Group,
}

/// 共享范围定义：`members` 为 z32 设备 id（EndpointId 文本）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareDomain {
    pub id: Uuid,
    pub name: String,
    pub kind: DomainKind,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    BadName,
    /// 成员册非法：index 指向首个非法或重复出现的成员。
    BadMember {
        index: usize,
    },
    /// 成员数不符合域形态（个人域必须恰 1；群组域任意，含 0 = 草稿）。
    BadMembership {
        kind: DomainKind,
        count: usize,
    },
}

impl core::fmt::Display for DomainError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DomainError::BadName => write!(f, "共享域名称不合法"),
            DomainError::BadMember { index } => {
                write!(f, "第 {index} 位成员的设备标识不合法或重复")
            }
            DomainError::BadMembership { kind, count } => write!(
                f,
                "成员数 {count} 不符合{}域要求（{}）",
                match kind {
                    DomainKind::Personal => "个人",
                    DomainKind::Group => "群组",
                },
                match kind {
                    DomainKind::Personal => "恰 1 名",
                    DomainKind::Group => "0 名（草稿）或任意名",
                }
            ),
        }
    }
}

impl std::error::Error for DomainError {}

impl ShareDomain {
    /// 结构性守卫：名称安全 + 成员形状（z32）+ 无重复 + 成员数符合形态。
    /// 成员是否真的已配对由调用方对照 [`PairedDevices`] 决定（域定义可先
    /// 于配对存在，启用时再核对）。
    pub fn validate(&self) -> Result<(), DomainError> {
        if !is_safe_label(&self.name) {
            return Err(DomainError::BadName);
        }
        let expected: std::ops::RangeInclusive<usize> = match self.kind {
            DomainKind::Personal => 1..=1,
            // 空群组 = 草稿态：零成员对任何人都不可见（fail-closed），
            // UI 允许先建组再勾成员。个人域恒单成员，无草稿态。
            DomainKind::Group => 0..=usize::MAX,
        };
        if !expected.contains(&self.members.len()) {
            return Err(DomainError::BadMembership {
                kind: self.kind,
                count: self.members.len(),
            });
        }
        for (index, member) in self.members.iter().enumerate() {
            if !is_device_id(member) || self.members[..index].contains(member) {
                return Err(DomainError::BadMember { index });
            }
        }
        Ok(())
    }

    /// 该设备是否在本域成员册中。
    pub fn has_member(&self, device_id: &str) -> bool {
        self.members.iter().any(|m| m == device_id)
    }
}

/// 已配对设备：信任的原子记录。`name` 是用户备注名（可改），`paired_at`
/// 为 Unix 秒——展示用，不参与身份判定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub paired_at: i64,
}

impl PairedDevice {
    pub fn validate(&self) -> Result<(), DomainError> {
        if !is_safe_label(&self.name) {
            return Err(DomainError::BadName);
        }
        if !is_device_id(&self.id) {
            return Err(DomainError::BadMember { index: 0 });
        }
        Ok(())
    }
}

/// 已配对设备册：按 id 原位合并（同 alias 册精神）——重复配对同一设备
/// 只更新备注名，不产生重复行，顺序保持稳定（UI 列表不跳）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevices {
    devices: Vec<PairedDevice>,
}

impl PairedDevices {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记或更新（同 id 合并）；返回是否新增。
    pub fn pair(&mut self, device: PairedDevice) -> bool {
        if let Some(slot) = self.devices.iter_mut().find(|d| d.id == device.id) {
            slot.name = device.name;
            return false;
        }
        self.devices.push(device);
        true
    }

    /// 解除配对；返回是否真的移除了。
    pub fn unpair(&mut self, device_id: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|d| d.id != device_id);
        self.devices.len() != before
    }

    pub fn contains(&self, device_id: &str) -> bool {
        self.devices.iter().any(|d| d.id == device_id)
    }

    pub fn get(&self, device_id: &str) -> Option<&PairedDevice> {
        self.devices.iter().find(|d| d.id == device_id)
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PairedDevice> {
        self.devices.iter()
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

    fn personal_domain() -> ShareDomain {
        ShareDomain {
            id: Uuid::new_v4(),
            name: "客厅机".to_string(),
            kind: DomainKind::Personal,
            members: vec![device_id(1)],
        }
    }

    #[test]
    fn valid_domains_pass() {
        personal_domain().validate().unwrap();

        ShareDomain {
            id: Uuid::new_v4(),
            name: "家庭组".to_string(),
            kind: DomainKind::Group,
            members: vec![device_id(1), device_id(2), device_id(3)],
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn bad_names_are_rejected() {
        let mut domain = personal_domain();
        domain.name = String::new();
        assert_eq!(domain.validate(), Err(DomainError::BadName));

        domain.name = "bad\tname".to_string();
        assert_eq!(domain.validate(), Err(DomainError::BadName));

        domain.name = "x".repeat(65);
        assert_eq!(domain.validate(), Err(DomainError::BadName));
    }

    #[test]
    fn membership_shape_is_enforced() {
        // 个人域两成员 → 拒绝。
        let mut domain = personal_domain();
        domain.members.push(device_id(2));
        assert_eq!(
            domain.validate(),
            Err(DomainError::BadMembership {
                kind: DomainKind::Personal,
                count: 2
            })
        );

        // 个人域零成员 → 拒绝（个人域无草稿态）。
        let empty_personal = ShareDomain {
            id: Uuid::new_v4(),
            name: "空个人域".to_string(),
            kind: DomainKind::Personal,
            members: Vec::new(),
        };
        assert_eq!(
            empty_personal.validate(),
            Err(DomainError::BadMembership {
                kind: DomainKind::Personal,
                count: 0
            })
        );

        // 空群组 = 草稿态，合法（零成员零可见）。
        ShareDomain {
            id: Uuid::new_v4(),
            name: "空组草稿".to_string(),
            kind: DomainKind::Group,
            members: Vec::new(),
        }
        .validate()
        .unwrap();

        // 成员 id 非 z32 形状 → 拒绝并指位。
        let group = ShareDomain {
            id: Uuid::new_v4(),
            name: "形状坏组".to_string(),
            kind: DomainKind::Group,
            members: vec!["short-id".to_string()],
        };
        assert_eq!(group.validate(), Err(DomainError::BadMember { index: 0 }));
    }

    #[test]
    fn duplicate_members_are_rejected() {
        let mut domain = personal_domain();
        domain.kind = DomainKind::Group;
        let same = device_id(7);
        domain.members = vec![same.clone(), device_id(8), same];
        assert_eq!(domain.validate(), Err(DomainError::BadMember { index: 2 }));
    }

    #[test]
    fn has_member_matches_exact_id() {
        let domain = personal_domain();
        assert!(domain.has_member(&device_id(1)));
        assert!(!domain.has_member(&device_id(2)));
    }

    #[test]
    fn paired_device_registry_merges_by_id() {
        let mut registry = PairedDevices::new();
        let id = device_id(5);

        let added = registry.pair(PairedDevice {
            id: id.clone(),
            name: "手机".to_string(),
            paired_at: 100,
        });
        assert!(added);
        assert!(registry.contains(&id));

        // 同 id 再配对 = 改名，不新增行。
        let added = registry.pair(PairedDevice {
            id: id.clone(),
            name: "我的手机".to_string(),
            paired_at: 200,
        });
        assert!(!added);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get(&id).unwrap().name, "我的手机");
        // 原位合并：paired_at 保留首见值。
        assert_eq!(registry.get(&id).unwrap().paired_at, 100);

        assert!(registry.unpair(&id));
        assert!(!registry.unpair(&id));
        assert!(registry.is_empty());
    }

    #[test]
    fn paired_device_validates_shape() {
        PairedDevice {
            id: device_id(9),
            name: "笔记本".to_string(),
            paired_at: 0,
        }
        .validate()
        .unwrap();

        let bad = PairedDevice {
            id: "not-z32".to_string(),
            name: "笔记本".to_string(),
            paired_at: 0,
        };
        assert_eq!(bad.validate(), Err(DomainError::BadMember { index: 0 }));
    }

    #[test]
    fn domain_serde_roundtrip() {
        let domain = personal_domain();
        let json = serde_json::to_string(&domain).unwrap();
        let back: ShareDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(back, domain);
        assert!(json.contains("\"kind\":\"Personal\""));
    }
}
