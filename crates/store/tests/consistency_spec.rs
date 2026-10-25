//! D91 一致性对账的 store 层契约：V10 迁移、缺失标记、基线回填、重索引写回。

use store::{AssetMeta, ReindexedFields, Store};

fn meta(uuid: &str, name: &str) -> AssetMeta {
    AssetMeta {
        uuid: uuid.to_string(),
        file_name: name.to_string(),
        rel_path: format!("objects/{uuid}/raw.txt"),
        category: None,
        category_id: None,
        tags: vec![],
        size_bytes: 10,
        created_at: 0,
        imported_at: 0,
        phash: None,
        content_hash: None,
        width: None,
        height: None,
        mtime: Some(1_700_000_000),
        missing: false,
    }
}

/// v10 增列对旧库透明：mtime NULL = 未基线，missing 0 = 未标记。
#[test]
fn migration_v10_adds_consistency_columns() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.schema_version(), store::SUPPORTED_SCHEMA_VERSION);
    let m = s.get_asset("u-1").unwrap();
    assert!(m.is_none());

    s.upsert_asset(&meta("u-1", "a.txt")).unwrap();
    let asset = s.get_asset("u-1").unwrap().unwrap();
    assert_eq!(asset.mtime, Some(1_700_000_000));
    assert!(!asset.missing);
}

#[test]
fn missing_flags_roundtrip_and_exclude_recycle_bin() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-live", "a.txt")).unwrap();
    s.upsert_asset(&meta("u-dead", "b.txt")).unwrap();
    s.soft_delete_assets(&["u-dead"]).unwrap();

    assert_eq!(s.mark_missing(&["u-live", "u-dead", "u-ghost"]).unwrap(), 1);
    assert_eq!(s.missing_uuids().unwrap(), vec!["u-live".to_string()]);

    assert_eq!(s.clear_missing(&["u-live"]).unwrap(), 1);
    assert!(s.missing_uuids().unwrap().is_empty());
}

/// 基线回填只填 NULL 行：已有 mtime 的行不得被覆盖（对账只判脏不重写基线）。
#[test]
fn backfill_mtimes_never_overwrites_existing_baseline() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-base", "a.txt")).unwrap();
    let mut legacy = meta("u-legacy", "b.txt");
    legacy.mtime = None;
    s.upsert_asset(&legacy).unwrap();

    let rows = vec![("u-base".to_string(), 111), ("u-legacy".to_string(), 222)];
    assert_eq!(s.backfill_mtimes(&rows).unwrap(), 1);

    assert_eq!(
        s.get_asset("u-base").unwrap().unwrap().mtime,
        Some(1_700_000_000)
    );
    assert_eq!(s.get_asset("u-legacy").unwrap().unwrap().mtime, Some(222));
}

/// 重索引写回：派生字段一次写齐并顺带清 missing；不存在的行静默跳过。
#[test]
fn reindexed_batch_writes_derived_fields_and_clears_missing() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-1", "a.txt")).unwrap();
    s.mark_missing(&["u-1"]).unwrap();

    let rows = vec![ReindexedFields {
        uuid: "u-1".to_string(),
        size_bytes: 4096,
        mtime: 1_700_000_100,
        phash: None,
        content_hash: Some(vec![0xAA; 32]),
    }];
    assert_eq!(s.set_reindexed_batch(&rows).unwrap(), 1);

    let asset = s.get_asset("u-1").unwrap().unwrap();
    assert_eq!(asset.size_bytes, 4096);
    assert_eq!(asset.mtime, Some(1_700_000_100));
    assert_eq!(asset.content_hash.as_deref(), Some(&[0xAA; 32][..]));
    assert!(!asset.missing);

    assert_eq!(
        s.set_reindexed_batch(&[ReindexedFields {
            uuid: "u-ghost".to_string(),
            size_bytes: 0,
            mtime: 0,
            phash: None,
            content_hash: None,
        }])
        .unwrap(),
        0
    );
}

/// 孤儿判定基线含回收站行：软删素材的 uuid 仍算「被引用」。
#[test]
fn all_uuids_includes_deleted_rows() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_asset(&meta("u-live", "a.txt")).unwrap();
    s.upsert_asset(&meta("u-dead", "b.txt")).unwrap();
    s.soft_delete_assets(&["u-dead"]).unwrap();

    let mut uuids = s.all_uuids().unwrap();
    uuids.sort();
    assert_eq!(uuids, vec!["u-dead".to_string(), "u-live".to_string()]);
}
