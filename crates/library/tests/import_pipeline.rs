use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use image::DynamicImage;
use library::{
    CopyState, EnqueueOutcome, ImportRequest, ImportTicket, Library, MediaKind, RecordingDispatcher,
};

fn make_png(dir: &Path, name: &str, gray: u8) -> PathBuf {
    let path = dir.join(name);
    // 结构化渐变而非纯色：近纯色图 pHash 不可信（D65 低信息守卫），
    // 涉及 phash 断言的 fixture 必须有真实结构。
    let img = image::GrayImage::from_fn(32, 32, |x, y| {
        let v = gray
            .saturating_add((x as u8).saturating_mul(3))
            .saturating_add(y as u8 / 2);
        image::Luma([v])
    });
    DynamicImage::ImageLuma8(img)
        .save(&path)
        .expect("写测试 PNG 失败");
    path
}

fn make_image(dir: &Path, name: &str, gray: u8) -> PathBuf {
    let path = dir.join(name);
    // RGB 而非 Luma8：GIF 编码器不接受灰度，webp/bmp 用 RGB 亦无碍。
    // 结构化渐变：理由同 make_png（D65 低信息守卫）。
    let img = image::RgbImage::from_fn(32, 32, |x, y| {
        let v = gray
            .saturating_add((x as u8).saturating_mul(3))
            .saturating_add(y as u8 / 2);
        image::Rgb([v, v, v])
    });
    DynamicImage::ImageRgb8(img)
        .save(&path)
        .expect("写测试图片失败");
    path
}

fn wait_for(lib: &Library, ticket: &ImportTicket, pred: impl Fn(&CopyState) -> bool) -> CopyState {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(state) = lib.state_of(ticket) {
            if pred(&state) {
                return state;
            }
        }
        assert!(
            Instant::now() <= deadline,
            "等待状态超时，当前 {:?}",
            lib.state_of(ticket)
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn expect_ticket(outcome: EnqueueOutcome) -> ImportTicket {
    match outcome {
        EnqueueOutcome::Ticket { ticket, .. } => ticket,
        other => panic!("应受理导入，实际 {other:?}"),
    }
}

#[test]
fn import_copies_file_into_library_layout() {
    let dir = tempfile::tempdir().unwrap();
    let source = make_png(dir.path(), "photo.png", 90);
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    let t = expect_ticket(
        lib.enqueue(ImportRequest {
            source: source.clone(),
            category: Some("产品图".into()),
            tags: vec!["红".into()],
        })
        .unwrap(),
    );
    wait_for(&lib, &t, |s| matches!(s, CopyState::Done));

    let meta = lib.store().get_asset(&t.uuid).unwrap().unwrap();
    assert_eq!(meta.category.as_deref(), Some("产品图"));
    assert_eq!(meta.file_name, "photo.png");
    assert!(meta.phash.is_some());
    let expected_rel = format!("objects/{}/raw.png", t.uuid);
    assert_eq!(meta.rel_path, expected_rel);
    assert!(lib_dir.join(&expected_rel).exists(), "库内文件应存在");
}

#[test]
fn duplicate_byte_identical_image_rejected_no_second_copy() {
    // D65：图片重复判定收归 SHA-256 字节等值（pHash 不再判死）。同一文件
    // 重导入 → Duplicate，objects 下只留一份。
    let dir = tempfile::tempdir().unwrap();
    let source = make_png(dir.path(), "same.png", 120);
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    let t1 = expect_ticket(
        lib.enqueue(ImportRequest {
            source: source.clone(),
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t1, |s| matches!(s, CopyState::Done));

    match lib
        .enqueue(ImportRequest {
            source,
            category: None,
            tags: vec![],
        })
        .unwrap()
    {
        EnqueueOutcome::Duplicate { existing_uuid } => assert_eq!(existing_uuid, t1.uuid),
        other => panic!("应判定重复，实际 {other:?}"),
    }

    let objects = lib_dir.join("objects");
    assert_eq!(
        std::fs::read_dir(objects).unwrap().count(),
        1,
        "去重后 objects 下只能有一份资产"
    );
    assert_eq!(lib.store().all_assets_count().unwrap(), 1);
}

#[test]
fn async_copy_metadata_visible_before_done() {
    let dir = tempfile::tempdir().unwrap();
    let big_source = make_png(dir.path(), "big.png", 77);
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();
    lib.set_paused(true);

    let t = expect_ticket(
        lib.enqueue(ImportRequest {
            source: big_source,
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    assert!(
        lib.store().get_asset(&t.uuid).unwrap().is_some(),
        "元数据必须在拷贝开始前即可查（D7 体感瞬时入库）"
    );

    lib.set_paused(false);
    wait_for(&lib, &t, |s| matches!(s, CopyState::Done));
}

#[test]
fn copy_queue_respects_backpressure_cap() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open_with_capacity(&lib_dir, 1).unwrap();
    lib.set_paused(true);

    let src0 = make_png(dir.path(), "bp0.png", 60);
    let outcome = lib
        .enqueue(ImportRequest {
            source: src0,
            category: None,
            tags: vec![],
        })
        .unwrap();
    assert!(matches!(outcome, EnqueueOutcome::Ticket { .. }));

    let src1 = make_png(dir.path(), "bp1.png", 61);
    let outcome = lib
        .enqueue(ImportRequest {
            source: src1,
            category: None,
            tags: vec![],
        })
        .unwrap();
    assert!(
        matches!(outcome, EnqueueOutcome::Backpressure),
        "cap=1 且任务未消费时必须背压拒绝，实际 {outcome:?}"
    );

    lib.set_paused(false);
}

#[test]
fn manual_category_and_inbox_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    let manual_src = make_png(dir.path(), "manual.png", 30);
    let t1 = expect_ticket(
        lib.enqueue(ImportRequest {
            source: manual_src,
            category: Some("表情包".into()),
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t1, |s| matches!(s, CopyState::Done));
    assert_eq!(
        lib.store().get_asset(&t1.uuid).unwrap().unwrap().category,
        Some("表情包".into())
    );

    let inbox_src = make_png(dir.path(), "inbox.png", 31);
    let t2 = expect_ticket(
        lib.enqueue(ImportRequest {
            source: inbox_src,
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t2, |s| matches!(s, CopyState::Done));
    assert_eq!(
        lib.store().get_asset(&t2.uuid).unwrap().unwrap().category,
        Some(library::INBOX_CATEGORY.to_string())
    );
}

#[test]
fn misnamed_png_with_jpg_extension_imports_via_content_sniffing() {
    // 回归：PNG 内容挂 .jpg 名，旧实现按扩展名走 JPEG 解码器，
    // 报「Format error decoding Jpeg: Illegal start bytes: 89504e47…」
    // 并让整批导入失败。现在按内容嗅探格式，应正常入库且带 phash。
    let dir = tempfile::tempdir().unwrap();
    let png_bytes = {
        let img = image::GrayImage::from_fn(32, 32, |x, y| {
            // 结构化渐变：近纯色图 pHash 不可信（D65），phash 断言需要真实结构。
            image::Luma([(64 + x * 2 + y / 2).min(255) as u8])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    };
    let source = dir.path().join("伪装图.jpg");
    std::fs::write(&source, &png_bytes).unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    let t = expect_ticket(
        lib.enqueue(ImportRequest {
            source,
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t, |s| matches!(s, CopyState::Done));
    let meta = lib.store().get_asset(&t.uuid).unwrap().unwrap();
    assert!(meta.phash.is_some(), "伪装扩展名不影响 phash 计算");
}

#[test]
fn undecodable_image_is_rejected_without_touching_library() {
    // 回归：损坏图片曾是 `?` 硬错误，整批导入中止（sample-library failed）。
    // 现在返回 Unsupported：无入库数据、无残留对象目录，批内其他素材不受影响。
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("broken.jpg");
    std::fs::write(&bad, b"\x00not-an-image-payload\xff").unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    let outcome = lib
        .enqueue(ImportRequest {
            source: bad,
            category: None,
            tags: vec![],
        })
        .unwrap();
    match outcome {
        EnqueueOutcome::Unsupported { reason } => {
            assert!(!reason.is_empty(), "失败原因不应为空");
        }
        other => panic!("损坏图片应返回 Unsupported，实际 {other:?}"),
    }
    assert_eq!(lib.store().all_assets_count().unwrap(), 0);
    assert_eq!(
        std::fs::read_dir(lib_dir.join("objects")).unwrap().count(),
        0,
        "被拒素材不得留下半成品对象目录"
    );
}

#[test]
fn registry_image_formats_webp_gif_bmp_are_decodable() {
    // 回归：media 注册表把 webp/gif/bmp 标为可导入图片，但 library 的 image
    // 依赖只开了 png/jpeg feature——扩展名正确的 webp 也会 Unsupported。
    // features 补齐后三种注册格式都必须能走完解码→phash→入库。
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    for (name, gray) in [("pic.webp", 40u8), ("ani.gif", 150), ("icon.bmp", 220)] {
        let source = make_image(dir.path(), name, gray);
        let t = expect_ticket(
            lib.enqueue(ImportRequest {
                source,
                category: None,
                tags: vec![],
            })
            .unwrap(),
        );
        wait_for(&lib, &t, |s| matches!(s, CopyState::Done));
        let meta = lib.store().get_asset(&t.uuid).unwrap().unwrap();
        assert!(meta.phash.is_some(), "{name} 应算出 phash");
    }
    assert_eq!(lib.store().all_assets_count().unwrap(), 3);
}

#[test]
fn video_import_dispatches_media_job() {
    let dir = tempfile::tempdir().unwrap();
    let fake_mp4 = dir.path().join("clip.mp4");
    std::fs::write(&fake_mp4, b"\x00\x00\x00\x18ftypmp42").unwrap();

    let recorder = RecordingDispatcher::new();
    let lib_dir = dir.path().join("library");
    let lib = Library::open_with_dispatcher(&lib_dir, 16, Box::new(recorder.clone())).unwrap();

    let t = expect_ticket(
        lib.enqueue(ImportRequest {
            source: fake_mp4.clone(),
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t, |s| !matches!(s, CopyState::Pending));

    let jobs = recorder.jobs();
    assert_eq!(
        jobs.len(),
        1,
        "视频导入必须派发 media job（红线：UI 进程不解码）"
    );
    assert_eq!(jobs[0].uuid, t.uuid);
    assert_eq!(jobs[0].kind, MediaKind::Video);
}

// ---------------------------------------------------------------------------
// D61 非图片内容等值去重：视频/文本此前重复导入即双份占盘（pHash 只覆盖
// 图片）。契约：逐字节相同的非图片素材第二份判 Duplicate；同尺寸不同内容
// 不得误杀；摘要落库供跨批次复用。
// ---------------------------------------------------------------------------

#[test]
fn video_content_hash_dedups_identical_and_keeps_different() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    let payload = b"\x00\x00\x00\x18ftypmp42same-video-payload";
    let source_a = dir.path().join("a1.mp4");
    std::fs::write(&source_a, payload).unwrap();
    let source_a2 = dir.path().join("a2.mp4");
    std::fs::write(&source_a2, payload).unwrap();
    // 同尺寸不同内容：预过滤命中、摘要不同——不得误判重复。
    let mut other_payload = payload.to_vec();
    other_payload[20] ^= 0xFF;
    let source_b = dir.path().join("b.mp4");
    std::fs::write(&source_b, &other_payload).unwrap();

    let t1 = expect_ticket(
        lib.enqueue(ImportRequest {
            source: source_a,
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t1, |s| matches!(s, CopyState::Done));

    match lib
        .enqueue(ImportRequest {
            source: source_a2,
            category: None,
            tags: vec![],
        })
        .unwrap()
    {
        EnqueueOutcome::Duplicate { existing_uuid } => assert_eq!(existing_uuid, t1.uuid),
        other => panic!("逐字节相同的视频应判重复，实际 {other:?}"),
    }

    let t2 = expect_ticket(
        lib.enqueue(ImportRequest {
            source: source_b,
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t2, |s| matches!(s, CopyState::Done));

    let meta1 = lib.store().get_asset(&t1.uuid).unwrap().unwrap();
    assert!(meta1.phash.is_none(), "视频不走 pHash");
    assert_eq!(
        meta1.content_hash.as_deref().map(<[u8]>::len),
        Some(32),
        "视频必须落 SHA-256 摘要"
    );
    assert_eq!(
        lib.store().all_assets_count().unwrap(),
        2,
        "同尺寸不同内容要保留"
    );
    assert_eq!(
        std::fs::read_dir(lib_dir.join("objects")).unwrap().count(),
        2,
        "去重后不得留第二份资产目录"
    );
}

#[test]
fn text_content_hash_dedups_after_utf8_normalization() {
    let dir = tempfile::tempdir().unwrap();
    let lib_dir = dir.path().join("library");
    let lib = Library::open(&lib_dir).unwrap();

    // 同一份 GBK 字节写两个名字：入库文本 = 归一化 UTF-8，两份归一化结果
    // 相同 → 第二份判重复；尺寸按归一化字节计（D60 库内文本不变量）。
    let gbk = [0xC4u8, 0xE3, 0xBA, 0xC3, 0x2E, 0x74, 0x78, 0x74];
    let source_1 = dir.path().join("t1.txt");
    std::fs::write(&source_1, gbk).unwrap();
    let source_2 = dir.path().join("t2.txt");
    std::fs::write(&source_2, gbk).unwrap();

    let t1 = expect_ticket(
        lib.enqueue(ImportRequest {
            source: source_1,
            category: None,
            tags: vec![],
        })
        .unwrap(),
    );
    wait_for(&lib, &t1, |s| matches!(s, CopyState::Done));

    match lib
        .enqueue(ImportRequest {
            source: source_2,
            category: None,
            tags: vec![],
        })
        .unwrap()
    {
        EnqueueOutcome::Duplicate { existing_uuid } => assert_eq!(existing_uuid, t1.uuid),
        other => panic!("归一化后相同的文本应判重复，实际 {other:?}"),
    }
    assert_eq!(lib.store().all_assets_count().unwrap(), 1);
}
