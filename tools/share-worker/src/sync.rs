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
//! 最后读取应用报文的一方关闭会话确认送达，发送方有界等待该事件。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, EndpointAddr};
use share::{
    admit_join, parse_space_invite_code, SpaceAssetMeta, SpaceBookState, SpaceJoinError,
    SpaceMessage, SpaceMeta, SpaceOp, SpaceSummaryEntry, SyncBookState, SyncMessage,
};

use crate::engine::{
    finish_with_receipt, write_final_line, EngineError, CONNECT_TIMEOUT, MANIFEST_LINE_MAX,
    READ_RECEIPT,
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
        joiner_name: String,
    },
    /// An authenticated peer opened a pull session; UI may perform one bounded
    /// reciprocal pull even when no DEVICE discovery event was observed.
    PeerActive { peer_id: String },
    /// D90-P2: 字节索取经名册授权后交 UI 侧投递（含已解析的本地字节路径）。
    SpaceFetch {
        peer_id: String,
        space_id: uuid::Uuid,
        items: Vec<(SpaceAssetMeta, String)>,
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

    pub fn summary_for(&self, peer_id: &str) -> SpaceMessage {
        let state = self.state.lock().unwrap();
        let entries = state
            .spaces
            .iter()
            .filter(|(_, entry)| entry.meta.is_member(peer_id))
            .map(|(id, entry)| SpaceSummaryEntry {
                space_id: *id,
                keys: entry.ops.iter().map(share::OpKey::of).collect(),
            })
            .collect();
        SpaceMessage::Summary { entries }
    }

    pub fn member_space_ids(&self, peer_id: &str) -> Vec<uuid::Uuid> {
        self.state
            .lock()
            .unwrap()
            .spaces
            .iter()
            .filter(|(_, entry)| entry.meta.is_member(peer_id))
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn page_after(
        &self,
        peer_id: &str,
        space_id: uuid::Uuid,
        after: Option<&share::OpKey>,
    ) -> Option<SpaceMessage> {
        let state = self.state.lock().unwrap();
        if !state
            .spaces
            .get(&space_id)
            .is_some_and(|entry| entry.meta.is_member(peer_id))
        {
            return None;
        }
        Some(state.page_after(space_id, after))
    }

    pub fn diff_against(&self, peer_id: &str, entries: &[SpaceSummaryEntry]) -> Vec<SpaceOp> {
        let state = self.state.lock().unwrap();
        let authorized: Vec<_> = entries
            .iter()
            .filter(|summary| {
                state
                    .spaces
                    .get(&summary.space_id)
                    .is_some_and(|entry| entry.meta.is_member(peer_id))
            })
            .cloned()
            .collect();
        state.diff_against(&authorized)
    }

    /// The authenticated peer must be a roster member before any serve-index lookup.
    pub fn can_fetch(&self, space_id: uuid::Uuid, peer_id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .spaces
            .get(&space_id)
            .is_some_and(|entry| entry.meta.is_member(peer_id))
    }

    /// The authenticated peer must be a roster member before any serve-index lookup.
    /// Returns only requested items that have a local byte path.
    pub fn authorized_fetch(
        &self,
        space_id: uuid::Uuid,
        peer_id: &str,
        items: &[SpaceAssetMeta],
    ) -> Option<Vec<(SpaceAssetMeta, String)>> {
        let state = self.state.lock().unwrap();
        let entry = state.spaces.get(&space_id)?;
        if !entry.meta.is_member(peer_id) {
            return None;
        }
        let mut ordered: Vec<_> = entry.ops.iter().collect();
        ordered.sort_by_key(|op| share::OpKey::of(op));
        let mut active = std::collections::HashMap::new();
        for op in ordered {
            match &op.op {
                share::SpaceOpKind::Add { asset } => {
                    active.insert(asset.uuid, asset);
                }
                share::SpaceOpKind::Remove { asset_uuid } => {
                    active.remove(asset_uuid);
                }
                _ => {}
            }
        }
        let by_hash: std::collections::HashMap<_, _> = active
            .into_values()
            .map(|asset| (asset.content_hash.as_slice(), asset))
            .collect();
        let mut served_hashes = std::collections::HashSet::new();
        Some(
            items
                .iter()
                .filter_map(|item| {
                    if !served_hashes.insert(item.content_hash.as_slice()) {
                        return None;
                    }
                    let asset = by_hash.get(item.content_hash.as_slice())?;
                    let served = entry
                        .serve
                        .iter()
                        .find(|served| served.content_hash == asset.content_hash)?;
                    Some(((*asset).clone(), served.path.clone()))
                })
                .collect(),
        )
    }

    /// Join受理（空间主侧）：验码入册 + 返回 JoinOk 材料。
    pub fn admit(
        &self,
        code: &str,
        local_owner_id: &iroh::EndpointId,
        joiner_id: &str,
        joiner_name: &str,
    ) -> Result<AdmittedSpace, SpaceJoinError> {
        let parsed = parse_space_invite_code(code).map_err(SpaceJoinError::BadCode)?;
        let owner_id = local_owner_id.to_z32();
        let mut state = self.state.lock().unwrap();
        let Some(entry) = state.spaces.get_mut(&parsed.space_id) else {
            return Err(SpaceJoinError::NoSuchSpace);
        };
        if entry.meta.owner_id != owner_id || parsed.owner_id != owner_id {
            return Err(SpaceJoinError::NoSuchSpace);
        }
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
    local_owner_id: iroh::EndpointId,
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
    let message: SyncMessage = serde_json::from_str(&line).map_err(|e| {
        EngineError::Protocol(format!(
            "同步报文解析失败 line={} column={}",
            e.line(),
            e.column()
        ))
    })?;
    message
        .validate()
        .map_err(|e| EngineError::Protocol(format!("同步报文不合法: {e}")))?;
    match message {
        // 上线拉取：按册应答（视角报文，终行冲刷纪律照旧）。
        SyncMessage::Pull { .. } => {
            let reply = book.snapshot_for(&peer_id);
            let json = serde_json::to_string(&reply)
                .map_err(|e| EngineError::Protocol(format!("快照序列化失败: {e}")))?;
            write_final_line(&conn, &mut send, &json).await?;
            Ok(())
        }
        // D90-P2 空间协议：Summary/Join 同步应答；Fetch 不占同步通道字节。
        SyncMessage::Space { message } => match message {
            SpaceMessage::PullOps { space_id, after } => {
                on_event(SyncInbound::PeerActive {
                    peer_id: peer_id.clone(),
                });
                let reply = SyncMessage::Space {
                    message: spaces
                        .page_after(&peer_id, space_id, after.as_ref())
                        .unwrap_or(SpaceMessage::OpsPage {
                            space_id,
                            after,
                            next: None,
                            ops: Vec::new(),
                        }),
                };
                let json = serde_json::to_string(&reply)
                    .map_err(|e| EngineError::Protocol(format!("空间分页序列化失败: {e}")))?;
                write_final_line(&conn, &mut send, &json).await?;
                Ok(())
            }
            SpaceMessage::Summary { entries } => {
                on_event(SyncInbound::PeerActive {
                    peer_id: peer_id.clone(),
                });
                let reply = SyncMessage::Space {
                    message: SpaceMessage::Ops {
                        ops: spaces.diff_against(&peer_id, &entries),
                    },
                };
                let json = serde_json::to_string(&reply)
                    .map_err(|e| EngineError::Protocol(format!("空间应答序列化失败: {e}")))?;
                write_final_line(&conn, &mut send, &json).await?;
                Ok(())
            }
            SpaceMessage::Join { code, joiner_name } => {
                let reply = match spaces.admit(&code, &local_owner_id, &peer_id, &joiner_name) {
                    Ok(admitted) => {
                        if admitted.admitted {
                            on_event(SyncInbound::SpaceAdmitted {
                                space_id: admitted.space_id,
                                peer_id,
                                joiner_name,
                            });
                        }
                        SyncMessage::Space {
                            message: SpaceMessage::JoinOk {
                                meta: admitted.meta,
                                ops: admitted
                                    .ops
                                    .into_iter()
                                    .take(share::MAX_OPS_PER_MESSAGE)
                                    .collect(),
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
                write_final_line(&conn, &mut send, &json).await?;
                Ok(())
            }
            SpaceMessage::OpsPage { .. } => {
                on_event(SyncInbound::Message {
                    peer_id,
                    message: SyncMessage::Space { message },
                });
                conn.close(VarInt::from_u32(0), READ_RECEIPT);
                Ok(())
            }
            SpaceMessage::Fetch { space_id, items } => {
                let authorized = spaces.authorized_fetch(space_id, &peer_id, &items);
                let served: std::collections::HashSet<_> = authorized
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .map(|(asset, _)| asset.content_hash.clone())
                    .collect();
                let reply = SyncMessage::Space {
                    message: SpaceMessage::FetchUnavailable {
                        space_id,
                        hashes: items
                            .iter()
                            .filter(|asset| !served.contains(&asset.content_hash))
                            .map(|asset| asset.content_hash.clone())
                            .collect(),
                    },
                };
                if let Some(items) = authorized.filter(|items| !items.is_empty()) {
                    on_event(SyncInbound::SpaceFetch {
                        peer_id: peer_id.clone(),
                        space_id,
                        items,
                    });
                }
                let json = serde_json::to_string(&reply)
                    .map_err(|e| EngineError::Protocol(format!("索取应答序列化失败: {e}")))?;
                write_final_line(&conn, &mut send, &json).await?;
                Ok(())
            }
            other => {
                on_event(SyncInbound::Message {
                    peer_id,
                    message: SyncMessage::Space { message: other },
                });
                conn.close(VarInt::from_u32(0), READ_RECEIPT);
                Ok(())
            }
        },
        other => {
            on_event(SyncInbound::Message {
                peer_id,
                message: other,
            });
            conn.close(VarInt::from_u32(0), READ_RECEIPT);
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
                message: SpaceMessage::Join { .. }
                    | SpaceMessage::Summary { .. }
                    | SpaceMessage::PullOps { .. }
                    | SpaceMessage::Fetch { .. },
            }
    );
    let conn = tokio::time::timeout(
        CONNECT_TIMEOUT,
        crate::dial::connect(endpoint, target, share::ALPN_SYNC.as_bytes()),
    )
    .await
    .map_err(|_| EngineError::Timeout("连接"))??;
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
        finish_with_receipt(&conn, &mut send).await?;
        return Ok(None);
    }
    let reply = tokio::time::timeout(SYNC_REPLY_TIMEOUT, reader.read_line(MANIFEST_LINE_MAX))
        .await
        .map_err(|_| EngineError::Timeout("同步应答"))?
        .map_err(|e| EngineError::Io(format!("读取同步应答失败: {e}")))?
        .ok_or_else(|| EngineError::Protocol("对端未回同步应答即断开".into()))?;
    let reply: SyncMessage = serde_json::from_str(&reply).map_err(|e| {
        EngineError::Protocol(format!(
            "同步应答解析失败 line={} column={}",
            e.line(),
            e.column()
        ))
    })?;
    reply
        .validate()
        .map_err(|e| EngineError::Protocol(format!("同步应答不合法: {e}")))?;
    let _ = send.finish();
    conn.close(VarInt::from_u32(0), READ_RECEIPT);
    Ok(Some(reply))
}
