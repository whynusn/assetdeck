//! 旧版库迁移（D61）：v0.1.0 的「exe 同目录 library」→ 统一库根的一次性搬迁。
//!
//! 策略 = **先改名留档，再重放导入**：把旧目录整体改名 `library.migrated-<unix 秒>`
//! 之后，用既有 `--import-paths` 清单导入把 `objects/` 里全部素材文件重放进统一库
//! （图片 pHash 去重；其余类型 SHA-256 内容等值去重，D61 起全类目覆盖）。理由：
//!
//! - **不搬 meta.db**：8-29 事故实证安装目录旁的库会被 payload 示例库覆盖
//!   （真机遗留库的索引只剩 11 条示例、7 个用户对象成了孤儿）——索引不可信，
//!   文件本体才是真相源；重放导入顺带重建缩略图与 FTS，不继承旧索引的任何账。
//! - **改名先于导入**：改名本身就是防重标记，不存在「导入成功、改名失败 →
//!   再点全量重复」窗口；导入失败把改名回滚，素材原位无损。
//! - 分类/标签不迁移：v0.1.0 无用户分类语义（全部待分类），重放后统一进待分类。
//!
//! 本模块全是纯文件系统函数（零解码零 SQL），UI 进程调用安全；重活（解码/拷贝/
//! pHash/落库）全在 sample-library 子进程——进程模型红线不破。

use std::io;
use std::path::{Path, PathBuf};

/// 迁移完成标记：写入备份目录后 [`detect_legacy_library`] 视为已收账，不再提供入口。
/// 防的是「改名留档成功 + 导入去重全跳过」的备份被反复端上桌面。
pub const MIGRATION_MARKER: &str = "migration.done";
/// 备份目录名前缀（后缀 = unix 秒时间戳；字典序即时间序）。
pub const BACKUP_PREFIX: &str = "library.migrated-";

/// 一个可迁移的旧版库候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyLibrary {
    /// 待迁移素材目录（主候选 `library` 或已改名备份）。
    pub source: PathBuf,
    /// true = 已是改名备份（重试场景，无需再改名）。
    pub is_backup: bool,
    /// `objects/` 下素材文件数（预扫计数，供 UI 文案与空库短路）。
    pub file_count: usize,
    /// `objects/` 下素材总字节（供 UI 文案）。
    pub total_bytes: u64,
}

/// 检测可迁移的旧版库：优先 exe 旁 `library/`，其次最新的未收账改名备份
/// （重试场景——上次导入失败且回滚改名也失败时，备份目录仍在）。
///
/// `current_root` 是当前统一库根：候选与它同一路径时返回 None——绝不允许
/// 把库导进自己（`--library-root` 指到 exe 旁的开发场景）。
pub fn detect_legacy_library(exe_dir: &Path, current_root: &Path) -> Option<LegacyLibrary> {
    let primary = exe_dir.join("library");
    if is_migratable(&primary, current_root) {
        return scan(&primary, false);
    }
    let mut backups: Vec<PathBuf> = std::fs::read_dir(exe_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            name.starts_with(BACKUP_PREFIX).then_some(path)
        })
        .filter(|path| path.is_dir() && !path.join(MIGRATION_MARKER).exists())
        .collect();
    // 时间戳后缀字典序 == 时间序；最新备份优先。
    backups.sort();
    while let Some(dir) = backups.pop() {
        if is_migratable(&dir, current_root) {
            return scan(&dir, true);
        }
    }
    None
}

/// 候选可迁移 = 是目录、不是当前库根本身、`objects/` 下至少有一个真实文件
/// （排除空壳目录与误建目录）。
fn is_migratable(dir: &Path, current_root: &Path) -> bool {
    if !dir.is_dir() || same_path(dir, current_root) {
        return false;
    }
    matches!(walk_objects(&dir.join("objects"), &mut |_path, _len| false), Ok(false))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// 递归遍历目录下的 canonical 素材文件（`raw.<ext>`）；回调返回 false 提前终止。
/// 返回 `Ok(false)` = 提前终止（调用方自行区分语义）。
/// 旧库 objects 形态固定为 `objects/<uuid>/raw.<ext>`，按递归写以兜住历史
/// 目录形态；对象目录里还有上框物化写的 `paste.png` 载荷旁车（trash.rs 同款
/// 认知），它不是素材——只放行 `raw.*`，否则每次迁移都会把旁车重放成重复图。
/// 只做 readdir + metadata，不读文件字节。
fn walk_objects(dir: &Path, visit: &mut dyn FnMut(&Path, u64) -> bool) -> io::Result<bool> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // objects/ 缺失视为空目录（空壳候选由调用方判定）。
        Err(_) => return Ok(true),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if !walk_objects(&path, visit)? {
                return Ok(false);
            }
        } else {
            let is_raw = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("raw."))
                .unwrap_or(false);
            if !is_raw {
                continue;
            }
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if !visit(&path, len) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn scan(source: &Path, is_backup: bool) -> Option<LegacyLibrary> {
    let mut file_count = 0usize;
    let mut total_bytes = 0u64;
    walk_objects(&source.join("objects"), &mut |_path, len| {
        file_count += 1;
        total_bytes += len;
        true
    })
    .ok()?;
    Some(LegacyLibrary {
        source: source.to_path_buf(),
        is_backup,
        file_count,
        total_bytes,
    })
}

/// 读旧库 `uuid → category` 映射（best-effort）：旧库 `meta.db` 缺失/损坏/
/// 非 SQLite 时返回 `Err`，调用方按「无分类可读」降级——迁移照常走，分类
/// 落待分类。详见 [`store::read_category_by_uuid`]（裸 SELECT 不跑迁移，
/// 不改写用户旧库）。
pub fn read_legacy_categories(
    db_path: &Path,
) -> io::Result<std::collections::HashMap<String, Option<String>>> {
    store::read_category_by_uuid(db_path)
}

/// 把旧库 `objects/` 下全部素材文件写成 `--import-paths` 清单（D49 格式：
/// `f\t<mode>\t<绝对路径>` 逐行）。流式写盘不物化路径表，十万级素材也不进内存。
/// 返回写入行数。
///
/// **mode 列随分类映射走**（D61 分类保留）：旧库 uuid → Some(分类名) 写
/// `category:<清洗后的名>`；None 或映射缺项写 `auto`（落待分类）。分类名
/// 里的制表符/换行/首尾空白会被清洗（清单是 TAB 分隔的，脏字符会断行）。
pub fn write_import_manifest(
    source: &Path,
    list_path: &Path,
    category_by_uuid: &std::collections::HashMap<String, Option<String>>,
) -> io::Result<usize> {
    use std::io::Write;

    let objects = source.join("objects");
    let mut file = std::fs::File::create(list_path)?;
    let mut buffer: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut count = 0usize;
    let mut write_err: Option<io::Error> = None;
    walk_objects(&objects, &mut |path, _len| {
        // 对象目录名即旧库 uuid（v0.1.0 起 rel_path = objects/<uuid>/raw.<ext>）；
        // 拿不到（历史畸形布局）→ auto。
        let mode = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .and_then(|uuid| category_by_uuid.get(uuid))
            .and_then(|opt| opt.as_deref())
            .map(sanitize_category_directive)
            .filter(|name| !name.is_empty())
            .map(|name| format!("category:{name}"))
            .unwrap_or_else(|| "auto".to_string());
        let line = format!("f\t{mode}\t{}\n", path.display());
        buffer.extend_from_slice(line.as_bytes());
        count += 1;
        if buffer.len() >= 64 * 1024 {
            match file.write_all(&buffer) {
                Ok(()) => buffer.clear(),
                Err(err) => {
                    write_err = Some(err);
                    return false;
                }
            }
        }
        true
    })?;
    if let Some(err) = write_err {
        return Err(err);
    }
    file.write_all(&buffer)?;
    Ok(count)
}

/// 清洗分类名用于 `category:` 指令：制表符/换行替换为空格、首尾空白裁掉；
/// 空串 → None（调用方回退 auto）。清单一行一个素材，断行会撕裂映射。
fn sanitize_category_directive(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\t' | '\r' | '\n' => ' ',
            other => other,
        })
        .collect();
    cleaned.trim().to_string()
}

/// 把旧库目录整体改名留档：`library` → `library.migrated-<unix 秒>`。
/// 同卷 rename 是元数据操作，素材文件零拷贝零风险；时间戳撞名（同一秒内
/// 二次改名）退化为追加序号。返回备份目录路径。
pub fn rename_to_backup(source: &Path, exe_dir: &Path) -> io::Result<PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut candidate = exe_dir.join(format!("{BACKUP_PREFIX}{ts}"));
    let mut ordinal = 0u32;
    while candidate.exists() {
        ordinal += 1;
        candidate = exe_dir.join(format!("{BACKUP_PREFIX}{ts}.{ordinal}"));
    }
    std::fs::rename(source, &candidate)?;
    Ok(candidate)
}

/// 迁移收账：备份目录写完成标记。写失败只挡「不再重复提示」这一件事，
/// 不回滚迁移本身（素材已入统一库）。
pub fn mark_migrated(backup: &Path) -> io::Result<()> {
    std::fs::write(backup.join(MIGRATION_MARKER), "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("legacy_migration_{tag}_{}_{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// 造一个 v0.1.0 形态的旧库：objects/<uuid>/raw.<ext> + meta.db + thumbs。
    fn make_legacy_library(root: &Path, files: &[(&str, &[u8])]) {
        for (rel, bytes) in files {
            let path = root.join("objects").join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
        }
        fs::write(root.join("meta.db"), b"sqlite").unwrap();
        fs::create_dir_all(root.join("thumbs")).unwrap();
    }

    #[test]
    fn detect_finds_primary_library_with_counts() {
        let base = temp_dir("primary");
        let exe_dir = base.join("exe");
        let library = exe_dir.join("library");
        make_legacy_library(
            &library,
            &[
                ("aaaaaaaa-1111/raw.png", b"pngdata"),
                ("bbbbbbbb-2222/raw.mp4", b"mp4data-"),
            ],
        );
        fs::create_dir_all(base.join("unified")).unwrap();

        let detected =
            detect_legacy_library(&exe_dir, &base.join("unified")).expect("应检测到主候选");
        assert_eq!(detected.source, library);
        assert!(!detected.is_backup);
        assert_eq!(detected.file_count, 2);
        assert_eq!(detected.total_bytes, 15);
        assert!(!base.join("library.migrated-0").exists());
    }

    #[test]
    fn detect_rejects_empty_shell_and_self_root() {
        let base = temp_dir("reject");
        let exe_dir = base.join("exe");
        // 空壳 library（无 objects 文件）不算旧库。
        fs::create_dir_all(exe_dir.join("library").join("objects")).unwrap();
        let unified = base.join("unified");
        fs::create_dir_all(&unified).unwrap();
        assert!(detect_legacy_library(&exe_dir, &unified).is_none());

        // 候选 == 当前库根：绝不导自己。
        let real_library = exe_dir.join("real-library");
        make_legacy_library(&real_library, &[("x/raw.png", b"x")]);
        assert!(detect_legacy_library(&exe_dir, &real_library).is_none());
    }

    #[test]
    fn rename_then_detect_offers_backup_marker_closes_entry() {
        let base = temp_dir("rename");
        let exe_dir = base.join("exe");
        let library = exe_dir.join("library");
        make_legacy_library(&library, &[("cccccccc-3333/raw.jpg", b"jpgdata")]);
        let unified = base.join("unified");
        fs::create_dir_all(&unified).unwrap();

        let backup = rename_to_backup(&library, &exe_dir).unwrap();
        assert!(backup.starts_with(&exe_dir));
        assert!(backup.file_name().unwrap().to_string_lossy().starts_with(BACKUP_PREFIX));
        assert!(!library.exists(), "原目录应已改名为备份");

        // 备份未收账 → 仍可作为重试候选（is_backup=true）。
        let retry = detect_legacy_library(&exe_dir, &unified).expect("备份应可重试");
        assert!(retry.is_backup);
        assert_eq!(retry.source, backup);

        // 写完成标记 → 入口关闭。
        mark_migrated(&backup).unwrap();
        assert!(detect_legacy_library(&exe_dir, &unified).is_none());
    }

    #[test]
    fn detect_prefers_primary_over_backups_and_latest_backup_first() {
        let base = temp_dir("prefer");
        let exe_dir = base.join("exe");
        let unified = base.join("unified");
        fs::create_dir_all(&unified).unwrap();

        // 两个备份：时间戳小的已收账、大的未收账；主候选也在。
        let old_backup = exe_dir.join(format!("{BACKUP_PREFIX}100"));
        make_legacy_library(&old_backup, &[("a/raw.png", b"a")]);
        mark_migrated(&old_backup).unwrap();
        let new_backup = exe_dir.join(format!("{BACKUP_PREFIX}200"));
        make_legacy_library(&new_backup, &[("b/raw.png", b"b")]);
        let primary = exe_dir.join("library");
        make_legacy_library(&primary, &[("c/raw.png", b"c")]);

        let detected = detect_legacy_library(&exe_dir, &unified).expect("主候选优先");
        assert_eq!(detected.source, primary);
        assert!(!detected.is_backup);

        // 主候选没了 → 最新未收账备份顶上，已收账的不再出现。
        fs::remove_dir_all(&primary).unwrap();
        let retry = detect_legacy_library(&exe_dir, &unified).expect("应回落最新备份");
        assert_eq!(retry.source, new_backup);
        assert!(retry.is_backup);
    }

    #[test]
    fn manifest_lines_match_import_paths_format() {
        let base = temp_dir("manifest");
        let library = base.join("library");
        make_legacy_library(
            &library,
            &[("dddddddd-4444/raw.png", b"p"), ("eeeeeeee-5555/raw.txt", b"t")],
        );
        let list = base.join("list.tsv");
        let count =
            write_import_manifest(&library, &list, &std::collections::HashMap::new()).unwrap();
        assert_eq!(count, 2);

        let text = fs::read_to_string(&list).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let mut fields = line.splitn(3, '\t');
            assert_eq!(fields.next(), Some("f"), "kind=f 散文件");
            assert_eq!(fields.next(), Some("auto"), "无分类映射时归待分类");
            let path = fields.next().expect("路径列");
            assert!(Path::new(path).is_file(), "路径必须是真实文件: {path}");
        }
    }

    #[test]
    fn manifest_lines_carry_category_directives_and_sanitize() {
        let base = temp_dir("manifest-cat");
        let library = base.join("library");
        make_legacy_library(
            &library,
            &[
                ("11111111-aaaa/raw.png", b"p1"),
                ("22222222-bbbb/raw.png", b"p2"),
                ("33333333-cccc/raw.jpg", b"j"),
            ],
        );
        // 映射覆盖 uuid1（正常分类）、uuid2（含制表符/换行/首尾空白 → 清洗）；
        // uuid3 不在映射里 → auto；映射里有但磁盘没有的 uuid4 不产生行。
        let mut categories = std::collections::HashMap::new();
        categories.insert("11111111-aaaa".to_string(), Some("海报".to_string()));
        categories.insert(
            "22222222-bbbb".to_string(),
            Some(" \t脏\n名字\t ".to_string()),
        );
        categories.insert("44444444-dddd".to_string(), Some("幽灵".to_string()));

        let list = base.join("list.tsv");
        let count = write_import_manifest(&library, &list, &categories).unwrap();
        assert_eq!(count, 3);

        let text = fs::read_to_string(&list).unwrap();
        let mut modes: Vec<&str> = Vec::new();
        for line in text.lines() {
            let mut fields = line.splitn(3, '\t');
            assert_eq!(fields.next(), Some("f"));
            modes.push(fields.next().expect("mode 列"));
        }
        modes.sort();
        assert_eq!(
            modes,
            vec!["auto", "category:海报", "category:脏 名字"],
            "分类名必须清洗制表符/换行/首尾空白（清单是 TAB 分隔的）"
        );
    }

    #[test]
    fn paste_sidecars_are_excluded_from_counts_and_manifest() {
        let base = temp_dir("sidecar");
        let library = base.join("exe").join("library");
        make_legacy_library(
            &library,
            &[
                ("ffffffff-6666/raw.jpg", b"original-jpg"),
                // 上框物化旁车：真机旧库里每个上过框的对象都带一份。
                ("ffffffff-6666/paste.png", b"paste-payload"),
                ("abababab-7777/raw.png", b"plain-png"),
            ],
        );

        let unified = base.join("unified");
        fs::create_dir_all(&unified).unwrap();
        let detected = detect_legacy_library(&base.join("exe"), &unified).expect("应检测到旧库");
        assert_eq!(detected.file_count, 2, "旁车不计入文件数");
        assert_eq!(detected.total_bytes, 21, "旁车不计入体积");

        let list = base.join("list.tsv");
        let count =
            write_import_manifest(&library, &list, &std::collections::HashMap::new()).unwrap();
        assert_eq!(count, 2, "旁车不进清单");
        let text = fs::read_to_string(&list).unwrap();
        assert!(!text.contains("paste.png"), "清单不得包含旁车: {text}");
    }
}
