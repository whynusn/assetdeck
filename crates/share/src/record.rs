//! 共享记录（D80）：发送/接收批次的结局真源。
//!
//! 瓦片角标（传输中/已送达/失败）与共享记录面消费同一份类型——状态不设第二真源。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 记录方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareDirection {
    Sent,
    Received,
}

/// 单批共享结局。`Failed.reason` 是人话摘要（VM 层文案口径），不是诊断串；
/// 诊断细节落 worker 日志（D39 纪律），不进记录面。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ShareOutcome {
    /// 已排队 / 握手 / 传输中。
    Transferring,
    /// 对端确认且批次完结。exact 重复被跳过亦算送达——D65 幂等语义。
    Delivered,
    Failed {
        reason: String,
    },
}

/// 一次推送或接收的批次记录（共享记录面的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRecord {
    pub batch_id: Uuid,
    pub direction: ShareDirection,
    /// 对端设备显示名（当时快照，改名不回溯）。
    pub peer_name: String,
    pub asset_uuids: Vec<Uuid>,
    pub outcome: ShareOutcome,
    pub started_unix: i64,
    pub finished_unix: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(outcome: ShareOutcome) -> ShareRecord {
        ShareRecord {
            batch_id: Uuid::new_v4(),
            direction: ShareDirection::Sent,
            peer_name: "张越的机器".to_string(),
            asset_uuids: vec![Uuid::new_v4(), Uuid::new_v4()],
            outcome,
            started_unix: 1_757_000_000,
            finished_unix: None,
        }
    }

    #[test]
    fn outcome_states_roundtrip_snake_case() {
        for outcome in [
            ShareOutcome::Transferring,
            ShareOutcome::Delivered,
            ShareOutcome::Failed {
                reason: "对方设备未运行".to_string(),
            },
        ] {
            let r = record(outcome.clone());
            let json = serde_json::to_string(&r).expect("序列化");
            assert!(
                json.contains("\"state\":\"transferring\"")
                    || json.contains("\"state\":\"delivered\"")
                    || json.contains("\"state\":\"failed\""),
                "状态应为 snake_case：{json}"
            );
            let back: ShareRecord = serde_json::from_str(&json).expect("反序列化");
            assert_eq!(back, r);
        }
    }

    #[test]
    fn direction_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&ShareDirection::Sent).expect("序列化"),
            "\"sent\""
        );
        assert_eq!(
            serde_json::to_string(&ShareDirection::Received).expect("序列化"),
            "\"received\""
        );
    }
}
