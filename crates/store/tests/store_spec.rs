use std::path::Path;

use rusqlite::Connection;
use store::{AssetMeta, Store, StoreError};

fn meta(uuid: &str, name: &str) -> AssetMeta {
    AssetMeta {
        uuid: uuid.to_string(),
        file_name: name.to_string(),
        rel_path: format!("objects/{uuid}/{name}"),
        category: Some("photo".to_string()),
        category_id: None,
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

/// D88 正文检索：正文命中进 search 白名单；片段只发给正文命中的行
/// （纯文件名命中无标记，UI 回落头部预览）；标记夹住的正是命中词本身。
#[test]
fn fts_content_search_hits_body_and_snippets() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-body", "会议纪要.txt")).unwrap();
    s.upsert_asset(&meta("u-name", "季度预算表.txt")).unwrap();
    assert!(s
        .set_text_content("u-body", "本季度预算削减百分之十二，渠道费同步下调")
        .unwrap());
    assert!(s.set_text_content("u-name", "与本季度无关的正文内容").unwrap());

    let hits = s.search("季度预算", 10).unwrap();
    assert_eq!(hits.len(), 2, "文件名命中与正文命中都进结果白名单");

    let snippets = s.content_snippets("季度预算", 10).unwrap();
    assert_eq!(snippets.len(), 1, "纯文件名命中不产生片段");
    assert_eq!(snippets[0].0, "u-body");
    let marked = &snippets[0].1;
    let (start_mark, end_mark) = (store::SNIPPET_MARK_START, store::SNIPPET_MARK_END);
    let start = marked.find(start_mark).expect("片段应含起始标记");
    let end = marked.find(end_mark).expect("片段应含结束标记");
    assert!(start < end);
    assert_eq!(&marked[start + start_mark.len_utf8()..end], "季度预算");

    // 名字命中、正文无关键词 → 片段为空但不报错。
    let s2 = Store::open_in_memory().unwrap();
    s2.upsert_asset(&meta("u-only", "季度预算表.txt")).unwrap();
    assert!(s2.content_snippets("季度预算", 10).unwrap().is_empty());

    assert!(
        !s.set_text_content("missing", "x").unwrap(),
        "uuid 不存在只返回 false，不报错"
    );
}

/// D88 正文写入经 upsert 改名后必须保留：au 触发器从 assets.text_content
/// 事实源重建 FTS 行，冲突路径不动该列 → 内容跟随，不随改名蒸发。
#[test]
fn fts_upsert_rename_preserves_content() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-r", "旧名.txt")).unwrap();
    assert!(s.set_text_content("u-r", "正文带检索关键词的内容").unwrap());

    let renamed = AssetMeta {
        file_name: "新名.txt".to_string(),
        ..meta("u-r", "旧名.txt")
    };
    s.upsert_asset(&renamed).unwrap();

    assert_eq!(s.search("检索关键词", 10).unwrap().len(), 1);
    assert!(s.search("旧名", 10).unwrap().is_empty(), "旧名索引随改名消失");
    let snippets = s.content_snippets("检索关键词", 10).unwrap();
    assert_eq!(snippets.len(), 1, "改名后正文片段必须保留");
    assert_eq!(snippets[0].0, "u-r");
}

/// D88 store_meta 旗标：存量库回填防重入的载体，set/get/覆盖语义固化。
#[test]
fn meta_flag_roundtrip() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.get_meta_flag("fts_content_backfilled").unwrap(), None);
    s.set_meta_flag("fts_content_backfilled", "1").unwrap();
    assert_eq!(
        s.get_meta_flag("fts_content_backfilled").unwrap().as_deref(),
        Some("1")
    );
    s.set_meta_flag("fts_content_backfilled", "2").unwrap();
    assert_eq!(
        s.get_meta_flag("fts_content_backfilled").unwrap().as_deref(),
        Some("2")
    );
}

/// v8 旧库必须能原地升到 v9：FTS 表重建（加 content 列、行号锚定 rowid），
/// 旧名字索引语义原样保留，正文回写走新触发器即刻可检索。
#[test]
fn v8_database_migrates_to_v9_rebuilding_fts() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v8.db");

    // 造一个 v8 形态的库：无 text_content 列、FTS 无 content 列、旧触发器。
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE assets (
               uuid TEXT PRIMARY KEY NOT NULL,
               file_name TEXT NOT NULL,
               rel_path TEXT NOT NULL DEFAULT '',
               size_bytes INTEGER NOT NULL DEFAULT 0,
               created_at INTEGER NOT NULL DEFAULT 0,
               imported_at INTEGER NOT NULL DEFAULT 0,
               phash BLOB,
               content_hash BLOB,
               width INTEGER,
               height INTEGER,
               deleted INTEGER NOT NULL DEFAULT 0,
               category_id INTEGER
             );
             CREATE VIRTUAL TABLE assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
             CREATE TRIGGER assets_fts_ai AFTER INSERT ON assets BEGIN
               INSERT INTO assets_fts(uuid, name) VALUES (new.uuid, new.file_name);
             END;
             CREATE TRIGGER assets_fts_au AFTER UPDATE OF file_name ON assets BEGIN
               DELETE FROM assets_fts WHERE uuid = old.uuid;
               INSERT INTO assets_fts(uuid, name) VALUES (new.uuid, new.file_name);
             END;
             CREATE TRIGGER assets_fts_ad AFTER DELETE ON assets BEGIN
               DELETE FROM assets_fts WHERE uuid = old.uuid;
             END;
             INSERT INTO assets (uuid, file_name) VALUES ('legacy-1', '旧笔记.txt');
             INSERT INTO assets_fts (uuid, name) VALUES ('legacy-1', '旧笔记.txt');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 8).unwrap();
    }

    let s = Store::open(&db_path).unwrap();
    assert_eq!(
        s.schema_version(),
        store::SUPPORTED_SCHEMA_VERSION,
        "打开 v8 库后必须升到当前支持版本"
    );

    // 名字索引在 FTS 重建后原样可用。
    let hits = s.search("旧笔记", 10).unwrap();
    assert_eq!(hits.len(), 1, "迁移重建不得丢名字索引");

    // 正文回写 → 触发器同步 → 片段即刻可检索。
    assert!(s.set_text_content("legacy-1", "迁移前的存量正文含季度预算字样").unwrap());
    let snippets = s.content_snippets("季度预算", 10).unwrap();
    assert_eq!(snippets.len(), 1);
    assert_eq!(snippets[0].0, "legacy-1");
    assert!(snippets[0].1.contains(store::SNIPPET_MARK_START));

    // 重开幂等：不得重复 ALTER / 重建（会报 duplicate column）。
    drop(s);
    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(reopened.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    assert_eq!(reopened.search("季度预算", 10).unwrap().len(), 1);
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

/// v5 旧库打开必须原地升 v6：字面量「待分类」行折成 NULL（收件箱哨兵），
/// 其余分类原样；回收站行同步折叠（恢复后语义一致）。
#[test]
fn v5_database_folds_literal_inbox_into_null() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy5.db");
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
               deleted INTEGER NOT NULL DEFAULT 0,
               content_hash BLOB
             );
             CREATE TABLE tags (
               asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
               tag TEXT NOT NULL,
               PRIMARY KEY (asset_uuid, tag)
             );
             CREATE VIRTUAL TABLE assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
             CREATE INDEX idx_assets_phash ON assets(phash);
             CREATE INDEX idx_assets_content_hash ON assets(content_hash);
             INSERT INTO assets (uuid, file_name, rel_path, category) VALUES ('old-inbox', 'a.png', 'objects/old-inbox/raw.png', '待分类');
             INSERT INTO assets (uuid, file_name, rel_path, category) VALUES ('old-normal', 'b.png', 'objects/old-normal/raw.png', '风景');
             INSERT INTO assets (uuid, file_name, rel_path, category, deleted) VALUES ('old-trash', 'c.png', 'objects/old-trash/raw.png', '待分类', 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();
    }

    let s = Store::open(&db_path).unwrap();
    assert_eq!(s.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    assert_eq!(
        s.get_asset("old-inbox").unwrap().unwrap().category,
        None,
        "字面量「待分类」必须折成 NULL 哨兵"
    );
    assert_eq!(
        s.get_asset("old-normal")
            .unwrap()
            .unwrap()
            .category
            .as_deref(),
        Some("风景"),
        "普通分类不得被迁移波及"
    );
    assert_eq!(
        s.get_asset("old-trash").unwrap().unwrap().category,
        None,
        "回收站行同步折叠，恢复后不复活字面量"
    );

    // 重开幂等：UPDATE 已无命中，不报错、不留半态。
    drop(s);
    let again = Store::open(&db_path).unwrap();
    assert_eq!(again.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    assert_eq!(
        again.get_asset("old-inbox").unwrap().unwrap().category,
        None
    );
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

/// D84 分类重命名：引用旧名的所有行（含回收站行）批量跟随到新名，
/// 其余分类不动；无引用改 0 行。
#[test]
fn rename_category_rewrites_all_referencing_rows() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("cat-a", "a.jpg");
    a.category = Some("海报".to_string());
    let mut b = meta("cat-b", "b.jpg");
    b.category = Some("海报".to_string());
    let mut c = meta("cat-c", "c.jpg");
    c.category = Some("贴纸".to_string());
    s.upsert_asset(&a).unwrap();
    s.upsert_asset(&b).unwrap();
    s.upsert_asset(&c).unwrap();
    s.soft_delete_assets(&["cat-b"]).unwrap();

    assert_eq!(s.rename_category("海报", "宣传图").unwrap(), 2);
    assert_eq!(
        s.get_asset("cat-a").unwrap().unwrap().category.as_deref(),
        Some("宣传图")
    );
    // 回收站行同步改名：恢复后不复活旧分类。
    assert_eq!(
        s.get_asset("cat-b").unwrap().unwrap().category.as_deref(),
        Some("宣传图")
    );
    assert_eq!(
        s.get_asset("cat-c").unwrap().unwrap().category.as_deref(),
        Some("贴纸"),
        "其余分类不动"
    );
    assert_eq!(
        s.rename_category("不存在", "x").unwrap(),
        0,
        "无引用改 0 行"
    );
}

/// D84 分类重命名护栏：同名是 0 行 no-op；新名已存在时拒绝，禁止静默合并
/// 两个分类（UI 层虽已预检，store 层仍守住单写入口的最后一道门）。
#[test]
fn rename_category_same_name_is_noop_and_existing_name_is_rejected() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("dup-a", "a.jpg");
    a.category = Some("海报".to_string());
    let mut b = meta("dup-b", "b.jpg");
    b.category = Some("贴纸".to_string());
    s.upsert_asset(&a).unwrap();
    s.upsert_asset(&b).unwrap();

    assert_eq!(s.rename_category("海报", "海报").unwrap(), 0, "同名 no-op");
    assert!(matches!(
        s.rename_category("海报", "贴纸"),
        Err(StoreError::CategoryExists(name)) if name == "贴纸"
    ));
    assert_eq!(
        s.get_asset("dup-a").unwrap().unwrap().category.as_deref(),
        Some("海报"),
        "被拒绝的改名不得留下半改状态"
    );
    assert_eq!(
        s.get_asset("dup-b").unwrap().unwrap().category.as_deref(),
        Some("贴纸")
    );
}

/// D84 删除分类：引用行 category 置 NULL（读取侧归待分类），素材行本身
/// 不动；回收站行同样置 NULL，恢复后不复活已删分类。
#[test]
fn clear_category_nulls_referencing_rows_and_keeps_assets() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("del-a", "a.jpg");
    a.category = Some("旧分类".to_string());
    let mut b = meta("del-b", "b.jpg");
    b.category = Some("旧分类".to_string());
    let mut c = meta("del-c", "c.jpg");
    c.category = Some("贴纸".to_string());
    s.upsert_asset(&a).unwrap();
    s.upsert_asset(&b).unwrap();
    s.upsert_asset(&c).unwrap();
    s.soft_delete_assets(&["del-b"]).unwrap();

    assert_eq!(s.clear_category("旧分类").unwrap(), 2);
    assert_eq!(s.get_asset("del-a").unwrap().unwrap().category, None);
    assert_eq!(s.get_asset("del-b").unwrap().unwrap().category, None);
    assert_eq!(
        s.get_asset("del-c").unwrap().unwrap().category.as_deref(),
        Some("贴纸"),
        "其余分类不动"
    );
    // 行本身不动：素材仍在库，恢复后归待分类。
    assert_eq!(s.all_assets_count().unwrap(), 3);
    s.restore_assets(&["del-b"]).unwrap();
    assert_eq!(s.get_asset("del-b").unwrap().unwrap().category, None);
}

/// D85 收件箱保留名：不可重命名/删除，也不许普通分类改名成它；普通分类
/// 之间的重命名照常（产品需求保留）。
#[test]
fn inbox_name_is_reserved_but_normal_categories_still_rename() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("res-a", "a.jpg");
    a.category = Some("海报".to_string());
    let mut inbox = meta("res-inbox", "b.jpg");
    inbox.category = None;
    s.upsert_asset(&a).unwrap();
    s.upsert_asset(&inbox).unwrap();

    assert_eq!(
        s.rename_category("海报", "宣传图").unwrap(),
        1,
        "普通分类重命名不受影响"
    );
    assert!(matches!(
        s.rename_category(store::INBOX_CATEGORY, "收件箱"),
        Err(StoreError::ReservedInbox)
    ));
    assert!(matches!(
        s.rename_category("宣传图", store::INBOX_CATEGORY),
        Err(StoreError::ReservedInbox)
    ));
    assert!(matches!(
        s.clear_category(store::INBOX_CATEGORY),
        Err(StoreError::ReservedInbox)
    ));
    assert_eq!(
        s.get_asset("res-a").unwrap().unwrap().category.as_deref(),
        Some("宣传图"),
        "被拒绝的保留名操作不得留下半改状态"
    );
    assert_eq!(s.get_asset("res-inbox").unwrap().unwrap().category, None);
}

/// D85 写入层归一：CLI/旧脚本直传字面量「待分类」也落 NULL，不产生影子分类。
#[test]
fn set_category_normalizes_inbox_literal_to_null() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("norm-a", "a.jpg");
    a.category = Some("海报".to_string());
    s.upsert_asset(&a).unwrap();

    assert!(s
        .set_category("norm-a", Some(store::INBOX_CATEGORY))
        .unwrap());
    assert_eq!(s.get_asset("norm-a").unwrap().unwrap().category, None);

    let cats: std::collections::HashMap<_, _> =
        s.distinct_categories().unwrap().into_iter().collect();
    assert_eq!(
        cats.get(store::INBOX_CATEGORY),
        Some(&1),
        "字面量写入必须计入收件箱而不是新分类"
    );
    assert_eq!(cats.len(), 1);
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

// ---------------------------------------------------------------------------
// D87 分类一等化（schema v8）：分类从「资产行上的一个字符串」升为独立表 +
// 外键。契约：v7 旧库原地迁移不丢分类归属；改名只动 categories 一行、
// 引用方 id 不变（O(1)）；空分类仍可枚举；收件箱不进表（NULL 外键）。
// ---------------------------------------------------------------------------

/// 名字 → 分类 id 直查（测试用；生产路径走 list_categories）。
fn category_id_of(s: &Store, name: &str) -> u32 {
    s.list_categories()
        .unwrap()
        .into_iter()
        .find(|(_, n)| n == name)
        .unwrap_or_else(|| panic!("分类 {name} 不存在"))
        .0
}

/// v7 旧库（分类是 assets.category 字符串列 + 索引）打开必须原地升 v8：
/// 去重分类落到 categories 表并回填外键，旧列 `category` 消失，历史行不丢。
#[test]
fn v7_database_migrates_in_place_to_v8() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy7.db");
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
               deleted INTEGER NOT NULL DEFAULT 0,
               content_hash BLOB
             );
             CREATE TABLE tags (
               asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
               tag TEXT NOT NULL,
               PRIMARY KEY (asset_uuid, tag)
             );
             CREATE VIRTUAL TABLE assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
             CREATE INDEX idx_assets_phash ON assets(phash);
             CREATE INDEX idx_assets_content_hash ON assets(content_hash);
             CREATE INDEX idx_assets_category ON assets(category);
             INSERT INTO assets (uuid, file_name, rel_path, category) VALUES ('a-1', '甲.png', 'objects/a-1/raw.png', '风景');
             INSERT INTO assets (uuid, file_name, rel_path, category) VALUES ('a-2', '乙.png', 'objects/a-2/raw.png', '风景');
             INSERT INTO assets (uuid, file_name, rel_path, category) VALUES ('a-3', '丙.png', 'objects/a-3/raw.png', '表情');
             INSERT INTO assets (uuid, file_name, rel_path, category) VALUES ('a-4', '丁.png', 'objects/a-4/raw.png', NULL);
             INSERT INTO assets (uuid, file_name, rel_path, category, deleted) VALUES ('a-5', '戊.png', 'objects/a-5/raw.png', '表情', 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
    }

    let s = Store::open(&db_path).unwrap();
    assert_eq!(s.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    assert!(s.has_table("categories"), "v8 必须建出 categories 表");

    // 去重分类（含只被回收站行引用的）各一行，按名给出稳定 id。
    let cats = s.list_categories().unwrap();
    let names: Vec<&str> = cats.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(names, vec!["表情", "风景"], "分类按名升序落表");
    let scenery = category_id_of(&s, "风景");

    // 归属按外键回填；旧列已删，读取走 JOIN 出的显示名。
    let a1 = s.get_asset("a-1").unwrap().unwrap();
    assert_eq!(a1.category.as_deref(), Some("风景"));
    assert_eq!(a1.category_id, Some(scenery));
    assert_eq!(
        s.get_asset("a-4").unwrap().unwrap().category_id,
        None,
        "NULL 行（收件箱）迁移后仍是 NULL，不得凭空建分类"
    );
    assert_eq!(
        s.get_asset("a-5").unwrap().unwrap().category_id,
        s.get_asset("a-3").unwrap().unwrap().category_id,
        "回收站行同步带上外键（恢复后归属不丢）"
    );

    // 旧列确实被删掉：直查会报 no such column。
    {
        let conn = Connection::open(&db_path).unwrap();
        assert!(
            conn.query_row("SELECT category FROM assets", [], |r| r.get::<_, String>(0))
                .is_err(),
            "v8 后 assets.category 列必须消失（名字降为 categories 行的属性）"
        );
    }

    // 重开幂等：不得重复 ALTER / 重复回填。
    drop(s);
    let again = Store::open(&db_path).unwrap();
    assert_eq!(again.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    assert_eq!(again.list_categories().unwrap().len(), 2);
}

/// D87 核心收益：改名只改 categories 一行，引用方持有的 id 不变——
/// 素材侧零改写，故「改名代价与引用行数解耦」。
#[test]
fn rename_category_keeps_ids_stable_for_referencing_rows() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("r-1", "a.jpg");
    a.category = Some("旧名".to_string());
    let mut b = meta("r-2", "b.jpg");
    b.category = Some("旧名".to_string());
    s.upsert_asset(&a).unwrap();
    s.upsert_asset(&b).unwrap();

    let before = category_id_of(&s, "旧名");
    assert_eq!(s.rename_category("旧名", "新名").unwrap(), 2);

    // 同一个 id，换了个名字：引用方无需任何改写。
    assert_eq!(category_id_of(&s, "新名"), before);
    assert_eq!(
        s.get_asset("r-1").unwrap().unwrap().category_id,
        Some(before)
    );
    assert_eq!(
        s.get_asset("r-1").unwrap().unwrap().category.as_deref(),
        Some("新名")
    );
    assert_eq!(
        s.get_asset("r-2").unwrap().unwrap().category.as_deref(),
        Some("新名")
    );
    assert!(s
        .list_categories()
        .unwrap()
        .iter()
        .all(|(_, n)| n != "旧名"));
}

/// 空分类是一等实体：最后一个素材移走后分类行仍在（可继续归入），
/// `list_categories` 必须列得出来。
#[test]
fn empty_category_survives_until_explicitly_deleted() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("e-1", "a.jpg");
    a.category = Some("会变空的".to_string());
    s.upsert_asset(&a).unwrap();
    let id = category_id_of(&s, "会变空的");

    s.set_category("e-1", None).unwrap();
    let cats = s.list_categories().unwrap();
    assert_eq!(
        cats,
        vec![(id, "会变空的".to_string())],
        "素材移走后分类行保留（D87：分类独立于引用而存在）"
    );
    assert!(
        s.distinct_categories()
            .unwrap()
            .iter()
            .all(|(name, _)| name != "会变空的"),
        "空分类不占资产计数（素材已归收件箱）"
    );

    // 显式删除才消失。
    assert_eq!(s.clear_category("会变空的").unwrap(), 0);
    assert!(s.list_categories().unwrap().is_empty());
}

/// 外键归属：写入侧只给名字（`category_id: None`）时由 store 边界 ensure，
/// 且同一名字跨调用拿到同一 id（不产生影子分类）。
#[test]
fn write_by_name_resolves_to_same_category_row() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("w-1", "a.jpg");
    a.category = Some("图片".to_string());
    let mut b = meta("w-2", "b.jpg");
    b.category = Some("图片".to_string());
    b.phash = Some(vec![0xCD; 8]);
    s.upsert_assets(&[a, b]).unwrap();

    let cats = s.list_categories().unwrap();
    assert_eq!(cats.len(), 1, "同名只应有一行");
    let id = cats[0].0;
    assert_eq!(s.get_asset("w-1").unwrap().unwrap().category_id, Some(id));
    assert_eq!(s.get_asset("w-2").unwrap().unwrap().category_id, Some(id));

    // 显式给 id 的写入路径直接落库（不经名字解析）。
    assert_eq!(s.ensure_category("图片").unwrap(), id, "ensure 幂等");
    assert!(matches!(
        s.ensure_category(store::INBOX_CATEGORY),
        Err(StoreError::ReservedInbox)
    ));
}

/// 收件箱不进表：NULL 外键是其唯一身份，`clear_category`/`rename_category`
/// 两侧都拒绝，`list_categories` 永不返回该系统名。
#[test]
fn inbox_never_becomes_a_category_row() {
    let s = Store::open_in_memory().unwrap();
    let mut a = meta("i-1", "a.jpg");
    a.category = None;
    s.upsert_asset(&a).unwrap();

    assert!(s.list_categories().unwrap().is_empty(), "收件箱不建行");
    assert!(
        s.distinct_categories()
            .unwrap()
            .iter()
            .any(|(name, _)| name == store::INBOX_CATEGORY),
        "但读取侧仍以系统名显示 NULL 行"
    );
    assert!(matches!(
        s.clear_category(store::INBOX_CATEGORY),
        Err(StoreError::ReservedInbox)
    ));
}
