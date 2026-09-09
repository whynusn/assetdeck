//! 共享注册表（D80-M1-a 批2）：控制面三类状态的持久化契约。
//!
//! 只定类型与 JSON 形状，零 IO——落盘由 app-ui 用 `atomic_write` 完成
//! （settings/targets.json 同款：库根优先回退 exe 旁，tmp+rename 原子覆盖）。
//! 一个文件一个 `ShareRegistry`：设备册 + 域册 + 共享态，整体序列化；
//! 控制面数据量级是「台数 × 域数 × 共享条数」，远小于素材库，整体写
//! 没有性能问题，不需要分文件或增量格式。

use serde::{Deserialize, Serialize};

use crate::domain::{PairedDevices, ShareDomain};
use crate::sharing::SharedState;

/// 控制面持久化单元（share_registry.json 的文档形状）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRegistry {
    pub devices: PairedDevices,
    pub domains: Vec<ShareDomain>,
    pub shared: SharedState,
}

impl ShareRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 序列化为落盘文本（同 alias 册的 to_json 模式：壳层只管原子写）。
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 从落盘文本恢复；解析失败 = None（调用方按空册起步，不 panic 不半载）。
    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DomainKind, PairedDevice};
    use crate::request::is_device_id;

    fn device_id(seed: u8) -> String {
        let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
        (0..52)
            .map(|i| alphabet[(seed as usize + i) % 36] as char)
            .collect()
    }

    #[test]
    fn registry_json_roundtrip_keeps_all_three_sections() {
        let mut registry = ShareRegistry::new();
        assert!(registry.devices.pair(PairedDevice {
            id: device_id(1),
            name: "客厅机".to_string(),
            paired_at: 100,
        }));
        registry.domains.push(ShareDomain {
            id: uuid::Uuid::new_v4(),
            name: "家庭组".to_string(),
            kind: DomainKind::Group,
            members: vec![device_id(1), device_id(2)],
        });
        registry
            .shared
            .mark(uuid::Uuid::new_v4(), registry.domains[0].id, 42);
        let expected_rev = registry.shared.rev();

        let json = serde_json::to_string_pretty(&registry).unwrap();
        let back: ShareRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, registry);
        assert_eq!(back.shared.rev(), expected_rev);
        assert!(is_device_id(&back.devices.iter().next().unwrap().id));
    }

    #[test]
    fn empty_registry_is_default_shape() {
        let json = serde_json::to_string(&ShareRegistry::new()).unwrap();
        let back: ShareRegistry = serde_json::from_str(&json).unwrap();
        assert!(back.devices.is_empty());
        assert!(back.domains.is_empty());
        assert_eq!(back.shared.rev(), 0);
    }
}
