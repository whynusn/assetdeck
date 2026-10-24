//! D91 内容对账（stat 级）端到端：基线回填、判脏、缺失翻转、孤儿清扫。

use std::fs;
use std::path::Path;

use library::consistency::{self, ConsistencyReport, ContentState};
use store::{AssetMeta, Store};

fn meta(uuid: &str, rel_path: &str, size: i64, mtime: Option<i64>) -> AssetMeta {
    AssetMeta {
        uuid: uuid.to_string(),
        file_name: "a.txt".to_string(),
        rel_path: rel_path.to_string(),
        category: None,
        category_id: None,
        tags: vec![],
        size_bytes: size,
        created_at: 0,
        imported_at: 0,
        phash: None,
        content_hash: None,
        width: None,
        height: None,
        mtime,
        missing: false,
    }
}

fn write_raw(root: &Path, uuid: &str, body: &str) -> std::fs::Metadata {
    let dir = root.join("objects").join(uuid);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("raw.txt");
    fs::write(&path, body).unwrap();
    fs::metadata(&path).unwrap()
}

#[test]
fn audit_baselines_then_flags_stale_missing_and_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = Store::open_in_memory().unwrap();

    // u-base：文件在、未基线 → 首扫只回填。
    let md = write_raw(root, "u-base", "基线内容");
    let base_mtime = consistency::mtime_secs(&md).unwrap();
    store
        .upsert_asset(&meta(
            "u-base",
            "objects/u-base/raw.txt",
            md.len() as i64,
            None,
        ))
        .unwrap();

    // u-stale：文件被「外部修改」（mtime/size 与库记录不符）。
    write_raw(root, "u-stale", "被外部改过的内容，比库里长了一截");
    store
        .upsert_asset(&meta(
            "u-stale",
            "objects/u-stale/raw.txt",
            10,
            Some(base_mtime - 999),
        ))
        .unwrap();

    // u-gone：正本不存在 → 缺失。
    store
        .upsert_asset(&meta("u-gone", "objects/u-gone/raw.txt", 10, Some(1)))
        .unwrap();

    let report = consistency::audit_content_with(root, &store, 0).unwrap();
    assert_eq!(report.scanned, 3);
    assert_eq!(report.baselined, 1, "只有 u-base 需要基线");
    assert_eq!(report.stale, vec!["u-stale".to_string()]);
    assert_eq!(report.missing, vec!["u-gone".to_string()]);

    // 基线落库；缺失旗标落库。
    assert_eq!(
        store.get_asset("u-base").unwrap().unwrap().mtime,
        Some(base_mtime)
    );
    assert!(store.get_asset("u-gone").unwrap().unwrap().missing);

    // 第二扫：审计只读不治——stale 行持续上报（修复要等 reindex 工序把
    // size/mtime 写回），基线与缺失语义幂等。
    let report = consistency::audit_content_with(root, &store, 0).unwrap();
    assert_eq!(report.baselined, 0);
    assert_eq!(report.stale, vec!["u-stale".to_string()]);
    assert_eq!(
        report.missing,
        vec!["u-gone".to_string()],
        "缺失现状全量上报"
    );
    assert_eq!(report.recovered, Vec::<String>::new());

    // 缺失文件回来了 → recovered + 旗标清除；重制的文件 mtime 已变，
    // 同轮照常进 stale（恢复与修改互不冲突，修复交给重索引工序）。
    write_raw(root, "u-gone", "回来了");
    let report = consistency::audit_content_with(root, &store, 0).unwrap();
    assert_eq!(report.recovered, vec!["u-gone".to_string()]);
    assert_eq!(
        report.stale.len(),
        2,
        "u-stale 未愈 + u-gone 重制均为待重算"
    );
    assert!(report.stale.contains(&"u-gone".to_string()));
    assert!(!store.get_asset("u-gone").unwrap().unwrap().missing);
}

/// 孤儿清扫：无行引用、年龄足额的 objects/ 子目录整目录回收；年龄不足不动
/// （在途导入保护）；被引用的目录（含回收站行的 uuid）永不回收。
#[test]
fn orphan_reaping_respects_age_and_references() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = Store::open_in_memory().unwrap();

    // 被引用目录（活行 + 回收站行各一）。
    write_raw(root, "u-live", "x");
    write_raw(root, "u-dead", "x");
    store
        .upsert_asset(&meta("u-live", "objects/u-live/raw.txt", 1, Some(1)))
        .unwrap();
    store
        .upsert_asset(&meta("u-dead", "objects/u-dead/raw.txt", 1, Some(1)))
        .unwrap();
    store.soft_delete_assets(&["u-dead"]).unwrap();

    // 孤儿目录。
    fs::create_dir_all(root.join("objects").join("orphan-crash")).unwrap();

    // 年龄保护：阈值极大时不回收。
    let report = consistency::audit_content_with(root, &store, u64::MAX).unwrap();
    assert_eq!(report.orphans_reaped, 0);
    assert!(root.join("objects").join("orphan-crash").is_dir());

    // 阈值 0：孤儿回收，被引用目录原样。
    let report = consistency::audit_content_with(root, &store, 0).unwrap();
    assert_eq!(report.orphans_reaped, 1);
    assert!(!root.join("objects").join("orphan-crash").exists());
    assert!(root.join("objects").join("u-live").is_dir());
    assert!(root.join("objects").join("u-dead").is_dir());
}

/// rel_path 为空的历史行诚实跳过（无正本地址可查），不算缺失也不算扫描异常。
#[test]
fn empty_rel_path_is_skipped_honestly() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    store.upsert_asset(&meta("u-old", "", 10, Some(1))).unwrap();

    let report = consistency::audit_content(dir.path(), &store).unwrap();
    assert_eq!(report.skipped_no_path, 1);
    assert!(report.missing.is_empty());
}

/// classify：对账判定唯一出处的口径表（与 reindex 工序共用同一实现）。
#[test]
fn classify_predicate_is_the_single_source() {
    let m = meta("u", "objects/u/raw.txt", 10, Some(100));
    assert_eq!(consistency::classify(&m, 100, 10), ContentState::InSync);
    assert_eq!(consistency::classify(&m, 101, 10), ContentState::Stale);
    let unbased = meta("u", "objects/u/raw.txt", 10, None);
    assert_eq!(
        consistency::classify(&unbased, 100, 10),
        ContentState::NeedsBaseline
    );
}

#[allow(dead_code)]
fn report_type_shape(_: ConsistencyReport) {}
