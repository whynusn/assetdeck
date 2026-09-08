//! 回环端到端测试（D80 守卫④的引擎侧）：两台 endpoint 在 127.0.0.1 上
//! 走完整线协议——清单先行 → 首连确认 → 逐文件裸流 + SHA-256 → 导入
//! 收尾 ACK → DELIVERED。不碰 mDNS、不出本机。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use domain::AssetKind;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr};
use share::{SendItem, SendRequest};
use share_worker::engine::{
    self, BatchOutcome, Decision, DecisionHub, IncomingEvent, ReceiveState, SendEvent,
};
use tokio::sync::oneshot;
use uuid::Uuid;

const ALPN_BYTES: &[u8] = b"assetdeck-share/1";

async fn bind_endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN_BYTES.to_vec()])
        .bind()
        .await
        .expect("测试端点绑定失败")
}

fn loopback_target(endpoint: &Endpoint) -> EndpointAddr {
    let port = endpoint
        .bound_sockets()
        .iter()
        .find(|a| a.is_ipv4())
        .expect("应有 IPv4 绑定套接字")
        .port();
    EndpointAddr::new(endpoint.id()).with_ip_addr(SocketAddr::from(([127, 0, 0, 1], port)))
}

fn write_source(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("写测试源文件失败");
    path
}

fn request_for(batch_id: Uuid, receiver: &Endpoint, items: Vec<SendItem>) -> SendRequest {
    SendRequest {
        batch_id,
        sender_name: "TESTER".to_string(),
        receiver_id: receiver.id().to_z32(),
        receiver_addrs: receiver
            .bound_sockets()
            .iter()
            .filter(|a| a.is_ipv4())
            .map(|a| SocketAddr::new("127.0.0.1".parse().unwrap(), a.port()).to_string())
            .collect(),
        items,
    }
}

async fn with_timeout<T: std::fmt::Debug>(
    what: &str,
    fut: impl std::future::Future<Output = T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .unwrap_or_else(|_| panic!("{what} 超时"))
}

#[tokio::test]
async fn loopback_delivers_and_verifies_bytes() {
    let recv_ep = bind_endpoint().await;
    let send_ep = bind_endpoint().await;
    let target = loopback_target(&recv_ep);

    let src = tempfile::tempdir().unwrap();
    let file_a = write_source(src.path(), "a.png", b"hello assetdeck a");
    // >256KB：跨多个分块，顺带走进度节流路径。
    let file_b = write_source(src.path(), "b.bin", &vec![7u8; 300_000]);
    let uuid_a = Uuid::new_v4();
    let uuid_b = Uuid::new_v4();
    let batch_id = Uuid::new_v4();
    let request = request_for(
        batch_id,
        &recv_ep,
        vec![
            SendItem {
                asset_uuid: uuid_a,
                file_name: "a.png".into(),
                kind: AssetKind::Image,
                size_bytes: 0,
                category_hint: Some("壁纸".into()),
                path: file_a.clone(),
            },
            SendItem {
                asset_uuid: uuid_b,
                file_name: "b.bin".into(),
                kind: AssetKind::Other,
                size_bytes: 0,
                category_hint: None,
                path: file_b.clone(),
            },
        ],
    );

    let staging = tempfile::tempdir().unwrap();
    let hub = DecisionHub::new();

    // 接收侧：捕获 Offer 与 Staged 两个事件。
    let (offer_tx, offer_rx) = oneshot::channel();
    let (staged_tx, staged_rx) = oneshot::channel();
    let mut offer_tx = Some(offer_tx);
    let mut staged_tx = Some(staged_tx);
    let serve_hub = hub.clone();
    let staging_path = staging.path().to_path_buf();
    let serve_ep = recv_ep.clone();
    let serve = tokio::spawn(async move {
        let incoming = serve_ep.accept().await.expect("accept 返回 None");
        let conn = incoming.await.expect("握手失败");
        engine::handle_connection(conn, &staging_path, &serve_hub, |event| match event {
            IncomingEvent::Offer(manifest) => {
                if let Some(tx) = offer_tx.take() {
                    let _ = tx.send(manifest);
                }
            }
            IncomingEvent::Staged { paths, .. } => {
                if let Some(tx) = staged_tx.take() {
                    let _ = tx.send(paths);
                }
            }
            IncomingEvent::ItemDone { .. } => {}
        })
        .await
    });

    // 发送侧：事件收口用于断言节流进度出现过。
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let send_task = tokio::spawn(async move {
        let progress_seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = progress_seen.clone();
        let outcome = engine::send_batch(&send_ep, target, request, |event| {
            if matches!(event, SendEvent::Progress { .. }) {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let _ = event_tx.send(event);
        })
        .await;
        (
            outcome,
            progress_seen.load(std::sync::atomic::Ordering::Relaxed),
        )
    });

    // 首连确认：清单到达后放行。
    let manifest = with_timeout("Offer", offer_rx)
        .await
        .expect("Offer 通道断裂");
    assert_eq!(manifest.batch_id, batch_id);
    assert_eq!(manifest.sender_name, "TESTER");
    assert_eq!(manifest.items.len(), 2);
    assert_eq!(manifest.items[0].file_name, "a.png");
    assert_eq!(manifest.items[0].category_hint.as_deref(), Some("壁纸"));
    assert_eq!(
        manifest.items[0].size_bytes,
        std::fs::metadata(&file_a).unwrap().len()
    );
    assert_eq!(manifest.items[1].size_bytes, 300_000);
    // 清单里的 SHA-256 必须与源文件实算一致（发送侧现算，非 UI 传入）。
    let expected_a = sha2_hex(&file_a);
    assert_eq!(manifest.items[0].sha256_hex, expected_a);
    assert!(hub.resolve(batch_id, Decision::Accept));

    // 导入收尾：暂存完成后放行。真实流程里导入管线在 ACK 前已把暂存文件
    // 复制入库，而引擎在 DELIVERED 后即清空暂存区——所以字节等值断言必须
    // 在放行 Ack 之前取快照（此刻文件还在暂存区）。
    let staged_paths = with_timeout("Staged", staged_rx)
        .await
        .expect("Staged 通道断裂");
    assert_eq!(staged_paths.len(), 2);
    assert!(staged_paths[0].ends_with(format!("{uuid_a}.png").as_str()));
    assert!(staged_paths[1].ends_with(format!("{uuid_b}.bin").as_str()));
    let staged_a = std::fs::read(&staged_paths[0]).unwrap();
    let staged_b = std::fs::read(&staged_paths[1]).unwrap();
    assert!(hub.resolve(batch_id, Decision::Ack));

    let (outcome, progress_seen) = with_timeout("send", send_task).await.expect("send panic");
    assert_eq!(outcome.expect("发送侧引擎报错"), BatchOutcome::Delivered);
    assert!(progress_seen, "300KB 传输应触发节流进度事件");

    let receive = with_timeout("serve", serve)
        .await
        .expect("serve task panic");
    let receive = receive.expect("接收侧引擎报错");
    assert_eq!(receive.batch_id, batch_id);
    assert_eq!(receive.state, ReceiveState::Transferred);

    // 字节等值（D65 判死权的上游保真）。
    assert_eq!(staged_a, std::fs::read(&file_a).unwrap());
    assert_eq!(staged_b, std::fs::read(&file_b).unwrap());
    // ACK 后暂存区清空（导入已复制入库）。
    assert!(!staging.path().join(batch_id.to_string()).exists());
    // 发送侧逐项事件齐了。
    let mut items = 0;
    while let Ok(event) = event_rx.try_recv() {
        if matches!(event, SendEvent::ItemDone { .. }) {
            items += 1;
        }
    }
    assert_eq!(items, 2);
}

#[tokio::test]
async fn loopback_reject_leaves_no_staging() {
    let recv_ep = bind_endpoint().await;
    let send_ep = bind_endpoint().await;
    let target = loopback_target(&recv_ep);

    let src = tempfile::tempdir().unwrap();
    let file = write_source(src.path(), "x.png", b"payload");
    let batch_id = Uuid::new_v4();
    let request = request_for(
        batch_id,
        &recv_ep,
        vec![SendItem {
            asset_uuid: Uuid::new_v4(),
            file_name: "x.png".into(),
            kind: AssetKind::Image,
            size_bytes: 0,
            category_hint: None,
            path: file,
        }],
    );

    let staging = tempfile::tempdir().unwrap();
    let hub = DecisionHub::new();
    let (offer_tx, offer_rx) = oneshot::channel();
    let mut offer_tx = Some(offer_tx);
    let serve_hub = hub.clone();
    let staging_path = staging.path().to_path_buf();
    let serve_ep = recv_ep.clone();
    let serve = tokio::spawn(async move {
        let conn = serve_ep
            .accept()
            .await
            .expect("accept")
            .await
            .expect("conn");
        engine::handle_connection(conn, &staging_path, &serve_hub, |event| {
            if let IncomingEvent::Offer(manifest) = event {
                if let Some(tx) = offer_tx.take() {
                    let _ = tx.send(manifest.batch_id);
                }
            }
        })
        .await
    });

    let send_task =
        tokio::spawn(
            async move { engine::send_batch(&send_ep, target, request, |_event| {}).await },
        );

    let offered_batch = with_timeout("Offer", offer_rx).await.expect("Offer 断裂");
    assert_eq!(offered_batch, batch_id);
    assert!(hub.resolve(batch_id, Decision::Reject("不想要".into())));

    let outcome = with_timeout("send", send_task).await.expect("send panic");
    assert_eq!(
        outcome.expect("引擎报错"),
        BatchOutcome::Rejected("不想要".into())
    );
    let receive = with_timeout("serve", serve).await.expect("serve panic");
    let receive = receive.expect("接收引擎报错");
    assert!(matches!(receive.state, ReceiveState::Rejected(_)));
    // 拒绝发生在任何文件落盘之前：暂存目录保持空。
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn loopback_nack_marks_failed_and_cleans_staging() {
    let recv_ep = bind_endpoint().await;
    let send_ep = bind_endpoint().await;
    let target = loopback_target(&recv_ep);

    let src = tempfile::tempdir().unwrap();
    let file = write_source(src.path(), "y.png", b"payload-y");
    let batch_id = Uuid::new_v4();
    let request = request_for(
        batch_id,
        &recv_ep,
        vec![SendItem {
            asset_uuid: Uuid::new_v4(),
            file_name: "y.png".into(),
            kind: AssetKind::Image,
            size_bytes: 0,
            category_hint: None,
            path: file,
        }],
    );

    let staging = tempfile::tempdir().unwrap();
    let hub = DecisionHub::new();
    let (offer_tx, offer_rx) = oneshot::channel();
    let mut offer_tx = Some(offer_tx);
    let (staged_tx, staged_rx) = oneshot::channel();
    let mut staged_tx = Some(staged_tx);
    let serve_hub = hub.clone();
    let staging_path = staging.path().to_path_buf();
    let serve_ep = recv_ep.clone();
    let serve = tokio::spawn(async move {
        let conn = serve_ep
            .accept()
            .await
            .expect("accept")
            .await
            .expect("conn");
        engine::handle_connection(conn, &staging_path, &serve_hub, |event| match event {
            IncomingEvent::Offer(_) => {
                if let Some(tx) = offer_tx.take() {
                    let _ = tx.send(());
                }
            }
            IncomingEvent::Staged { .. } => {
                if let Some(tx) = staged_tx.take() {
                    let _ = tx.send(());
                }
            }
            IncomingEvent::ItemDone { .. } => {}
        })
        .await
    });

    let send_task =
        tokio::spawn(
            async move { engine::send_batch(&send_ep, target, request, |_event| {}).await },
        );

    with_timeout("Offer", offer_rx).await.expect("Offer 断裂");
    assert!(hub.resolve(batch_id, Decision::Accept));
    with_timeout("Staged", staged_rx)
        .await
        .expect("Staged 断裂");
    // 导入失败（用户在归类弹窗取消）→ NACK。
    assert!(hub.resolve(batch_id, Decision::Nack("用户取消归类".into())));

    let outcome = with_timeout("send", send_task).await.expect("send panic");
    assert_eq!(
        outcome.expect("引擎报错"),
        BatchOutcome::Failed("用户取消归类".into())
    );
    let receive = with_timeout("serve", serve).await.expect("serve panic");
    let receive = receive.expect("接收引擎报错");
    assert!(matches!(receive.state, ReceiveState::Failed(_)));
    assert!(!staging.path().join(batch_id.to_string()).exists());
}

fn sha2_hex(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).unwrap();
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
