//! D92 共享可见性投影：分类聚合（NULL → 收件箱命名）、成员集空零成本、
//! 「将分类共享至」源路径解析（具名分类 / 收件箱 / 未知名）。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use store::{AssetMeta, Store};
use ui_viewmodels::{category_source_paths, share_projection, ShareProjection};

/// 建库并铺 5 行：相册 2 行（1 行共享）、风景 1 行（共享）、收件箱
///（category NULL）2 行（1 行共享、1 行软删——投影只算活动行）。
/// content_hash = 32 字节全同尾值，成员集按尾值构造。
fn scaffold(tag: &str) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建库目录失败");
    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    // (uuid 尾字节, 分类, content_hash 尾值, 是否软删)
    let rows: [(u8, Option<&str>, u8, bool); 5] = [
        (1, Some("相册"), 1, false),
        (2, Some("相册"), 2, false),
        (3, Some("风景"), 3, false),
        (4, None, 4, false),
        (5, None, 5, true),
    ];
    for (i, (tail, category, hash_tail, deleted)) in rows.iter().enumerate() {
        let uuid = uuid_of(*tail);
        store
            .upsert_asset(&AssetMeta {
                uuid: uuid.clone(),
                file_name: format!("f{tail}.png"),
                rel_path: format!("objects/{uuid}/raw.png"),
                category: category.map(|c| c.to_string()),
                category_id: None,
                tags: vec![],
                size_bytes: 1,
                created_at: i as i64,
                imported_at: i as i64,
                phash: None,
                content_hash: Some(vec![*hash_tail; 32]),
                width: None,
                height: None,
                mtime: None,
                missing: false,
            })
            .expect("写资产元数据失败");
        if *deleted {
            store
                .soft_delete_assets(&[uuid.as_str()])
                .expect("软删失败");
        }
    }
    root
}

fn members_of(tails: &[u8]) -> HashSet<Vec<u8>> {
    tails.iter().map(|t| vec![*t; 32]).collect()
}

fn uuid_of(tail: u8) -> String {
    format!("00000000-0000-0000-0000-{:012x}", tail)
}

#[test]
fn share_projection_aggregates_by_category_with_inbox_naming() {
    let root = scaffold("share-projection-agg");
    let projection = share_projection(&root, &members_of(&[1, 3, 4]));

    // 分类聚合（总数, 已共享）：相册 (2,1) 部分、风景 (1,1) 全、收件箱
    // (1,0)——软删行不计，NULL 分类名 = 收件箱常量（与 facets 同源）。
    assert_eq!(projection.by_category.get("相册"), Some(&(2, 1)));
    assert_eq!(projection.by_category.get("风景"), Some(&(1, 1)));
    assert_eq!(
        projection.by_category.get(store::INBOX_CATEGORY),
        Some(&(1, 1)),
        "收件箱：软删的尾 5 行不计，活动的尾 4 行共享"
    );

    // 共享 uuid 集只含命中行（尺寸 = 实际共享数）。
    let expected: HashSet<String> = [1u8, 3, 4].into_iter().map(uuid_of).collect();
    assert_eq!(projection.shared_uuids, expected);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn empty_member_set_yields_empty_projection() {
    let root = scaffold("share-projection-empty");
    let projection = share_projection(&root, &HashSet::new());
    assert_eq!(projection, ShareProjection::default());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn category_source_paths_covers_named_inbox_and_unknown() {
    let root = scaffold("share-projection-paths");
    // 具名分类：相册 2 条。
    let album = category_source_paths(&root, "相册");
    assert_eq!(album.len(), 2, "相册 2 行都应产出源路径");
    assert!(album.iter().all(|p| p.is_absolute()));

    // 收件箱（NULL）：只余 1 条活动行（软删的尾 5 行不在）。
    let inbox = category_source_paths(&root, store::INBOX_CATEGORY);
    assert_eq!(inbox.len(), 1);
    // ends_with 按路径组件比较，不受 Windows 反斜杠影响。
    assert!(
        inbox[0].ends_with(
            std::path::Path::new("objects")
                .join(uuid_of(4))
                .join("raw.png")
        ),
        "收件箱路径应指向尾 4 行: {}",
        inbox[0].display()
    );

    // 未知分类名（菜单滞留旧名）：空清单，不炸。
    assert!(category_source_paths(&root, "不存在的分类").is_empty());
    let _ = fs::remove_dir_all(&root);
}
