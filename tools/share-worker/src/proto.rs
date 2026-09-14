//! stdin/stdout 行协议（D80 / D80-M1）：
//!
//! stdin 命令（UI → worker）：
//! - `SCAN` —— 触发一轮 mDNS 浏览，结果以 `DEVICE` 行回报
//! - `SEND\t<SendRequest JSON>` —— 发起一次显式推送（JSON 单行）
//! - `ACCEPT\t<batch_id>` / `REJECT\t<batch_id>\t<原因>` —— 接收侧首连确认
//! - `ACK\t<batch_id>` / `NACK\t<batch_id>\t<原因>` —— 导入管线收尾回执
//! - `SYNC_STATE\t<SyncBookState JSON>` —— 整册替换「我共享给谁什么」
//!   （M1-b；worker 应答 PULL 的唯一数据源）
//! - `SYNC_SEND\t<对端id>\t<addr;addr>\t<SyncMessage JSON>` —— 发出一条
//!   同步报文（M1-b；Pull 等应答，Delta 即发即收）
//! - `EXIT` —— 优雅退出（stdin 关闭/EOF 同效：父进程死则 worker 死）
//!
//! stdout 事件（worker → UI）：
//! - `READY\t<id_z32>\t<设备名>` —— endpoint 就绪
//! - `DEVICE\t<id_z32>\t<名称>\t<addr;addr>` —— 扫描命中
//! - `SCAN_DONE` —— 一轮 mDNS 浏览完成（UI 据此撤扫描态，不靠定时器）
//! - `INCOMING\t<TransferManifest JSON>` —— 收到推送，待首连确认
//! - `STAGED\t<batch_id>\t<[路径] JSON>` —— 文件落暂存区，待导入
//! - `PROGRESS\t<batch_id>\t<已发>\t<总计>` —— 发送侧节流进度
//! - `ITEM\t<batch_id>\t<send|recv>\t<uuid>\t<ok|failed>\t<明细>` —— 逐项结果
//! - `done\t<batch_id>\t<transferred|delivered|rejected|failed>\t<明细>`
//! - `SYNC_RECV\t<对端id>\t<SyncMessage JSON>` —— 同步报文到达（M1-b；
//!   对端身份取自 QUIC 握手，非报文自报）
//! - `SYNC_SENT\t<对端id>\t<ok|failed>\t<明细>` —— 我方同步报文结局
//!   （M1-b；推送丢失由对端补拉自愈，仅知情不重试）
//! - `NOTICE\t<提示：…|警示：…>` —— 用户知情消息（D64 前缀约定）
//!
//! stderr 归日志门面（D39），不承载协议。

use uuid::Uuid;

use share::{is_device_id, SendRequest, SyncBookState, SyncMessage};

/// stdin 一条命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Scan,
    Send(SendRequest),
    Accept(Uuid),
    Reject(Uuid, String),
    Ack(Uuid),
    Nack(Uuid, String),
    /// M1-b：整册替换「我共享给谁什么」。
    SyncState(SyncBookState),
    /// M1-b：发出一条同步报文。
    SyncSend {
        peer_id: String,
        addrs: Vec<String>,
        message: SyncMessage,
    },
    Exit,
}

/// 解析失败（协议违约行）：不退出，记日志丢弃——stdin 混入脏行不应当
/// 弄死整个 worker（导入期 UI 侧 bug 不该演化成共享通道失联）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError(pub String);

pub fn parse_command(line: &str) -> Result<Command, CommandError> {
    let (head, rest) = match line.split_once('\t') {
        Some((head, rest)) => (head, Some(rest)),
        None => (line, None),
    };
    match head {
        "SCAN" => Ok(Command::Scan),
        "EXIT" => Ok(Command::Exit),
        "SEND" => {
            let json = rest.ok_or(CommandError("SEND 缺 JSON 载荷".into()))?;
            let request: SendRequest = serde_json::from_str(json)
                .map_err(|e| CommandError(format!("SEND JSON 解析失败: {e}")))?;
            request
                .validate()
                .map_err(|e| CommandError(format!("SEND 请求不合法: {e}")))?;
            Ok(Command::Send(request))
        }
        "ACCEPT" | "REJECT" | "ACK" | "NACK" => {
            let rest = rest.ok_or_else(|| CommandError(format!("{head} 缺 batch_id")))?;
            let (batch_raw, reason) = match rest.split_once('\t') {
                Some((id, reason)) => (id, Some(reason)),
                None => (rest, None),
            };
            let batch_id: Uuid = batch_raw
                .parse()
                .map_err(|_| CommandError(format!("{head} batch_id 不合法: {batch_raw}")))?;
            match head {
                "ACCEPT" => Ok(Command::Accept(batch_id)),
                "ACK" => Ok(Command::Ack(batch_id)),
                _ => {
                    let reason = reason.unwrap_or("接收方拒绝").trim().to_string();
                    let reason = if reason.is_empty() {
                        "接收方拒绝".to_string()
                    } else {
                        reason
                    };
                    if head == "REJECT" {
                        Ok(Command::Reject(batch_id, reason))
                    } else {
                        Ok(Command::Nack(batch_id, reason))
                    }
                }
            }
        }
        "SYNC_STATE" => {
            let json = rest.ok_or(CommandError("SYNC_STATE 缺 JSON 载荷".into()))?;
            let book: SyncBookState = serde_json::from_str(json)
                .map_err(|e| CommandError(format!("SYNC_STATE JSON 解析失败: {e}")))?;
            book.validate()
                .map_err(|e| CommandError(format!("SYNC_STATE 册不合法: {e}")))?;
            Ok(Command::SyncState(book))
        }
        "SYNC_SEND" => {
            let rest = rest.ok_or(CommandError("SYNC_SEND 缺载荷".into()))?;
            let (peer_id, rest) = rest
                .split_once('\t')
                .ok_or(CommandError("SYNC_SEND 缺地址段".into()))?;
            if !is_device_id(peer_id) {
                return Err(CommandError(format!("SYNC_SEND 设备标识不合法: {peer_id}")));
            }
            let (addr_text, json) = rest
                .split_once('\t')
                .ok_or(CommandError("SYNC_SEND 缺报文段".into()))?;
            let addrs: Vec<String> = addr_text
                .split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            let message: SyncMessage = serde_json::from_str(json)
                .map_err(|e| CommandError(format!("SYNC_SEND 报文解析失败: {e}")))?;
            message
                .validate()
                .map_err(|e| CommandError(format!("SYNC_SEND 报文不合法: {e}")))?;
            Ok(Command::SyncSend {
                peer_id: peer_id.to_string(),
                addrs,
                message,
            })
        }
        other => Err(CommandError(format!("未知命令: {other}"))),
    }
}

/// stdout 事件行构造集中在同一处，协议字段顺序只有一个真相。
pub mod out {
    use uuid::Uuid;

    pub fn ready(id_z32: &str, name: &str) -> String {
        format!("READY\t{id_z32}\t{name}")
    }

    pub fn device(id_z32: &str, name: &str, addrs: &[String]) -> String {
        format!("DEVICE\t{id_z32}\t{name}\t{}", addrs.join(";"))
    }

    pub fn scan_done() -> String {
        "SCAN_DONE".to_string()
    }

    pub fn incoming(manifest_json: &str) -> String {
        format!("INCOMING\t{manifest_json}")
    }

    pub fn staged(batch_id: Uuid, paths_json: &str) -> String {
        format!("STAGED\t{batch_id}\t{paths_json}")
    }

    pub fn progress(batch_id: Uuid, sent: u64, total: u64) -> String {
        format!("PROGRESS\t{batch_id}\t{sent}\t{total}")
    }

    pub fn item(batch_id: Uuid, role: &str, uuid: Uuid, ok: bool, detail: &str) -> String {
        format!(
            "ITEM\t{batch_id}\t{role}\t{uuid}\t{}\t{detail}",
            if ok { "ok" } else { "failed" }
        )
    }

    pub fn done(batch_id: Uuid, state: &str, detail: &str) -> String {
        format!("done\t{batch_id}\t{state}\t{detail}")
    }

    pub fn notice(text: &str) -> String {
        format!("NOTICE\t{text}")
    }

    /// 同步报文到达（对端身份来自 QUIC 握手，worker 侧已核 z32 形状）。
    pub fn sync_recv(peer_id: &str, message_json: &str) -> String {
        format!("SYNC_RECV\t{peer_id}\t{message_json}")
    }

    pub fn sync_sent(peer_id: &str, ok: bool, detail: &str) -> String {
        format!(
            "SYNC_SENT\t{peer_id}\t{}\t{detail}",
            if ok { "ok" } else { "failed" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::AssetKind;
    use share::SendItem;
    use std::path::PathBuf;

    fn request() -> SendRequest {
        SendRequest {
            batch_id: Uuid::new_v4(),
            sender_name: "WS".into(),
            receiver_id: "a".repeat(52),
            receiver_addrs: vec!["192.168.1.8:40000".into()],
            items: vec![SendItem {
                asset_uuid: Uuid::new_v4(),
                file_name: "a.png".into(),
                kind: AssetKind::Image,
                size_bytes: 10,
                category_hint: None,
                path: PathBuf::from(r"C:\x\a.png"),
            }],
        }
    }

    #[test]
    fn bare_commands_parse() {
        assert_eq!(parse_command("SCAN"), Ok(Command::Scan));
        assert_eq!(parse_command("EXIT"), Ok(Command::Exit));
        let err = parse_command("HELLO").unwrap_err();
        assert!(err.0.contains("未知命令"));
    }

    #[test]
    fn send_roundtrip_via_json() {
        let req = request();
        let line = format!("SEND\t{}", serde_json::to_string(&req).unwrap());
        assert_eq!(parse_command(&line), Ok(Command::Send(req)));
    }

    #[test]
    fn send_rejects_invalid_request() {
        let mut req = request();
        req.items.clear();
        let line = format!("SEND\t{}", serde_json::to_string(&req).unwrap());
        assert!(parse_command(&line).is_err());
        assert!(parse_command("SEND\tnot-json").is_err());
        assert!(parse_command("SEND").is_err());
    }

    #[test]
    fn decision_commands_parse() {
        let id = Uuid::new_v4();
        assert_eq!(
            parse_command(&format!("ACCEPT\t{id}")),
            Ok(Command::Accept(id))
        );
        assert_eq!(
            parse_command(&format!("REJECT\t{id}\t不要")),
            Ok(Command::Reject(id, "不要".into()))
        );
        // 缺原因给默认文案；空原因同理。
        assert_eq!(
            parse_command(&format!("REJECT\t{id}")),
            Ok(Command::Reject(id, "接收方拒绝".into()))
        );
        assert_eq!(parse_command(&format!("ACK\t{id}")), Ok(Command::Ack(id)));
        assert_eq!(
            parse_command(&format!("NACK\t{id}\t分类失败")),
            Ok(Command::Nack(id, "分类失败".into()))
        );
        assert!(parse_command("ACCEPT\tzzz").is_err());
        assert!(parse_command("ACK").is_err());
    }

    #[test]
    fn out_lines_have_stable_field_order() {
        let id = Uuid::new_v4();
        let uuid = Uuid::nil();
        assert!(out::ready("abc", "本机").starts_with("READY\tabc\t本机"));
        assert_eq!(
            out::device("abc", "本机", &["1.2.3.4:5".into(), "[::1]:6".into()]),
            "DEVICE\tabc\t本机\t1.2.3.4:5;[::1]:6"
        );
        assert!(out::staged(id, r#"["C:\\s\\a.png"]"#).starts_with("STAGED\t"));
        assert!(out::progress(id, 1, 2).contains('\t'));
        assert!(out::item(id, "send", uuid, false, "io").contains("\tfailed\tio"));
        assert!(out::done(id, "delivered", "").starts_with("done\t"));
        assert!(out::notice("提示：hi").starts_with("NOTICE\t"));
        let peer = "a".repeat(52);
        assert!(out::sync_recv(&peer, "{}").starts_with(&format!("SYNC_RECV\t{peer}\t")));
        assert_eq!(
            out::sync_sent(&peer, false, "超时"),
            format!("SYNC_SENT\t{peer}\tfailed\t超时")
        );
    }

    #[test]
    fn sync_commands_parse_and_validate() {
        let peer = "a".repeat(52);
        // SYNC_STATE：合法册整册通过；坏键/坏文件名被契约校验拦下。
        let mut book = SyncBookState::default();
        book.set(
            peer.clone(),
            share::PeerSyncState {
                rev: 3,
                offers: vec![share::ShareOffer {
                    asset_uuid: Uuid::new_v4(),
                    file_name: "a.png".into(),
                    kind: domain::AssetKind::Image,
                    size_bytes: 8,
                }],
            },
        );
        let line = format!("SYNC_STATE\t{}", serde_json::to_string(&book).unwrap());
        assert_eq!(parse_command(&line), Ok(Command::SyncState(book)));
        assert!(parse_command("SYNC_STATE\tnot-json").is_err());
        assert!(parse_command("SYNC_STATE").is_err());
        assert!(parse_command("SYNC_STATE\t{\"peers\":{\"short\":{}}}").is_err());

        // SYNC_SEND：id/地址/报文三段齐备才通过；区间倒挂的报文被拦。
        let message = SyncMessage::Delta {
            from_rev: 4,
            to_rev: 5,
            added: vec![share::ShareOffer {
                asset_uuid: Uuid::new_v4(),
                file_name: "b.png".into(),
                kind: domain::AssetKind::Video,
                size_bytes: 16,
            }],
            removed: vec![Uuid::new_v4()],
        };
        let line = format!(
            "SYNC_SEND\t{peer}\t192.168.1.8:4433;[::1]:4433\t{}",
            serde_json::to_string(&message).unwrap()
        );
        assert_eq!(
            parse_command(&line),
            Ok(Command::SyncSend {
                peer_id: peer.clone(),
                addrs: vec!["192.168.1.8:4433".into(), "[::1]:4433".into()],
                message
            })
        );
        let bad = SyncMessage::Delta {
            from_rev: 5,
            to_rev: 4,
            added: vec![],
            removed: vec![],
        };
        let line = format!(
            "SYNC_SEND\t{peer}\t192.168.1.8:4433\t{}",
            serde_json::to_string(&bad).unwrap()
        );
        assert!(parse_command(&line).is_err());
        assert!(parse_command("SYNC_SEND\tshort\t1.2.3.4:5\t{}").is_err());
        assert!(parse_command(&format!("SYNC_SEND\t{peer}\t1.2.3.4:5")).is_err());
        assert!(parse_command("SYNC_SEND").is_err());
    }
}
