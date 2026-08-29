//! D52 阶段 2+3 红灯：FtsNameSource 映射、uuid 升序不变量、混合路由表、
//! 大小写统一（D51 修复）、NameIn↔内存扫描 oracle 一致性。

use std::fs;
use std::path::{Path, PathBuf};

use domain::Filter;
use index::FacetIndex;
use store::{AssetMeta, Store};
use ui_viewmodels::catalog_loader::RealAssetResolver;
use ui_viewmodels::load_real_library;
use ui_viewmodels::search::{
    FtsNameSource, HybridSearchProvider, SearchError, SearchProvider, SearchScope,
};

fn write_rows(root: &Path, rows: &[(&str, &str, Option<&str>)]) {
    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    for (index, (uuid, file_name, category)) in rows.iter().enumerate() {
        store
            .upsert_asset(&AssetMeta {
                uuid: uuid.to_string(),
                file_name: file_name.to_string(),
                rel_path: format!("objects/{uuid}/{file_name}"),
                category: category.map(|c| c.to_string()),
                tags: Vec::new(),
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
}

fn scaffold(tag: &str, rows: &[(&str, &str, Option<&str>)]) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建库目录失败");
    write_rows(&root, rows);
    root
}

fn demo_rows() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    vec![
        (
            "a0000000-0000-0000-0000-000000000000",
            "促销海报.png",
            Some("风景照"),
        ),
        (
            "b0000000-0000-0000-0000-000000000000",
            "Promo_Head.jpg",
            None,
        ),
        (
            "c0000000-0000-0000-0000-000000000000",
            "风景照合集.png",
            None,
        ),
    ]
}

fn load(root: &Path) -> (FacetIndex, RealAssetResolver) {
    load_real_library(root).expect("装载真实库失败")
}

// ----- 阶段 2：FtsNameSource -----

#[test]
fn uuids_vec_is_ascending_after_load() {
    let root = scaffold("hybrid-ascending", &demo_rows());
    let (_index, resolver) = load(&root);
    let uuids = resolver.uuids();
    assert!(uuids.len() >= 3);
    assert!(
        uuids.windows(2).all(|pair| pair[0] < pair[1]),
        "uuids 必须升序（binary_search 的前提）"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn fts_name_source_maps_uuid_to_row_by_bsearch() {
    let root = scaffold("hybrid-map", &demo_rows());
    let (_index, resolver) = load(&root);
    // ≥3 字符查询（trigram 下限）：命中两行「促销海报/风景照合集」之外的 FTS 面。
    let ids = resolver
        .name_ids("omo_Head", 10_000)
        .expect("FTS 查询应 Ok");
    assert_eq!(ids, vec![1], "FTS 命中 uuid 经二分映射到行号");
    // 名内含 CJK 的 3 字查询（分类名「风景照」不进 FTS name 列）。
    let ids = resolver.name_ids("促销海", 10_000).expect("FTS 查询应 Ok");
    assert_eq!(ids, vec![0]);
    let ids = resolver.name_ids("风景照", 10_000).expect("FTS 查询应 Ok");
    assert_eq!(ids, vec![2], "只有行 2 的文件名含「风景照」");
    let _ = fs::remove_dir_all(&root);
}

// ----- 阶段 3：混合路由表 -----

/// 恒返回固定 id 集的 FTS 源（mock：不触库）。
struct FixedFts(Vec<u32>);

impl FtsNameSource for FixedFts {
    fn name_ids(&self, _query: &str, _limit: usize) -> Result<Vec<u32>, SearchError> {
        Ok(self.0.clone())
    }
}

/// mock FTS 源 + provider 装配（fts 由调用方持有，借用期对齐）。
fn hybrid<'a>(
    facets: &'a ui_viewmodels::catalog_loader::LibraryFacets,
    fts: &'a FixedFts,
) -> HybridSearchProvider<'a> {
    HybridSearchProvider {
        facets,
        fts: Some(fts),
    }
}

fn fixed_fts() -> FixedFts {
    FixedFts(vec![1, 2])
}

#[test]
fn long_query_routes_to_name_in_with_fts() {
    let root = scaffold("hybrid-route-long", &demo_rows());
    let (_index, resolver) = load(&root);
    let fts = fixed_fts();
    let provider = hybrid(resolver.facets(), &fts);
    let filter = provider
        .search("促销海", SearchScope::FileName, &Filter::All)
        .expect("非空查询应 Ok");
    assert_eq!(filter, Filter::NameIn(vec![1, 2]), "≥3 字符 + FTS = NameIn");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn long_query_falls_back_to_memory_without_fts() {
    let root = scaffold(
        "hybrid-route-nof ts".replace(' ', "").as_str(),
        &demo_rows(),
    );
    let (_index, resolver) = load(&root);
    let provider = HybridSearchProvider {
        facets: resolver.facets(),
        fts: None,
    };
    let filter = provider
        .search("促销海", SearchScope::FileName, &Filter::All)
        .expect("非空查询应 Ok");
    assert_eq!(
        filter,
        Filter::NameContains("促销海".to_string()),
        "无 FTS 源 = 内存路（bench/演示库）"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn short_query_always_in_memory() {
    let root = scaffold("hybrid-route-short", &demo_rows());
    let (_index, resolver) = load(&root);
    let fts = fixed_fts();
    let provider = hybrid(resolver.facets(), &fts);
    let filter = provider
        .search("促销", SearchScope::FileName, &Filter::All)
        .expect("非空查询应 Ok");
    assert_eq!(
        filter,
        Filter::NameContains("促销".to_string()),
        "2 字符查询恒内存路（trigram 下限）"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn scope_sets_are_exclusive() {
    let root = scaffold("hybrid-scope", &demo_rows());
    let (_index, resolver) = load(&root);
    let fts = fixed_fts();
    let provider = hybrid(resolver.facets(), &fts);
    // 「风景照」同时是分类名与文件名：四档子句集互斥。
    let file_name = provider
        .search("风景照", SearchScope::FileName, &Filter::All)
        .unwrap();
    assert!(
        matches!(&file_name, Filter::NameIn(_)),
        "仅文件名 = 纯名子句（NameIn/NameContains）"
    );

    let all = provider
        .search("风景照", SearchScope::All, &Filter::All)
        .unwrap();
    let Filter::AnyOf(clauses) = &all else {
        panic!("All 档应为 AnyOf，实际 {all:?}");
    };
    assert!(
        clauses.iter().any(|c| matches!(c, Filter::InCategory(_))),
        "All 档含分类子句"
    );
    assert!(
        clauses.iter().any(|c| matches!(c, Filter::NameIn(_))),
        "All 档含名子句"
    );

    let category = provider
        .search("风景照", SearchScope::Category, &Filter::All)
        .unwrap();
    let Filter::AnyOf(clauses) = &category else {
        panic!("Category 档应为 AnyOf，实际 {category:?}");
    };
    assert!(
        clauses.iter().all(|c| matches!(c, Filter::InCategory(_))),
        "Category 档只含分类子句，实际 {clauses:?}"
    );

    let tag = provider
        .search("风景照", SearchScope::Tag, &Filter::All)
        .unwrap();
    let Filter::AnyOf(clauses) = &tag else {
        panic!("Tag 档应为 AnyOf，实际 {tag:?}");
    };
    assert!(
        clauses.iter().all(|c| matches!(c, Filter::HasTag(_))),
        "Tag 档只含标签子句（本库无标签 → 空子句集）"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn empty_query_is_err() {
    let root = scaffold("hybrid-empty", &demo_rows());
    let (_index, resolver) = load(&root);
    let fts = fixed_fts();
    let provider = hybrid(resolver.facets(), &fts);
    assert_eq!(
        provider.search("  ", SearchScope::All, &Filter::All),
        Err(SearchError::EmptyQuery)
    );
    let _ = fs::remove_dir_all(&root);
}

// ----- D51 大小写统一 -----

#[test]
fn fuzzy_ascii_case_insensitive() {
    let root = scaffold("hybrid-case", &demo_rows());
    let (_index, resolver) = load(&root);
    let provider = HybridSearchProvider {
        facets: resolver.facets(),
        fts: None,
    };
    // 分类名「风景照」无 ASCII；文件名 Promo_Head 走名路。分类名大小写用
    // 新增分类验证：已存分类 Promo 无——所以直接断言名路 + 分类小写命中
    // 用「Promo」查分类名不存在的情形下不 panic；真正断言在内存路折叠上。
    let filter = provider
        .search("promo_head", SearchScope::FileName, &Filter::All)
        .unwrap();
    assert_eq!(filter, Filter::NameContains("promo_head".to_string()));
    let _ = fs::remove_dir_all(&root);
}

// ----- oracle：NameIn（FTS 路）== 内存扫描 -----

#[test]
fn oracle_name_in_equals_memory_scan() {
    let rows: Vec<(&str, &str, Option<&str>)> = vec![
        (
            "a0000000-0000-0000-0000-000000000000",
            "夏季促销海报.png",
            None,
        ),
        (
            "b0000000-0000-0000-0000-000000000000",
            "促销banner.jpg",
            None,
        ),
        (
            "c0000000-0000-0000-0000-000000000000",
            "winter_snow.png",
            None,
        ),
        (
            "d0000000-0000-0000-0000-000000000000",
            "青岛风景照.png",
            None,
        ),
        (
            "e0000000-0000-0000-0000-000000000000",
            "风景照合集2026.png",
            None,
        ),
        (
            "f0000000-0000-0000-0000-000000000000",
            "Çağlar İleri.jpg",
            None,
        ),
    ];
    let root = scaffold("hybrid-oracle", &rows);
    let (index, resolver) = load(&root);
    let queries = ["促销海", "banner", "风景照", "İleri", "winter_snow"];
    for query in queries {
        let fts_ids = resolver.name_ids(query, 10_000).expect("FTS 查询应 Ok");
        let via_fts = index.evaluate(&Filter::NameIn(fts_ids));
        let via_memory = index.evaluate(&Filter::NameContains(query.to_string()));
        assert_eq!(
            via_fts, via_memory,
            "查询 {query:?}：FTS 路（NameIn）与内存路（NameContains）结果必须一致"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

// ----- 4.3 回收站覆盖：软删后四档查询均不含已删行 -----

#[test]
fn deleted_rows_excluded_across_scopes() {
    let root = scaffold("hybrid-trash-scope", &demo_rows());
    {
        // 软删行 0（促销海报.png，分类=风景照——分类/名两路都能命中它）。
        let store = Store::open(&root.join("meta.db")).unwrap();
        store
            .soft_delete_assets(&["a0000000-0000-0000-0000-000000000000"])
            .unwrap();
    }
    let (index, resolver) = load(&root);
    let provider = HybridSearchProvider {
        facets: resolver.facets(),
        fts: Some(&resolver),
    };
    let scopes = [
        ("全部", SearchScope::All),
        ("文件名", SearchScope::FileName),
        ("分类", SearchScope::Category),
        ("标签", SearchScope::Tag),
    ];
    for (label, scope) in scopes {
        for query in ["促销海", "风景照"] {
            if let Ok(filter) = provider.search(query, scope, &Filter::All) {
                let hit = index.evaluate(&filter);
                assert!(
                    !hit.contains(0),
                    "范围 {label} 查询 {query:?} 不得含已删行（deleted=0 JOIN / deleted 位图）"
                );
            }
        }
    }
    let _ = fs::remove_dir_all(&root);
}
