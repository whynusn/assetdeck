//! 缩略图寻址与真实宽高比：素材瓦片「能看出是什么」的数据侧契约。
//!
//! 背景（真实缺陷）：瓦片此前一律显示 `id % 8` 的纯色块，因为没有任何生产
//! 代码去找 `thumbs/<分片>/<uuid>.png`；版式用的也是与画面无关的占位公式。
//! 本测试锁定两件事：缩略图路径按 uuid 分片规则可寻址且不存在时诚实回 None；
//! `assets.width/height` 能变成 `AssetId → w/h` 表驱动版式。

use std::fs;
use std::path::{Path, PathBuf};

use domain::AssetId;
use store::{AssetMeta, Store};
use ui_viewmodels::load_real_library;

/// 三条素材：有缩略图且有尺寸 / 有缩略图无尺寸 / 两者皆无。
/// uuid 刻意用不同首字符，覆盖两级分片目录。
const ROWS: [(&str, &str); 3] = [
    ("a1111111-0000-0000-0000-000000000000", "wide.jpg"),
    ("b2222222-0000-0000-0000-000000000000", "tall.jpg"),
    ("c3333333-0000-0000-0000-000000000000", "bare.mp4"),
];

fn scaffold(tag: &str) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建库目录失败");
    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    for (index, (uuid, file_name)) in ROWS.iter().enumerate() {
        let object_dir = root.join("objects").join(uuid);
        fs::create_dir_all(&object_dir).expect("建对象目录失败");
        fs::write(object_dir.join(file_name), b"x").expect("写素材字节失败");
        // 前两条给尺寸（横图 / 竖图），第三条留空模拟抽帧失败。
        let (width, height) = match index {
            0 => (Some(1920), Some(1080)),
            1 => (Some(720), Some(1280)),
            _ => (None, None),
        };
        store
            .upsert_asset(&AssetMeta {
                uuid: uuid.to_string(),
                file_name: file_name.to_string(),
                rel_path: format!("objects/{uuid}/{file_name}"),
                category: None,
                tags: vec![],
                size_bytes: 1,
                created_at: index as i64,
                imported_at: index as i64,
                phash: None,
                width,
                height,
            })
            .expect("写资产元数据失败");
    }
    root
}

fn write_thumb(root: &Path, uuid: &str) -> PathBuf {
    let dest = root.join(Store::thumbnail_cache_path(uuid, "png"));
    fs::create_dir_all(dest.parent().expect("缩略图路径必须有父目录")).expect("建缩略图目录失败");
    fs::write(&dest, b"fake-png").expect("写缩略图失败");
    dest
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

/// 装载顺序即 AssetId 顺序（`for_each_asset` 按 uuid 升序），故 a→0、b→1、c→2。
#[test]
fn thumbnail_path_resolves_existing_file_and_reports_missing_honestly() {
    let root = scaffold("thumb-lookup");
    write_thumb(&root, ROWS[0].0);
    write_thumb(&root, ROWS[1].0);

    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");

    for id in [AssetId(0), AssetId(1)] {
        let path = resolver
            .thumbnail_path(id)
            .unwrap_or_else(|| panic!("{id:?} 应能寻址到缩略图"));
        assert!(
            path.is_absolute(),
            "缩略图路径必须绝对（库 root 允许是相对路径），实际 {}",
            path.display()
        );
        assert!(path.is_file(), "返回的路径必须真实存在: {}", path.display());
    }
    assert!(
        resolver.thumbnail_path(AssetId(2)).is_none(),
        "缩略图未派生时必须回 None，让瓦片回落纯色而不是渲染空图"
    );
    assert!(
        resolver.thumbnail_path(AssetId(99)).is_none(),
        "越界 id 不得 panic，只回 None"
    );
    cleanup(&root);
}

#[test]
fn aspects_table_carries_real_dimensions_and_omits_unsized_rows() {
    let root = scaffold("thumb-aspects");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let aspects = resolver.aspects().expect("读取宽高比表失败");

    let wide = aspects.get(&AssetId(0)).copied().expect("横图应有宽高比");
    let tall = aspects.get(&AssetId(1)).copied().expect("竖图应有宽高比");
    assert!(
        (wide - 1920.0 / 1080.0).abs() < 1e-4,
        "横图宽高比应为 16:9，实际 {wide}"
    );
    assert!(
        (tall - 720.0 / 1280.0).abs() < 1e-4,
        "竖图宽高比应为 9:16，实际 {tall}"
    );
    assert!(
        !aspects.contains_key(&AssetId(2)),
        "缺尺寸的行不得进表，交由 VM 回落占位比例"
    );
    cleanup(&root);
}
