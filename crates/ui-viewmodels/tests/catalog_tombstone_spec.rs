//! D46 装载过滤契约：回收站行「占号不显形」。
//!
//! 背景：AssetId 是行号，`RealAssetResolver.uuids` 与行表按下标一一对应，
//! FTS5 结果靠 uuids 二分回行号（D52）。所以装载时回收站行**必须照常分配
//! 行号**（insert_as_deleted），只从活集与分面注册表退席——若跳过不插，
//! 后续行的号全体前移，二分映射与双击寻址全部错位。本文件锁死这条地基。

use std::fs;
use std::path::{Path, PathBuf};

use domain::Filter;
use store::{AssetMeta, Store};
use ui_viewmodels::load_real_library;

fn row(uuid: &str, name: &str, category: Option<&str>) -> AssetMeta {
    AssetMeta {
        uuid: uuid.to_string(),
        file_name: name.to_string(),
        rel_path: format!("objects/{uuid}/{name}"),
        category: category.map(|c| c.to_string()),
        tags: vec![],
        size_bytes: 10,
        created_at: 1,
        imported_at: 1,
        phash: None,
        content_hash: None,
        width: None,
        height: None,
    }
}

fn scaffold(tag: &str) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("meta.db")).unwrap();
    // uuid 升序装载：a(活) / b(回收站) / c(活)。b 居中才能测出号对齐。
    store
        .upsert_asset(&row("a-0000", "甲.png", Some("风景")))
        .unwrap();
    store
        .upsert_asset(&row("b-0000", "乙.png", Some("风景")))
        .unwrap();
    store
        .upsert_asset(&row("c-0000", "丙.png", Some("表情")))
        .unwrap();
    store.soft_delete_assets(&["b-0000"]).unwrap();
    drop(store);
    root
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

#[test]
fn loaded_index_hides_trash_but_keeps_row_alignment() {
    let root = scaffold("loader-tombstone-align");
    let (index, resolver) = load_real_library(&root).expect("装载真实库失败");

    // 活集只有两行，且行号按 uuid 升序落位（a=0, c=2）——b 占住 1 号。
    assert_eq!(index.len(), 2);
    let all = index.evaluate(&Filter::All);
    assert!(all.contains(0));
    assert!(!all.contains(1), "回收站行不得显形");
    assert!(all.contains(2), "号对齐：c 的行号必须是 2 不是 1");
    assert_eq!(index.name(0), Some("甲.png"));
    assert_eq!(index.name(2), Some("丙.png"));
    assert_eq!(
        index.name(1),
        Some("乙.png"),
        "回收站行数据仍可读（回收站视图）"
    );

    // 分面计数不含回收站：风景只剩甲一张。
    let photo = resolver
        .facets()
        .category_id("风景")
        .expect("风景分类应注册（甲在册）");
    assert_eq!(index.evaluate(&Filter::InCategory(photo)).len(), 1);

    cleanup(&root);
}

#[test]
fn search_provider_never_returns_trashed_rows() {
    let root = scaffold("loader-tombstone-search");
    let (index, resolver) = load_real_library(&root).expect("装载真实库失败");

    // 内存名扫描：乙在回收站，扫不到。
    let hits = index.evaluate(&Filter::NameContains("乙".into()));
    assert!(hits.is_empty(), "NameContains 不得命中回收站行");

    // FTS 路（Store::search）同样过滤；两路在装载层都不泄漏。
    let fts = resolver.store_search("乙.png", 10).expect("FTS 查询失败");
    assert!(fts.is_empty(), "FTS 不得返回回收站行");

    cleanup(&root);
}
