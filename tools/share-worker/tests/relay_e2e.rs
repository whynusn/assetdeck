//! 依赖 n0 公共中继；两端禁用 IP 传输，避免把本机直连误判成中继成功。

use std::time::Duration;

use domain::AssetKind;
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayMode};
use share::{
    PeerSyncState, SendItem, SendRequest, SpaceBookEntry, SpaceBookState, SpaceMessage,
    SpaceRegistry, SyncBookState, SyncMessage, ALPN, ALPN_SYNC,
};
use share_worker::engine::{self, BatchOutcome, Decision, DecisionHub, IncomingEvent};
use share_worker::sync::{handle_sync_connection, send_sync_message, SpaceBook, SyncBook};
use tokio::sync::oneshot;
use uuid::Uuid;

async fn relay_endpoint() -> Endpoint {
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Default)
        .clear_ip_transports()
        .alpns(vec![
            ALPN.as_bytes().to_vec(),
            ALPN_SYNC.as_bytes().to_vec(),
        ])
        .bind()
        .await
        .expect("绑定中继端点失败");
    tokio::time::timeout(Duration::from_secs(60), endpoint.online())
        .await
        .expect("公共中继上线超时");
    assert!(endpoint.bound_sockets().is_empty());
    assert_eq!(endpoint.addr().ip_addrs().count(), 0);
    assert!(endpoint.addr().relay_urls().next().is_some());
    endpoint
}

#[tokio::test]
#[ignore = "需要访问 n0 公共中继；手动运行"]
async fn public_relay_sync_and_file_transfer() {
    tokio::time::timeout(Duration::from_secs(150), async {
        let server = relay_endpoint().await;
        let client = relay_endpoint().await;
        let book = SyncBook::new();
        let mut state = SyncBookState::default();
        state.set(
            client.id().to_z32(),
            PeerSyncState {
                rev: 7,
                offers: Vec::new(),
            },
        );
        book.replace(state);
        let receiver = server.clone();
        let serve = tokio::spawn(async move {
            let conn = receiver
                .accept()
                .await
                .expect("accept")
                .await
                .expect("握手失败");
            handle_sync_connection(conn, &book, &SpaceBook::new(), |_| {})
                .await
                .expect("中继同步服务失败");
        });
        let reply = send_sync_message(&client, server.addr(), SyncMessage::Pull { since_rev: 0 })
            .await
            .expect("中继同步拨号失败");
        assert!(
            matches!(reply, Some(SyncMessage::Snapshot { rev: 7, offers }) if offers.is_empty())
        );
        serve.await.expect("同步任务失败");

        let mut registry = SpaceRegistry::new();
        registry.self_id = Some(server.id().to_z32());
        registry.self_name = "relay-owner".into();
        let meta = registry.create_space("relay-team", 1).unwrap();
        let code = share::space_invite_code(&meta);
        let spaces = SpaceBook::new();
        spaces.replace(SpaceBookState {
            spaces: [(
                meta.id,
                SpaceBookEntry {
                    meta: meta.clone(),
                    ops: Vec::new(),
                    serve: Vec::new(),
                },
            )]
            .into_iter()
            .collect(),
        });
        let receiver = server.clone();
        let serve = tokio::spawn(async move {
            let conn = receiver
                .accept()
                .await
                .expect("accept")
                .await
                .expect("握手失败");
            handle_sync_connection(conn, &SyncBook::new(), &spaces, |_| {})
                .await
                .expect("中继加入服务失败");
        });
        let reply = send_sync_message(
            &client,
            server.addr(),
            SyncMessage::Space {
                message: SpaceMessage::Join {
                    code,
                    joiner_name: "relay-member".into(),
                },
            },
        )
        .await
        .expect("中继加入拨号失败");
        assert!(matches!(reply, Some(SyncMessage::Space {
            message: SpaceMessage::JoinOk { meta: joined, .. },
        }) if joined.id == meta.id && joined.is_member(&client.id().to_z32())));
        serve.await.expect("加入任务失败");

        let source = tempfile::tempdir().unwrap();
        let payload = vec![37u8; 300_000];
        let path = source.path().join("relay.bin");
        std::fs::write(&path, &payload).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let staging_path = staging.path().to_path_buf();
        let request = SendRequest {
            batch_id: Uuid::new_v4(),
            sender_name: "relay-test".into(),
            receiver_id: server.id().to_z32(),
            receiver_addrs: Vec::new(),
            space_id: None,
            items: vec![SendItem {
                asset_uuid: Uuid::new_v4(),
                file_name: "relay.bin".into(),
                kind: AssetKind::Other,
                size_bytes: payload.len() as u64,
                category_hint: None,
                path,
            }],
        };
        let hub = DecisionHub::new();
        let decisions = hub.clone();
        let receiver = server.clone();
        let (bytes_tx, bytes_rx) = oneshot::channel();
        let serve = tokio::spawn(async move {
            let mut bytes_tx = Some(bytes_tx);
            let conn = receiver
                .accept()
                .await
                .expect("accept")
                .await
                .expect("握手失败");
            engine::handle_connection(conn, &staging_path, &decisions, |event| match event {
                IncomingEvent::Offer(manifest) => {
                    assert!(decisions.resolve(manifest.batch_id, Decision::Accept));
                }
                IncomingEvent::Staged { batch_id, paths } => {
                    let bytes = std::fs::read(&paths[0]).unwrap();
                    bytes_tx.take().unwrap().send(bytes).unwrap();
                    assert!(decisions.resolve(batch_id, Decision::Ack));
                }
                IncomingEvent::ItemDone { .. } => {}
            })
            .await
            .expect("中继接收失败");
        });
        let result = engine::send_batch(&client, server.addr(), request, |_| {})
            .await
            .expect("中继发送失败");
        assert_eq!(result, BatchOutcome::Delivered);
        assert_eq!(bytes_rx.await.unwrap(), payload);
        serve.await.expect("接收任务失败");
        client.close().await;
        server.close().await;
    })
    .await
    .expect("公共中继端到端验证超时");
}
