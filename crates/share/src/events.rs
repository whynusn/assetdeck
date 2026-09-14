//! worker → UI stdout 事件行的解析端（tools/share-worker proto::out 构造端
//! 的对偶）。契约在 share-worker/src/proto.rs 头注释，此处只有一个消费者
//! （ui-viewmodels 的共享 VM），放在本 crate 让 VM 层零 tokio/iroh。
//!
//! 解析纪律：坏行**不致命**——返回 [`WorkerEvent::Unknown`] 携带原文，UI 侧
//! 记日志/警示即可；协议新增行种不得让旧 UI 丢消息。

use std::path::PathBuf;

use uuid::Uuid;

use crate::manifest::TransferManifest;
use crate::request::{is_device_id, DeviceEntry};
use crate::sync::SyncMessage;

/// 逐项结果的方向标记（ITEM 行第三段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRole {
    Send,
    Recv,
}

/// done 行终态。`rejected` 单列：拒收是对端显式决定，UI 文案要区别于故障。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoneState {
    /// 接收侧完成暂存+导入。
    Transferred,
    /// 发送侧收到 DELIVERED。
    Delivered,
    Rejected(String),
    Failed(String),
}

/// 一行 worker 输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    Ready {
        id_z32: String,
        name: String,
    },
    Device(DeviceEntry),
    /// 一轮 mDNS 浏览完成（SCAN_DONE）：UI 据此撤扫描态。
    ScanDone,
    Incoming(TransferManifest),
    Staged {
        batch_id: Uuid,
        paths: Vec<PathBuf>,
    },
    Progress {
        batch_id: Uuid,
        sent: u64,
        total: u64,
    },
    Item {
        batch_id: Uuid,
        role: ItemRole,
        uuid: Uuid,
        ok: bool,
        detail: String,
    },
    Done {
        batch_id: Uuid,
        state: DoneState,
        detail: String,
    },
    /// NOTICE 行（D64 前缀约定由 worker 负责，此处只剥头）。
    Notice(String),
    /// 同步通道（M1-b）：来自对端的报文（我方 Pull 的应答，或对端的在线增量）。
    /// 发送方身份由 worker 从 QUIC 连接取（TLS 证书背书，非报文自报）。
    SyncRecv {
        peer_id: String,
        message: SyncMessage,
    },
    /// 同步通道（M1-b）：我方发出的报文的结局（成功 detail 为空；失败仅
    /// 知情——推送丢失由对端上线补拉自愈，不重试）。
    SyncSent {
        peer_id: String,
        ok: bool,
        detail: String,
    },
    /// 空行以外的未知行：原文保留供日志。
    Unknown(String),
}

fn bad(line: &str) -> WorkerEvent {
    WorkerEvent::Unknown(line.to_string())
}

/// 解析一行 stdout。行尾 `\r` 容忍（管道跨进程时的历史包袱）。
pub fn parse_worker_line(line: &str) -> WorkerEvent {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() {
        return WorkerEvent::Unknown(String::new());
    }
    let mut fields = line.split('\t');
    let head = fields.next().unwrap_or_default();
    match head {
        "READY" => {
            let (Some(id_z32), Some(name)) = (fields.next(), fields.next()) else {
                return bad(line);
            };
            WorkerEvent::Ready {
                id_z32: id_z32.to_string(),
                name: name.to_string(),
            }
        }
        "DEVICE" => {
            let (Some(id), Some(name), Some(addr_text)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return bad(line);
            };
            let addrs: Vec<String> = addr_text
                .split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            let entry = DeviceEntry {
                id: id.to_string(),
                name: name.to_string(),
                addrs,
            };
            // 无任何可拨地址的「设备」不是可发现设备（worker 侧已过滤，UI 防御性兜底）。
            match entry.validate() {
                Ok(()) if !entry.addrs.is_empty() => WorkerEvent::Device(entry),
                _ => bad(line),
            }
        }
        "INCOMING" => {
            let Some(json) = fields.next() else {
                return bad(line);
            };
            match serde_json::from_str::<TransferManifest>(json) {
                Ok(manifest) => WorkerEvent::Incoming(manifest),
                Err(_) => bad(line),
            }
        }
        "STAGED" => {
            let (Some(batch_raw), Some(paths_json)) = (fields.next(), fields.next()) else {
                return bad(line);
            };
            let Ok(batch_id) = batch_raw.parse::<Uuid>() else {
                return bad(line);
            };
            match serde_json::from_str::<Vec<String>>(paths_json) {
                Ok(paths) => WorkerEvent::Staged {
                    batch_id,
                    paths: paths.into_iter().map(PathBuf::from).collect(),
                },
                Err(_) => bad(line),
            }
        }
        "PROGRESS" => {
            let (Some(batch_raw), Some(sent), Some(total)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return bad(line);
            };
            match (
                batch_raw.parse::<Uuid>(),
                sent.parse::<u64>(),
                total.parse::<u64>(),
            ) {
                (Ok(batch_id), Ok(sent), Ok(total)) => WorkerEvent::Progress {
                    batch_id,
                    sent,
                    total,
                },
                _ => bad(line),
            }
        }
        "ITEM" => {
            let (Some(batch_raw), Some(role), Some(uuid_raw), Some(ok), detail) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                return bad(line);
            };
            let role = match role {
                "send" => ItemRole::Send,
                "recv" => ItemRole::Recv,
                _ => return bad(line),
            };
            let ok = match ok {
                "ok" => true,
                "failed" => false,
                _ => return bad(line),
            };
            match (batch_raw.parse::<Uuid>(), uuid_raw.parse::<Uuid>()) {
                (Ok(batch_id), Ok(uuid)) => WorkerEvent::Item {
                    batch_id,
                    role,
                    uuid,
                    ok,
                    detail: detail.unwrap_or_default().to_string(),
                },
                _ => bad(line),
            }
        }
        "done" => {
            let (Some(batch_raw), Some(state)) = (fields.next(), fields.next()) else {
                return bad(line);
            };
            let detail = fields.next().unwrap_or_default().to_string();
            let Ok(batch_id) = batch_raw.parse::<Uuid>() else {
                return bad(line);
            };
            let parsed = match state {
                "transferred" => Some(DoneState::Transferred),
                "delivered" => Some(DoneState::Delivered),
                "rejected" => Some(DoneState::Rejected(detail.clone())),
                "failed" => Some(DoneState::Failed(detail.clone())),
                _ => None,
            };
            match parsed {
                Some(state) => WorkerEvent::Done {
                    batch_id,
                    state,
                    detail,
                },
                None => bad(line),
            }
        }
        "NOTICE" => {
            // 消息本体可能含 TAB：把剩余字段原样拼回。
            let text = rest_after(line, "NOTICE\t");
            WorkerEvent::Notice(text)
        }
        "SCAN_DONE" => WorkerEvent::ScanDone,
        "SYNC_RECV" => {
            let (Some(peer_id), Some(json)) = (fields.next(), fields.next()) else {
                return bad(line);
            };
            if !is_device_id(peer_id) {
                return bad(line);
            }
            match serde_json::from_str::<SyncMessage>(json) {
                Ok(message) if message.validate().is_ok() => WorkerEvent::SyncRecv {
                    peer_id: peer_id.to_string(),
                    message,
                },
                _ => bad(line),
            }
        }
        "SYNC_SENT" => {
            let (Some(peer_id), Some(ok)) = (fields.next(), fields.next()) else {
                return bad(line);
            };
            if !is_device_id(peer_id) {
                return bad(line);
            }
            let ok = match ok {
                "ok" => true,
                "failed" => false,
                _ => return bad(line),
            };
            WorkerEvent::SyncSent {
                peer_id: peer_id.to_string(),
                ok,
                detail: fields.next().unwrap_or_default().to_string(),
            }
        }
        _ => bad(line),
    }
}

fn rest_after(line: &str, prefix: &str) -> String {
    line.strip_prefix(prefix)
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_and_device_roundtrip_shape() {
        assert_eq!(
            parse_worker_line("READY\tabcd\tx\t本机"),
            WorkerEvent::Ready {
                id_z32: "abcd".into(),
                name: "x".into()
            }
        );
        let id = "d".repeat(52);
        let ev = parse_worker_line(&format!(
            "DEVICE\t{id}\t小李的机器\t192.168.1.8:4433;[fe80::1]:4433"
        ));
        match ev {
            WorkerEvent::Device(d) => {
                assert_eq!(d.name, "小李的机器");
                assert_eq!(d.addrs.len(), 2);
                assert_eq!(d.addrs[0], "192.168.1.8:4433");
            }
            other => panic!("应为 DEVICE，实得 {other:?}"),
        }
    }

    #[test]
    fn device_validation_rejects_garbage() {
        // 短 id / 空地址 都必须落 Unknown 而不是伪装成设备。
        assert!(matches!(
            parse_worker_line("DEVICE\tshort\t名\t1.2.3.4:5"),
            WorkerEvent::Unknown(_)
        ));
        assert!(matches!(
            parse_worker_line("DEVICE\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t名\t"),
            WorkerEvent::Unknown(_)
        ));
    }

    #[test]
    fn staged_progress_item_done_parse() {
        let batch = Uuid::new_v4();
        let asset = Uuid::nil();
        let ev = parse_worker_line(&format!(
            "STAGED\t{batch}\t[\"C:\\\\share-inbox\\\\a.png\"]"
        ));
        match ev {
            WorkerEvent::Staged { batch_id, paths } => {
                assert_eq!(batch_id, batch);
                assert_eq!(paths[0], PathBuf::from(r"C:\share-inbox\a.png"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_worker_line(&format!("PROGRESS\t{batch}\t1024\t4096")),
            WorkerEvent::Progress {
                batch_id: batch,
                sent: 1024,
                total: 4096
            }
        );
        assert_eq!(
            parse_worker_line(&format!("ITEM\t{batch}\tsend\t{asset}\tfailed\t磁盘满")),
            WorkerEvent::Item {
                batch_id: batch,
                role: ItemRole::Send,
                uuid: asset,
                ok: false,
                detail: "磁盘满".into()
            }
        );
        let ev = parse_worker_line(&format!("done\t{batch}\trejected\t不想要"));
        match ev {
            WorkerEvent::Done {
                batch_id, state, ..
            } => {
                assert_eq!(batch_id, batch);
                assert_eq!(state, DoneState::Rejected("不想要".into()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn notice_keeps_tabs_and_unknown_is_not_fatal() {
        assert_eq!(
            parse_worker_line("NOTICE\t警示：发现\t制表符"),
            WorkerEvent::Notice("警示：发现\t制表符".into())
        );
        assert_eq!(
            parse_worker_line("SOMETHING\tnew"),
            WorkerEvent::Unknown("SOMETHING\tnew".into())
        );
        assert!(matches!(parse_worker_line(""), WorkerEvent::Unknown(_)));
        assert!(matches!(
            parse_worker_line("PROGRESS\tx\ty"),
            WorkerEvent::Unknown(_)
        ));
    }

    #[test]
    fn sync_lines_parse_and_reject_bad_shapes() {
        let peer = "a".repeat(52);
        let snapshot = SyncMessage::Snapshot {
            rev: 6,
            offers: vec![],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            parse_worker_line(&format!("SYNC_RECV\t{peer}\t{json}")),
            WorkerEvent::SyncRecv {
                peer_id: peer.clone(),
                message: snapshot
            }
        );
        assert_eq!(
            parse_worker_line(&format!("SYNC_SENT\t{peer}\tfailed\t连接超时")),
            WorkerEvent::SyncSent {
                peer_id: peer.clone(),
                ok: false,
                detail: "连接超时".into()
            }
        );
        assert_eq!(
            parse_worker_line(&format!("SYNC_SENT\t{peer}\tok\t")),
            WorkerEvent::SyncSent {
                peer_id: peer.clone(),
                ok: true,
                detail: String::new()
            }
        );

        // 坏形状全落 Unknown：短 id、非法状态字、报文过不了契约校验。
        assert!(matches!(
            parse_worker_line("SYNC_RECV\tshort\t{\"kind\":\"pull\",\"since_rev\":0}"),
            WorkerEvent::Unknown(_)
        ));
        assert!(matches!(
            parse_worker_line(&format!("SYNC_SENT\t{peer}\tmaybe\tx")),
            WorkerEvent::Unknown(_)
        ));
        assert!(matches!(
            parse_worker_line(&format!(
                "SYNC_RECV\t{peer}\t{{\"kind\":\"delta\",\"from_rev\":5,\"to_rev\":5,\"added\":[],\"removed\":[]}}"
            )),
            WorkerEvent::Unknown(_)
        ));
    }
}
