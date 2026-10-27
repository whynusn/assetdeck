use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{endpoint::presets, Endpoint, EndpointAddr};
use share::{
    RosterMember, ServeItem, SpaceAssetKind, SpaceAssetMeta, SpaceBookEntry, SpaceBookState,
    SpaceMessage, SpaceOp, SpaceOpKind, SpaceRegistry, SyncMessage, ALPN_SYNC,
};
use share_worker::sync::{
    handle_sync_connection, send_sync_message, SpaceBook, SyncBook, SyncInbound,
};
use uuid::Uuid;

async fn endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN_SYNC.as_bytes().to_vec()])
        .bind()
        .await
        .unwrap()
}

#[tokio::test]
async fn fetch_reports_unavailable_hashes_without_leaking_to_outsiders() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = endpoint().await;
        let client = endpoint().await;
        let outsider = endpoint().await;
        let port = server.bound_sockets().iter().find(|addr| addr.is_ipv4()).unwrap().port();
        let target = EndpointAddr::new(server.id()).with_ip_addr(([127, 0, 0, 1], port).into());
        let mut registry = SpaceRegistry::new();
        registry.self_id = Some(server.id().to_z32());
        registry.self_name = "owner".into();
        let mut meta = registry.create_space("synthetic", 1).unwrap();
        meta.members.push(RosterMember { id: client.id().to_z32(), name: "member".into() });
        let asset = SpaceAssetMeta { uuid: Uuid::new_v4(), file_name: "sample.txt".into(),
            kind: SpaceAssetKind::Text, size_bytes: 4, content_hash: vec![1; 32] };
        let mut absent = asset.clone();
        absent.uuid = Uuid::new_v4();
        absent.content_hash = vec![2; 32];
        let book = SpaceBook::new();
        book.replace(SpaceBookState { spaces: [(meta.id, SpaceBookEntry { meta: meta.clone(),
            ops: vec![SpaceOp { space_id: meta.id, lamport: 1, author: server.id().to_z32(), ts: 1,
                op: SpaceOpKind::Add { asset: asset.clone() } }],
            serve: vec![ServeItem { content_hash: asset.content_hash.clone(), path: r"C:\synthetic\sample.txt".into() }],
        })].into_iter().collect() });
        let served = Arc::new(Mutex::new(Vec::new()));
        let observed = served.clone();
        let serving = tokio::spawn(async move {
            for _ in 0..2 {
                let conn = server.accept().await.unwrap().await.unwrap();
                handle_sync_connection(conn, &SyncBook::new(), &book, server.id(), |event| {
                    if let SyncInbound::SpaceFetch { items, .. } = event {
                        observed.lock().unwrap().extend(items.into_iter().map(|(asset, _)| asset.content_hash));
                    }
                }).await.unwrap();
            }
        });
        let request = SyncMessage::Space { message: SpaceMessage::Fetch { space_id: meta.id,
            items: vec![asset.clone(), absent.clone()] } };
        let reply = send_sync_message(&client, target.clone(), request.clone()).await.unwrap();
        assert!(matches!(reply, Some(SyncMessage::Space { message: SpaceMessage::FetchUnavailable { hashes, .. } }) if hashes == vec![absent.content_hash.clone()]));
        let reply = send_sync_message(&outsider, target, request).await.unwrap();
        assert!(matches!(reply, Some(SyncMessage::Space { message: SpaceMessage::FetchUnavailable { hashes, .. } }) if hashes == vec![asset.content_hash.clone(), absent.content_hash]));
        serving.await.unwrap();
        assert_eq!(*served.lock().unwrap(), vec![asset.content_hash]);
    }).await.expect("bounded fetch exchange");
}
