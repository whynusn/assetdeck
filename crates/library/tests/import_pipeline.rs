use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use image::DynamicImage;
use library::{
    CopyState, EnqueueOutcome, ImportRequest, ImportTicket, Library, MediaKind, RecordingDispatcher,
};

fn make_png(dir: &Path, name: &str, gray: u8) -> PathBuf {
    let path = dir.join(name);
    let img = image::GrayImage::from_fn(32, 32, |_x, _y| image::Luma([gray]));
    DynamicImage::ImageLuma8(img)
        .save(&path)
        .expect("写测试 PNG 失败");
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
        EnqueueOutcome::Ticket(t) => t,
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
fn duplicate_phash_rejected_no_second_copy() {
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
    assert!(matches!(outcome, EnqueueOutcome::Ticket(_)));

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
