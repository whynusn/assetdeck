//! 邀请机制（D80-M2）：信任入口从「互贴设备标识」收敛为「贴一次邀请码」。
//!
//! 核心洞察：贴码方外拨签发方时，QUIC 握手证书天然交换双方身份——
//! **一次贴码、双向写册**。群组场景再靠群主把签名名册（[`GroupRoster`]）
//! 经同步通道分发给全群，N 人只需 N 次贴码（成员间永不需要直配）。
//!
//! 两种邀请码共用一个输入框，按形状自区分：
//! - **设备邀请码** = 签发方 52 位设备标识本身：贴入 = 互认入册（与
//!   旧「手贴 52 位标识配对」是同一串字符，语义升级为双向互认）；
//! - **群组邀请码** = `{creator_id}.{z32(域 id)}`：贴入 = 互认 + 连群主
//!   加入域（owner 收 [`SyncMessage::JoinInvite`] 后入册并广播名册）。
//!
//! 信任模型修订（M2）：成员册**经群主显式分发的名册**同步（贴邀请码是
//! 显式动作，非静默泄漏）；域 id 即入群凭证（持有邀请码 = 被授权）。
//! 编码自洽：z32 表与 [`crate::pairing`] 共用（前 32 槽），域 id 16 字节
//! → 26 字符。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::manifest::is_safe_display_name;
use crate::request::{is_device_id, is_safe_label};

/// z32 表（与 [`crate::pairing::ALPHABET`] 共用；5 位索引只用前 32 槽）。
const Z32: &[u8; 36] = crate::pairing::ALPHABET;

/// 群组邀请码 = `{creator_id}.{z32(域 id)}`。
pub fn group_invite_code(creator_id: &str, domain_id: Uuid) -> String {
    format!("{creator_id}.{}", z32_encode(domain_id.as_bytes()))
}

/// 解析后的邀请码：creator_id 恒有；domain_id 只有群组邀请码才有。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteCode {
    pub creator_id: String,
    pub domain_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteError {
    /// 邀请码为空或缺少签发方标识。
    Empty,
    /// 签发方标识不是合法 z32 设备标识（52 位小写字母数字）。
    BadCreator,
    /// 群组邀请码的域段不是 26 字符 z32。
    BadDomain,
    /// 超过两段（`a.b.c`）。
    TooManyParts,
}

impl core::fmt::Display for InviteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InviteError::Empty => write!(f, "邀请码为空"),
            InviteError::BadCreator => write!(f, "邀请码的设备标识不合法"),
            InviteError::BadDomain => write!(f, "邀请码的群组段不合法"),
            InviteError::TooManyParts => write!(f, "邀请码格式不合法"),
        }
    }
}

impl std::error::Error for InviteError {}

/// 解析邀请码（设备/群组按形状自区分）。尾部空白由调用方 trim。
pub fn parse_invite_code(code: &str) -> Result<InviteCode, InviteError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(InviteError::Empty);
    }
    let mut parts = code.split('.');
    let creator = parts.next().ok_or(InviteError::Empty)?;
    if !is_device_id(creator) {
        return Err(InviteError::BadCreator);
    }
    let domain_id = match parts.next() {
        None => None,
        Some(domain) => Some(z32_decode(domain).map_err(|_| InviteError::BadDomain)?),
    };
    if parts.next().is_some() {
        return Err(InviteError::TooManyParts);
    }
    Ok(InviteCode {
        creator_id: creator.to_string(),
        domain_id,
    })
}

/// 16 字节（域 id）→ 26 字符 z32（128 位 → ceil(128/5)=26，末组零填充）。
fn z32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(26);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in bytes {
        acc = (acc << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(Z32[((acc >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(Z32[((acc << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// 26 字符 z32 → 16 字节（长度/表外字符即非法）。
fn z32_decode(text: &str) -> Result<Uuid, ()> {
    let bytes = text.as_bytes();
    if bytes.len() != 26 {
        return Err(());
    }
    let mut out = [0u8; 16];
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out_len = 0usize;
    for ch in bytes {
        let index = Z32.iter().take(32).position(|c| c == ch).ok_or(())?;
        acc = (acc << 5) | index as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            // 16 字节 = 128 位 = 25 组 5 位 + 3 位余量；第 26 组只取高 3 位。
            if out_len == 16 {
                return Err(());
            }
            out[out_len] = ((acc >> bits) & 0xff) as u8;
            out_len += 1;
        }
    }
    // 末组填充位必须为零（规范编码，防别名）。
    if bits != 2 || (acc & ((1 << bits) - 1)) != 0 {
        return Err(());
    }
    Ok(Uuid::from_bytes(out))
}

/// 名册成员：设备 id + 展示名（群主设备册里的备注名；接收方按 id 原位
/// 合并 = 自动改名）。签发方自身恒在册（接收方本地域含群主，群内互享
/// 语义成立）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterMember {
    pub id: String,
    pub name: String,
}

/// 群主签发的共享域名册（M2 经同步通道显式分发的唯一域上下文）。
/// `members` 含群主自身；被移出的成员收到不含自己的名册 = 踢出信号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRoster {
    pub domain_id: Uuid,
    pub domain_name: String,
    pub members: Vec<RosterMember>,
}

impl GroupRoster {
    /// 结构性守卫：域名/成员名安全（行协议分隔面）+ 成员 id 形状（z32）
    /// + 无重复。成员是否已配对由接收方控制面裁决（入册即配对）。
    pub fn validate(&self) -> Result<(), InviteError> {
        if !is_safe_label(&self.domain_name) {
            return Err(InviteError::BadDomain);
        }
        for (index, member) in self.members.iter().enumerate() {
            if !is_device_id(&member.id) || self.members[..index].iter().any(|m| m.id == member.id)
            {
                return Err(InviteError::BadCreator);
            }
            // 成员名可选（未知名接收方回落 uuid 前缀），非空则必须安全。
            if !member.name.is_empty() && !is_safe_display_name(&member.name) {
                return Err(InviteError::BadCreator);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_id(seed: u8) -> String {
        (0..52)
            .map(|i| Z32[(seed as usize + i) % 36] as char)
            .collect()
    }

    #[test]
    fn z32_roundtrip_all_zero_and_max_uuids() {
        for uuid in [Uuid::nil(), Uuid::max(), Uuid::new_v4()] {
            let code = z32_encode(uuid.as_bytes());
            assert_eq!(code.len(), 26);
            assert_eq!(z32_decode(&code).unwrap(), uuid);
        }
    }

    #[test]
    fn invite_code_parse_device_and_group_forms() {
        // 设备邀请码 = 52 位标识本身。
        let creator = device_id(1);
        let parsed = parse_invite_code(&creator).unwrap();
        assert_eq!(parsed.creator_id, creator);
        assert_eq!(parsed.domain_id, None);

        // 尾部空白由 parse trim。
        let padded = format!("  {creator}  ");
        assert_eq!(parse_invite_code(&padded).unwrap(), parsed);

        // 群组邀请码 = creator.domain（域 id 经 z32）。
        let domain_id = Uuid::new_v4();
        let code = group_invite_code(&creator, domain_id);
        assert!(code.starts_with(&format!("{creator}.")));
        let parsed = parse_invite_code(&code).unwrap();
        assert_eq!(parsed.creator_id, creator);
        assert_eq!(parsed.domain_id, Some(domain_id));

        // 坏形状各分支。
        let domain_code = z32_encode(domain_id.as_bytes());
        assert_eq!(parse_invite_code(""), Err(InviteError::Empty));
        assert_eq!(parse_invite_code("short"), Err(InviteError::BadCreator));
        assert_eq!(
            parse_invite_code(&format!("{creator}.short")),
            Err(InviteError::BadDomain)
        );
        assert_eq!(
            parse_invite_code(&format!("{creator}.{domain_code}.x")),
            Err(InviteError::TooManyParts)
        );
    }

    #[test]
    fn roster_validate_enforces_shapes() {
        let roster = GroupRoster {
            domain_id: Uuid::new_v4(),
            domain_name: "家庭组".into(),
            members: vec![
                RosterMember {
                    id: device_id(1),
                    name: "群主的电脑".into(),
                },
                RosterMember {
                    id: device_id(2),
                    name: String::new(),
                },
            ],
        };
        roster.validate().unwrap();

        // 重复成员 → 拒。
        let mut bad = roster.clone();
        bad.members.push(RosterMember {
            id: device_id(1),
            name: "重复".into(),
        });
        assert_eq!(bad.validate(), Err(InviteError::BadCreator));

        // 域名不安全 → 拒。
        let mut bad = roster.clone();
        bad.domain_name = "bad\tname".into();
        assert_eq!(bad.validate(), Err(InviteError::BadDomain));

        // 成员 id 非 z32 → 拒。
        let mut bad = roster;
        bad.members[1].id = "short".into();
        assert_eq!(bad.validate(), Err(InviteError::BadCreator));
    }

    #[test]
    fn roster_serde_roundtrip() {
        let roster = GroupRoster {
            domain_id: Uuid::new_v4(),
            domain_name: "家庭组".into(),
            members: vec![RosterMember {
                id: device_id(3),
                name: "客厅机".into(),
            }],
        };
        let json = serde_json::to_string(&roster).unwrap();
        let back: GroupRoster = serde_json::from_str(&json).unwrap();
        assert_eq!(back, roster);
    }
}
