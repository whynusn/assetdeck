//! 回环端到端测试（D80-M1 同步通道）：两台 endpoint 在 127.0.0.1 上走
//! NOTIFY/PULL/SNAPSHOT 线协议。不碰 mDNS、不出本机。

use std::net::SocketAddr;
use std::time::Duration;

use domain::AssetKind;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr};
use share::{
    OpKey, PeerSyncState, ShareOffer, SpaceBookEntry, SpaceBookState, SpaceMessage, SpaceMeta,
    SpaceOp, SpaceOpKind, SpaceRegistry, SpaceSummaryEntry, SyncBookState, SyncMessage, ALPN,
    ALPN_SYNC,
};
use share_worker::sync::{
    handle_sync_connection, send_sync_message, SpaceBook, SyncBook, SyncInbound,
};
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
        let spaces = SpaceBook::new();
        async move {
            for _ in 0..2 {
                let conn = server.accept().await.expect("accept").await.expect("conn");
                handle_sync_connection(conn, &book, &spaces, |_| {})
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
        handle_sync_connection(conn, &SyncBook::new(), &SpaceBook::new(), |event| {
            if let SyncInbound::Message { peer_id, message } = event {
                if let Some(tx) = msg_tx.take() {
                    let _ = tx.send((peer_id, message));
                }
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

// —— D90-P2 空间协议回环：抗熵差量 + 贴码加入 ——

fn owner_space_book() -> (std::sync::Arc<SpaceBook>, SpaceMeta, Vec<SpaceOp>) {
    let owner_id = "a".repeat(52);
    let mut registry = SpaceRegistry::new();
    registry.self_id = Some(owner_id.clone());
    registry.self_name = "店长".into();
    let meta = registry.create_space("团队素材", 1000).unwrap();
    let op = |lamport: u64| SpaceOp {
        space_id: meta.id,
        lamport,
        author: owner_id.clone(),
        ts: 0,
        op: SpaceOpKind::CategoryCreate {
            name: "新品".into(),
        },
    };
    let ops = vec![op(1), op(2), op(3)];
    let book = SpaceBook::new();
    book.replace(SpaceBookState {
        spaces: [(
            meta.id,
            SpaceBookEntry {
                meta: meta.clone(),
                ops: ops.clone(),
                serve: Vec::new(),
            },
        )]
        .into_iter()
        .collect(),
    });
    (book, meta, ops)
}

#[tokio::test]
async fn loopback_space_summary_returns_missing_ops() {
    let server = bind_endpoint().await;
    let client = bind_endpoint().await;
    let target = loopback_target(&server.clone());

    let (spaces, meta, ops) = owner_space_book();
    let summary = SyncMessage::Space {
        message: SpaceMessage::Summary {
            entries: vec![
                SpaceSummaryEntry {
                    space_id: meta.id,
                    // 对端已持有 op3 → 只应补 op1/op2。
                    keys: vec![OpKey::of(&ops[2])],
                },
                // 不在服务端册里的空间：静默跳过（存在性不出线）。
                SpaceSummaryEntry {
                    space_id: Uuid::new_v4(),
                    keys: Vec::new(),
                },
            ],
        },
    };

    let serve = tokio::spawn({
        let spaces = spaces.clone();
        async move {
            let conn = server.accept().await.expect("accept").await.expect("conn");
            handle_sync_connection(conn, &SyncBook::new(), &spaces, |_| {})
                .await
                .expect("服务端报错");
        }
    });

    let reply = with_timeout("summary", send_sync_message(&client, target, summary))
        .await
        .expect("摘要交换失败")
        .expect("Summary 必有应答");
    match reply {
        SyncMessage::Space {
            message: SpaceMessage::Ops { ops: reply_ops },
        } => {
            assert_eq!(reply_ops.len(), 2, "只补缺失的两条");
            assert_eq!(reply_ops[0].lamport, 1);
            assert_eq!(reply_ops[1].lamport, 2);
        }
        other => panic!("应答应为 Ops，实得 {other:?}"),
    }
    with_timeout("serve", serve).await.expect("serve panic");
}

#[tokio::test]
async fn loopback_space_join_recovers_after_service_book_is_republished() {
    let server = bind_endpoint().await;
    let client = bind_endpoint().await;
    let target = loopback_target(&server);
    let (_loaded, meta, ops) = owner_space_book();
    let empty = SpaceBook::new();
    let join = || SyncMessage::Space {
        message: SpaceMessage::Join {
            code: share::space_invite_code(&meta),
            joiner_name: "客服小美".into(),
        },
    };

    let serve_one = |spaces: std::sync::Arc<SpaceBook>| {
        let server = server.clone();
        tokio::spawn(async move {
            let conn = server.accept().await.expect("accept").await.expect("conn");
            handle_sync_connection(conn, &SyncBook::new(), &spaces, |_| {})
                .await
                .expect("服务端报错");
        })
    };
    let serve = serve_one(empty.clone());
    let reply = with_timeout(
        "missing-book",
        send_sync_message(&client, target.clone(), join()),
    )
    .await
    .expect("传输不应失败");
    assert!(
        matches!(
            reply,
            Some(SyncMessage::Space {
                message: SpaceMessage::JoinErr { .. },
            })
        ),
        "有连接但服务册为空时必须是业务拒绝"
    );
    serve.await.expect("serve panic");

    empty.replace(SpaceBookState {
        spaces: [(
            meta.id,
            SpaceBookEntry {
                meta: meta.clone(),
                ops,
                serve: Vec::new(),
            },
        )]
        .into_iter()
        .collect(),
    });
    let serve = serve_one(empty);
    let reply = with_timeout("loaded-book", send_sync_message(&client, target, join()))
        .await
        .expect("传输不应失败");
    assert!(matches!(reply, Some(SyncMessage::Space {
        message: SpaceMessage::JoinOk { meta: joined, .. },
    }) if joined.is_member(&client.id().to_z32())));
    serve.await.expect("serve panic");
}

#[tokio::test]
async fn loopback_space_join_admits_member_and_replies_full_state() {
    let server = bind_endpoint().await;
    let client = bind_endpoint().await;
    let target = loopback_target(&server.clone());

    let (spaces, meta, ops) = owner_space_book();
    let code = share::space_invite_code(&meta);
    let joiner = client.id().to_z32();

    let (event_tx, event_rx) = oneshot::channel();
    let mut event_tx = Some(event_tx);
    let spaces_for_serve = spaces.clone();
    let serve = tokio::spawn(async move {
        for _ in 0..3 {
            let conn = server.accept().await.expect("accept").await.expect("conn");
            handle_sync_connection(conn, &SyncBook::new(), &spaces_for_serve, |event| {
                if let SyncInbound::SpaceAdmitted {
                    space_id,
                    peer_id,
                    joiner_name,
                } = event
                {
                    assert_eq!(joiner_name, "客服小美", "Admitted 事件应带加入者名");
                    if let Some(tx) = event_tx.take() {
                        let _ = tx.send((space_id, peer_id));
                    }
                }
            })
            .await
            .expect("服务端报错");
        }
    });

    // 1) 贴码加入 → JoinOk（名册含加入者 + Roster op）+ Admitted 事件。
    let join = SyncMessage::Space {
        message: SpaceMessage::Join {
            code: code.clone(),
            joiner_name: "客服小美".into(),
        },
    };
    let reply = with_timeout("join", send_sync_message(&client, target.clone(), join))
        .await
        .expect("加入失败")
        .expect("Join 必有应答");
    match reply {
        SyncMessage::Space {
            message:
                SpaceMessage::JoinOk {
                    meta: ok_meta,
                    ops: ok_ops,
                },
        } => {
            assert!(ok_meta.is_member(&joiner));
            let roster = ok_ops.last().expect("JoinOk 应带 Roster op");
            assert_eq!(roster.lamport, ops.last().unwrap().lamport + 1);
        }
        other => panic!("应答应为 JoinOk，实得 {other:?}"),
    }
    let (space_id, peer_id) = with_timeout("admitted", event_rx)
        .await
        .expect("事件通道断裂");
    assert_eq!(space_id, meta.id);
    assert_eq!(peer_id, joiner);

    // 2) 同码重入 → JoinOk 幂等（名册不重复、op 数不再增长）。
    let rejoin = SyncMessage::Space {
        message: SpaceMessage::Join {
            code: code.clone(),
            joiner_name: "客服小美".into(),
        },
    };
    match with_timeout("rejoin", send_sync_message(&client, target.clone(), rejoin))
        .await
        .expect("重入失败")
        .expect("重入必有应答")
    {
        SyncMessage::Space {
            message:
                SpaceMessage::JoinOk {
                    meta: ok_meta,
                    ops: rejoin_ops,
                },
        } => {
            assert_eq!(ok_meta.members.len(), 2);
            assert_eq!(rejoin_ops.len(), ops.len() + 1);
        }
        other => panic!("应答应为 JoinOk，实得 {other:?}"),
    }

    // 3) 旋转后的旧码 → JoinErr（旧码即刻失效；旋转事实由 UI 整册重推）。
    let rotated_secret = {
        let mut registry = SpaceRegistry::new();
        registry.self_id = Some("a".repeat(52));
        registry.self_name = "店长".into();
        let space = registry.create_space("团队素材", 1000).unwrap();
        space.invite_secret
    };
    let mut rotated_meta = meta.clone();
    rotated_meta.invite_secret = rotated_secret;
    spaces.replace(SpaceBookState {
        spaces: [(
            rotated_meta.id,
            SpaceBookEntry {
                meta: rotated_meta,
                ops: ops.clone(),
                serve: Vec::new(),
            },
        )]
        .into_iter()
        .collect(),
    });
    let stale = SyncMessage::Space {
        message: SpaceMessage::Join {
            code,
            joiner_name: "客服小美".into(),
        },
    };
    match with_timeout("stale", send_sync_message(&client, target, stale))
        .await
        .expect("旧码请求失败")
        .expect("旧码必有应答")
    {
        SyncMessage::Space {
            message: SpaceMessage::JoinErr { reason },
        } => assert!(reason.contains("失效"), "拒绝理由应说明失效: {reason}"),
        other => panic!("应答应为 JoinErr，实得 {other:?}"),
    }
    with_timeout("serve", serve).await.expect("serve panic");
}
