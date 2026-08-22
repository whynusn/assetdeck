use std::path::Path;

use rusqlite::Connection;
use store::{AssetMeta, Store, StoreError};

fn meta(uuid: &str, name: &str) -> AssetMeta {
    AssetMeta {
        uuid: uuid.to_string(),
        file_name: name.to_string(),
        rel_path: format!("objects/{uuid}/{name}"),
        category: Some("photo".to_string()),
        tags: vec!["red".to_string(), "promo".to_string()],
        size_bytes: 1024,
        created_at: 1_700_000_000,
        imported_at: 1_700_000_001,
        phash: Some(vec![0xAB; 8]),
    }
}

#[test]
fn migration_v1_creates_assets_fts_tags_tables() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.schema_version(), 1);
    for table in ["assets", "tags", "assets_fts"] {
        assert!(s.has_table(table), "缺少表 {table}");
    }
}

#[test]
fn fts_search_chinese_hits_trigram() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-1", "红色卫衣.jpg")).unwrap();
    s.upsert_asset(&meta("u-2", "蓝色牛仔裤.png")).unwrap();

    let hits = s.search("色卫衣", 10).unwrap();
    assert_eq!(hits.len(), 1, "连续三字子串应命中");
    assert_eq!(hits[0].uuid, "u-1");
    assert_eq!(hits[0].file_name, "红色卫衣.jpg");

    let ext_hits = s.search(".jpg", 10).unwrap();
    assert_eq!(ext_hits.len(), 1);
    assert_eq!(ext_hits[0].uuid, "u-1");

    let short = s.search("卫衣", 10).unwrap();
    assert!(
        short.is_empty(),
        "trigram tokenizer 下不足 3 字的查询不命中（固化已知限制）"
    );

    let updated = AssetMeta {
        file_name: "深蓝夹克.jpg".to_string(),
        ..meta("u-2", "蓝色牛仔裤.png")
    };
    s.upsert_asset(&updated).unwrap();
    let stale = s.search("牛仔裤", 10).unwrap();
    assert!(stale.is_empty(), "改名后旧名不应再命中");
    let fresh = s.search("深蓝夹克", 10).unwrap();
    assert_eq!(fresh.len(), 1);
}

#[test]
fn metadata_roundtrip_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("lib.db");
    let original = meta("u-42", "产品图.png");

    {
        let s = Store::open(&db_path).unwrap();
        s.upsert_asset(&original).unwrap();
    }

    let s = Store::open(&db_path).unwrap();
    let loaded = s.get_asset("u-42").unwrap().expect("重开后应能读到资产");
    assert_eq!(loaded.uuid, original.uuid);
    assert_eq!(loaded.file_name, original.file_name);
    assert_eq!(loaded.rel_path, original.rel_path);
    assert_eq!(loaded.category, original.category);
    let mut a = loaded.tags.clone();
    a.sort();
    let mut b = original.tags.clone();
    b.sort();
    assert_eq!(a, b);
    assert_eq!(loaded.size_bytes, original.size_bytes);
    assert_eq!(loaded.created_at, original.created_at);
    assert_eq!(loaded.imported_at, original.imported_at);
    assert_eq!(loaded.phash, original.phash);

    assert!(s.get_asset("missing").unwrap().is_none());
}

#[test]
fn schema_version_rejects_newer_db_file() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("future.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
    }
    match Store::open(&db_path) {
        Err(StoreError::UnsupportedSchemaVersion { found }) => assert_eq!(found, 999),
        other => panic!("必须拒绝更高 schema 版本，实际 {other:?}"),
    }
}

#[test]
fn thumbnail_cache_path_stable_per_asset_id() {
    let id_a = "550e8400-e29b-41d4-a716-446655440000";
    let id_b = "f47ac10b-58cc-4372-a567-0e02b2c3d479";

    let p1 = Store::thumbnail_cache_path(id_a, "webp");
    let p2 = Store::thumbnail_cache_path(id_a, "webp");
    assert_eq!(p1, p2, "同 id 路径必须稳定");

    let p3 = Store::thumbnail_cache_path(id_b, "webp");
    assert_ne!(p1, p3, "不同 id 路径必须不同");

    let comps: Vec<_> = Path::new(&p1)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let file = comps.last().unwrap();
    assert_eq!(file, &format!("{id_a}.webp"));
    assert_eq!(&comps[comps.len() - 3], &id_a[0..1].to_lowercase());
    assert_eq!(&comps[comps.len() - 2], &id_a[0..2].to_lowercase());
    assert!(comps.contains(&"thumbs".to_string()));
}
