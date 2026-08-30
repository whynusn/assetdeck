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
        content_hash: None,
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

// ---------------------------------------------------------------------------
// D46 回收站（tombstone）：schema v4 软删除语义。
// 契约：软删不动正本行、FTS 行保留（查询侧必须 JOIN 过滤 deleted）、
// 恢复即复位标志；彻底删除仍走 delete_asset 硬删。
// ---------------------------------------------------------------------------

/// v3 旧库打开必须原地升 v4：`deleted` 列出现、历史行默认 0、FTS/数据不丢。
#[test]
fn v3_database_migrates_in_place_to_v4() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy3.db");
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
               phash BLOB,
               width INTEGER,
               height INTEGER
             );
             CREATE TABLE tags (
               asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
               tag TEXT NOT NULL,
               PRIMARY KEY (asset_uuid, tag)
             );
             CREATE VIRTUAL TABLE assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
             CREATE INDEX idx_assets_phash ON assets(phash);
             INSERT INTO assets (uuid, file_name, rel_path) VALUES ('old-9', '老图三.jpg', 'objects/old-9/raw.jpg');
             INSERT INTO assets_fts (uuid, name) VALUES ('old-9', '老图三.jpg');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
    }

    let s = Store::open(&db_path).unwrap();
    assert_eq!(s.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    // 历史行存在且默认未删除；FTS 命中保持。
    assert!(s.get_asset("old-9").unwrap().is_some());
    assert!(!s.is_deleted("old-9").unwrap());
    assert_eq!(s.search("老图三", 10).unwrap().len(), 1);
    // 软删该历史行 → deleted_uuids 含之，search 不再命中。
    assert_eq!(s.soft_delete_assets(&["old-9"]).unwrap(), 1);
    assert_eq!(s.deleted_uuids().unwrap(), vec!["old-9".to_string()]);
    assert!(
        s.search("老图三", 10).unwrap().is_empty(),
        "FTS 查询必须过滤回收站"
    );
    // 重开幂等：不得重复 ALTER。
    drop(s);
    let again = Store::open(&db_path).unwrap();
    assert!(again.is_deleted("old-9").unwrap());
}

// ---------------------------------------------------------------------------
// D61 内容等值去重（schema v5）：非图片素材的 SHA-256 摘要列 + 等值索引。
// 契约：写读往返一致；查重排除回收站行（与 all_phashes 的 D46 语义一致）；
// v4 旧库原地升级不丢历史行。
// ---------------------------------------------------------------------------

/// v4 旧库打开必须原地升 v5：`content_hash` 列出现、等值索引可用、历史行不丢。
#[test]
fn v4_database_migrates_in_place_to_v5() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy4.db");
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
               phash BLOB,
               width INTEGER,
               height INTEGER,
               deleted INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE tags (
               asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
               tag TEXT NOT NULL,
               PRIMARY KEY (asset_uuid, tag)
             );
             CREATE VIRTUAL TABLE assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
             CREATE INDEX idx_assets_phash ON assets(phash);
             INSERT INTO assets (uuid, file_name, rel_path) VALUES ('old-11', '老视频.mp4', 'objects/old-11/raw.mp4');
             INSERT INTO assets_fts (uuid, name) VALUES ('old-11', '老视频.mp4');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
    }

    let s = Store::open(&db_path).unwrap();
    assert_eq!(s.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    // 历史行存在、摘要为 NULL、查重不命中不报错。
    let old = s.get_asset("old-11").unwrap().expect("历史行不得丢失");
    assert_eq!(old.content_hash, None);
    assert_eq!(s.uuid_by_content_hash(&[0u8; 32]).unwrap(), None);

    // 重开幂等：不得重复 ALTER（会报 duplicate column）。
    drop(s);
    let again = Store::open(&db_path).unwrap();
    assert_eq!(again.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    assert!(again.get_asset("old-11").unwrap().is_some());
}

/// 内容摘要写读往返；查重只认活跃行——回收站素材不挡新导入（与 pHash 语义一致）。
#[test]
fn content_hash_roundtrip_and_lookup_excludes_deleted() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("h-1", "视频甲.mp4");
    a.phash = None;
    a.content_hash = Some(vec![0x5A; 32]);
    s.upsert_asset(&a).unwrap();

    // 往返 + 等值命中；不同摘要不命中。
    let loaded = s.get_asset("h-1").unwrap().unwrap();
    assert_eq!(loaded.content_hash.as_deref(), Some(&[0x5Au8; 32][..]));
    assert_eq!(
        s.uuid_by_content_hash(&[0x5A; 32]).unwrap().as_deref(),
        Some("h-1")
    );
    assert_eq!(s.uuid_by_content_hash(&[0x5B; 32]).unwrap(), None);

    // 软删后查重不再命中（回收站素材不得挡新导入）。
    s.soft_delete_assets(&["h-1"]).unwrap();
    assert_eq!(s.uuid_by_content_hash(&[0x5A; 32]).unwrap(), None);
}

/// 软删 → 恢复闭环：标志位翻转、行与 tags 全程保留。
#[test]
fn soft_delete_then_restore_keeps_row_and_tags() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-a", "甲图.jpg")).unwrap();
    s.upsert_asset(&meta("u-b", "乙图.jpg")).unwrap();

    assert_eq!(
        s.soft_delete_assets(&["u-a", "missing", "u-b"]).unwrap(),
        2,
        "不存在的 uuid 只少计，不报错"
    );
    assert!(s.is_deleted("u-a").unwrap());
    assert_eq!(
        s.deleted_uuids().unwrap(),
        vec!["u-a".to_string(), "u-b".to_string()]
    );
    // 软删不是删行：get_asset 仍可读（恢复/属性面板要用），计数不变。
    assert_eq!(s.get_asset("u-a").unwrap().unwrap().file_name, "甲图.jpg");
    assert_eq!(s.all_assets_count().unwrap(), 2);

    assert_eq!(s.restore_assets(&["u-a"]).unwrap(), 1);
    assert!(!s.is_deleted("u-a").unwrap());
    assert_eq!(s.deleted_uuids().unwrap(), vec!["u-b".to_string()]);

    // 恢复不存在/未删除的行：返回 0，不报错。
    assert_eq!(s.restore_assets(&["missing"]).unwrap(), 0);
    assert_eq!(s.restore_assets(&["u-a"]).unwrap(), 0);
}

/// 分类/标签分面计数、pHash 去重清单、active 遍历全部排除回收站。
#[test]
fn facet_and_dedup_reads_exclude_deleted() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("f-1", "a.jpg");
    a.category = Some("风景".to_string());
    let mut b = meta("f-2", "b.jpg");
    b.category = Some("风景".to_string());
    let mut c = meta("f-3", "c.jpg");
    c.category = Some("表情".to_string());
    s.upsert_asset(&a).unwrap();
    s.upsert_asset(&b).unwrap();
    s.upsert_asset(&c).unwrap();

    s.soft_delete_assets(&["f-2"]).unwrap();
    let map: std::collections::HashMap<_, _> =
        s.distinct_categories().unwrap().into_iter().collect();
    assert_eq!(map.get("风景"), Some(&1), "回收站素材不占分类计数");

    s.soft_delete_assets(&["f-1", "f-3"]).unwrap();
    assert!(s.distinct_categories().unwrap().is_empty());

    // 去重清单：三个 phash 相同，全删后清单为空（回收站素材不得挡新导入）。
    assert!(s.all_phashes().unwrap().is_empty());
    assert!(s.uuids_for_phash_exact(&[0xAB; 8]).unwrap().is_empty());
}

/// `for_each_asset_active` 跳过 deleted 且保持 uuid 升序（FTS 二分映射的不变量）。
#[test]
fn for_each_asset_active_skips_deleted_and_stays_sorted() {
    let s = Store::open_in_memory().unwrap();
    for (uuid, name) in [("z-1", "z.jpg"), ("a-1", "a.jpg"), ("m-1", "m.jpg")] {
        s.upsert_asset(&meta(uuid, name)).unwrap();
    }
    s.soft_delete_assets(&["m-1"]).unwrap();

    let mut seen = Vec::new();
    s.for_each_asset_active(|m| seen.push(m.uuid)).unwrap();
    assert_eq!(
        seen,
        vec!["a-1".to_string(), "z-1".to_string()],
        "跳删且升序"
    );
    // 无过滤版行为不变：仍见全部三行。
    let mut all = Vec::new();
    s.for_each_asset(|m| all.push(m.uuid)).unwrap();
    assert_eq!(all.len(), 3);
}

/// 窄列改名：UPDATE file_name 后 FTS 触发器自动重排索引（新名命中、旧名消失）。
#[test]
fn rename_asset_reindexes_fts_via_trigger() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("r-1", "红色卫衣.jpg")).unwrap();

    assert!(s.rename_asset("r-1", "蓝色夹克.jpg").unwrap());
    assert_eq!(
        s.get_asset("r-1").unwrap().unwrap().file_name,
        "蓝色夹克.jpg"
    );
    assert!(
        s.search("红色卫衣", 10).unwrap().is_empty(),
        "旧名不得残留索引"
    );
    assert_eq!(s.search("蓝色夹克", 10).unwrap().len(), 1, "新名必须可检索");
    assert!(
        !s.rename_asset("missing", "x.jpg").unwrap(),
        "行不存在返回 false"
    );
}

/// 窄列改分类：只动 category，其余字段（尺寸/tags/…）不受扰动。
#[test]
fn set_category_narrow_update_perturbs_nothing_else() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("c-1", "c.jpg")).unwrap();
    s.set_dimensions("c-1", 100, 50).unwrap();

    assert!(s.set_category("c-1", Some("新分类")).unwrap());
    let after = s.get_asset("c-1").unwrap().unwrap();
    assert_eq!(after.category.as_deref(), Some("新分类"));
    assert_eq!((after.width, after.height), (Some(100), Some(50)));
    assert_eq!(after.tags.len(), 2, "tags 不动");

    // Some(None 语义)：归入待分类 = 置 NULL。
    assert!(s.set_category("c-1", None).unwrap());
    assert_eq!(s.get_asset("c-1").unwrap().unwrap().category, None);
    assert!(!s.set_category("missing", Some("x")).unwrap());
}

/// 软删素材改名后再恢复：检索按新名命中（改名不要求先恢复）。
#[test]
fn renamed_deleted_asset_searchable_after_restore() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("n-1", "旧文件名.jpg")).unwrap();
    s.soft_delete_assets(&["n-1"]).unwrap();
    s.rename_asset("n-1", "新文件名.jpg").unwrap();
    assert!(
        s.search("新文件名", 10).unwrap().is_empty(),
        "回收站中不可见"
    );
    s.restore_assets(&["n-1"]).unwrap();
    assert_eq!(s.search("新文件名", 10).unwrap().len(), 1);
}

/// D61 分类保留：按 v0.1.0 时期的 v4 schema（用户真机旧库的真实形态）造库，
/// read_category_by_uuid 应返回 uuid→category 映射且回收站行不出现。
#[test]
fn read_category_by_uuid_maps_live_rows_and_skips_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("meta.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        // v0.1.0 schema v4 = MIGRATION_V1 + v2/v3/v4 ALTER（照抄 tag 快照）。
        conn.execute_batch(
            "CREATE TABLE assets (
               uuid        TEXT PRIMARY KEY NOT NULL,
               file_name   TEXT NOT NULL,
               rel_path    TEXT NOT NULL DEFAULT '',
               category    TEXT,
               size_bytes  INTEGER NOT NULL DEFAULT 0,
               created_at  INTEGER NOT NULL DEFAULT 0,
               imported_at INTEGER NOT NULL DEFAULT 0,
               phash       BLOB,
               width       INTEGER,
               height      INTEGER,
               deleted     INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO assets (uuid, file_name, rel_path, category)
               VALUES ('uuid-live-named', '促销海报.png', 'objects/uuid-live-named/raw.png', '海报');
             INSERT INTO assets (uuid, file_name, rel_path, category)
               VALUES ('uuid-live-null', '屏幕截图.png', 'objects/uuid-live-null/raw.png', NULL);
             INSERT INTO assets (uuid, file_name, rel_path, category, deleted)
               VALUES ('uuid-trashed', '回收站.png', '', '隐藏分类', 1);",
        )
        .unwrap();
    }

    let map = store::read_category_by_uuid(&db_path).expect("应读到旧库分类");
    assert_eq!(map.len(), 2, "回收站行不出现在映射里");
    assert_eq!(
        map.get("uuid-live-named").unwrap(),
        &Some("海报".to_string())
    );
    assert_eq!(map.get("uuid-live-null").unwrap(), &None);
}

#[test]
fn read_category_by_uuid_errors_on_missing_or_non_db() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        store::read_category_by_uuid(&dir.path().join("不存在.db")).is_err(),
        "缺文件必须报错（调用方降级为 auto）"
    );
    let fake = dir.path().join("fake.db");
    std::fs::write(&fake, b"definitely not sqlite").unwrap();
    assert!(
        store::read_category_by_uuid(&fake).is_err(),
        "非 SQLite 文件必须报错"
    );
}

/// 只读纪律：read_category_by_uuid 不跑 schema 迁移——v0.1.0 的 user_version=4
/// 读完必须原样保留（旧库属于用户，读取方不得改写）。
#[test]
fn read_category_by_uuid_does_not_migrate_user_version() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("meta.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE assets (
               uuid TEXT PRIMARY KEY NOT NULL, file_name TEXT NOT NULL,
               rel_path TEXT NOT NULL DEFAULT '', category TEXT,
               size_bytes INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL DEFAULT 0,
               imported_at INTEGER NOT NULL DEFAULT 0, phash BLOB,
               width INTEGER, height INTEGER, deleted INTEGER NOT NULL DEFAULT 0);
             INSERT INTO assets (uuid, file_name, category)
               VALUES ('u-1', 'a.png', '海报');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
    }
    let _ = store::read_category_by_uuid(&db_path).unwrap();
    let conn = Connection::open(&db_path).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(v, 4, "读取不得触发迁移改写");
}
