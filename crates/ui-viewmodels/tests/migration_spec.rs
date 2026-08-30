//! D61 分类保留：模拟 v0.1.0 用户迁移全流程。
//!
//! 造一个 v0.1.0 时期形态的旧库（schema v4：完整建表 + tags + FTS5 触发器 +
//! idx_phash + deleted 列，user_version=4），里面带着真实分类。走生产路径——
//! detect → rename_to_backup → read_legacy_categories → write_import_manifest
//! → 逐行按 D49 清单格式解析（镜像 sample-library 的 apply_directive）→
//! Library::enqueue 落库——断言新旧库分类一致。这覆盖了「单测全绿但组合序
//! 失败」的盲区（前一轮清单/改名序缺陷正是这类测试暴露的范式）。

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use image::DynamicImage;
use library::{CopyState, EnqueueOutcome, ImportRequest, Library};
use rusqlite::Connection;
use ui_viewmodels::legacy_migration::{
    detect_legacy_library, rename_to_backup, write_import_manifest,
};

/// v0.1.0 schema v4 的完整 DDL（照抄 v0.1.0 tag 快照的 MIGRATION_V1 + v2/v3/v4
/// ALTER；含 FTS5 + 触发器 + tags 表 + idx_phash + deleted，正是真机旧库形态）。
const V010_DDL: &str = "
CREATE TABLE IF NOT EXISTS assets (
  uuid        TEXT PRIMARY KEY NOT NULL,
  file_name   TEXT NOT NULL,
  rel_path    TEXT NOT NULL DEFAULT '',
  category    TEXT,
  size_bytes  INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL DEFAULT 0,
  imported_at INTEGER NOT NULL DEFAULT 0,
  phash       BLOB,
  width       INTEGER,
  height      INTEGER,
  deleted     INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS tags (
  asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
  tag        TEXT NOT NULL,
  PRIMARY KEY (asset_uuid, tag)
);
CREATE VIRTUAL TABLE IF NOT EXISTS assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS assets_fts_ai AFTER INSERT ON assets BEGIN
  INSERT INTO assets_fts(uuid, name) VALUES (new.uuid, new.file_name);
END;
CREATE TRIGGER IF NOT EXISTS assets_fts_au AFTER UPDATE OF file_name ON assets BEGIN
  DELETE FROM assets_fts WHERE uuid = old.uuid;
  INSERT INTO assets_fts(uuid, name) VALUES (new.uuid, new.file_name);
END;
CREATE TRIGGER IF NOT EXISTS assets_fts_ad AFTER DELETE ON assets BEGIN
  DELETE FROM assets_fts WHERE uuid = old.uuid;
END;
CREATE INDEX IF NOT EXISTS idx_assets_phash ON assets(phash);
";

struct LegacyAsset<'a> {
    uuid: &'a str,
    file_name: &'a str,
    rel_path: &'a str,
    category: Option<&'a str>,
    bytes: Vec<u8>,
}

/// 造一张视觉上唯一的灰度 PNG（梯度模式随 seed 变化，保证 pHash 互不命中）。
fn png_bytes(seed: u8) -> Vec<u8> {
    let img = image::GrayImage::from_fn(48, 48, |x, y| {
        image::Luma([((x * (u32::from(seed) + 1) + y * (u32::from(seed) + 3)) % 256) as u8])
    });
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut buf,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Up,
    );
    DynamicImage::ImageLuma8(img)
        .write_with_encoder(encoder)
        .expect("编码 PNG 失败");
    buf
}

/// 造一个 v0.1.0 旧库：完整 v4 schema + 带分类的行 + 对应 objects/<uuid>/raw.* 文件。
/// 返回旧库根目录。
fn make_v010_library(root: &Path, assets: &[LegacyAsset<'_>]) {
    std::fs::create_dir_all(root).unwrap();
    let objects = root.join("objects");
    let db_path = root.join("meta.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(V010_DDL).unwrap();
        for a in assets {
            let object_dir = objects.join(a.uuid);
            std::fs::create_dir_all(&object_dir).unwrap();
            std::fs::write(object_dir.join(file_name_of(a.rel_path)), &a.bytes).unwrap();
            conn.execute(
                "INSERT INTO assets (uuid, file_name, rel_path, category, size_bytes) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    a.uuid,
                    a.file_name,
                    a.rel_path,
                    a.category,
                    a.bytes.len() as i64,
                ],
            )
            .unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
    }
    std::fs::create_dir_all(root.join("thumbs")).unwrap();
}

/// `rel_path` 形如 `objects/<uuid>/raw.<ext>`，取末段（磁盘文件名）。
fn file_name_of(rel_path: &str) -> String {
    Path::new(rel_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "raw".to_string())
}

/// 读清单里某路径对应的磁盘绝对路径 → 取其 raw 文件名。
fn raw_file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 镜像 sample-library::import_paths::parse_import_paths + apply_directive：把清单
/// 一行解析成 (源路径, category)。auto → None；category:<名> → Some(名)。
fn parse_manifest_line(line: &str) -> (PathBuf, Option<String>) {
    let mut fields = line.splitn(3, '\t');
    let _kind = fields.next().unwrap_or_default();
    let mode = fields.next().unwrap_or_default();
    let path = PathBuf::from(fields.next().unwrap_or_default());
    let category = mode.strip_prefix("category:").map(str::to_string);
    (path, category)
}

fn wait_for(
    lib: &Library,
    ticket: &library::ImportTicket,
    pred: impl Fn(&CopyState) -> bool,
) -> CopyState {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(state) = lib.state_of(ticket) {
            if pred(&state) {
                return state;
            }
        }
        assert!(Instant::now() <= deadline, "等待状态超时");
        thread::sleep(Duration::from_millis(10));
    }
}

/// 走完整迁移链路（除了真正 spawn sample-library.exe——用 Library::enqueue 复刻它
/// 每素材的调用），返回新库以便断言。
fn migrate_legacy(legacy_root: &Path, unified_root: &Path) -> Library {
    let exe_dir = legacy_root.parent().expect("legacy 在 exe 子目录");
    std::fs::create_dir_all(unified_root).unwrap();

    // 1. 检测（current_root = 新库根，避免把自己当旧库）
    let detected = detect_legacy_library(exe_dir, unified_root).expect("应检测到旧库");
    assert!(!detected.is_backup);
    // 2. 改名先行
    let backup = rename_to_backup(&detected.source, exe_dir).expect("改名留档");
    // 3. 读旧库分类（best-effort；这里旧库可读）
    let categories =
        ui_viewmodels::legacy_migration::read_legacy_categories(&backup.join("meta.db"))
            .expect("应读到旧库分类");
    // 4. 清单从备份路径生成、携带分类指令
    let list = unified_root.join("manifest.tsv");
    let count = write_import_manifest(&backup, &list, &categories).expect("写清单");
    assert!(count > 0);

    // 5. 逐行落库（镜像 sample-library 的 apply_directive + enqueue）
    let lib = Library::open(unified_root).unwrap();
    let text = std::fs::read_to_string(&list).unwrap();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (source, category) = parse_manifest_line(line);
        let outcome = lib
            .enqueue(ImportRequest {
                source,
                category,
                tags: Vec::new(),
            })
            .expect("enqueue 应受理");
        match outcome {
            EnqueueOutcome::Ticket(t) => {
                wait_for(&lib, &t, |s| {
                    matches!(s, CopyState::Done | CopyState::Failed(_))
                });
            }
            // 重复内容 → 库内已有，不创建新行（语义与生产 sample-library 一致）。
            EnqueueOutcome::Duplicate { .. } | EnqueueOutcome::Backpressure => {}
            EnqueueOutcome::Unsupported { .. } => {}
        }
    }
    lib
}

/// 旧库里的分类在新库里全部保留（auto/None 的落待分类）。
#[test]
fn migration_preserves_all_legacy_categories() {
    let base = tempfile::tempdir().unwrap();
    let legacy = base.path().join("exe").join("library");
    let unified = base.path().join("unified");

    let assets = vec![
        LegacyAsset {
            uuid: "a1111111-1111-1111-1111-111111111111",
            file_name: "促销海报.png",
            rel_path: "objects/a1111111-1111-1111-1111-111111111111/raw.png",
            category: Some("海报"),
            bytes: png_bytes(7),
        },
        LegacyAsset {
            uuid: "b2222222-2222-2222-2222-222222222222",
            file_name: "屏幕截图.png",
            rel_path: "objects/b2222222-2222-2222-2222-222222222222/raw.png",
            category: None,
            bytes: png_bytes(19),
        },
        LegacyAsset {
            uuid: "c3333333-3333-3333-3333-333333333333",
            file_name: "笔记.txt",
            rel_path: "objects/c3333333-3333-3333-3333-333333333333/raw.txt",
            category: Some("参考图"),
            bytes: b"\xe7\xac\x94\xe8\xae\xb0\xe5\x86\x85\xe5\xae\xb9".to_vec(),
        },
    ];
    make_v010_library(&legacy, &assets);

    // 迁移前旧库 user_version=4，迁移后必须原样（读取不得改写用户旧库）。
    let lib = migrate_legacy(&legacy, &unified);

    let mut live: Vec<(Option<String>, String)> = Vec::new();
    lib.store()
        .for_each_asset_active(|m| {
            live.push((m.category.clone(), m.file_name.clone()));
        })
        .unwrap();
    assert_eq!(live.len(), 3, "三条都应入库");

    live.sort();
    // 旧库 category=None 的行 → 清单 auto → 入库落 INBOX_CATEGORY（"待分类"
    // 字面量，与 library enqueue 缺省一致；分面 COALESCE 同义）。
    let mut expected = vec![
        (Some("参考图".to_string()), "raw.txt".to_string()),
        (Some("海报".to_string()), "raw.png".to_string()),
        (Some("待分类".to_string()), "raw.png".to_string()),
    ];
    expected.sort();

    assert_eq!(
        live, expected,
        "分类必须逐条保留：海报/参考图保留原值，旧库 NULL 的落待分类（None）"
    );

    // 旧库未被读取改写：user_version 仍是 4。
    let backup = base
        .path()
        .join("exe")
        .read_dir()
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("library.migrated-"))
                .unwrap_or(false)
        })
        .expect("备份应存在");
    let conn = Connection::open(backup.join("meta.db")).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(v, 4, "读取旧库分类不得触发 schema 迁移");
}

/// 重复内容：新库里已有该素材时，迁移遇到 duplicate 跳过，**既有行的分类不被
/// 覆盖**（内容相同即视为同一素材，保留用户在新库的分类决策）。
#[test]
fn migration_duplicate_does_not_overwrite_existing_category() {
    let base = tempfile::tempdir().unwrap();
    let legacy = base.path().join("exe").join("library");
    let unified = base.path().join("unified");
    std::fs::create_dir_all(&unified).unwrap();

    let poster = png_bytes(7);
    let assets = vec![LegacyAsset {
        uuid: "a1111111-1111-1111-1111-111111111111",
        file_name: "促销海报.png",
        rel_path: "objects/a1111111-1111-1111-1111-111111111111/raw.png",
        category: Some("海报"),
        bytes: poster.clone(),
    }];
    make_v010_library(&legacy, &assets);

    // 新库已预置同内容素材，分类为「已有」（用户已重新归类）。
    let lib = Library::open(&unified).unwrap();
    let existing_src = unified.join("seed.png");
    std::fs::write(&existing_src, &poster).unwrap();
    let t = match lib
        .enqueue(ImportRequest {
            source: existing_src,
            category: Some("已有".into()),
            tags: Vec::new(),
        })
        .unwrap()
    {
        EnqueueOutcome::Ticket(t) => t,
        other => panic!("预置素材应入库，实际 {other:?}"),
    };
    wait_for(&lib, &t, |s| matches!(s, CopyState::Done));

    // 迁移这条同内容素材 → 应判 duplicate，既有「已有」分类不动。
    let migrated = migrate_legacy(&legacy, &unified);
    let mut live: Vec<(Option<String>, String)> = Vec::new();
    migrated
        .store()
        .for_each_asset_active(|m| {
            live.push((m.category.clone(), m.file_name.clone()));
        })
        .unwrap();
    assert_eq!(live.len(), 1, "重复内容不新增行");
    assert_eq!(
        live[0].0.as_deref(),
        Some("已有"),
        "duplicate 跳过不得用旧库分类覆盖既有分类"
    );
}

/// 镜像 raw 文件名取末段，给 parse_manifest_line 的反向校验用（仅测试内部）。
#[test]
#[allow(dead_code)]
fn raw_name_extraction() {
    assert_eq!(raw_file_name_of(Path::new("a/b/raw.png")), "raw.png");
    assert_eq!(file_name_of("objects/u/raw.txt"), "raw.txt");
}
