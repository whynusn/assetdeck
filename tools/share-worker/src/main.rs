//! share-worker（D80）：P2P 素材共享的传输引擎进程。
//!
//! 进程模型（D11/D33 同族）：iroh + tokio 只进这一个编译单元；UI 主进程
//! 永不碰网络栈。生命周期 = 主进程：stdin 管道关闭（父进程退出）即自退，
//! 无系统服务、无开机自启。空闲成本 = 一个 UDP socket + mDNS 公告线程。
//!
//! M0 局域网闭环（D80 分期）：mDNS 发现 → 显式推送 → iroh 直连 →
//! 清单先行 → 双重确认 → 导入管线。零 STUN/打洞/信令服务器；
//! `presets::Minimal` + `PortmapperConfig::Disabled` 把「不出户」钉死在
//! 配置层（钉子 1/2 的 M0 形态：M1 才引入 pkarr 信令与端口映射）。
//!
//! 协议见 [`proto`]；引擎见 [`engine`]。

use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::presets;
use iroh::endpoint::PortmapperConfig;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use mdns_sd::ServiceDaemon;
use share_worker::{engine, mdns, proto};

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

/// 设备表 → EndpointAddr（id + 直连地址）。地址全非法 = None。
fn target_addr(id_z32: &str, addrs: &[String]) -> Option<EndpointAddr> {
    let id: EndpointId = EndpointId::from_z32(id_z32).ok()?;
    let mut addr = EndpointAddr::new(id);
    let mut any = false;
    for value in addrs {
        if let Ok(socket) = value.parse::<SocketAddr>() {
            addr = addr.with_ip_addr(socket);
            any = true;
        }
    }
    any.then_some(addr)
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

    let endpoint = match Endpoint::builder(presets::Minimal)
        .alpns(vec![share::ALPN.as_bytes().to_vec()])
        .portmapper_config(PortmapperConfig::Disabled)
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
        staging_root,
        mdns_state,
        id_z32,
    )
    .await;
}

async fn run(
    endpoint: Endpoint,
    hub: Arc<DecisionHub>,
    staging_root: PathBuf,
    mdns_state: Option<(ServiceDaemon, Option<mdns::Announcer>)>,
    self_id: String,
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
                }
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let hub = hub.clone();
                let staging = staging_root.clone();
                tokio::spawn(async move {
                    let conn = match incoming.await {
                        Ok(conn) => conn,
                        Err(error) => {
                            logging::warn!("入站连接失败: {error}");
                            return;
                        }
                    };
                    let result = engine::handle_connection(conn, &staging, &hub, emit_incoming).await;
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
                });
            }
        }
    }

    if let Some((_daemon, Some(announcer))) = mdns_state {
        announcer.unregister();
    }
    endpoint.close().await;
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
            assert!(target_addr(&"a".repeat(52), &[]).is_none());
            assert!(target_addr(&"a".repeat(52), &["garbage".into()]).is_none());
        }
        assert!(target_addr("short", &["1.2.3.4:5".into()]).is_none());
    }
}
