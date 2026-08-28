//! 素材物化契约：交给剪贴板的 `source_path` 必须是绝对路径。
//!
//! 回归背景（真实 IM 实测）：`rel_path` 以 '/' 分隔存储，此前直接 `root.join(rel_path)`
//! 且 root 允许是相对路径（`--library samples/library`），结果写进 CF_HDROP 的是
//! 相对路径。接收方 IM 进程按自己的工作目录解析，找不到文件就**静默丢弃整次粘贴**：
//! 输入框毫无变化，且没有任何错误可捕获。本测试锁定「相对 root + '/' 分隔 rel_path
//! → 绝对且真实存在的路径」。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use store::{AssetMeta, Store};
use ui_viewmodels::{load_real_library, AssetId, AssetKind};

/// 在**相对**路径下搭一个最小库：objects/<uuid>/<file>，meta.rel_path 用 '/' 分隔。
fn scaffold_relative_library(tag: &str, file_name: &str, bytes: &[u8]) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    let uuid = "0000-object";
    let object_dir = root.join("objects").join(uuid);
    fs::create_dir_all(&object_dir).expect("建立对象目录失败");
    fs::write(object_dir.join(file_name), bytes).expect("写入素材字节失败");

    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    store
        .upsert_asset(&AssetMeta {
            uuid: uuid.to_string(),
            file_name: file_name.to_string(),
            rel_path: format!("objects/{uuid}/{file_name}"),
            category: None,
            tags: vec![],
            size_bytes: bytes.len() as i64,
            created_at: 1,
            imported_at: 1,
            phash: None,
            width: None,
            height: None,
        })
        .expect("写入资产元数据失败");
    assert!(
        root.is_relative(),
        "本测试的前提是 root 为相对路径，实际 {}",
        root.display()
    );
    root
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

#[test]
fn materialized_source_path_is_absolute_for_relative_library_root() {
    let root = scaffold_relative_library("payload-abs-jpg", "dog.jpg", &[0xFF, 0xD8, 0xFF, 0xD9]);
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let asset = resolver
        .materialize_by_file_name("dog.jpg")
        .expect("物化过程报错")
        .expect("按文件名未找到素材");

    assert!(
        asset.source_path.is_absolute(),
        "HDROP 载荷路径必须绝对，实际 {}",
        asset.source_path.display()
    );
    assert!(
        asset.source_path.is_file(),
        "绝对化后的路径必须真实存在（'/' 分隔的 rel_path 需逐段拼接），实际 {}",
        asset.source_path.display()
    );
    assert_eq!(asset.kind, AssetKind::Image);
    cleanup(&root);
}

#[test]
fn video_payload_keeps_absolute_file_path_and_no_inline_bytes() {
    let root = scaffold_relative_library("payload-abs-mp4", "clip.mp4", &[0, 0, 0, 0x18]);
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let asset = resolver
        .materialize_by_file_name("clip.mp4")
        .expect("物化过程报错")
        .expect("按文件名未找到素材");

    assert_eq!(asset.kind, AssetKind::Video);
    assert!(
        asset.source_path.is_absolute() && asset.source_path.is_file(),
        "视频只能靠文件路径上框，路径必须绝对且存在，实际 {}",
        asset.source_path.display()
    );
    assert!(
        asset.png_bytes.is_empty(),
        "非 PNG 素材不得内联字节（UI 进程不解码红线）"
    );
    cleanup(&root);
}

/// D41：图片物化**零读盘**——png_bytes 恒空，协商自然回落 CF_HDROP。
///
/// 旧契约（物化期预读旁挂派生 paste.png / PNG 原图字节）已废除：内联 PNG 只在
/// source_path 缺失的库外素材场景才被消费，v1 素材恒来自库内，预读是纯浪费——
/// 缓存（4 条 LRU）每 miss 一次就是一次 UI 线程同步读盘，低配机上正是
/// 「点击素材到出结果肉眼可见地慢」的主要来源。存在派生文件也不读：
/// 派生 paste.png 的价值（对端解码成本封顶）已由 files 路径天然获得
/// （对端只向外壳要缩略图，D22）。
#[test]
fn image_materialization_never_reads_png_bytes() {
    let root =
        scaffold_relative_library("payload-derived-png", "dog.jpg", &[0xFF, 0xD8, 0xFF, 0xD9]);
    let derived = root.join(Store::paste_png_path("0000-object"));
    fs::create_dir_all(derived.parent().expect("派生路径必须有父目录")).expect("建派生目录失败");
    fs::write(&derived, b"fake-png-bytes").expect("写派生 PNG 失败");

    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let asset = resolver
        .materialize_by_file_name("dog.jpg")
        .expect("物化过程报错")
        .expect("按文件名未找到素材");

    assert_eq!(asset.kind, AssetKind::Image);
    assert!(
        asset.png_bytes.is_empty(),
        "图片物化不得读盘内联 PNG 字节（D41：交给协商回落 files）"
    );
    assert!(
        asset.source_path.is_absolute() && asset.source_path.is_file(),
        "派生存在与否都不影响 HDROP 承载路径，仍须绝对且存在"
    );
    cleanup(&root);
}

/// PNG 原图素材同样零读盘：文件引用本身就是合法且更低成本的承载（D22 实测：
/// files 路径对端只取缩略图，PNG 内联则要全量解码）。
#[test]
fn png_original_materialization_also_stays_zero_io() {
    let root = scaffold_relative_library("payload-png-zero-io", "big.png", b"RAW-PNG-BYTES");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let asset = resolver
        .materialize_by_file_name("big.png")
        .expect("物化过程报错")
        .expect("按文件名未找到素材");

    assert_eq!(asset.kind, AssetKind::Image);
    assert!(
        asset.png_bytes.is_empty(),
        "PNG 原图不得内联字节，一律交文件引用（D41）"
    );
    assert!(
        asset.source_path.is_absolute() && asset.source_path.is_file(),
        "PNG 素材的 HDROP 路径必须绝对且存在，实际 {}",
        asset.source_path.display()
    );
    cleanup(&root);
}

/// 在相对路径下搭一个含 N 个 PNG 素材的最小库（uuid/文件名按序号生成）。
fn scaffold_many(tag: &str, count: usize) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建立库根目录失败");
    let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
    for i in 0..count {
        let uuid = format!("0000-object-{i:02}");
        let file_name = format!("img-{i:02}.png");
        let object_dir = root.join("objects").join(&uuid);
        fs::create_dir_all(&object_dir).expect("建立对象目录失败");
        fs::write(object_dir.join(&file_name), format!("PNG-{i}").as_bytes())
            .expect("写入素材字节失败");
        let rel_path = format!("objects/{uuid}/{file_name}");
        store
            .upsert_asset(&AssetMeta {
                uuid,
                file_name,
                rel_path,
                category: None,
                tags: vec![],
                size_bytes: 0,
                created_at: 1,
                imported_at: 1,
                phash: None,
                width: None,
                height: None,
            })
            .expect("写入资产元数据失败");
    }
    assert!(
        root.is_relative(),
        "本测试的前提是 root 为相对路径，实际 {}",
        root.display()
    );
    root
}

/// 重复物化命中 LRU 缓存：同一素材不重复查库，两次拿到同一 Arc 实例。
#[test]
fn repeat_materialize_serves_from_cache() {
    let root = scaffold_relative_library("payload-cache-hit", "big.png", b"PNG-1");
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let first = resolver
        .materialize(AssetId(0))
        .expect("物化过程报错")
        .expect("未找到素材");
    let second = resolver
        .materialize(AssetId(0))
        .expect("物化过程报错")
        .expect("未找到素材");

    assert!(
        Arc::ptr_eq(&first, &second),
        "重复物化必须命中缓存返回同一实例（不重复读盘）"
    );
    cleanup(&root);
}

/// LRU 淘汰：素材数超过缓存条目上限后，最早物化的条目被逐出，再次物化重新读盘。
#[test]
fn cache_evicts_oldest_when_full() {
    let root = scaffold_many("payload-cache-evict", 5);
    let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
    let first = resolver
        .materialize(AssetId(0))
        .expect("物化过程报错")
        .expect("未找到素材");
    for i in 1..5 {
        resolver
            .materialize(AssetId(i))
            .expect("物化过程报错")
            .expect("未找到素材");
    }
    let again = resolver
        .materialize(AssetId(0))
        .expect("物化过程报错")
        .expect("未找到素材");

    assert!(
        !Arc::ptr_eq(&first, &again),
        "缓存满后最旧条目必须被逐出（再次物化重新读盘得到新实例）"
    );
    cleanup(&root);
}
