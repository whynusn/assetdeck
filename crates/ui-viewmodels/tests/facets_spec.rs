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
                tags: tags.iter().map(|t| t.to_string()).collect(),
                size_bytes: 1,
                created_at: index as i64,
                imported_at: index as i64,
                phash: None,
                content_hash: None,
                width: None,
                height: None,
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
        vec![("测试素材", 2), ("风景", 1)],
        "分类按名升序 + 计数"
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
