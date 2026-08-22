//! 红灯测试 1：`synthetic_library_generator_produces_100k_metadata_rows`
//! （PRD 需求 1，常规测试不 ignore）。
//!
//! 断言：行数 == 100k；字段契约（uuid/file_name/created_at 抽查）；
//! 确定性（两次生成 DB 行集全等 + 缩略图逐字节相等）；缩略图文件数与
//! thumbnail_cache_path 路径形态。

use bench_harness::generate::{generate_library, uuid_of, BASE_EPOCH_SECS};
use store::Store;

const ROWS: u64 = 100_000;
const THUMBS: usize = 100;
/// 全等抽查步长：互质的素数步遍历覆盖头/中/尾且避免与任何周期性模式共振。
const STRIDE: u64 = 997;

#[test]
fn synthetic_library_generator_produces_100k_metadata_rows() {
    let first = tempfile::tempdir().expect("tempdir 失败");
    let lib_a = first.path().join("lib");
    generate_library(&lib_a, ROWS, THUMBS).expect("首次生成失败");

    // 行数（重开 Store 读回，验证真实落库）
    let store = Store::open(&lib_a.join("meta.db")).expect("重开 Store 失败");
    assert_eq!(
        store.all_assets_count().expect("计数失败"),
        ROWS as i64,
        "assets 表行数必须等于 {ROWS}"
    );

    // 字段契约抽查（首/尾 + 步长采样）
    for i in stride_indices(ROWS) {
        let meta = store
            .get_asset(&uuid_of(i))
            .expect("查询失败")
            .unwrap_or_else(|| panic!("缺少行 {}", uuid_of(i)));
        assert_eq!(meta.uuid, uuid_of(i));
        assert_eq!(meta.file_name, format!("asset_{i}.png"));
        assert_eq!(
            meta.created_at,
            BASE_EPOCH_SECS + i as i64,
            "created_at 必须是基准+{i}（确定性红线）"
        );
        let expected_phash = i.to_be_bytes();
        assert_eq!(meta.phash.as_deref(), Some(expected_phash.as_slice()));
    }

    // 确定性：第二次独立生成的行集全等（步长全字段比对）+ 计数相等
    let second = tempfile::tempdir().expect("tempdir 失败");
    let lib_b = second.path().join("lib");
    generate_library(&lib_b, ROWS, THUMBS).expect("二次生成失败");

    let store_b = Store::open(&lib_b.join("meta.db")).expect("重开 Store(b) 失败");
    assert_eq!(
        store_b.all_assets_count().expect("计数失败"),
        store.all_assets_count().expect("计数失败")
    );
    for i in stride_indices(ROWS) {
        let a = store.get_asset(&uuid_of(i)).expect("查询失败").unwrap();
        let b = store_b.get_asset(&uuid_of(i)).expect("查询失败").unwrap();
        assert_eq!(a.uuid, b.uuid);
        assert_eq!(a.file_name, b.file_name);
        assert_eq!(a.rel_path, b.rel_path);
        assert_eq!(a.size_bytes, b.size_bytes);
        assert_eq!(a.created_at, b.created_at);
        assert_eq!(a.imported_at, b.imported_at);
        assert_eq!(a.phash, b.phash);
    }
    drop(store);
    drop(store_b);

    // 缩略图子集：文件数 == 100 且路径符合 thumbnail_cache_path 形态（thumbs/b/be/<uuid>.png）
    let mut thumb_files = 0usize;
    for i in 0..THUMBS as u64 {
        let rel = Store::thumbnail_cache_path(&uuid_of(i), "png");
        let path_a = lib_a.join(&rel);
        assert!(path_a.is_file(), "缩略图缺失: {}", path_a.display());
        thumb_files += 1;
        // 编码输出逐字节可复现（确定性红线在编码器层的体现）
        let bytes_a = std::fs::read(&path_a).expect("读缩略图(a) 失败");
        let bytes_b = std::fs::read(lib_b.join(&rel)).expect("读缩略图(b) 失败");
        assert_eq!(bytes_a, bytes_b, "第 {i} 张缩略图两次生成不一致");
        assert!(
            bytes_a.starts_with(&[0x89, b'P', b'N', b'G']),
            "占位图必须是 PNG"
        );
    }
    assert_eq!(thumb_files, THUMBS);

    // 缩略图子集化边界：THUMBS 之后不应有文件落盘
    let beyond = lib_a.join(Store::thumbnail_cache_path(&uuid_of(THUMBS as u64), "png"));
    assert!(
        !beyond.exists(),
        "子集之外不应生成缩略图: {}",
        beyond.display()
    );
}

fn stride_indices(rows: u64) -> impl Iterator<Item = u64> {
    (0..rows).step_by(STRIDE as usize)
}
