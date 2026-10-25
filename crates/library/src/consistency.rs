//! D91 内容一致性对账：DB 索引 ⇄ 文件系统正本的漂移检测与目录级自愈。
//!
//! 分裂存储（SQLite 索引 + `objects/` 正本）的固有风险是两边静默分叉：
//! - 正本被外部修改 → DB 派生字段（size/pHash/宽高/正文/缩略图/内容摘要）
//!   全旧，出现「卡片预览是旧文案、粘贴出去是新文案」的脑裂（用户可见症状）。
//! - 正本被外部删除 → 索引指向悬空路径。
//! - 导入 mkdir 与 DB 提交之间崩溃 → `objects/` 留下无行孤儿目录。
//!
//! 职责边界（单写者纪律 + D11）：本模块只做 **stat 级对账**（元数据比对 +
//! 目录清扫），不解码、不算哈希、不写派生字段——判定为「外部修改」的素材由
//! tools/reindex 子进程重索引后写回。因此 UI 进程可安全调用（与 trash 对账
//! 同层：元数据 IO，非派生计算）；写库走 store 的批量接口（WAL 多进程并发
//! 已由 busy_timeout 兜住）。
//!
//! 判脏口径（[`classify`] 纯函数是唯一出处，reindex 工序复用同一实现）：
//! - mtime 未基线（NULL，存量行）→ [`ContentState::NeedsBaseline`]，只记录
//!   现状不判脏——首扫语义，避免升级后第一次启动就触发全库重索引风暴。
//! - 文件系统 mtime（截断到秒）≠ DB mtime，或 size ≠ DB size →
//!   [`ContentState::Stale`]。秒级截断意味着同秒内等尺寸的两次修改不可见，
//!   是标准 mtime 对账的已知边界；换内容哈希逐行全读违背预算纪律，不接受。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use store::{AssetMeta, Store};

use crate::{LibraryError, Result};

/// 孤儿目录的最小年龄：导入 mkdir 与 DB 提交之间只有毫秒级窗口，10 分钟
/// 足以让「正在进行的导入」与「上次崩溃的残留」在时间轴上彻底分开。
pub const ORPHAN_MIN_AGE_SECS: u64 = 600;

/// 单行内容对账判定（[`classify`] 的产物）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentState {
    /// mtime/size 与库内记录一致。
    InSync,
    /// 从未基线（存量行 / 导入时 stat 失败）：本轮记录现状，不判脏。
    NeedsBaseline,
    /// 正本被外部修改（mtime 或 size 与库内记录不符）。
    Stale,
}

/// 对账判定唯一出处。重索引工序（tools/reindex）复用本函数决定「这条要不要
/// 重算」，与 UI 侧对账共用同一条口径——两处各写一份迟早漂移。
pub fn classify(meta: &AssetMeta, fs_mtime: i64, fs_size: i64) -> ContentState {
    match meta.mtime {
        None => ContentState::NeedsBaseline,
        Some(mtime) => {
            if mtime != fs_mtime || meta.size_bytes != fs_size {
                ContentState::Stale
            } else {
                ContentState::InSync
            }
        }
    }
}

/// 一次内容对账的报告。集合成员供重索引工序/日志消费；数值供测试断言。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConsistencyReport {
    /// 扫过的活行数。
    pub scanned: usize,
    /// 无法对账的行数（rel_path 为空的历史行——没有正本地址可查，诚实跳过）。
    pub skipped_no_path: usize,
    /// 首扫基线回填行数。
    pub baselined: usize,
    /// 正本缺失的活行 uuid（含已在库内标记过的——报告反映现状全量）。
    pub missing: Vec<String>,
    /// 上次标缺失、本次文件已回来的 uuid（missing 标志清除）。
    pub recovered: Vec<String>,
    /// 判定被外部修改、待重索引的 uuid。
    pub stale: Vec<String>,
    /// 清掉的 `objects/` 孤儿目录数。
    pub orphans_reaped: usize,
}

/// 执行一次内容对账：比对全部活行、写回缺失/基线状态、清扫孤儿目录。
///
/// 缺失判定是**现状全量**：文件不在 → 进 `report.missing`；DB 旗标只翻转
/// 有变化的部分（新缺失 mark_missing，回归 clear_missing），幂等可重入。
/// 孤儿判定基于 `store::all_uuids`（含回收站行——软删正本在 trash/，其
/// objects 目录残留属 trash 对账管辖，这里只收「任何行都没有」的目录）。
pub fn audit_content(root: &Path, store: &Store) -> Result<ConsistencyReport> {
    audit_content_with(root, store, ORPHAN_MIN_AGE_SECS)
}

/// [`audit_content`] 的可注入版（孤儿最小年龄参数化，测试用 0 立即回收）。
pub fn audit_content_with(
    root: &Path,
    store: &Store,
    orphan_min_age_secs: u64,
) -> Result<ConsistencyReport> {
    let mut report = ConsistencyReport::default();
    let mut new_missing: Vec<String> = Vec::new();
    let mut baselines: Vec<(String, i64)> = Vec::new();

    store
        .for_each_asset_active(|meta| {
            report.scanned += 1;
            if meta.rel_path.is_empty() {
                report.skipped_no_path += 1;
                return;
            }
            let path = join_rel(root, &meta.rel_path);
            let Ok(md) = fs::metadata(&path) else {
                report.missing.push(meta.uuid.clone());
                if !meta.missing {
                    new_missing.push(meta.uuid.clone());
                }
                return;
            };
            if meta.missing {
                // 文件回来了：清旗标。内容是否也被改过交给下面的统一比对
                // （mtime 对不上照样进 stale，一条路径两个结论不冲突）。
                report.recovered.push(meta.uuid.clone());
            }
            let Some(fs_mtime) = mtime_secs(&md) else {
                return; // 读不到 mtime 的文件系统无法对账，视为现状未知。
            };
            let fs_size = md.len() as i64;
            match classify(&meta, fs_mtime, fs_size) {
                ContentState::InSync => {}
                ContentState::NeedsBaseline => baselines.push((meta.uuid.clone(), fs_mtime)),
                ContentState::Stale => report.stale.push(meta.uuid.clone()),
            }
        })
        .map_err(LibraryError::Store)?;

    if !new_missing.is_empty() {
        let refs: Vec<&str> = new_missing.iter().map(String::as_str).collect();
        store.mark_missing(&refs).map_err(LibraryError::Store)?;
    }
    if !report.recovered.is_empty() {
        let refs: Vec<&str> = report.recovered.iter().map(String::as_str).collect();
        store.clear_missing(&refs).map_err(LibraryError::Store)?;
    }
    if !baselines.is_empty() {
        report.baselined = baselines.len();
        store
            .backfill_mtimes(&baselines)
            .map_err(LibraryError::Store)?;
    }

    report.orphans_reaped = reap_orphans(root, store, orphan_min_age_secs)?;
    Ok(report)
}

/// `objects/` 孤儿目录清扫：任何 DB 行都不引用、且目录 mtime 早于阈值的
/// 整目录删除（raw/paste.png 一并走）。正本目录只被导入路径创建、只被
/// DB 行寻址，无行 = 不可达垃圾；年龄阈值把「正在进行的导入」隔在墙外。
fn reap_orphans(root: &Path, store: &Store, min_age_secs: u64) -> Result<usize> {
    let objects = root.join("objects");
    if !objects.is_dir() {
        return Ok(0);
    }
    let known: HashSet<String> = store
        .all_uuids()
        .map_err(LibraryError::Store)?
        .into_iter()
        .collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut reaped = 0usize;
    for entry in fs::read_dir(&objects).map_err(LibraryError::Io)? {
        let entry = entry.map_err(LibraryError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let uuid = entry.file_name().to_string_lossy().to_string();
        if known.contains(&uuid) {
            continue;
        }
        let old_enough = mtime_secs(&entry.metadata().map_err(LibraryError::Io)?)
            .is_some_and(|mtime| now.saturating_sub(mtime as u64) >= min_age_secs);
        if !old_enough {
            continue;
        }
        fs::remove_dir_all(&path).map_err(LibraryError::Io)?;
        reaped += 1;
    }
    Ok(reaped)
}

/// `rel_path` 以 '/' 分隔存储，逐段拼接（整串 join 会产出混合分隔路径，
/// 与 derive-thumbs 同款约定）。pub：重索引工序定位正本走同一实现。
pub fn join_rel(root: &Path, rel_path: &str) -> PathBuf {
    let mut joined = root.to_path_buf();
    for segment in rel_path.split('/').filter(|s| !s.is_empty()) {
        joined.push(segment);
    }
    joined
}

/// 文件系统 mtime → unix 秒（截断）。读失败返回 None。
/// pub：重索引工序写回基线/新值时用同一截断口径。
pub fn mtime_secs(md: &fs::Metadata) -> Option<i64> {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

#[cfg(test)]
mod classify_spec {
    use super::{classify, AssetMeta, ContentState};

    fn meta(mtime: Option<i64>, size: i64) -> AssetMeta {
        AssetMeta {
            uuid: "u".into(),
            file_name: "a.txt".into(),
            rel_path: "objects/u/raw.txt".into(),
            category: None,
            category_id: None,
            tags: vec![],
            size_bytes: size,
            created_at: 0,
            imported_at: 0,
            phash: None,
            content_hash: None,
            width: None,
            height: None,
            mtime,
            missing: false,
        }
    }

    #[test]
    fn classify_table() {
        type Case = (Option<i64>, i64, i64, i64, ContentState, &'static str);
        let cases: &[Case] = &[
            // (db mtime, db size, fs mtime, fs size, 期望, 说明)
            (
                Some(100),
                10,
                100,
                10,
                ContentState::InSync,
                "mtime/size 全一致",
            ),
            (
                None,
                10,
                100,
                10,
                ContentState::NeedsBaseline,
                "未基线只回填",
            ),
            (
                Some(100),
                10,
                101,
                10,
                ContentState::Stale,
                "mtime 漂移 = 修改",
            ),
            (
                Some(100),
                10,
                100,
                11,
                ContentState::Stale,
                "size 漂移 = 修改",
            ),
            (Some(100), 10, 101, 11, ContentState::Stale, "双漂移 = 修改"),
        ];
        for &(db_mtime, db_size, fs_mtime, fs_size, expected, why) in cases {
            assert_eq!(
                classify(&meta(db_mtime, db_size), fs_mtime, fs_size),
                expected,
                "{why}"
            );
        }
    }
}
