//! 共享空间（D90）：副本集模型的契约层——空间元数据、注册表、邀请码与
//! 操作日志（op）形状。纯类型与校验，零 IO、零网络。
//!
//! 取代 D80-M1/M2 的「域控制面 + 可获得性广播」：一个空间 = 命名的素材
//! 集合 + 成员名单，素材进入空间即向全体成员复制本地副本。用户交互只有
//! 加入（贴邀请码）/放入（Add）/使用（浏览换根的库视图）三个动作。
//!
//! 关键形状：
//! - [`SpaceMeta`]：空间元数据（名册含空间主本人）；角色按 owner_id 派生，
//!   不单独存储（单一事实源）。
//! - [`SpaceRegistry`]：spaces.json 的文档形状（本机已加入的空间清单），
//!   落盘由 app-ui 原子写（share_registry.json 同模式）。
//! - 邀请码 = `{owner_id}.{z32(space_id)}.{z32(secret)}`——第三段是**可
//!   旋转的随机 secret**（域 id 即密码的 M2 形态作废：码泄漏 = 永久后门）。
//!   secret_gen 单调递增，旋转后旧码即刻失效。
//! - [`SpaceOp`]：操作日志的单行形状（P2 同步载荷，字节不进 log——按
//!   content_hash 后取，D65 的 SHA-256 复用为传输完整性校验）。排序键
//!   (lamport, author) 全序，任意成员按同序应用即确定收敛。
//! - 写权限分工（把冲突消灭在规则层）：人人可 Add（并集天然无冲突，
//!   两人传同文件被内容等值去重吸收）；Remove = 空间主或添加者本人；
//!   结构（分类/改名/归类）= 空间主；Roster/转让 = 空间主。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::invite::{z32_decode, z32_encode, RosterMember};
use crate::manifest::is_safe_display_name;
use crate::request::{is_device_id, is_safe_label};

/// 空间角色（按 owner_id 派生，不落盘）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceRole {
    Owner,
    Member,
}

/// 一个已加入的共享空间（spaces.json 的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceMeta {
    pub id: Uuid,
    pub name: String,
    /// 空间主的 52 位设备标识。
    pub owner_id: String,
    /// 空间主展示名（名册行自带，便于接收方显示）。
    pub owner_name: String,
    /// 成员名册（含空间主本人）。被移出的成员收到不含自己的名册 = 踢出信号
    /// （M2 语义保留）。
    pub members: Vec<RosterMember>,
    /// 邀请码 secret（16 字节随机数的 z32 形态，26 字符）。仅空间主侧用于
    /// 展示/校验；成员侧收到 Roster op 后原位覆盖。
    pub invite_secret: String,
    /// secret 世代（旋转 +1，入组校验 gen；旧 gen 的码即刻失效）。
    pub secret_gen: u32,
    pub created_at: i64,
    pub joined_at: i64,
}

impl SpaceMeta {
    pub fn role_of(&self, device_id: &str) -> Option<SpaceRole> {
        if !self.is_member(device_id) {
            return None;
        }
        Some(if device_id == self.owner_id {
            SpaceRole::Owner
        } else {
            SpaceRole::Member
        })
    }

    pub fn is_member(&self, device_id: &str) -> bool {
        self.members.iter().any(|m| m.id == device_id)
    }

    /// 结构性守卫：名字/成员名安全、id 形状合法、无重复、空间主在册、
    /// secret 是 26 字符 z32。
    pub fn validate(&self) -> Result<(), SpaceError> {
        if !is_safe_label(&self.name) {
            return Err(SpaceError::BadName);
        }
        if !is_device_id(&self.owner_id)
            || (!self.owner_name.is_empty() && !is_safe_display_name(&self.owner_name))
        {
            return Err(SpaceError::BadOwner);
        }
        if z32_decode(&self.invite_secret).is_err() {
            return Err(SpaceError::BadSecret);
        }
        for (index, member) in self.members.iter().enumerate() {
            if !is_device_id(&member.id) || self.members[..index].iter().any(|m| m.id == member.id)
            {
                return Err(SpaceError::BadMember);
            }
            if !member.name.is_empty() && !is_safe_display_name(&member.name) {
                return Err(SpaceError::BadMember);
            }
        }
        if !self.is_member(&self.owner_id) {
            return Err(SpaceError::BadOwner);
        }
        Ok(())
    }
}

/// spaces.json 的文档形状（本机空间清单 + 设备自身身份）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRegistry {
    /// 本机 52 位设备标识（worker READY 回填；未就绪 = None，创建/加入被拒）。
    pub self_id: Option<String>,
    pub self_name: String,
    pub spaces: Vec<SpaceMeta>,
}

impl SpaceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 序列化为落盘文本（shell 层只管原子写）。
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 从落盘文本恢复；解析失败 = None（调用方按空册起步，不 panic 不半载）。
    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    pub fn find(&self, id: Uuid) -> Option<&SpaceMeta> {
        self.spaces.iter().find(|s| s.id == id)
    }

    pub fn find_mut(&mut self, id: Uuid) -> Option<&mut SpaceMeta> {
        self.spaces.iter_mut().find(|s| s.id == id)
    }

    /// 创建空间（本机即空间主，名册只有自己）。名字在本机清单内唯一——
    /// 侧栏以名寻址，重名会让用户分不清两个空间。
    pub fn create_space(&mut self, name: &str, now: i64) -> Result<SpaceMeta, SpaceError> {
        let Some(self_id) = self.self_id.clone() else {
            return Err(SpaceError::NotReady);
        };
        if self.spaces.iter().any(|s| s.name == name) {
            return Err(SpaceError::NameExists(name.to_string()));
        }
        let space = SpaceMeta {
            id: Uuid::new_v4(),
            name: name.to_string(),
            owner_id: self_id.clone(),
            owner_name: self.self_name.clone(),
            members: vec![RosterMember {
                id: self_id,
                name: self.self_name.clone(),
            }],
            // 16 字节随机 secret：uuid v4 即 128 位随机源，不引新依赖。
            invite_secret: z32_encode(Uuid::new_v4().as_bytes()),
            secret_gen: 1,
            created_at: now,
            joined_at: now,
        };
        space.validate()?;
        self.spaces.push(space.clone());
        Ok(space)
    }

    /// 退出空间（成员侧本地行为：删清单行；空间主退出 = 先转让或解散——
    /// v1 由 UI 引导解散，转让 op 见 [`SpaceOpKind::TransferOwner`]）。
    pub fn leave_space(&mut self, id: Uuid) -> bool {
        let before = self.spaces.len();
        self.spaces.retain(|s| s.id != id);
        before != self.spaces.len()
    }

    /// 旋转邀请码（空间主动作）：新 secret + gen + 1。返回新码段。
    pub fn rotate_secret(&mut self, id: Uuid) -> Option<String> {
        let space = self.find_mut(id)?;
        space.invite_secret = z32_encode(Uuid::new_v4().as_bytes());
        space.secret_gen += 1;
        Some(space.invite_secret.clone())
    }
}

/// 空间邀请码 = `{owner_id}.{z32(space_id)}.{z32(secret)}`。
pub fn space_invite_code(space: &SpaceMeta) -> String {
    format!(
        "{}.{}.{}",
        space.owner_id,
        z32_encode(space.id.as_bytes()),
        space.invite_secret
    )
}

/// 解析后的空间邀请码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceInviteCode {
    pub owner_id: String,
    pub space_id: Uuid,
    pub secret: String,
}

pub fn parse_space_invite_code(code: &str) -> Result<SpaceInviteCode, SpaceError> {
    let code = code.trim();
    let mut parts = code.split('.');
    let owner = parts.next().unwrap_or_default();
    if !is_device_id(owner) {
        return Err(SpaceError::BadInvite);
    }
    let space_id = parts
        .next()
        .and_then(|segment| z32_decode(segment).ok())
        .ok_or(SpaceError::BadInvite)?;
    let secret = parts.next().ok_or(SpaceError::BadInvite)?;
    if z32_decode(secret).is_err() || parts.next().is_some() {
        return Err(SpaceError::BadInvite);
    }
    Ok(SpaceInviteCode {
        owner_id: owner.to_string(),
        space_id,
        secret: secret.to_string(),
    })
}

/// op log 里的素材元数据（Add 载荷；字节按 content_hash 后取，不进 log）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceAssetMeta {
    pub uuid: Uuid,
    pub file_name: String,
    pub kind: SpaceAssetKind,
    pub size_bytes: u64,
    /// SHA-256 内容摘要（32 字节）——副本完整性校验 + 内容等值去重锚。
    pub content_hash: Vec<u8>,
}

impl SpaceAssetMeta {
    /// 结构性守卫：文件名安全、摘要 32 字节、大小在上限内（Add/Fetch 共用）。
    pub fn validate(&self) -> Result<(), SpaceError> {
        if !is_safe_display_name(&self.file_name)
            || self.content_hash.len() != 32
            || self.size_bytes > MAX_ASSET_BYTES
        {
            Err(SpaceError::BadAsset)
        } else {
            Ok(())
        }
    }
}

/// op 载荷的素材类别（store 的 AssetKind 不进契约层：share 不依赖 domain
/// 之外的分类面，枚举自持避免 crate 间漂移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceAssetKind {
    Image,
    Video,
    Text,
    Other,
}

impl From<domain::AssetKind> for SpaceAssetKind {
    fn from(kind: domain::AssetKind) -> Self {
        match kind {
            domain::AssetKind::Image => SpaceAssetKind::Image,
            domain::AssetKind::Video => SpaceAssetKind::Video,
            domain::AssetKind::Text => SpaceAssetKind::Text,
            domain::AssetKind::Other => SpaceAssetKind::Other,
        }
    }
}

impl From<SpaceAssetKind> for domain::AssetKind {
    /// 反向映射：Fetch 应答走引擎推送通道，SendItem.kind 需要 store 的枚举。
    fn from(kind: SpaceAssetKind) -> Self {
        match kind {
            SpaceAssetKind::Image => domain::AssetKind::Image,
            SpaceAssetKind::Video => domain::AssetKind::Video,
            SpaceAssetKind::Text => domain::AssetKind::Text,
            SpaceAssetKind::Other => domain::AssetKind::Other,
        }
    }
}

impl SpaceAssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceAssetKind::Image => "image",
            SpaceAssetKind::Video => "video",
            SpaceAssetKind::Text => "text",
            SpaceAssetKind::Other => "other",
        }
    }
}

/// 单条空间操作的载荷（serde tag 形态与既有 SyncMessage 同风格——改字段
/// 名就是破协议）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpaceOpKind {
    /// 放入素材（人人可发；并集收敛，内容等值由接收侧导入管线去重）。
    Add {
        asset: SpaceAssetMeta,
    },
    /// 移出素材（空间主或添加者本人）。
    Remove {
        asset_uuid: Uuid,
    },
    /// 素材归类（空间主；结构归空间主的分工面）。
    SetCategory {
        asset_uuid: Uuid,
        category: Option<String>,
    },
    /// 素材改名（空间主）。
    RenameAsset {
        asset_uuid: Uuid,
        new_name: String,
    },
    /// 分类结构（空间主）：建 / 改名 / 删除（引用行归待分类，D84 语义）。
    CategoryCreate {
        name: String,
    },
    CategoryRename {
        old: String,
        new: String,
    },
    CategoryDelete {
        name: String,
    },
    /// 名册变更（空间主；secret_gen 随最新 secret 一起下发，成员原位覆盖）。
    Roster {
        space: SpaceMeta,
    },
    /// 转让空间主（空间主）。
    TransferOwner {
        new_owner: String,
    },
}

/// op log 的一行：全序键 = (lamport, author)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceOp {
    pub space_id: Uuid,
    pub lamport: u64,
    pub author: String,
    pub ts: i64,
    #[serde(flatten)]
    pub op: SpaceOpKind,
}

impl SpaceOp {
    /// 结构性守卫：作者 id 形状、载荷安全标签、content_hash 32 字节。
    pub fn validate(&self) -> Result<(), SpaceError> {
        if !is_device_id(&self.author) {
            return Err(SpaceError::BadAuthor);
        }
        match &self.op {
            SpaceOpKind::Add { asset } => asset.validate()?,
            SpaceOpKind::SetCategory { category, .. } => {
                if let Some(name) = category {
                    if !is_safe_label(name) {
                        return Err(SpaceError::BadCategory);
                    }
                }
            }
            SpaceOpKind::RenameAsset { new_name, .. } => {
                if !is_safe_display_name(new_name) {
                    return Err(SpaceError::BadAsset);
                }
            }
            SpaceOpKind::CategoryCreate { name } | SpaceOpKind::CategoryDelete { name } => {
                if !is_safe_label(name) {
                    return Err(SpaceError::BadCategory);
                }
            }
            SpaceOpKind::CategoryRename { old, new } => {
                if !is_safe_label(old) || !is_safe_label(new) {
                    return Err(SpaceError::BadCategory);
                }
            }
            SpaceOpKind::Roster { space } => space.validate()?,
            SpaceOpKind::TransferOwner { new_owner } => {
                if !is_device_id(new_owner) {
                    return Err(SpaceError::BadOwner);
                }
            }
            SpaceOpKind::Remove { .. } => {}
        }
        Ok(())
    }

    /// 写权限裁决（纯函数；added_by = op log 侧的 uuid → 添加者索引，
    /// Remove 的「添加者本人」判定要查它）。
    pub fn permits(&self, space: &SpaceMeta, added_by: &HashMap<Uuid, String>) -> bool {
        let role = match space.role_of(&self.author) {
            Some(role) => role,
            None => return false,
        };
        match &self.op {
            // 人人可添。
            SpaceOpKind::Add { .. } => true,
            // 移出 = 空间主或添加者本人。
            SpaceOpKind::Remove { asset_uuid } => {
                role == SpaceRole::Owner
                    || added_by
                        .get(asset_uuid)
                        .is_some_and(|who| who == &self.author)
            }
            // 其余一切（结构/名册/转让）= 空间主。
            _ => role == SpaceRole::Owner,
        }
    }
}

/// 单素材字节上限（防误传大文件吃满成员磁盘；与导入管线口径一致放宽到
/// 4GB 档，真正的闸门是 D90 大小闸的离线累计量）。
pub const MAX_ASSET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// lamport 合并规则：收到更大的计数时本地跳到 max+1（保序不回退）。
pub fn merge_lamport(mine: u64, incoming: u64) -> u64 {
    if incoming >= mine {
        incoming + 1
    } else {
        mine
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceError {
    /// worker 未就绪（self_id 未回填），创建/加入被拒。
    NotReady,
    BadName,
    /// 空间主标识/名不合法。
    BadOwner,
    BadMember,
    /// secret 不是 26 字符 z32。
    BadSecret,
    NameExists(String),
    BadInvite,
    BadAuthor,
    BadAsset,
    BadCategory,
}

impl core::fmt::Display for SpaceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpaceError::NotReady => write!(f, "共享引擎未就绪，请稍后再试"),
            SpaceError::BadName => write!(f, "空间名不合法（空或含控制字符）"),
            SpaceError::BadOwner => write!(f, "空间主标识或名不合法"),
            SpaceError::BadMember => write!(f, "名册成员不合法（id 形状/重名/展示名不安全）"),
            SpaceError::BadSecret => write!(f, "邀请码 secret 段不合法"),
            SpaceError::NameExists(name) => write!(f, "已有同名空间：{name}"),
            SpaceError::BadInvite => write!(f, "邀请码格式不合法"),
            SpaceError::BadAuthor => write!(f, "操作作者标识不合法"),
            SpaceError::BadAsset => write!(f, "素材载荷不合法（文件名/摘要/大小）"),
            SpaceError::BadCategory => write!(f, "分类名不合法"),
        }
    }
}

impl std::error::Error for SpaceError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_id(seed: u8) -> String {
        (0..52)
            .map(|i| crate::pairing::ALPHABET[(seed as usize + i) % 36] as char)
            .collect()
    }

    fn ready_registry() -> SpaceRegistry {
        let mut registry = SpaceRegistry::new();
        registry.self_id = Some(device_id(1));
        registry.self_name = "店长的电脑".into();
        registry
    }

    #[test]
    fn create_and_validate_space() {
        let mut registry = ready_registry();
        let space = registry.create_space("团队素材", 1000).unwrap();
        assert_eq!(space.role_of(&device_id(1)), Some(SpaceRole::Owner));
        assert_eq!(space.members.len(), 1);
        assert_eq!(space.secret_gen, 1);
        assert!(z32_decode(&space.invite_secret).is_ok());
        space.validate().unwrap();

        // 重名拒绝。
        assert_eq!(
            registry.create_space("团队素材", 2000),
            Err(SpaceError::NameExists("团队素材".into()))
        );

        // 未就绪（无 self_id）拒绝。
        let mut not_ready = SpaceRegistry::new();
        assert_eq!(not_ready.create_space("x", 1), Err(SpaceError::NotReady));
    }

    #[test]
    fn registry_json_roundtrip() {
        let mut registry = ready_registry();
        registry.create_space("团队素材", 1000).unwrap();
        registry.create_space("售前组", 2000).unwrap();
        let json = registry.to_json();
        let back = SpaceRegistry::from_json(&json).unwrap();
        assert_eq!(back, registry);
        assert_eq!(back.spaces.len(), 2);
        // 坏文本 = None（空册起步，不 panic）。
        assert!(SpaceRegistry::from_json("{oops").is_none());
    }

    #[test]
    fn invite_code_roundtrip_and_rotation() {
        let mut registry = ready_registry();
        let space = registry.create_space("团队素材", 1000).unwrap();
        let code = space_invite_code(&space);
        let parsed = parse_space_invite_code(&code).unwrap();
        assert_eq!(parsed.owner_id, device_id(1));
        assert_eq!(parsed.space_id, space.id);
        assert_eq!(parsed.secret, space.invite_secret);

        // 旋转后旧 secret 不再匹配新码（gen 由 Roster op 同步）。
        let old_secret = space.invite_secret.clone();
        let new_secret = registry.rotate_secret(space.id).unwrap();
        assert_ne!(old_secret, new_secret);
        let rotated = registry.find(space.id).unwrap();
        assert_eq!(rotated.secret_gen, 2);
        assert_eq!(parse_space_invite_code(&code).unwrap().secret, old_secret);

        // 坏形状：两段（M2 旧形态）/ 坏 owner / 坏 secret 段 / 四段。
        assert_eq!(
            parse_space_invite_code(&format!("{}.x", device_id(1))),
            Err(SpaceError::BadInvite)
        );
        assert_eq!(
            parse_space_invite_code(&format!(
                "short.{}.{}",
                code.split('.').nth(1).unwrap(),
                new_secret
            )),
            Err(SpaceError::BadInvite)
        );
        assert_eq!(parse_space_invite_code(""), Err(SpaceError::BadInvite));
    }

    #[test]
    fn leave_space_removes_row() {
        let mut registry = ready_registry();
        let space = registry.create_space("团队素材", 1000).unwrap();
        assert!(registry.leave_space(space.id));
        assert!(!registry.leave_space(space.id));
        assert!(registry.find(space.id).is_none());
    }

    #[test]
    fn op_validate_branches() {
        let space = ready_registry().create_space("团队素材", 1000).unwrap();
        let base = |op: SpaceOpKind| SpaceOp {
            space_id: space.id,
            lamport: 1,
            author: device_id(1),
            ts: 0,
            op,
        };

        base(SpaceOpKind::Add {
            asset: SpaceAssetMeta {
                uuid: Uuid::new_v4(),
                file_name: "a.png".into(),
                kind: SpaceAssetKind::Image,
                size_bytes: 10,
                content_hash: vec![0u8; 32],
            },
        })
        .validate()
        .unwrap();

        // 摘要长度错 / 文件名不安全 → BadAsset。
        let mut bad_hash = base(SpaceOpKind::Add {
            asset: SpaceAssetMeta {
                uuid: Uuid::new_v4(),
                file_name: "a.png".into(),
                kind: SpaceAssetKind::Image,
                size_bytes: 10,
                content_hash: vec![0u8; 31],
            },
        });
        assert_eq!(bad_hash.validate(), Err(SpaceError::BadAsset));
        bad_hash.op = SpaceOpKind::Add {
            asset: SpaceAssetMeta {
                uuid: Uuid::new_v4(),
                file_name: "..\\evil.png".into(),
                kind: SpaceAssetKind::Image,
                size_bytes: 10,
                content_hash: vec![0u8; 32],
            },
        };
        assert_eq!(bad_hash.validate(), Err(SpaceError::BadAsset));

        // serde 锚点：tag 字段名与 snake_case 值锁定。
        let removed = Uuid::new_v4();
        let json = serde_json::to_string(&base(SpaceOpKind::Remove {
            asset_uuid: removed,
        }))
        .unwrap();
        assert!(json.contains(r#""kind":"remove""#));
        let back: SpaceOp = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.op,
            SpaceOpKind::Remove {
                asset_uuid: removed
            }
        );

        // 作者 id 非 z32 → BadAuthor。
        let mut bad_author = base(SpaceOpKind::CategoryCreate {
            name: "新品".into(),
        });
        bad_author.author = "short".into();
        assert_eq!(bad_author.validate(), Err(SpaceError::BadAuthor));
    }

    /// 测试辅助：从 op 载荷取回 asset_uuid（Remove/归类/改名共用寻址）。

    #[test]
    fn permits_matrix() {
        let mut space = ready_registry().create_space("团队素材", 1000).unwrap();
        let owner = device_id(1);
        let member = device_id(2);
        space.members.push(RosterMember {
            id: member.clone(),
            name: "客服小李".into(),
        });
        let outsider = device_id(3);
        let asset = Uuid::new_v4();

        let make = |author: String, op: SpaceOpKind| SpaceOp {
            space_id: space.id,
            lamport: 1,
            author,
            ts: 0,
            op,
        };
        let add = |author: &str| {
            make(
                author.to_string(),
                SpaceOpKind::Add {
                    asset: SpaceAssetMeta {
                        uuid: asset,
                        file_name: "a.png".into(),
                        kind: SpaceAssetKind::Image,
                        size_bytes: 10,
                        content_hash: vec![0u8; 32],
                    },
                },
            )
        };
        let remove = |author: &str| {
            make(
                author.to_string(),
                SpaceOpKind::Remove { asset_uuid: asset },
            )
        };

        // 人人可添（成员与空间主都行），外人不可。
        assert!(add(&owner).permits(&space, &HashMap::new()));
        assert!(add(&member).permits(&space, &HashMap::new()));
        assert!(!add(&outsider).permits(&space, &HashMap::new()));

        // Remove：添加者本人可、其他成员不可、空间主恒可、外人不可。
        let mut added_by = HashMap::new();
        added_by.insert(asset, member.clone());
        assert!(remove(&member).permits(&space, &added_by));
        assert!(remove(&owner).permits(&space, &HashMap::new()));
        assert!(!remove(&outsider).permits(&space, &added_by));
        // 在册但不是添加者的其他成员不可 Remove。
        let other = device_id(9);
        space.members.push(RosterMember {
            id: other.clone(),
            name: String::new(),
        });
        assert!(!remove(&other).permits(&space, &added_by));

        // 结构/名册/转让 = 仅空间主。
        let structure = |author: &str| {
            make(
                author.to_string(),
                SpaceOpKind::CategoryCreate {
                    name: "新品".into(),
                },
            )
        };
        assert!(structure(&owner).permits(&space, &HashMap::new()));
        assert!(!structure(&member).permits(&space, &HashMap::new()));
    }

    #[test]
    fn lamport_merge_never_regresses() {
        assert_eq!(merge_lamport(5, 3), 5);
        assert_eq!(merge_lamport(5, 5), 6);
        assert_eq!(merge_lamport(5, 9), 10);
    }
}
