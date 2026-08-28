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
        width: None,
        height: None,
    }
}

#[test]
fn migration_v1_creates_assets_fts_tags_tables() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    for table in ["assets", "tags", "assets_fts"] {
        assert!(s.has_table(table), "缺少表 {table}");
    }
}

/// 媒体尺寸是瀑布流按真实宽高比排版的唯一数据来源：写入必须能原样读回，
/// 且缺尺寸时 `aspect()` 明确返回 None（调用方回落占位值，不得静默给 1.0）。
#[test]
fn dimensions_roundtrip_and_drive_aspect() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-dim", "横图.jpg")).unwrap();

    let before = s.get_asset("u-dim").unwrap().unwrap();
    assert_eq!((before.width, before.height), (None, None));
    assert_eq!(before.aspect(), None, "缺尺寸必须是 None 而非兜底值");

    assert!(s.set_dimensions("u-dim", 1920, 1080).unwrap());
    let after = s.get_asset("u-dim").unwrap().unwrap();
    assert_eq!((after.width, after.height), (Some(1920), Some(1080)));
    let aspect = after.aspect().expect("尺寸齐备时必有宽高比");
    assert!((aspect - 1920.0 / 1080.0).abs() < 1e-6);

    // 零尺寸是坏数据，不能算出 inf/0 高度污染布局。
    assert!(s.set_dimensions("u-dim", 0, 0).unwrap());
    assert_eq!(s.get_asset("u-dim").unwrap().unwrap().aspect(), None);

    assert!(
        !s.set_dimensions("missing", 10, 10).unwrap(),
        "uuid 不存在只返回 false，不报错"
    );

    // 遍历路径也必须带上尺寸（VM 装载走的是 for_each_asset）。
    s.set_dimensions("u-dim", 800, 600).unwrap();
    let mut seen = Vec::new();
    s.for_each_asset(|m| seen.push((m.uuid, m.width, m.height)))
        .unwrap();
    assert_eq!(seen, vec![("u-dim".to_string(), Some(800), Some(600))]);
}

/// v1 旧库必须能原地升到 v2：增列而非重建，历史行与 FTS 索引都不能丢。
#[test]
fn v1_database_migrates_in_place_to_v2() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");

    // 造一个 v1 形态的库：无 width/height 列，user_version = 1。
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE assets (
               uuid TEXT PRIMARY KEY NOT NULL,
               file_name TEXT NOT NULL,
               rel_path TEXT NOT NULL DEFAULT '',
               category TEXT,
               size_bytes INTEGER NOT NULL DEFAULT 0,
               created_at INTEGER NOT NULL DEFAULT 0,
               imported_at INTEGER NOT NULL DEFAULT 0,
               phash BLOB
             );
             CREATE TABLE tags (
               asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
               tag TEXT NOT NULL,
               PRIMARY KEY (asset_uuid, tag)
             );
             CREATE VIRTUAL TABLE assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
             INSERT INTO assets (uuid, file_name, rel_path) VALUES ('old-1', '旧素材.jpg', 'objects/old-1/raw.jpg');
             INSERT INTO assets_fts (uuid, name) VALUES ('old-1', '旧素材.jpg');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }

    let s = Store::open(&db_path).unwrap();
    assert_eq!(
        s.schema_version(),
        store::SUPPORTED_SCHEMA_VERSION,
        "打开 v1 库后必须升到当前支持版本"
    );

    let old = s.get_asset("old-1").unwrap().expect("历史行不得丢失");
    assert_eq!(old.file_name, "旧素材.jpg");
    assert_eq!(old.aspect(), None, "历史行尺寸为空，回落占位值");
    assert!(s.set_dimensions("old-1", 640, 480).unwrap());
    assert_eq!(s.get_asset("old-1").unwrap().unwrap().width, Some(640));
    // v3 phash 等值索引：历史行（无 phash）不影响索引创建；反查空结果不报错。
    assert!(s.uuids_for_phash_exact(&[0u8; 8]).unwrap().is_empty());

    // 重开幂等：不得重复 ALTER（会报 duplicate column）。
    drop(s);
    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(reopened.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    assert_eq!(
        reopened.get_asset("old-1").unwrap().unwrap().height,
        Some(480)
    );
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
    assert_eq!(loaded.width, original.width);
    assert_eq!(loaded.height, original.height);

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

/// 派生 PNG 路径是「上框」链路与「派生」工序之间唯一的约定，必须两侧同源。
/// 落在 `objects/<uuid>/paste.png`：与 `raw.<ext>` 同目录，删资产即连带回收。
#[test]
fn paste_png_path_lives_beside_raw_object() {
    let id = "550E8400-E29B-41D4-A716-446655440000";
    let path = Store::paste_png_path(id);

    assert_eq!(path, Store::paste_png_path(id), "同 uuid 路径必须稳定");
    assert_ne!(
        path,
        Store::paste_png_path("f47ac10b-58cc-4372-a567-0e02b2c3d479"),
        "不同 uuid 路径必须不同"
    );

    let comps: Vec<_> = Path::new(&path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    assert_eq!(comps, vec!["objects", &id.to_lowercase(), "paste.png"]);
}

/// 分面视图入口：distinct 分类含计数、按名升序，NULL 归「待分类」。
#[test]
fn distinct_categories_group_count_and_null_goes_to_inbox() {
    let s = Store::open_in_memory().unwrap();

    let mut photo = meta("u-1", "a.jpg");
    photo.category = Some("风景".to_string());
    s.upsert_asset(&photo).unwrap();

    let mut photo2 = meta("u-2", "b.jpg");
    photo2.category = Some("风景".to_string());
    s.upsert_asset(&photo2).unwrap();

    let mut sticker = meta("u-3", "c.png");
    sticker.category = Some("表情".to_string());
    s.upsert_asset(&sticker).unwrap();

    let mut orphan = meta("u-4", "d.png");
    orphan.category = None;
    s.upsert_asset(&orphan).unwrap();

    let cats = s.distinct_categories().unwrap();
    // 升序：待分类 < 表情 < 风景（按 Unicode 码位）。只断言集合与计数，不锁具体码位序。
    assert_eq!(cats.len(), 3, "三个去重分类");
    let map: std::collections::HashMap<_, _> = cats.into_iter().collect();
    assert_eq!(map.get("风景"), Some(&2));
    assert_eq!(map.get("表情"), Some(&1));
    assert_eq!(map.get(store::INBOX_CATEGORY), Some(&1), "NULL 归待分类");
}

/// distinct 标签含计数，跨资产聚合。
#[test]
fn distinct_tags_group_count_across_assets() {
    let s = Store::open_in_memory().unwrap();

    let mut a = meta("t-1", "a.jpg");
    a.tags = vec!["红色".to_string(), "促销".to_string()];
    s.upsert_asset(&a).unwrap();

    let mut b = meta("t-2", "b.jpg");
    b.tags = vec!["红色".to_string()];
    s.upsert_asset(&b).unwrap();

    let tags = s.distinct_tags().unwrap();
    let map: std::collections::HashMap<_, _> = tags.into_iter().collect();
    assert_eq!(map.get("红色"), Some(&2));
    assert_eq!(map.get("促销"), Some(&1));
    assert_eq!(map.len(), 2);
}
