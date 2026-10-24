//! 同步通道引擎（D80-M1 批1）：NOTIFY/PULL/SNAPSHOT 的 QUIC 载体。
//!
//! 独立 ALPN（[`share::ALPN_SYNC`]），与推送通道（engine）并存于同一
//! endpoint。会话形态 = 单 bi 流 + 一行 JSON（[`share::SyncMessage`]）：
//! - 我方发出 Pull → 对端回一行 Snapshot（已按我方过滤）；
//! - 我方发出 Delta（在线增量推送）→ 无应答；
//! - 入站 Pull → 按 [`SyncBook`] 里 UI 推送的整册取该对端视角应答
//!   （册里没有的对端 = 零可得，fail-closed）；入站 Delta/Snapshot →
//!   上抛事件交 UI 处置。
//!
//! 对端身份取自 QUIC 握手（`Connection::remote_id`，TLS 证书背书），
//! 报文本身不自报身份——发现面的「来自谁」不可伪造。未配对对端的
//! 报文照样上抛，是否采信由 UI 依配对册裁决（引擎不持信任状态）。
//!
//! QUIC 冲刷纪律与 engine 相同：终行写出必须 finish + 冲刷窗再放连接。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use share::{
    admit_join, parse_space_invite_code, SpaceAssetMeta, SpaceBookState, SpaceJoinError,
    SpaceMessage, SpaceMeta, SpaceOp, SpaceSummaryEntry, SyncBookState, SyncMessage,
};

use crate::engine::{
    write_final_line, EngineError, CONNECT_TIMEOUT, FLUSH_GRACE, MANIFEST_LINE_MAX,
};
use crate::lines::LineReader;

/// PULL 应答等待上限：应答只含元数据清单，局域网量级里 15s 封顶足够。
const SYNC_REPLY_TIMEOUT: Duration = Duration::from_secs(15);

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// UI 推送的「我共享给谁什么」整册（M1-b）：worker 只读，应答 PULL 的
/// 唯一数据源。整册替换语义 = UI 每次共享态变化后重发全量，不存在
/// 增量合并路径（册里永远只有最新视角）。
#[derive(Default)]
pub struct SyncBook {
    state: Mutex<SyncBookState>,
}

impl SyncBook {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 整册替换（读侧锁内换值；调用频率 = 共享态变化频率，量级极低）。
    pub fn replace(&self, state: SyncBookState) {
        *self.state.lock().unwrap() = state;
    }

    /// 该对端的视角快照：册里没有 = 零可得（rev 0），fail-closed。
    pub fn snapshot_for(&self, peer_id: &str) -> SyncMessage {
        let state = self.state.lock().unwrap();
        match state.get(peer_id) {
            Some(peer) => SyncMessage::Snapshot {
                rev: peer.rev,
                offers: peer.offers.clone(),
            },
            None => SyncMessage::Snapshot {
                rev: 0,
                offers: Vec::new(),
            },
        }
    }
}

/// 入站同步报文（main.rs 映射为 stdout 行）。
#[derive(Debug, Clone)]
pub enum SyncInbound {
    /// 非应答类报文（Delta/Snapshot/空间 Ops/JoinOk）：交 UI 处置。
    Message {
        peer_id: String,
        message: SyncMessage,
    },
    /// D90-P2：空间主侧受理了新成员（worker 册副本已入册；UI 必须落盘
    /// spaces.json + 追加 Roster op 并重推 SPACE_STATE——否则下次整册
    /// 替换会把该变更抹掉）。
    SpaceAdmitted {
        space_id: uuid::Uuid,
        peer_id: String,
    },
    /// D90-P2：收到字节索取（同步通道不传字节——main 侧组装推送通道投递）。
    SpaceFetch {
        peer_id: String,
        space_id: uuid::Uuid,
        items: Vec<SpaceAssetMeta>,
    },
}

/// D90-P2 空间服务册：UI 经 SPACE_STATE 整册替换，worker 只读应答；
/// 唯一的写路径是 Join 受理（名册 + Roster op 入册副本，变更经
/// [`SyncInbound::SpaceAdmitted`] 交 UI 落盘后重推回来对齐）。
#[derive(Default)]
pub struct SpaceBook {
    state: Mutex<SpaceBookState>,
}

/// Join 受理的应答材料（JoinOk 组装 + admitted 事件）。
pub struct AdmittedSpace {
    pub space_id: uuid::Uuid,
    /// false = 幂等重入（名册无变更，不重发事件给 UI）。
    pub admitted: bool,
    pub meta: SpaceMeta,
    pub ops: Vec<SpaceOp>,
}

impl SpaceBook {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn replace(&self, state: SpaceBookState) {
        *self.state.lock().unwrap() = state;
    }

    /// 抗熵发起方的 Summary（本地全部空间的 op 键全集）。
    pub fn summary(&self) -> SpaceMessage {
        let state = self.state.lock().unwrap();
        let entries = state
            .spaces
            .iter()
            .map(|(id, entry)| SpaceSummaryEntry {
                space_id: *id,
                keys: entry.ops.iter().map(share::OpKey::of).collect(),
            })
            .collect();
        SpaceMessage::Summary { entries }
    }

    pub fn diff_against(&self, entries: &[SpaceSummaryEntry]) -> Vec<SpaceOp> {
        self.state.lock().unwrap().diff_against(entries)
    }

    /// Fetch 的本地落位查询（hash → 绝对路径；拷贝返回避免锁跨 await）。
    pub fn serve_path(&self, space_id: uuid::Uuid, content_hash: &[u8]) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .serve_path(space_id, content_hash)
            .map(str::to_string)
    }

    /// Join 受理（空间主侧）：验码入册副本 + 返回 JoinOk 材料。
    pub fn admit(
        &self,
        code: &str,
        joiner_id: &str,
        joiner_name: &str,
    ) -> Result<AdmittedSpace, SpaceJoinError> {
        let parsed = parse_space_invite_code(code).map_err(SpaceJoinError::BadCode)?;
        let mut state = self.state.lock().unwrap();
        let Some(entry) = state.spaces.get_mut(&parsed.space_id) else {
            // 不泄漏册里有没有这个空间。
            return Err(SpaceJoinError::NoSuchSpace);
        };
        let admitted = admit_join(entry, code, joiner_id, joiner_name, unix_now())?;
        Ok(AdmittedSpace {
            space_id: parsed.space_id,
            admitted,
            meta: entry.meta.clone(),
            ops: entry.ops.clone(),
        })
    }
}

/// 处理一条同步通道入站连接（Pull/Summary/Join 在引擎内应答，不上抛）。
pub async fn handle_sync_connection(
    conn: Connection,
    book: &SyncBook,
    spaces: &SpaceBook,
    mut on_event: impl FnMut(SyncInbound) + Send,
) -> Result<(), EngineError> {
    let peer_id = conn.remote_id().to_z32();
    let (mut send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| EngineError::Protocol(format!("接受同步流失败: {e}")))?;
    let mut reader = LineReader::new(recv);
    let line = reader
        .read_line(MANIFEST_LINE_MAX)
        .await?
        .ok_or_else(|| EngineError::Protocol("对端未发同步报文即断开".into()))?;
    let message: SyncMessage = serde_json::from_str(&line)
        .map_err(|e| EngineError::Protocol(format!("同步报文解析失败: {e}")))?;
    message
        .validate()
        .map_err(|e| EngineError::Protocol(format!("同步报文不合法: {e}")))?;
    match message {
        // 上线拉取：按册应答（视角报文，终行冲刷纪律照旧）。
        SyncMessage::Pull { .. } => {
            let reply = book.snapshot_for(&peer_id);
            let json = serde_json::to_string(&reply)
                .map_err(|e| EngineError::Protocol(format!("快照序列化失败: {e}")))?;
            write_final_line(&mut send, &json).await;
            Ok(())
        }
        // D90-P2 空间协议：Summary/Join 同步应答；Fetch 不占同步通道字节。
        SyncMessage::Space { message } => match message {
            SpaceMessage::Summary { entries } => {
                let reply = SyncMessage::Space {
                    message: SpaceMessage::Ops {
                        ops: spaces.diff_against(&entries),
                    },
                };
                let json = serde_json::to_string(&reply)
                    .map_err(|e| EngineError::Protocol(format!("空间应答序列化失败: {e}")))?;
                write_final_line(&mut send, &json).await;
                Ok(())
            }
            SpaceMessage::Join { code, joiner_name } => {
                let reply = match spaces.admit(&code, &peer_id, &joiner_name) {
                    Ok(admitted) => {
                        if admitted.admitted {
                            on_event(SyncInbound::SpaceAdmitted {
                                space_id: admitted.space_id,
                                peer_id,
                            });
                        }
                        SyncMessage::Space {
                            message: SpaceMessage::JoinOk {
                                meta: admitted.meta,
                                ops: admitted.ops,
                            },
                        }
                    }
                    Err(error) => SyncMessage::Space {
                        message: SpaceMessage::JoinErr {
                            reason: error.to_string(),
                        },
                    },
                };
                let json = serde_json::to_string(&reply)
                    .map_err(|e| EngineError::Protocol(format!("加入应答序列化失败: {e}")))?;
                write_final_line(&mut send, &json).await;
                Ok(())
            }
            SpaceMessage::Fetch { space_id, items } => {
                on_event(SyncInbound::SpaceFetch {
                    peer_id: peer_id.clone(),
                    space_id,
                    items,
                });
                Ok(())
            }
            other => {
                on_event(SyncInbound::Message {
                    peer_id,
                    message: SyncMessage::Space { message: other },
                });
                Ok(())
            }
        },
        other => {
            on_event(SyncInbound::Message {
                peer_id,
                message: other,
            });
            Ok(())
        }
    }
}

/// 发出一条同步报文。Pull/Join/Summary 等应答（`Ok(Some(reply))`）；
/// 其余即发即收（`Ok(None)`）——推送丢失不重试，由对端上线补拉自愈。
pub async fn send_sync_message(
    endpoint: &Endpoint,
    target: EndpointAddr,
    message: SyncMessage,
) -> Result<Option<SyncMessage>, EngineError> {
    message
        .validate()
        .map_err(|e| EngineError::Protocol(format!("同步报文自检失败: {e}")))?;
    let wants_reply = matches!(
        message,
        SyncMessage::Pull { .. }
            | SyncMessage::Space {
                message: SpaceMessage::Join { .. } | SpaceMessage::Summary { .. }
            }
    );
    let conn = tokio::time::timeout(
        CONNECT_TIMEOUT,
        endpoint.connect(target, share::ALPN_SYNC.as_bytes()),
    )
    .await
    .map_err(|_| EngineError::Timeout("连接"))?
    .map_err(|e| EngineError::Dial(e.to_string()))?;
    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| EngineError::Protocol(format!("打开同步流失败: {e}")))?;
    let mut reader = LineReader::new(recv);
    let json = serde_json::to_string(&message)
        .map_err(|e| EngineError::Protocol(format!("同步报文序列化失败: {e}")))?;
    let mut payload = json.into_bytes();
    payload.push(b'\n');
    send.write_all(&payload)
        .await
        .map_err(|e| EngineError::Io(format!("发送同步报文失败: {e}")))?;
    if !wants_reply {
        // 推送即收尾：finish + 冲刷窗，防连接句柄 drop 带走在途字节。
        let _ = send.finish();
        tokio::time::sleep(FLUSH_GRACE).await;
        return Ok(None);
    }
    let reply = tokio::time::timeout(SYNC_REPLY_TIMEOUT, reader.read_line(MANIFEST_LINE_MAX))
        .await
        .map_err(|_| EngineError::Timeout("同步应答"))?
        .map_err(|e| EngineError::Io(format!("读取同步应答失败: {e}")))?
        .ok_or_else(|| EngineError::Protocol("对端未回同步应答即断开".into()))?;
    let reply: SyncMessage = serde_json::from_str(&reply)
        .map_err(|e| EngineError::Protocol(format!("同步应答解析失败: {e}")))?;
    reply
        .validate()
        .map_err(|e| EngineError::Protocol(format!("同步应答不合法: {e}")))?;
    // 应答已到手；finish 本侧发送方向再收尾（对端按终行纪律早已回完）。
    let _ = send.finish();
    Ok(Some(reply))
}
