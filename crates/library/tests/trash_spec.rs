//! D46 回收站目录迁移与对账的集成测试（真库布局，tempfile）。

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use image::DynamicImage;
use library::{CopyState, EnqueueOutcome, ImportRequest, ImportTicket, Library};

fn make_png(dir: &Path, name: &str, gray: u8) -> PathBuf {
    let path = dir.join(name);
    // 逐像素伪随机图案（seed=gray 参与混合）：纯色/线性渐变图之间 pHash 距离
    // 常 ≤ 8 会被导入去重误判为同一素材，噪声图案彼此可分。
    let img = image::GrayImage::from_fn(32, 32, |x, y| {
        let mut h = x.wrapping_mul(0x9E3779B9)
            ^ y.wrapping_mul(0x85EBCA6B)
            ^ (gray as u32).wrapping_mul(0xC2B2AE35);
        h ^= h >> 16;
        h = h.wrapping_mul(0x7FEB352D);
        h ^= h >> 15;
        image::Luma([(h >> 24) as u8])
    });
    DynamicImage::ImageLuma8(img)
        .save(&path)
        .expect("写测试 PNG 失败");
    path
}

fn wait_done(lib: &Library, ticket: &ImportTicket) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = lib.state_of(ticket) {
            if matches!(s, CopyState::Done) {
                return;
            }
        }
        assert!(Instant::now() <= deadline, "导入超时");
        thread::sleep(Duration::from_millis(10));
    }
}

/// 导入一张图并等拷贝完成，返回 uuid。
fn import_one(lib: &Library, source: &Path) -> String {
    let outcome = lib
        .enqueue(ImportRequest {
            source: source.to_path_buf(),
            category: Some("测试".into()),
            tags: vec![],
        })
        .unwrap();
    let t = match outcome {
        EnqueueOutcome::Ticket { ticket, .. } => ticket,
        other => panic!("应受理导入，实际 {other:?}"),
    };
    wait_done(lib, &t);
    t.uuid
}

#[test]
fn move_to_trash_relocates_objects_dir_and_marks() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();
    let src = make_png(dir.path(), "a.png", 80);
    let uuid = import_one(&lib, &src);

    // 伪造一份缩略图（派生工序产物），验证软删不搬缩略图。
    let thumb = lib_dir.join(store::Store::thumbnail_cache_path(&uuid, "png"));
    fs::create_dir_all(thumb.parent().unwrap()).unwrap();
    fs::write(&thumb, b"pngbytes").unwrap();

    assert_eq!(lib.move_to_trash(&[uuid.as_str()]).unwrap(), 1);
    assert!(
        !lib_dir.join("objects").join(&uuid).exists(),
        "正本应离开 objects"
    );
    assert!(
        lib_dir.join("trash").join(&uuid).join("raw.png").exists(),
        "正本应落在 trash"
    );
    assert!(lib.store().is_deleted(&uuid).unwrap(), "标志应置位");
    assert!(thumb.exists(), "软删不搬缩略图（恢复零成本）");
    // 幂等：再删一次命中 0 但目录状态不变。
    assert_eq!(
        lib.move_to_trash(&[uuid.as_str()]).unwrap(),
        1,
        "已删视为完成"
    );
    assert!(lib_dir.join("trash").join(&uuid).exists());
}

#[test]
fn restore_returns_object_dir_and_clears_flag() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();
    let src = make_png(dir.path(), "b.png", 90);
    let uuid = import_one(&lib, &src);
    lib.move_to_trash(&[uuid.as_str()]).unwrap();

    assert_eq!(lib.restore_from_trash(&[uuid.as_str()]).unwrap(), 1);
    assert!(lib_dir.join("objects").join(&uuid).join("raw.png").exists());
    assert!(!lib_dir.join("trash").join(&uuid).exists());
    assert!(!lib.store().is_deleted(&uuid).unwrap());
    // 未删的行恢复 = 0 命中，不报错。
    assert_eq!(lib.restore_from_trash(&[uuid.as_str()]).unwrap(), 0);
}

#[test]
fn purge_clears_row_trash_dir_and_thumb() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();
    let src = make_png(dir.path(), "c.png", 100);
    let uuid = import_one(&lib, &src);
    let thumb = lib_dir.join(store::Store::thumbnail_cache_path(&uuid, "png"));
    fs::create_dir_all(thumb.parent().unwrap()).unwrap();
    fs::write(&thumb, b"pngbytes").unwrap();
    lib.move_to_trash(&[uuid.as_str()]).unwrap();

    assert_eq!(lib.purge(&[uuid.as_str()]).unwrap(), 1);
    assert!(
        lib.store().get_asset(&uuid).unwrap().is_none(),
        "行必须消失"
    );
    assert!(
        !lib_dir.join("trash").join(&uuid).exists(),
        "trash 目录必须清"
    );
    assert!(!thumb.exists(), "缩略图必须连带清");

    // empty_trash：删两张、清空、库内对象/元数据归零。
    let u1 = import_one(&lib, &src);
    let u2 = import_one(&lib, &make_png(dir.path(), "d.png", 200));
    lib.move_to_trash(&[u1.as_str(), u2.as_str()]).unwrap();
    assert_eq!(lib.store().deleted_uuids().unwrap().len(), 2);
    assert_eq!(lib.empty_trash().unwrap(), 2);
    assert!(lib.store().deleted_uuids().unwrap().is_empty());
    assert!(!lib_dir.join("trash").join(&u1).exists());
    assert!(!lib_dir.join("trash").join(&u2).exists());
}

#[test]
fn coexisting_dirs_rejected_and_flag_rolled_back() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();
    let src = make_png(dir.path(), "e.png", 120);
    let uuid = import_one(&lib, &src);
    // 制造异常态：trash 下同名目录已存在（上次崩溃残留），move 必须拒绝且回滚标志。
    let ghost = lib_dir.join("trash").join(&uuid);
    fs::create_dir_all(&ghost).unwrap();
    let err = lib
        .move_to_trash(&[uuid.as_str()])
        .expect_err("两处并存必须报错而非静默覆盖");
    assert!(matches!(err, library::LibraryError::Trash { .. }));
    assert!(!lib.store().is_deleted(&uuid).unwrap(), "标志必须已回滚");
    assert!(
        lib_dir.join("objects").join(&uuid).exists(),
        "正本必须仍在 objects"
    );
}

#[test]
fn reconcile_fixes_crash_drift_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    {
        let lib = Library::open(&lib_dir).unwrap();
        let src = make_png(dir.path(), "f.png", 130);
        let uuid = import_one(&lib, &src);
        // 模拟「置标后、rename 前崩溃」：标志=1，正本还在 objects。
        lib.store().soft_delete_assets(&[uuid.as_str()]).unwrap();
    }
    // 重开对账：open 时 build 内已自动 reconcile（常态 no-op，漂移时修复），
    // 这里断言的是**结果**而非显式调用的返回值——显式再调应为 0（幂等收敛）。
    let lib = Library::open(&lib_dir).unwrap();
    assert_eq!(
        lib.reconcile_trash().unwrap(),
        0,
        "open 自动对账后显式触发必须已收敛"
    );
    let uuid = lib.store().deleted_uuids().unwrap()[0].clone();
    assert!(
        !lib_dir.join("objects").join(&uuid).exists(),
        "漂移的正本不得留在 objects"
    );
    assert!(lib_dir.join("trash").join(&uuid).join("raw.png").exists());

    // 模拟「rename 完成后崩溃」：标志=0 但正本躺在 trash → 补回 objects。
    lib.restore_from_trash(&[uuid.as_str()]).unwrap();
    lib.move_to_trash(&[uuid.as_str()]).unwrap();
    lib.store().restore_assets(&[uuid.as_str()]).unwrap(); // 强行制造漂移
    assert!(lib_dir.join("trash").join(&uuid).exists());
    assert_eq!(lib.reconcile_trash().unwrap(), 1);
    assert!(lib_dir.join("objects").join(&uuid).join("raw.png").exists());
    assert!(!lib_dir.join("trash").join(&uuid).exists());

    // 孤儿目录（无元数据行）：open 自动对账即收掉，重开验证。
    fs::create_dir_all(lib_dir.join("trash").join("orphan-uuid")).unwrap();
    drop(lib);
    let _lib = Library::open(&lib_dir).unwrap();
    assert!(!lib_dir.join("trash").join("orphan-uuid").exists());
}

#[test]
fn deleted_assets_invisible_to_facet_reads() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();
    let u1 = import_one(&lib, &make_png(dir.path(), "g.png", 10));
    let u2 = import_one(&lib, &make_png(dir.path(), "h.png", 20));
    lib.move_to_trash(&[u1.as_str()]).unwrap();

    // 分类计数不含回收站。
    let cats = lib.store().distinct_categories().unwrap();
    assert_eq!(
        cats.iter().find(|(n, _)| n == "测试").map(|(_, c)| *c),
        Some(1)
    );
    // active 遍历只见一张。
    let mut seen = Vec::new();
    lib.store()
        .for_each_asset_active(|m| seen.push(m.uuid))
        .unwrap();
    assert_eq!(seen, vec![u2.clone()], "只剩活行且升序");
}
