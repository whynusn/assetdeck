//! 回环端到端测试（D80-M1 同步通道）：两台 endpoint 在 127.0.0.1 上走
//! NOTIFY/PULL/SNAPSHOT 线协议。不碰 mDNS、不出本机。

use std::net::SocketAddr;
use std::time::Duration;

use domain::AssetKind;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr};
use share::{PeerSyncState, ShareOffer, SyncBookState, SyncMessage, ALPN, ALPN_SYNC};
use share_worker::sync::{handle_sync_connection, send_sync_message, SyncBook, SyncInbound};
use tokio::sync::oneshot;
use uuid::Uuid;

async fn bind_endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .alpns(vec![
            ALPN.as_bytes().to_vec(),
            ALPN_SYNC.as_bytes().to_vec(),
        ])
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

fn offer(name: &str) -> ShareOffer {
    ShareOffer {
        asset_uuid: Uuid::new_v4(),
        file_name: name.to_string(),
        kind: AssetKind::Image,
        size_bytes: 4096,
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
async fn loopback_pull_answers_peer_filtered_snapshot() {
    let server = bind_endpoint().await;
    let client = bind_endpoint().await;
    let outsider = bind_endpoint().await;
    // Endpoint 句柄克隆与 server 指向同一实体，专供计算回环目标。
    let server_handle = server.clone();
    let target = loopback_target(&server_handle);

    let book = SyncBook::new();
    let mut state = SyncBookState::default();
    state.set(
        client.id().to_z32(),
        PeerSyncState {
            rev: 7,
            offers: vec![offer("a.png"), offer("b.png")],
        },
    );
    // 另一对端的条目不得漏进本对端的视角。
    state.set(
        "b".repeat(52),
        PeerSyncState {
            rev: 9,
            offers: vec![offer("secret.png")],
        },
    );
    book.replace(state);

    // 服务端连收两条：已配对对端 + 册里没有的陌生人。
    let serve = tokio::spawn({
        let book = book.clone();
        async move {
            for _ in 0..2 {
                let conn = server.accept().await.expect("accept").await.expect("conn");
                handle_sync_connection(conn, &book, |_| {})
                    .await
                    .expect("服务端报错");
            }
        }
    });

    let reply = with_timeout(
        "pull",
        send_sync_message(&client, target, SyncMessage::Pull { since_rev: 0 }),
    )
    .await
    .expect("拉取失败");
    match reply.expect("Pull 必有应答") {
        SyncMessage::Snapshot { rev, offers } => {
            assert_eq!(rev, 7);
            let names: Vec<&str> = offers.iter().map(|o| o.file_name.as_str()).collect();
            assert_eq!(names, vec!["a.png", "b.png"], "只含本对端视角");
        }
        other => panic!("应答应为 Snapshot，实得 {other:?}"),
    }

    // 册里没有的对端 = 零可得（fail-closed，不给任何元数据）。
    let reply = with_timeout(
        "pull-outsider",
        send_sync_message(
            &outsider,
            loopback_target(&server_handle),
            SyncMessage::Pull { since_rev: 0 },
        ),
    )
    .await
    .expect("陌生人拉取失败");
    match reply.expect("Pull 必有应答") {
        SyncMessage::Snapshot { rev, offers } => {
            assert_eq!(rev, 0);
            assert!(offers.is_empty());
        }
        other => panic!("应答应为 Snapshot，实得 {other:?}"),
    }

    with_timeout("serve", serve).await.expect("serve panic");
}

#[tokio::test]
async fn loopback_delta_reaches_server_with_verified_sender() {
    let server = bind_endpoint().await;
    let client = bind_endpoint().await;
    let server_handle = server.clone();
    let target = loopback_target(&server_handle);

    let (msg_tx, msg_rx) = oneshot::channel();
    let mut msg_tx = Some(msg_tx);
    let serve = tokio::spawn(async move {
        let conn = server.accept().await.expect("accept").await.expect("conn");
        handle_sync_connection(conn, &SyncBook::new(), |event| {
            let SyncInbound::Message { peer_id, message } = event;
            if let Some(tx) = msg_tx.take() {
                let _ = tx.send((peer_id, message));
            }
        })
        .await
        .expect("服务端报错")
    });

    let delta = SyncMessage::Delta {
        from_rev: 4,
        to_rev: 5,
        added: vec![offer("new.png")],
        removed: vec![Uuid::new_v4()],
    };
    let result = with_timeout("delta", send_sync_message(&client, target, delta))
        .await
        .expect("推送失败");
    assert_eq!(result, None, "Delta 无应答");

    let (peer_id, message) = with_timeout("inbound", msg_rx).await.expect("事件通道断裂");
    assert_eq!(
        peer_id,
        client.id().to_z32(),
        "发送方身份必须来自 QUIC 握手而非报文自报"
    );
    match message {
        SyncMessage::Delta {
            from_rev,
            to_rev,
            added,
            removed,
        } => {
            assert_eq!((from_rev, to_rev), (4, 5));
            assert_eq!(added.len(), 1);
            assert_eq!(removed.len(), 1);
        }
        other => panic!("应送达 Delta，实得 {other:?}"),
    }
    with_timeout("serve", serve).await.expect("serve panic");
}
