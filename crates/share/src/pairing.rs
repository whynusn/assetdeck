//! 配对码（D80-M1-c）：设备标识的短人类可核指纹。
//!
//! 52 位 z32 标识无法口算比对——配对码 = SHA-256(id) 截断 5 字节 → 与设备
//! 标识同字母表的 8 字符（40 位，抄写碰撞需 2⁴⁰）。两侧管理弹窗各显本机码，
//! 配对完成后对方行也显对端码，用户比对确认「配对的是同一台设备」。
//! 纯函数零 IO：码只做展示核对，不参与任何判定（信任收口仍是显式配对册）。

use sha2::{Digest, Sha256};

/// 与设备标识一致的 z32 字母表（base32 小写字母数字表），复制粘贴兼容。
/// 配对码只取前 32 槽（5bit 索引 0..31），尾 4 字符不参与编码；
/// invite.rs（邀请码的域段编码）复用同一张表。
pub const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// 配对码长度：5 字节 40 位 → 8 × 5bit。
pub const PAIRING_CODE_LEN: usize = 8;

/// 设备标识 → 8 字符配对码（SHA-256 截断 40 位，z32 字母表）。
/// 入参不校验形状：任何串都有稳定码（展示用指纹，不参与判定）。
pub fn pairing_code(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    let mut code = String::with_capacity(PAIRING_CODE_LEN);
    let mut acc: u64 = 0;
    let mut bits = 0u32;
    for byte in digest.iter().take(5) {
        acc = (acc << 8) | u64::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((acc >> bits) & 0x1f) as usize;
            code.push(ALPHABET[index] as char);
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_id(seed: u8) -> String {
        (0..52)
            .map(|i| ALPHABET[(seed as usize + i) % 36] as char)
            .collect()
    }

    #[test]
    fn pairing_code_is_stable_and_fingerprint_like() {
        let id = device_id(3);
        let code = pairing_code(&id);
        assert_eq!(code.len(), PAIRING_CODE_LEN);
        // 确定性：同 id 同码。
        assert_eq!(pairing_code(&id), code);
        // z32 字母表。
        assert!(code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        // 不同 id 不同码（指纹语义）。
        assert_ne!(pairing_code(&device_id(4)), code);
        // 非法形状串也有稳定码（展示用，不校验入参）。
        assert_eq!(pairing_code("junk"), pairing_code("junk"));
        assert_eq!(pairing_code("").len(), PAIRING_CODE_LEN);
    }
}
