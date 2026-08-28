//! D52 阶段 1 红灯：`Filter::NameIn` 求值语义 + 文件名大小写折叠扫描。
//!
//! NameIn 的关键不变量：FTS 命中的 uuid 可能是回收站行（v4 起 FTS 行不随软删
//! 移除），所以 evaluate(NameIn) 必须**与活集求交**——传入已删 id 被静默剔除，
//! 传入未知 id 同样被剔除（与 all 求交=恒等的名字由此而来：合法入参恰好全保留）。

use std::fs;
use std::path::{Path, PathBuf};

use domain::{CategoryId, Filter};
use index::FacetIndex;
use store::{AssetMeta, Store};

fn scaffold(tag: &str) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建库目录失败");
    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    let rows: [(&str, &str); 3] = [
        ("a0000000-0000-0000-0000-000000000000", "促销海报.png"),
        ("b0000000-0000-0000-0000-000000000000", "Promo_Head.jpg"),
        ("c0000000-0000-0000-0000-000000000000", "İstanbul ırmak.png"),
    ];
    for (index, (uuid, file_name)) in rows.iter().enumerate() {
        store
            .upsert_asset(&AssetMeta {
                uuid: uuid.to_string(),
                file_name: file_name.to_string(),
                rel_path: format!("objects/{uuid}/{file_name}"),
                category: None,
                tags: Vec::new(),
                size_bytes: 1,
                created_at: index as i64,
                imported_at: index as i64,
                phash: None,
                width: None,
                height: None,
            })
            .expect("写资产元数据失败");
    }
    root
}

fn load(root: &Path) -> FacetIndex {
    let (index, _resolver) = ui_viewmodels::load_real_library(root).expect("装载真实库失败");
    index
}

#[test]
fn name_in_filter_returns_given_ids_intersected_with_live_set() {
    let root = scaffold("namein-live");
    let index = load(&root);
    // 行号：uuid 字典序 → a=0, b=1, c=2。
    let filter = Filter::NameIn(vec![0, 2, 77]);
    let hit = index.evaluate(&filter);
    assert!(hit.contains(0) && hit.contains(2), "合法 id 恒等保留");
    assert!(!hit.contains(77), "未知 id 被活集求交剔除");

    // 软删一行后重算：NameIn 的已删 id 必须被剔除（FTS 行在、查询侧限活集）。
    let store = Store::open(&root.join("meta.db")).unwrap();
    store
        .soft_delete_assets(&["a0000000-0000-0000-0000-000000000000"])
        .unwrap();
    let index = load(&root);
    let hit = index.evaluate(&Filter::NameIn(vec![0, 1, 2]));
    assert!(!hit.contains(0), "回收站行不进 NameIn 结果");
    assert!(hit.contains(1) && hit.contains(2));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn search_names_unicode_case_folding() {
    let root = scaffold("namein-fold");
    let index = load(&root);
    // CJK 无大小写：子串照常命中。
    assert_eq!(
        index.search_names("促销"),
        index.evaluate(&Filter::NameContains("促销".into()))
    );
    // ASCII 大小写折叠：小写 needle 命中大写文件名。
    assert!(
        index.search_names("promo_head").contains(1),
        "ASCII needle 应折叠命中 Promo_Head.jpg"
    );
    // 土耳其语 İ：to_lowercase 展开为 i̇，逐字符折叠后 needle「i̇stanbul」命中。
    assert!(
        index.search_names("i̇stanbul").contains(2),
        "İ 的 Unicode 折叠展开须双侧一致"
    );
    assert!(
        index.search_names("ırmak").contains(2),
        "无大写形式的 ı 恒等命中"
    );
    let _ = fs::remove_dir_all(&root);
}

/// 既有语义守卫：NameContains 大小写不敏感子串不退行（改造前后一致）。
#[test]
fn name_contains_filter_matches_case_insensitive_substring() {
    let root = scaffold("namein-contains");
    let index = load(&root);
    let hit = index.evaluate(&Filter::NameContains("promo".into()));
    assert!(hit.contains(1), "小写 needle 命中 Promo_Head.jpg");
    let _ = fs::remove_dir_all(&root);
}

/// NameIn 与分类子句的合成走既有 AnyOf 路径（D4：Filter 纯声明，可组合）。
#[test]
fn name_in_composes_with_category_clause() {
    let root = scaffold("namein-compose");
    load(&root);
    let store = Store::open(&root.join("meta.db")).unwrap();
    store
        .set_category("b0000000-0000-0000-0000-000000000000", Some("风景"))
        .unwrap();
    let index = load(&root);
    let (_index2, resolver) = ui_viewmodels::load_real_library(&root).unwrap();
    let category_id = resolver.facets().category_id("风景").expect("分类应注册").0;
    let hit = index.evaluate(&Filter::AllOf(vec![
        Filter::InCategory(CategoryId(category_id)),
        Filter::NameIn(vec![0, 1, 2]),
    ]));
    assert!(hit.contains(1), "交集只留该分类内的 NameIn 命中");
    assert!(!hit.contains(0) && !hit.contains(2));
    let _ = fs::remove_dir_all(&root);
}
