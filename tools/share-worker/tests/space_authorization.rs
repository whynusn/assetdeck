use iroh::SecretKey;
use share::{
    RosterMember, ServeItem, SpaceAssetKind, SpaceAssetMeta, SpaceBookEntry, SpaceBookState,
    SpaceMessage, SpaceOp, SpaceOpKind, SpaceRegistry, SpaceSummaryEntry,
};
use share_worker::sync::SpaceBook;
use uuid::Uuid;

#[test]
fn roster_gates_summary_pages_fetch_and_owner_admission() {
    let owner = SecretKey::generate().public();
    let member = SecretKey::generate().public();
    let outsider = SecretKey::generate().public();
    let mut registry = SpaceRegistry::new();
    registry.self_id = Some(owner.to_z32());
    registry.self_name = "owner".into();
    let mut meta = registry.create_space("team", 1).unwrap();
    meta.members.push(RosterMember {
        id: member.to_z32(),
        name: "member".into(),
    });
    let asset = SpaceAssetMeta {
        uuid: Uuid::new_v4(),
        file_name: "trusted.txt".into(),
        kind: SpaceAssetKind::Text,
        size_bytes: 4,
        content_hash: vec![1; 32],
    };
    let add = SpaceOp {
        space_id: meta.id,
        lamport: 1,
        author: owner.to_z32(),
        ts: 1,
        op: SpaceOpKind::Add {
            asset: asset.clone(),
        },
    };
    let entry = SpaceBookEntry {
        meta: meta.clone(),
        ops: vec![add.clone()],
        serve: vec![ServeItem {
            content_hash: asset.content_hash.clone(),
            path: r"C:\synthetic\trusted.txt".into(),
        }],
    };
    let book = SpaceBook::new();
    book.replace(SpaceBookState {
        spaces: [(meta.id, entry.clone())].into_iter().collect(),
    });
    let summary = vec![SpaceSummaryEntry {
        space_id: meta.id,
        keys: Vec::new(),
    }];
    assert!(book.diff_against(&outsider.to_z32(), &summary).is_empty());
    assert_eq!(book.diff_against(&member.to_z32(), &summary).len(), 1);
    assert!(
        matches!(book.summary_for(&outsider.to_z32()), SpaceMessage::Summary { entries } if entries.is_empty())
    );
    assert!(book.page_after(&outsider.to_z32(), meta.id, None).is_none());
    assert!(
        matches!(book.page_after(&member.to_z32(), meta.id, None), Some(SpaceMessage::OpsPage { ops, next: None, .. }) if ops.len() == 1)
    );
    assert!(book
        .authorized_fetch(meta.id, &outsider.to_z32(), std::slice::from_ref(&asset))
        .is_none());
    let mut forged = asset.clone();
    forged.file_name = "forged.bin".into();
    forged.size_bytes = 999;
    let served = book
        .authorized_fetch(meta.id, &member.to_z32(), &[forged])
        .unwrap();
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].0, asset);
    let code = share::space_invite_code(&meta);
    assert!(book
        .admit(&code, &member, &outsider.to_z32(), "outsider")
        .is_err());
    assert!(book
        .admit(&code, &owner, &outsider.to_z32(), "new member")
        .is_ok());

    let mut removed = entry;
    removed.ops.push(SpaceOp {
        space_id: meta.id,
        lamport: 2,
        author: owner.to_z32(),
        ts: 2,
        op: SpaceOpKind::Remove {
            asset_uuid: asset.uuid,
        },
    });
    book.replace(SpaceBookState {
        spaces: [(meta.id, removed)].into_iter().collect(),
    });
    assert!(book
        .authorized_fetch(meta.id, &member.to_z32(), &[asset])
        .unwrap()
        .is_empty());
}
