//! Public-network acceptance: production endpoints, fresh identities, no IP transport,
//! no online prewarming, and only the remote identity supplied to each dial.

use std::time::Duration;

use domain::AssetKind;
use iroh::{EndpointAddr, SecretKey};
use share::{
    SendItem, SendRequest, SpaceBookEntry, SpaceBookState, SpaceMessage, SpaceRegistry, SyncMessage,
};
use share_worker::dial::production_endpoint;
use share_worker::engine::{self, BatchOutcome, Decision, DecisionHub, IncomingEvent};
use share_worker::sync::{handle_sync_connection, send_sync_message, SpaceBook, SyncBook};
use tokio::sync::oneshot;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires n0 public discovery and relay access"]
async fn public_relay_cold_identity_join_and_file_transfer() {
    tokio::time::timeout(Duration::from_secs(150), async {
        let server = production_endpoint(SecretKey::generate(), true)
            .await
            .unwrap();
        let client = production_endpoint(SecretKey::generate(), true)
            .await
            .unwrap();
        assert!(server.bound_sockets().is_empty());
        assert!(client.bound_sockets().is_empty());
        assert_eq!(server.addr().ip_addrs().count(), 0);
        assert_eq!(client.addr().ip_addrs().count(), 0);

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
                .expect("handshake");
            handle_sync_connection(conn, &SyncBook::new(), &spaces, receiver.id(), |_| {})
                .await
                .unwrap();
        });
        let reply = send_sync_message(
            &client,
            EndpointAddr::new(server.id()),
            SyncMessage::Space {
                message: SpaceMessage::Join {
                    code,
                    joiner_name: "relay-member".into(),
                },
            },
        )
        .await
        .expect("cold pure-ID Join failed");
        assert!(matches!(reply, Some(SyncMessage::Space {
            message: SpaceMessage::JoinOk { meta: joined, .. },
        }) if joined.id == meta.id && joined.is_member(&client.id().to_z32())));
        serve.await.unwrap();

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
            space_id: Some(meta.id),
            items: vec![SendItem {
                asset_uuid: Uuid::new_v4(),
                file_name: "relay.bin".into(),
                kind: AssetKind::Other,
                size_bytes: payload.len() as u64,
                category_hint: None,
                path,
            }],
        };
        let decisions = DecisionHub::new();
        let receiver = server.clone();
        let expected_sender = client.id().to_z32();
        let (bytes_tx, bytes_rx) = oneshot::channel();
        let serve = tokio::spawn(async move {
            let mut bytes_tx = Some(bytes_tx);
            let conn = receiver
                .accept()
                .await
                .expect("accept")
                .await
                .expect("handshake");
            engine::handle_connection(conn, &staging_path, &decisions, |event| match event {
                IncomingEvent::Offer {
                    manifest,
                    authenticated_sender_id,
                } => {
                    assert_eq!(authenticated_sender_id, expected_sender);
                    assert!(decisions.resolve(manifest.batch_id, Decision::Accept));
                }
                IncomingEvent::Staged { batch_id, paths } => {
                    bytes_tx
                        .take()
                        .unwrap()
                        .send(std::fs::read(&paths[0]).unwrap())
                        .unwrap();
                    assert!(decisions.resolve(batch_id, Decision::Ack));
                }
                IncomingEvent::ItemDone { .. } => {}
            })
            .await
            .unwrap();
        });
        let result = engine::send_batch(&client, EndpointAddr::new(server.id()), request, |_| {})
            .await
            .unwrap();
        assert_eq!(result, BatchOutcome::Delivered);
        assert_eq!(bytes_rx.await.unwrap(), payload);
        serve.await.unwrap();
        client.close().await;
        server.close().await;
    })
    .await
    .expect("public relay acceptance timed out");
}
