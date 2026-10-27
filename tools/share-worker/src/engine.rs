//! 传输引擎（D80 M0）：清单先行 + 逐文件裸流 + SHA-256 收口校验。
//!
//! 线协议（单条双向流，全部 `\n` 行 + 裸字节段）：
//! 1. 发送方 open_bi → 写 `TransferManifest JSON\n`；
//! 2. 接收方校验清单 → 回 `ACCEPT\n` 或 `REJECT\t<原因>\n`（首连确认在 UI
//!    侧完成，引擎只等 [`DecisionHub`] 的裁决，超时 120s 视为拒绝）；
//! 3. 逐文件：发送方写 `ITEM\t<uuid>\t<size>\n` 后紧跟 size 字节裸流；
//!    中途放弃写 `ABORT\t<uuid>\t<原因>\n`；接收方边收边算 SHA-256，
//!    与清单不符即整批失败（D65 判死权在字节等值，这里先保真）；
//! 4. 全部落盘后接收方发 `Staged` 事件等导入管线收尾（UI 回 ACK/NACK，
//!    超时 30min），最终在同一方向回 `DELIVERED\n` 或 `NACK\t<原因>\n`。
//!
//! 引擎不碰 stdout：一切经事件回调上抛，main.rs 是唯一出口。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use domain::AssetKind;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use uuid::Uuid;

use iroh::endpoint::{Connection, ConnectionError, SendStream, VarInt};
use iroh::Endpoint;
use iroh::EndpointAddr;

use share::{ManifestItem, SendRequest, TransferManifest};

use crate::lines::LineReader;

/// 清单 JSON 行上限：1000 项 × ~250B ≈ 250KB，8MB 是十倍余量。
pub(crate) const MANIFEST_LINE_MAX: usize = 8 * 1024 * 1024;
/// 首连确认等待上限（人离开电脑忘了点，连接不能无限挂着）。
pub const OFFER_TIMEOUT: Duration = Duration::from_secs(120);
/// 导入收尾等待上限（千项批量导入可能很久）。
pub const IMPORT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// 连接建立上限（局域网直连，15s 不通就是不通）。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 进度节流粒度：每 256KB 上报一次。
const PROGRESS_STEP: u64 = 256 * 1024;
/// 裸流分块大小。
const STREAM_CHUNK: usize = 128 * 1024;

#[derive(Debug)]
pub enum EngineError {
    Io(String),
    Dial(String),
    Protocol(String),
    Timeout(&'static str),
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineError::Io(detail) => write!(f, "本地 IO 失败：{detail}"),
            EngineError::Dial(detail) => write!(f, "无法连接对方设备：{detail}"),
            EngineError::Protocol(detail) => write!(f, "对端协议异常：{detail}"),
            EngineError::Timeout(stage) => write!(f, "{stage}超时"),
        }
    }
}

impl From<io::Error> for EngineError {
    fn from(e: io::Error) -> Self {
        EngineError::Io(e.to_string())
    }
}

const RECEIPT_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const READ_RECEIPT: &[u8] = b"received";

/// 只有最后读取应用数据的一方才能安全关闭连接；QUIC ACK 不保证应用已读。
pub(crate) async fn finish_with_receipt(
    conn: &Connection,
    send: &mut SendStream,
) -> Result<(), EngineError> {
    send.finish()
        .map_err(|e| EngineError::Io(format!("结束发送流失败: {e}")))?;
    match tokio::time::timeout(RECEIPT_TIMEOUT, conn.closed())
        .await
        .map_err(|_| EngineError::Timeout("对端读取确认"))?
    {
        ConnectionError::ApplicationClosed(close)
            if close.error_code == VarInt::from_u32(0) && close.reason.as_ref() == READ_RECEIPT =>
        {
            Ok(())
        }
        _ => Err(EngineError::Io("对端未确认读取即断开".into())),
    }
}

/// Terminal decisions already known to the engine must survive transport
/// failures: the local batch outcome is returned even when the final write or
/// its read receipt fails. The error is logged, not propagated — main.rs maps
/// `Err` to "manifest never parsed" and would drop the batch-specific DONE.
async fn final_line(conn: &Connection, send: &mut SendStream, line: &str) {
    if let Err(error) = write_final_line(conn, send, line).await {
        crate::log_line(format!("终行送达失败（本地终态保留）: {error}"));
    }
}
pub(crate) async fn write_final_line(
    conn: &Connection,
    send: &mut SendStream,
    line: &str,
) -> Result<(), EngineError> {
    tokio::time::timeout(RECEIPT_TIMEOUT, async {
        send.write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| EngineError::Io(format!("发送终行失败: {e}")))?;
        finish_with_receipt(conn, send).await
    })
    .await
    .map_err(|_| EngineError::Timeout("终行送达"))?
}

/// 发送侧终态（Err(EngineError) 表示本地/链路故障，非对端意志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    /// 对端已确认导入完成。
    Delivered,
    /// 对端拒绝接收（含首连确认超时）。
    Rejected(String),
    /// 对端收到但未完成导入（NACK）。
    Failed(String),
}

/// 接收侧终态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveOutcome {
    pub batch_id: Uuid,
    pub state: ReceiveState,
    pub staged_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveState {
    /// 文件已暂存且导入管线确认完成。
    Transferred,
    /// 首连确认被拒。
    Rejected(String),
    /// 校验失败 / 导入 NACK / 链路中断。
    Failed(String),
}

/// 发送侧事件（main.rs 映射为 stdout 行）。
#[derive(Debug, Clone)]
pub enum SendEvent {
    Hashing,
    Connecting,
    Sending,
    ItemDone { uuid: Uuid },
    Progress { sent: u64, total: u64 },
}

/// 接收侧事件。
#[derive(Debug, Clone)]
pub enum IncomingEvent {
    /// 清单到达及 QUIC 认证的发送方 id，等待首连确认（UI 弹确认框）。
    Offer {
        manifest: TransferManifest,
        authenticated_sender_id: String,
    },
    /// 单文件落盘结果（role=recv 的 ITEM 行）。
    ItemDone {
        batch_id: Uuid,
        uuid: Uuid,
        ok: bool,
        detail: String,
    },
    /// 全部落盘，等待导入管线收尾（UI 走 D50 归类弹窗）。
    Staged { batch_id: Uuid, paths: Vec<PathBuf> },
}

/// UI 裁决（stdin ACCEPT/REJECT/ACK/NACK 归一）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Reject(String),
    Ack,
    Nack(String),
}

/// batch_id → 等待中的裁决通道。同一批次先后有两段等待（首连确认、
/// 导入收尾），resolve 按注册顺序派发。
#[derive(Default)]
pub struct DecisionHub {
    waiters: Mutex<HashMap<Uuid, Vec<oneshot::Sender<Decision>>>>,
}

impl DecisionHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 注册一次等待（引擎在发事件**之前**调用，杜绝「事件已弹、用户已点、
    /// 裁决无处投递」的竞态）。
    pub fn wait(&self, batch_id: Uuid) -> oneshot::Receiver<Decision> {
        let (tx, rx) = oneshot::channel();
        self.waiters
            .lock()
            .unwrap()
            .entry(batch_id)
            .or_default()
            .push(tx);
        rx
    }

    /// 派发一个裁决；无等待者返回 false（重复点击/迟到命令，忽略即可）。
    pub fn resolve(&self, batch_id: Uuid, decision: Decision) -> bool {
        let mut waiters = self.waiters.lock().unwrap();
        let Some(queue) = waiters.get_mut(&batch_id) else {
            return false;
        };
        match queue.pop() {
            Some(tx) => {
                let ok = tx.send(decision).is_ok();
                if queue.is_empty() {
                    waiters.remove(&batch_id);
                }
                ok
            }
            None => {
                waiters.remove(&batch_id);
                false
            }
        }
    }
}

/// 流式 SHA-256（小写 hex）+ 实际字节数。
async fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; STREAM_CHUNK];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((lower_hex(&hasher.finalize()), total))
}

fn lower_hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
    }
    out
}

fn kind_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Image => "image",
        AssetKind::Video => "video",
        AssetKind::Text => "text",
        AssetKind::Other => "other",
    }
}

/// 发起一次显式推送。`on_event` 同步回调（main.rs 里写 stdout）。
pub async fn send_batch(
    endpoint: &Endpoint,
    target: EndpointAddr,
    request: SendRequest,
    mut on_event: impl FnMut(SendEvent) + Send,
) -> Result<BatchOutcome, EngineError> {
    let batch_id = request.batch_id;
    on_event(SendEvent::Hashing);
    let mut items = Vec::with_capacity(request.items.len());
    let mut total: u64 = 0;
    for entry in &request.items {
        let (sha_hex, size) = hash_file(&entry.path)
            .await
            .map_err(|e| EngineError::Io(format!("读取 {} 失败: {e}", entry.file_name)))?;
        total += size;
        items.push(ManifestItem {
            asset_uuid: entry.asset_uuid,
            file_name: entry.file_name.clone(),
            kind: entry.kind,
            size_bytes: size,
            sha256_hex: sha_hex,
            category_hint: entry.category_hint.clone(),
        });
    }
    let manifest = TransferManifest {
        batch_id,
        sender_name: request.sender_name.clone(),
        space_id: request.space_id,
        items,
    };
    manifest
        .validate()
        .map_err(|e| EngineError::Protocol(format!("清单自检失败: {e}")))?;

    on_event(SendEvent::Connecting);
    let conn = tokio::time::timeout(
        CONNECT_TIMEOUT,
        crate::dial::connect(endpoint, target, share::ALPN.as_bytes()),
    )
    .await
    .map_err(|_| EngineError::Timeout("连接"))??;

    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| EngineError::Protocol(format!("打开流失败: {e}")))?;
    let mut reader = LineReader::new(recv);

    let manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| EngineError::Protocol(format!("清单序列化失败: {e}")))?;
    let mut payload = manifest_json.into_bytes();
    payload.push(b'\n');
    send.write_all(&payload)
        .await
        .map_err(|e| EngineError::Io(format!("发送清单失败: {e}")))?;

    match reader.read_line(4096).await? {
        Some(line) if line == "ACCEPT" => {}
        Some(line) => {
            let reason = line
                .strip_prefix("REJECT\t")
                .unwrap_or("对端拒绝")
                .to_string();
            // 己方发送流收尾 + 由本侧（最后读到数据的一方）关连接。
            let _ = send.finish();
            conn.close(VarInt::from_u32(0), READ_RECEIPT);
            return Ok(BatchOutcome::Rejected(reason));
        }
        None => return Err(EngineError::Protocol("对端在读清单后断开".into())),
    }
    on_event(SendEvent::Sending);

    let mut sent_total = 0u64;
    let mut last_emit = 0u64;
    for (entry, item) in request.items.iter().zip(&manifest.items) {
        let header = format!("ITEM\t{}\t{}\n", item.asset_uuid, item.size_bytes);
        send.write_all(header.as_bytes())
            .await
            .map_err(|e| EngineError::Io(format!("写文件头失败: {e}")))?;
        let mut file = tokio::fs::File::open(&entry.path)
            .await
            .map_err(|e| EngineError::Io(format!("重开 {} 失败: {e}", entry.file_name)))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAM_CHUNK];
        loop {
            let n = file
                .read(&mut buf)
                .await
                .map_err(|e| EngineError::Io(format!("读 {} 失败: {e}", entry.file_name)))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            send.write_all(&buf[..n])
                .await
                .map_err(|e| EngineError::Io(format!("发送 {} 失败: {e}", entry.file_name)))?;
            sent_total += n as u64;
            if sent_total - last_emit >= PROGRESS_STEP {
                last_emit = sent_total;
                on_event(SendEvent::Progress {
                    sent: sent_total,
                    total,
                });
            }
        }
        let actual = lower_hex(&hasher.finalize());
        if actual != item.sha256_hex {
            return Err(EngineError::Protocol(format!(
                "{} 发送中内容变化（哈希漂移）",
                entry.file_name
            )));
        }
        on_event(SendEvent::ItemDone {
            uuid: item.asset_uuid,
        });
        last_emit = sent_total;
        on_event(SendEvent::Progress {
            sent: sent_total,
            total,
        });
    }
    // 接收方持有连接直到本侧读完终行并发回读取确认。
    let final_line = tokio::time::timeout(IMPORT_TIMEOUT, reader.read_line(4096))
        .await
        .map_err(|_| EngineError::Timeout("对端导入"))?
        .map_err(|e| EngineError::Io(format!("等待回执失败: {e}")))?;
    let outcome = match final_line {
        Some(line) if line == "DELIVERED" => BatchOutcome::Delivered,
        Some(line) => {
            let reason = line
                .strip_prefix("NACK\t")
                .unwrap_or("对端未完成导入")
                .to_string();
            BatchOutcome::Failed(reason)
        }
        None => BatchOutcome::Failed("对端在读回执前断开".into()),
    };
    let _ = send.finish();
    conn.close(VarInt::from_u32(0), READ_RECEIPT);
    Ok(outcome)
}

/// 处理一条已建立的连接（接收侧）。`hub` 提供两段等待的裁决来源。
pub async fn handle_connection(
    conn: Connection,
    staging_root: &Path,
    hub: &DecisionHub,
    mut on_event: impl FnMut(IncomingEvent) + Send,
) -> Result<ReceiveOutcome, EngineError> {
    let authenticated_sender_id = conn.remote_id().to_z32();
    let (mut send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| EngineError::Protocol(format!("接受流失败: {e}")))?;
    let mut reader = LineReader::new(recv);

    let manifest = match reader.read_line(MANIFEST_LINE_MAX).await? {
        Some(line) => match serde_json::from_str::<TransferManifest>(&line) {
            Ok(manifest) => match manifest.validate() {
                Ok(()) => manifest,
                Err(e) => {
                    let error = EngineError::Protocol(format!("清单不合法: {e}"));
                    write_final_line(&conn, &mut send, &format!("REJECT\t{error}")).await?;
                    return Err(error);
                }
            },
            Err(e) => {
                let error = EngineError::Protocol(format!("清单解析失败: {e}"));
                write_final_line(&conn, &mut send, &format!("REJECT\t{error}")).await?;
                return Err(error);
            }
        },
        None => return Err(EngineError::Protocol("对端未发清单即断开".into())),
    };
    let batch_id = manifest.batch_id;

    // 先注册等待再发事件：确认框弹出时裁决通道必已就位。
    let offer_rx = hub.wait(batch_id);
    on_event(IncomingEvent::Offer {
        manifest: manifest.clone(),
        authenticated_sender_id,
    });
    let decision = tokio::time::timeout(OFFER_TIMEOUT, offer_rx)
        .await
        .map_err(|_| EngineError::Timeout("首连确认"))?;
    match decision {
        Ok(Decision::Accept) => {}
        Ok(Decision::Reject(reason)) => {
            final_line(&conn, &mut send, &format!("REJECT\t{reason}")).await;
            return Ok(ReceiveOutcome {
                batch_id,
                state: ReceiveState::Rejected(reason),
                staged_paths: Vec::new(),
            });
        }
        Ok(other) => {
            final_line(&conn, &mut send, "REJECT\t裁决时序错误").await;
            return Ok(ReceiveOutcome {
                batch_id,
                state: ReceiveState::Failed(format!("首连确认阶段收到 {other:?}")),
                staged_paths: Vec::new(),
            });
        }
        Err(_) => {
            final_line(&conn, &mut send, "REJECT\t对端已断开").await;
            return Ok(ReceiveOutcome {
                batch_id,
                state: ReceiveState::Failed("裁决通道断裂".into()),
                staged_paths: Vec::new(),
            });
        }
    }
    send.write_all(b"ACCEPT\n")
        .await
        .map_err(|e| EngineError::Io(format!("回 ACCEPT 失败: {e}")))?;

    let dir = staging_root.join(batch_id.to_string());
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| EngineError::Io(format!("无法创建暂存目录 {}: {e}", dir.display())))?;

    let mut staged_paths: Vec<PathBuf> = Vec::new();
    for item in &manifest.items {
        let header = match reader.read_line(4096).await? {
            Some(line) => line,
            None => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(EngineError::Protocol("文件流在头部前断开".into()));
            }
        };
        if let Some(rest) = header.strip_prefix("ABORT\t") {
            let reason = rest.split('\t').nth(1).unwrap_or("发送方中止").to_string();
            let _ = tokio::fs::remove_dir_all(&dir).await;
            on_event(IncomingEvent::ItemDone {
                batch_id,
                uuid: item.asset_uuid,
                ok: false,
                detail: reason.clone(),
            });
            return Ok(ReceiveOutcome {
                batch_id,
                state: ReceiveState::Failed(format!("发送方中止：{reason}")),
                staged_paths: Vec::new(),
            });
        }
        let mut fields = header.split('\t');
        let (Some("ITEM"), Some(uuid_raw), Some(size_raw)) =
            (fields.next(), fields.next(), fields.next())
        else {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Ok(ReceiveOutcome {
                batch_id,
                state: ReceiveState::Failed(format!("坏文件头: {header}")),
                staged_paths: Vec::new(),
            });
        };
        let header_uuid: Uuid = uuid_raw
            .parse()
            .map_err(|_| EngineError::Protocol(format!("文件头 uuid 不合法: {uuid_raw}")))?;
        if header_uuid != item.asset_uuid {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(EngineError::Protocol("文件头与清单顺序不符".into()));
        }
        let size: u64 = size_raw
            .parse()
            .map_err(|_| EngineError::Protocol(format!("文件头 size 不合法: {size_raw}")))?;
        if size != item.size_bytes {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(EngineError::Protocol(format!(
                "{} 声明大小与清单不符",
                item.file_name
            )));
        }

        let ext = share::safe_ext(&item.file_name).unwrap_or_else(|| "bin".to_string());
        let staged_path = dir.join(format!("{}.{}", item.asset_uuid, ext));
        let mut file = tokio::fs::File::create(&staged_path).await.map_err(|e| {
            EngineError::Io(format!("无法写暂存文件 {}: {e}", staged_path.display()))
        })?;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        let mut buf = vec![0u8; STREAM_CHUNK];
        while received < size {
            let want = (size - received).min(buf.len() as u64) as usize;
            let n = reader
                .read_raw(&mut buf[..want])
                .await
                .map_err(|e| EngineError::Io(format!("收 {} 失败: {e}", item.file_name)))?;
            if n == 0 {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(EngineError::Protocol(format!(
                    "{} 流在 {received}/{size} 处断开",
                    item.file_name
                )));
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .await
                .map_err(|e| EngineError::Io(format!("写暂存失败: {e}")))?;
            received += n as u64;
        }
        file.flush()
            .await
            .map_err(|e| EngineError::Io(e.to_string()))?;
        let actual = lower_hex(&hasher.finalize());
        if actual != item.sha256_hex {
            on_event(IncomingEvent::ItemDone {
                batch_id,
                uuid: item.asset_uuid,
                ok: false,
                detail: "SHA-256 校验失败".into(),
            });
            final_line(&conn, &mut send, "NACK\tSHA-256 校验失败").await;
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Ok(ReceiveOutcome {
                batch_id,
                state: ReceiveState::Failed("SHA-256 校验失败".into()),
                staged_paths: Vec::new(),
            });
        }
        on_event(IncomingEvent::ItemDone {
            batch_id,
            uuid: item.asset_uuid,
            ok: true,
            detail: kind_name(item.kind).to_string(),
        });
        staged_paths.push(staged_path);
    }

    let ack_rx = hub.wait(batch_id);
    on_event(IncomingEvent::Staged {
        batch_id,
        paths: staged_paths.clone(),
    });
    let outcome = async {
        let outcome = match tokio::time::timeout(IMPORT_TIMEOUT, ack_rx).await {
            Ok(Ok(Decision::Ack)) => {
                final_line(&conn, &mut send, "DELIVERED").await;
                ReceiveOutcome {
                    batch_id,
                    state: ReceiveState::Transferred,
                    staged_paths: staged_paths.clone(),
                }
            }
            Ok(Ok(Decision::Nack(reason))) => {
                final_line(&conn, &mut send, &format!("NACK\t{reason}")).await;
                ReceiveOutcome {
                    batch_id,
                    state: ReceiveState::Failed(reason),
                    staged_paths: Vec::new(),
                }
            }
            Ok(Ok(other)) => {
                final_line(&conn, &mut send, "NACK\t裁决时序错误").await;
                ReceiveOutcome {
                    batch_id,
                    state: ReceiveState::Failed(format!("导入阶段收到 {other:?}")),
                    staged_paths: Vec::new(),
                }
            }
            Ok(Err(_)) => ReceiveOutcome {
                batch_id,
                state: ReceiveState::Failed("对端已断开".into()),
                staged_paths: Vec::new(),
            },
            Err(_) => {
                final_line(&conn, &mut send, "NACK\t导入超时").await;
                ReceiveOutcome {
                    batch_id,
                    state: ReceiveState::Failed("导入超时".into()),
                    staged_paths: Vec::new(),
                }
            }
        };
        Ok(outcome)
    }
    .await;
    // 暂存区使命结束：导入已复制入库（Transferred）或用户放弃（其余分支）。
    let _ = tokio::fs::remove_dir_all(&dir).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::endpoint::presets;
    use std::net::SocketAddr;

    #[tokio::test]
    async fn terminal_reply_waits_for_peer_read_receipt() {
        assert!(terminal_reply_outcome(true).await.is_ok());
    }

    #[tokio::test]
    async fn terminal_reply_rejects_disconnect_without_read_receipt() {
        assert!(terminal_reply_outcome(false).await.is_err());
    }

    async fn terminal_reply_outcome(confirm: bool) -> Result<(), EngineError> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let server = Endpoint::builder(presets::Minimal)
                .alpns(vec![share::ALPN_SYNC.as_bytes().to_vec()])
                .bind()
                .await
                .unwrap();
            let client = Endpoint::builder(presets::Minimal).bind().await.unwrap();
            let port = server
                .bound_sockets()
                .iter()
                .find(|addr| addr.is_ipv4())
                .unwrap()
                .port();
            let target = EndpointAddr::new(server.id())
                .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], port)));
            let mut serve = tokio::spawn(async move {
                let conn = server.accept().await.unwrap().await.unwrap();
                let (mut send, recv) = conn.accept_bi().await.unwrap();
                let mut reader = LineReader::new(recv);
                assert_eq!(
                    reader.read_line(64).await.unwrap().as_deref(),
                    Some("request")
                );
                write_final_line(&conn, &mut send, "reply").await
            });
            let conn = client
                .connect(target, share::ALPN_SYNC.as_bytes())
                .await
                .unwrap();
            let (mut send, recv) = conn.open_bi().await.unwrap();
            send.write_all(b"request\n").await.unwrap();
            let mut reader = LineReader::new(recv);
            assert_eq!(
                reader.read_line(64).await.unwrap().as_deref(),
                Some("reply")
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(650), &mut serve)
                    .await
                    .is_err(),
                "terminal reply released the connection without the peer's read receipt"
            );
            send.finish().unwrap();
            conn.close(
                VarInt::from_u32(0),
                if confirm { READ_RECEIPT } else { b"cancelled" },
            );
            let outcome = serve.await.unwrap();
            client.close().await;
            outcome
        })
        .await
        .expect("terminal reply receipt test timed out")
    }
}
