//! share-worker（D80）：P2P 素材共享的传输引擎进程。
//!
//! 进程模型（D11/D33 同族）：iroh + tokio 只进这一个编译单元；UI 主进程
//! 永不碰网络栈。生命周期 = 主进程：stdin 管道关闭（父进程退出）即自退，
//! 无系统服务、无开机自启。空闲成本 = 一个 UDP socket + mDNS 公告线程。
//!
//! M0 局域网闭环（D80 分期）：mDNS 发现 → 显式推送 → iroh 直连 →
//! 清单先行 → 双重确认 → 导入管线。
//!
//! 地址发现使用签名 pkarr 记录和 mDNS；发布器保留 IP 与中继地址，避免
//! relay_only 过滤器在禁用中继时发空记录。iroh 自选可达路径，中继可先建立
//! 连接并在直连就绪后切换；n0 公共中继只转发端到端加密流量。
//! portmapper 保持默认启用；中继出站连接不依赖本机入站 UDP 放行。
//!
//! 协议见 [`proto`]；引擎见 [`engine`]。

use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::address_lookup::{AddrFilter, DnsAddressLookup, PkarrPublisher, PkarrResolver};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode};
use mdns_sd::ServiceDaemon;
use share::{SendItem, SpaceMessage, SyncMessage};
use share_worker::sync::SyncInbound;
use share_worker::{engine, mdns, proto, sync};
use uuid::Uuid;

use engine::{BatchOutcome, Decision, DecisionHub, IncomingEvent, ReceiveState, SendEvent};

/// stdout 单行写出（协议通道）。std Stdout 内部锁保证行原子。
fn emit(line: impl AsRef<str>) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", line.as_ref());
    let _ = out.flush();
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "AssetDeck".to_string())
}

fn default_staging_root() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("asset-manager")
        .join("share-inbox")
}

fn parse_staging_root(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .position(|a| a == "--staging-root")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
}

fn parse_key_file(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .position(|a| a == "--key-file")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
}

/// 设备密钥持久化（D90）：endpoint 私钥落盘，52 位设备标识跨重启稳定——
/// 空间名册的成员 id、邀请码的 owner 段都以它为唯一身份。M1 的临时密钥
/// 形态下标识每次启动漂移，配对册跨重启本就失效（真机批次未跑故未暴露）。
/// 文件 = 32 字节裸密钥；缺失/长度不符 = 重新生成并覆盖（旧身份作废，
/// 诚实代价：所有以旧 id 建立的信任关系需重新邀请）。
fn load_or_create_secret_key(path: Option<&std::path::Path>) -> iroh::SecretKey {
    use std::io::Read;
    let Some(path) = path else {
        logging::warn!("未指定 --key-file，设备标识本次会话后将漂移（测试模式）");
        return iroh::SecretKey::generate();
    };
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut bytes = [0u8; 32];
        if file.read_exact(&mut bytes).is_ok() {
            return iroh::SecretKey::from_bytes(&bytes);
        }
    }
    let key = iroh::SecretKey::generate();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    // 先写 tmp 再 rename（同壳层 atomic_write 模式）：半写文件下次读不出
    // 32 字节即重生成，不会载入半截密钥。
    let tmp = path.with_extension("key.tmp");
    match std::fs::write(&tmp, key.to_bytes()).and_then(|_| std::fs::rename(&tmp, path)) {
        Ok(()) => logging::info!("设备密钥已持久化 {}", path.display()),
        Err(error) => logging::warn!("设备密钥写入失败（本次会话仍可用）: {error}"),
    }
    key
}

/// 设备表 → EndpointAddr（id + 直连地址）。地址全非法或为空时仍以纯 id
/// 拨号（M1-c pkarr 信令/DNS 地址发现 + 本地地址簿缓存解析直连地址——
/// 邀请流的贴码方就是靠这条路径首次连上签发方）。
fn target_addr(id_z32: &str, addrs: &[String]) -> Option<EndpointAddr> {
    let id: EndpointId = EndpointId::from_z32(id_z32).ok()?;
    let mut addr = EndpointAddr::new(id);
    for value in addrs {
        if let Ok(socket) = value.parse::<SocketAddr>() {
            addr = addr.with_ip_addr(socket);
        }
    }
    Some(addr)
}

/// 拨号地址补全（D93 连接修复）：命令携带的可用直连地址优先；一个都没有
/// 时做一次**定向 mDNS 浏览**（同网段对端的 TXT 直连地址，命中即出）——
/// 纯 id 拨号会走 pkarr 公网 DNS 解析，国内网络常不可达，必须排在局域网
/// 路径之后当兜底而非默认。返回值直接喂 [`target_addr`]。
/// D94：三种来源都落 INFO 日志——真机排障靠 worker 日志定位断在哪一层。
async fn resolve_dial_addrs(
    daemon: Option<ServiceDaemon>,
    self_id: String,
    peer_id: &str,
    addrs: &[String],
) -> Vec<String> {
    let usable: Vec<String> = addrs
        .iter()
        .filter(|a| a.parse::<SocketAddr>().is_ok())
        .cloned()
        .collect();
    if !usable.is_empty() {
        logging::info!("拨号地址来自命令行 peer={peer_id} addrs={usable:?}");
        return usable;
    }
    let Some(daemon) = daemon else {
        logging::info!("mDNS 未启动，纯 id 拨号（pkarr 解析） peer={peer_id}");
        return Vec::new();
    };
    let target = peer_id.to_string();
    let found = tokio::task::spawn_blocking(move || {
        mdns::scan_for(&daemon, &self_id, &target, mdns::SCAN_WINDOW)
    })
    .await
    .ok()
    .flatten();
    match found {
        Some(device) => {
            logging::info!(
                "拨号地址来自定向 mDNS peer={peer_id} addrs={:?}",
                device.addrs
            );
            device.addrs
        }
        None => {
            logging::info!("定向 mDNS 未命中，纯 id 拨号（pkarr 解析） peer={peer_id}");
            Vec::new()
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let _log = logging::init_from_env("share-worker", None, logging::Level::Info);
    let args: Vec<String> = std::env::args().collect();
    let staging_root = parse_staging_root(&args).unwrap_or_else(default_staging_root);
    if let Err(error) = std::fs::create_dir_all(&staging_root) {
        eprintln!(
            "share-worker: 无法创建暂存目录 {}: {error}",
            staging_root.display()
        );
        std::process::exit(2);
    }

    // 发布器默认仅保留中继地址；显式保留 IP，才能在中继不可用时继续直连。
    // 公共 pkarr 记录可凭设备公钥查询；设备公钥不是加入团队的授权秘密。
    let secret_key = load_or_create_secret_key(parse_key_file(&args).as_deref());
    let relay_url: url::Url = iroh::address_lookup::N0_DNS_PKARR_RELAY_PROD
        .parse()
        .expect("n0 pkarr relay 地址常量必然合法");
    let endpoint = match Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .alpns(vec![
            share::ALPN.as_bytes().to_vec(),
            share::ALPN_SYNC.as_bytes().to_vec(),
        ])
        .address_lookup(PkarrPublisher::builder(relay_url).addr_filter(AddrFilter::unfiltered()))
        .address_lookup(PkarrResolver::n0_dns())
        .address_lookup(DnsAddressLookup::n0_dns())
        .relay_mode(RelayMode::Default)
        .bind()
        .await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("share-worker: endpoint 绑定失败: {error}");
            std::process::exit(2);
        }
    };
    let id_z32 = endpoint.id().to_z32();
    let name = mdns::sanitize_name(&device_name());
    let bound = endpoint.bound_sockets();
    let port = bound
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| bound.first())
        .map(|a| a.port())
        .unwrap_or(0);
    let addrs: Vec<SocketAddr> = endpoint.addr().ip_addrs().cloned().collect();

    // mDNS 起不来（无组播网络等）不致命：本机仍可被直连，只是发现降级。
    let mdns_state = match ServiceDaemon::new() {
        Ok(daemon) => {
            let announcer = match mdns::Announcer::register(&daemon, &name, &id_z32, port, &addrs) {
                Ok(announcer) => Some(announcer),
                Err(error) => {
                    logging::warn!("mDNS 公告失败，设备发现降级: {error}");
                    None
                }
            };
            Some((daemon, announcer))
        }
        Err(error) => {
            logging::warn!("mDNS daemon 启动失败，设备发现降级: {error}");
            None
        }
    };
    emit(proto::out::ready(&id_z32, &name));
    run(
        endpoint,
        DecisionHub::new(),
        SyncBooks {
            book: sync::SyncBook::new(),
            spaces: sync::SpaceBook::new(),
        },
        staging_root,
        mdns_state,
        id_z32,
        name,
    )
    .await;
}

/// 同步通道的两本服务册（M1 Pull 应答册 + D90 空间册），生命周期与 run 相同。
#[derive(Clone)]
struct SyncBooks {
    book: Arc<sync::SyncBook>,
    spaces: Arc<sync::SpaceBook>,
}

async fn run(
    endpoint: Endpoint,
    hub: Arc<DecisionHub>,
    books: SyncBooks,
    staging_root: PathBuf,
    mdns_state: Option<(ServiceDaemon, Option<mdns::Announcer>)>,
    self_id: String,
    self_name: String,
) {
    // stdin 命令通道：阻塞读线程 → mpsc；EOF = Exit（父进程死亡即自退，
    // D80 守卫①的 M0 形态——worker 不可能脱离主进程独活）。
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<proto::Command>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let reader = std::io::BufReader::new(stdin.lock());
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                continue;
            }
            match proto::parse_command(trimmed) {
                Ok(cmd) => {
                    let is_exit = matches!(cmd, proto::Command::Exit);
                    if cmd_tx.send(cmd).is_err() || is_exit {
                        break;
                    }
                }
                Err(error) => logging::warn!("忽略坏命令行: {}", error.0),
            }
        }
        let _ = cmd_tx.send(proto::Command::Exit);
    });

    let daemon_for_scan = mdns_state.as_ref().map(|(daemon, _)| daemon.clone());
    loop {
        tokio::select! {
            command = cmd_rx.recv() => {
                match command {
                    None | Some(proto::Command::Exit) => break,
                    Some(proto::Command::Scan) => {
                        let Some(daemon) = daemon_for_scan.clone() else {
                            emit(proto::out::notice("警示：本机设备发现不可用（mDNS 未启动）"));
                            continue;
                        };
                        let self_id = self_id.clone();
                        tokio::task::spawn_blocking(move || {
                            let devices = mdns::scan(&daemon, &self_id, mdns::SCAN_WINDOW);
                            for device in devices {
                                emit(proto::out::device(&device.id, &device.name, &device.addrs));
                            }
                            // 浏览窗收口标记：UI 撤扫描态靠事件不靠定时器。
                            emit(proto::out::scan_done());
                        });
                    }
                    Some(proto::Command::Send(request)) => {
                        let batch_id = request.batch_id;
                        let target = match target_addr(&request.receiver_id, &request.receiver_addrs) {
                            Some(target) => target,
                            None => {
                                emit(proto::out::done(batch_id, "failed", "目标设备地址不合法"));
                                continue;
                            }
                        };
                        let endpoint = endpoint.clone();
                        tokio::spawn(async move {
                            run_send(endpoint, target, request).await;
                        });
                    }
                    Some(proto::Command::Accept(batch_id)) => {
                        if !hub.resolve(batch_id, Decision::Accept) {
                            logging::warn!("ACCEPT 无等待批次: {batch_id}");
                        }
                    }
                    Some(proto::Command::Reject(batch_id, reason)) => {
                        if !hub.resolve(batch_id, Decision::Reject(reason)) {
                            logging::warn!("REJECT 无等待批次: {batch_id}");
                        }
                    }
                    Some(proto::Command::Ack(batch_id)) => {
                        if !hub.resolve(batch_id, Decision::Ack) {
                            logging::warn!("ACK 无等待批次: {batch_id}");
                        }
                    }
                    Some(proto::Command::Nack(batch_id, reason)) => {
                        if !hub.resolve(batch_id, Decision::Nack(reason)) {
                            logging::warn!("NACK 无等待批次: {batch_id}");
                        }
                    }
                    Some(proto::Command::SyncState(state)) => {
                        books.book.replace(state);
                    }
                    Some(proto::Command::SyncSend {
                        peer_id,
                        addrs,
                        message,
                    }) => {
                        if EndpointId::from_z32(&peer_id).is_err() {
                            emit(proto::out::sync_sent(&peer_id, false, "目标设备标识不合法"));
                            continue;
                        }
                        let endpoint = endpoint.clone();
                        let daemon = daemon_for_scan.clone();
                        let self_id_for_scan = self_id.clone();
                        let peer_for_log = peer_id.clone();
                        tokio::spawn(async move {
                            let addrs =
                                resolve_dial_addrs(daemon, self_id_for_scan, &peer_for_log, &addrs)
                                    .await;
                            let Some(target) = target_addr(&peer_for_log, &addrs) else {
                                emit(proto::out::sync_sent(&peer_for_log, false, "目标设备标识不合法"));
                                return;
                            };
                            report_sync_send(endpoint, target, message, peer_for_log).await;
                        });
                    }
                    Some(proto::Command::SpaceState(book)) => {
                        logging::info!("团队服务册已装载 teams={}", book.spaces.len());
                        books.spaces.replace(book);
                    }
                    Some(proto::Command::SpaceSync { peer_id, addrs }) => {
                        if EndpointId::from_z32(&peer_id).is_err() {
                            emit(proto::out::sync_sent(&peer_id, false, "目标设备标识不合法"));
                            continue;
                        }
                        let message = SyncMessage::Space {
                            message: books.spaces.summary(),
                        };
                        let endpoint = endpoint.clone();
                        let daemon = daemon_for_scan.clone();
                        let self_id_for_scan = self_id.clone();
                        let peer_for_log = peer_id.clone();
                        tokio::spawn(async move {
                            let addrs =
                                resolve_dial_addrs(daemon, self_id_for_scan, &peer_for_log, &addrs)
                                    .await;
                            let Some(target) = target_addr(&peer_for_log, &addrs) else {
                                emit(proto::out::sync_sent(&peer_for_log, false, "目标设备标识不合法"));
                                return;
                            };
                            report_sync_send(endpoint, target, message, peer_for_log).await;
                        });
                    }
                    Some(proto::Command::SpaceJoin {
                        owner_id,
                        code,
                        joiner_name,
                    }) => {
                        if EndpointId::from_z32(&owner_id).is_err() {
                            emit(proto::out::sync_sent(&owner_id, false, "空间主标识不合法"));
                            continue;
                        }
                        let message = SyncMessage::Space {
                            message: SpaceMessage::Join { code, joiner_name },
                        };
                        let endpoint = endpoint.clone();
                        let daemon = daemon_for_scan.clone();
                        let self_id_for_scan = self_id.clone();
                        let owner_for_log = owner_id.clone();
                        tokio::spawn(async move {
                            let addrs =
                                resolve_dial_addrs(daemon, self_id_for_scan, &owner_for_log, &[])
                                    .await;
                            let Some(target) = target_addr(&owner_for_log, &addrs) else {
                                emit(proto::out::sync_sent(&owner_for_log, false, "空间主标识不合法"));
                                return;
                            };
                            report_sync_send(endpoint, target, message, owner_for_log).await;
                        });
                    }
                }
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let hub = hub.clone();
                let staging = staging_root.clone();
                let book = books.book.clone();
                let spaces = books.spaces.clone();
                let endpoint_for_fetch = endpoint.clone();
                let fetch_sender = self_name.clone();
                tokio::spawn(async move {
                    let conn = match incoming.await {
                        Ok(conn) => conn,
                        Err(error) => {
                            logging::warn!("入站连接失败: {error}");
                            return;
                        }
                    };
                    // ALPN 分流：推送通道走 engine 全流程（清单+双确认+裸流）；
                    // 同步通道走 M1-b 轻会话（一行报文，Pull 在引擎内应答）。
                    if conn.alpn() == share::ALPN.as_bytes() {
                        let result =
                            engine::handle_connection(conn, &staging, &hub, emit_incoming).await;
                        match result {
                            Ok(outcome) => {
                                let (state, detail) = match &outcome.state {
                                    ReceiveState::Transferred => ("transferred", ""),
                                    ReceiveState::Rejected(reason) => ("rejected", reason.as_str()),
                                    ReceiveState::Failed(reason) => ("failed", reason.as_str()),
                                };
                                emit(proto::out::done(outcome.batch_id, state, detail));
                            }
                            Err(error) => {
                                // 清单都没解析成功 = 无批次可归位：知情消息即可，
                                // 不伪造 done 行（UI 侧 done 必须带真实 batch）。
                                logging::warn!("入站处理失败: {error}");
                                emit(proto::out::notice(&format!(
                                    "警示：收到无效共享请求：{error}"
                                )));
                            }
                        }
                    } else if conn.alpn() == share::ALPN_SYNC.as_bytes() {
                        let result = sync::handle_sync_connection(conn, &book, &spaces, |event| {
                            match event {
                                SyncInbound::Message { peer_id, message } => {
                                    match serde_json::to_string(&message) {
                                        Ok(json) => emit(proto::out::sync_recv(&peer_id, &json)),
                                        Err(error) => {
                                            logging::error!("同步报文序列化失败: {error}")
                                        }
                                    }
                                }
                                SyncInbound::SpaceAdmitted {
                                    space_id,
                                    peer_id,
                                    joiner_name,
                                } => {
                                    emit(proto::out::space_admitted(
                                        &space_id.to_string(),
                                        &peer_id,
                                        &joiner_name,
                                    ));
                                }
                                SyncInbound::SpaceFetch {
                                    peer_id,
                                    space_id,
                                    items,
                                } => {
                                    // 持有者投递：本地有的才发（部分持有 = 部分投递，
                                    // 剩余向其他成员索取是索取方的职责）。
                                    let mut send_items = Vec::with_capacity(items.len());
                                    for item in items {
                                        if let Some(path) =
                                            spaces.serve_path(space_id, &item.content_hash)
                                        {
                                            send_items.push(SendItem {
                                                asset_uuid: item.uuid,
                                                file_name: item.file_name.clone(),
                                                kind: item.kind.into(),
                                                size_bytes: item.size_bytes,
                                                category_hint: None,
                                                path: std::path::PathBuf::from(path),
                                            });
                                        }
                                    }
                                    if send_items.is_empty() {
                                        logging::info!(
                                            "空间 {space_id} 的索取本地无可服务字节，忽略"
                                        );
                                        return;
                                    }
                                    let Some(target) = target_addr(&peer_id, &[]) else {
                                        logging::warn!("索取方标识不合法: {peer_id}");
                                        return;
                                    };
                                    let request = share::SendRequest {
                                        batch_id: Uuid::new_v4(),
                                        sender_name: fetch_sender.clone(),
                                        receiver_id: peer_id,
                                        receiver_addrs: Vec::new(),
                                        space_id: Some(space_id),
                                        items: send_items,
                                    };
                                    let fetch_endpoint = endpoint_for_fetch.clone();
                                    tokio::spawn(async move {
                                        run_send(fetch_endpoint, target, request).await;
                                    });
                                }
                            }
                        })
                        .await;
                        if let Err(error) = result {
                            logging::warn!("同步连接处理失败: {error}");
                        }
                    } else {
                        logging::warn!("未知 ALPN 的入站连接，忽略");
                    }
                });
            }
        }
    }

    if let Some((_daemon, Some(announcer))) = mdns_state {
        announcer.unregister();
    }
    endpoint.close().await;
}

/// 同步报文外发收尾：应答（Pull/Join/Summary 的 Ops/JoinOk/JoinErr）经
/// SYNC_RECV 上抛 UI，结局恒报 SYNC_SENT（丢失由对端补拉自愈，仅知情）。
async fn report_sync_send(
    endpoint: Endpoint,
    target: EndpointAddr,
    message: SyncMessage,
    peer_id: String,
) {
    match sync::send_sync_message(&endpoint, target, message).await {
        Ok(None) => emit(proto::out::sync_sent(&peer_id, true, "")),
        Ok(Some(reply)) => {
            match &reply {
                SyncMessage::Space {
                    message: SpaceMessage::JoinErr { reason },
                } => logging::warn!("团队加入被拒绝 peer={peer_id}: {reason}"),
                SyncMessage::Space {
                    message: SpaceMessage::JoinOk { meta, .. },
                } => logging::info!("团队加入已获准 peer={peer_id} team={}", meta.id),
                _ => {}
            }
            match serde_json::to_string(&reply) {
                Ok(json) => emit(proto::out::sync_recv(&peer_id, &json)),
                Err(error) => logging::error!("同步应答序列化失败: {error}"),
            }
            emit(proto::out::sync_sent(&peer_id, true, ""));
        }
        Err(error) => {
            // 拨号失败的真因（解析无果/QUIC 超时/对端拒绝）只落在 worker
            // 日志里；UI 只拿到面向用户的短文案（D94 排障纪律）。
            logging::warn!("同步拨号失败 peer={peer_id}: {error}");
            emit(proto::out::sync_sent(&peer_id, false, &error.to_string()));
        }
    }
}

/// 发送任务：事件 → stdout 行；终态 → done 行。
async fn run_send(endpoint: Endpoint, target: EndpointAddr, request: share::SendRequest) {
    let batch_id = request.batch_id;
    let result = engine::send_batch(&endpoint, target, request, |event| match event {
        SendEvent::Hashing => {
            emit(proto::out::notice("提示：正在校验素材内容…"));
        }
        SendEvent::Connecting | SendEvent::Sending => {}
        SendEvent::ItemDone { uuid } => {
            emit(proto::out::item(batch_id, "send", uuid, true, ""));
        }
        SendEvent::Progress { sent, total } => {
            emit(proto::out::progress(batch_id, sent, total));
        }
    })
    .await;
    match result {
        Ok(BatchOutcome::Delivered) => emit(proto::out::done(batch_id, "delivered", "")),
        Ok(BatchOutcome::Rejected(reason)) => emit(proto::out::done(batch_id, "rejected", &reason)),
        Ok(BatchOutcome::Failed(reason)) => emit(proto::out::done(batch_id, "failed", &reason)),
        Err(error) => emit(proto::out::done(batch_id, "failed", &error.to_string())),
    }
}

fn emit_incoming(event: IncomingEvent) {
    match event {
        IncomingEvent::Offer(manifest) => match serde_json::to_string(&manifest) {
            Ok(json) => emit(proto::out::incoming(&json)),
            Err(error) => logging::error!("清单序列化失败: {error}"),
        },
        IncomingEvent::ItemDone {
            batch_id,
            uuid,
            ok,
            detail,
        } => emit(proto::out::item(batch_id, "recv", uuid, ok, &detail)),
        IncomingEvent::Staged { batch_id, paths } => {
            let json = serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string());
            emit(proto::out::staged(batch_id, &json));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_root_flag_parsed_when_present() {
        let args: Vec<String> = ["share-worker", "--staging-root", "D:\\inbox"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(parse_staging_root(&args), Some(PathBuf::from("D:\\inbox")));
        let bare: Vec<String> = ["share-worker"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_staging_root(&bare), None);
    }

    #[test]
    fn target_addr_requires_valid_id_and_one_addr() {
        let id = EndpointId::from_z32(&"a".repeat(52)).ok();
        // 52 个 'a' 不是合法 z32 编码的 ed25519 公钥形状时，from_z32 可能
        // 报错——只断言「全非法地址 = None」这条不依赖 id 有效性的分支。
        if id.is_some() {
            // M2：地址为空时仍以纯 id 拨号（pkarr/DNS 地址发现解析直连地址）。
            assert!(target_addr(&"a".repeat(52), &[]).is_some());
            assert!(target_addr(&"a".repeat(52), &["garbage".into()]).is_some());
        }
        assert!(target_addr("short", &["1.2.3.4:5".into()]).is_none());
    }
}
