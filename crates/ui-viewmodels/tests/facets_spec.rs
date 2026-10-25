//! LibraryFacets 名称注册表与 fuzzy_filter 四态：检索器数据侧契约。
//!
//! 用真实库装载路径构建注册表，锁定：分类/标签名子串命中组 AnyOf；
//! 空查询回 None（回落当前视图）；无命中回空集（瀑布流清空）。

use std::fs;
use std::path::PathBuf;

use domain::{CategoryId, Filter, TagId};
use store::{AssetMeta, Store};
use ui_viewmodels::load_real_library;

fn scaffold(tag: &str) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建库目录失败");
    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    let rows: [(&str, &str, Option<&str>, &[&str]); 3] = [
        (
            "a0000000-0000-0000-0000-000000000000",
            "one.png",
            Some("测试素材"),
            &["真实闭环"],
        ),
        (
            "b0000000-0000-0000-0000-000000000000",
            "two.png",
            Some("测试素材"),
            &["促销"],
        ),
        (
            "c0000000-0000-0000-0000-000000000000",
            "three.png",
            Some("风景"),
            &["促销"],
        ),
    ];
    for (index, (uuid, file_name, category, tags)) in rows.iter().enumerate() {
        store
            .upsert_asset(&AssetMeta {
                uuid: uuid.to_string(),
                file_name: file_name.to_string(),
                rel_path: format!("objects/{uuid}/{file_name}"),
                category: category.map(|c| c.to_string()),
                category_id: None,
                tags: tags.iter().map(|t| t.to_string()).collect(),
                size_bytes: 1,
                created_at: index as i64,
                imported_at: index as i64,
                phash: None,
                content_hash: None,
                width: None,
                height: None,
                mtime: None,
                missing: false,
            })
            .expect("写资产元数据失败");
    }
    root
}

fn cleanup(root: &std::path::Path) {
    let _ = fs::remove_dir_all(root);
}

#[test]
fn categories_and_tags_sorted_with_counts() {
    let root = scaffold("facets-counts");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let facets = resolver.facets();

    let cats: Vec<(&str, u32)> = facets
        .categories()
        .iter()
        .map(|e| (e.name.as_str(), e.count))
        .collect();
    assert_eq!(
        cats,
        vec![(store::INBOX_CATEGORY, 0), ("测试素材", 2), ("风景", 1)],
        "分类按名升序 + 计数；收件箱是固定系统项（无 NULL 行时计数 0）"
    );

    let tags: Vec<(&str, u32)> = facets
        .tags()
        .iter()
        .map(|e| (e.name.as_str(), e.count))
        .collect();
    assert_eq!(
        tags,
        vec![("促销", 2), ("真实闭环", 1)],
        "标签按名升序 + 计数"
    );
    cleanup(&root);
}

/// D85：收件箱是固定系统分面项（CategoryId(0)），成员 = `category IS NULL`
/// 的活跃行。删除分类处置 NULL 后，素材必须在这里重新可见。
#[test]
fn inbox_facet_is_synthesized_from_null_category_rows() {
    let root = PathBuf::from("target").join("tmp").join("facets-inbox");
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建库目录失败");
    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    let rows: [(&str, &str, Option<&str>); 2] = [
        (
            "a0000000-0000-0000-0000-000000000000",
            "categorized.png",
            Some("风景"),
        ),
        ("b0000000-0000-0000-0000-000000000000", "inbox.png", None),
    ];
    for (index, (uuid, file_name, category)) in rows.iter().enumerate() {
        store
            .upsert_asset(&AssetMeta {
                uuid: uuid.to_string(),
                file_name: file_name.to_string(),
                rel_path: format!("objects/{uuid}/{file_name}"),
                category: (*category).map(|c| c.to_string()),
                category_id: None,
                tags: vec![],
                size_bytes: 1,
                created_at: index as i64,
                imported_at: index as i64,
                phash: None,
                content_hash: None,
                width: None,
                height: None,
                mtime: None,
                missing: false,
            })
            .expect("写资产元数据失败");
    }
    drop(store);

    let (index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let facets = resolver.facets();
    let inbox = facets
        .category_id(store::INBOX_CATEGORY)
        .expect("收件箱应恒在分面注册表");
    assert_eq!(inbox.0, 0, "收件箱固定 CategoryId(0)，侧栏下标映射才稳定");
    let entry = facets
        .categories()
        .iter()
        .find(|e| e.name == store::INBOX_CATEGORY)
        .expect("收件箱应出现在分类条目中");
    assert_eq!(entry.count, 1, "NULL 行计入收件箱");
    assert_eq!(
        index.evaluate(&Filter::InCategory(inbox)).len(),
        1,
        "待分类视图必须能选出 NULL 行"
    );
    cleanup(&root);
}

#[test]
fn fuzzy_filter_matches_category_fragment() {
    let root = scaffold("facets-cat-hit");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let facets = resolver.facets();
    let id = facets.category_id("测试素材").expect("应有该分类").0;

    let filter = facets.fuzzy_filter("测试").expect("非空查询应返回 Some");
    assert_eq!(
        filter,
        Filter::AnyOf(vec![Filter::InCategory(CategoryId(id))])
    );
    cleanup(&root);
}

#[test]
fn fuzzy_filter_matches_tag_fragment() {
    let root = scaffold("facets-tag-hit");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let facets = resolver.facets();
    let promo_id = facets
        .tags()
        .iter()
        .find(|e| e.name == "促销")
        .expect("应有该标签")
        .id;

    let filter = facets.fuzzy_filter("促").expect("非空查询应返回 Some");
    assert_eq!(filter, Filter::AnyOf(vec![Filter::HasTag(TagId(promo_id))]));
    cleanup(&root);
}

#[test]
fn fuzzy_filter_empty_query_returns_none() {
    let root = scaffold("facets-empty");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    assert_eq!(
        resolver.facets().fuzzy_filter("   "),
        None,
        "空白查询回 None"
    );
    cleanup(&root);
}

#[test]
fn fuzzy_filter_no_match_returns_empty_set() {
    let root = scaffold("facets-miss");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    assert_eq!(
        resolver.facets().fuzzy_filter("不存在的词"),
        Some(Filter::AnyOf(vec![])),
        "无命中回空集"
    );
    cleanup(&root);
}

/// D87：分类 id 来自 categories 表，跨重载稳定——改名只换显示名，
/// 侧栏行序会因按名重排而变，但引用方持有的 CategoryId 不变。
#[test]
fn category_id_survives_rename_and_reload() {
    let root = scaffold("facets-stable-id");
    let (_index, first) = load_real_library(&root).expect("装载真实库失败");
    let before = first.facets().category_id("测试素材").expect("应有该分类");

    {
        let store = Store::open(&root.join("meta.db")).unwrap();
        store.rename_category("测试素材", "改名后").unwrap();
    }
    let (index, second) = load_real_library(&root).expect("重载失败");
    let after = second
        .facets()
        .category_id("改名后")
        .expect("改名后仍应可寻址");

    assert_eq!(after, before, "改名不得改变 CategoryId");
    assert!(
        second.facets().category_id("测试素材").is_none(),
        "旧名不再注册"
    );
    assert_eq!(
        index.evaluate(&Filter::InCategory(after)).len(),
        2,
        "稳定 id 继续指向同一批素材"
    );
    cleanup(&root);
}

/// D87：空分类是一等实体，装载后仍在侧栏条目里（计数 0），可被选中。
#[test]
fn empty_category_still_listed_with_zero_count() {
    let root = scaffold("facets-empty-cat");
    {
        let store = Store::open(&root.join("meta.db")).unwrap();
        store.ensure_category("还没素材").unwrap();
    }
    let (index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let entry = resolver
        .facets()
        .categories()
        .iter()
        .find(|e| e.name == "还没素材")
        .expect("空分类必须出现在侧栏条目");
    assert_eq!(entry.count, 0);
    assert_eq!(
        index
            .evaluate(&Filter::InCategory(CategoryId(entry.id)))
            .len(),
        0,
        "空分类视图合法（空集）"
    );
    cleanup(&root);
}
