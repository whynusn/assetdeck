//! D91 内容一致性对账与重索引（离线编排进程）。
//!
//! 分裂存储（DB 索引 + `objects/` 正本）的自愈闭环全部收进本进程——单写者
//! 纪律要求 app-ui 不直开 meta.db，对账写库（缺失标记/基线回填/派生字段
//! 写回）只能由子工序完成，宿主只消费本进程的 stdout 行协议刷新角标：
//! - `MARK\t<ok|missing|stale>\t<uuid>`：扫描发现的状态（宿主据此挂/摘角标）；
//! - `FIXED\t<uuid>`：单个素材重索引完成（宿主摘「已在外部修改」角标）；
//! - `PROGRESS\t<done>\t<total>`：重索引进度（宿主当前静默，仅日志）；
//! - `done: ...`：摘要行（宿主据 `reindexed>` 决定是否整库重载）。
//!
//! 工序分两层，职责与 derive-thumbs 对齐：
//! - stat 级对账（缺失/基线/判脏/孤儿清扫）→ [`library::consistency`]，
//!   判脏口径唯一出处 [`library::consistency::classify`]；
//! - 派生重算（缩略图/paste.png/尺寸）→ worker 子进程池（D11 红线对 UI 主
//!   进程的约束在这里以「解码只发生在子进程或本离线进程」的形式延续）。
//!   pHash 在本进程全分辨率解码（与导入侧同一 [`library::phash_of`] 实现，
//!   逐位一致；worker 缩略图走的是缩放后的像素，直接复用会让哈希口径漂移）。
//!
//! 分批纪律（D3/D4）：写回攒批（256 行一事务）、任务有界入队
//! （workers×4），峰值驻留与库规模无关。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use domain::AssetKind;
use library::consistency::{audit_content, join_rel, mtime_secs};
use media::{is_paste_derivable, is_thumbnailable, kind_of};
use store::{ReindexedFields, Store};
use worker::{JobRequest, JobResult, PoolPriority, WorkerPool};

/// 单个素材的解码预算（与 derive-thumbs 同档）：worker 跑在 idle 优先级，
/// 视频抽帧还要等 Shell，给足余量。
const JOB_TIMEOUT: Duration = Duration::from_secs(90);
/// 派生写回批次：每批共享一次事务（Windows 上逐行 UPDATE 各付一次 fsync）。
const WRITE_BATCH: usize = 256;

#[derive(Debug, Clone, Default)]
pub struct ReindexArgs {
    /// 库根目录（meta.db / objects/ / thumbs/ 的父目录）。
    pub library: PathBuf,
    /// decode-worker.exe 显式路径；None = 同目录自动定位。仅当确有缩略图
    /// 任务时才要求存在——纯文本库的对账不依赖解码设施。
    pub worker_exe: Option<PathBuf>,
    /// true = 前台高速档（BELOW_NORMAL），false = 后台 idle 档（D37 约定）。
    pub fast: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReindexSummary {
    pub scanned: usize,
    pub baselined: usize,
    pub missing: usize,
    pub recovered: usize,
    pub orphans_reaped: usize,
    pub stale: usize,
    pub reindexed: usize,
    pub failed: usize,
}

/// 执行完整闭环：对账 → MARK 行 → 缺失/基线回写 → 重索引 → 派生写回。
/// 致命错误（开库失败等）返回 Err；单素材失败计入 `failed` 不拖垮整批。
pub fn run(args: &ReindexArgs) -> Result<ReindexSummary, String> {
    let db = args.library.join("meta.db");
    if !db.is_file() {
        return Err(format!("{} 下无 meta.db", args.library.display()));
    }
    let store = Store::open(&db).map_err(|e| e.to_string())?;
    let report = audit_content(&args.library, &store).map_err(|e| e.to_string())?;

    // 扫描发现即时上报：宿主在重索引完成前就能挂上角标（诚实呈现现状）。
    for uuid in &report.recovered {
        println!("MARK\tok\t{uuid}");
    }
    for uuid in &report.missing {
        println!("MARK\tmissing\t{uuid}");
    }
    for uuid in &report.stale {
        println!("MARK\tstale\t{uuid}");
    }

    let mut summary = ReindexSummary {
        scanned: report.scanned,
        baselined: report.baselined,
        missing: report.missing.len(),
        recovered: report.recovered.len(),
        orphans_reaped: report.orphans_reaped,
        stale: report.stale.len(),
        reindexed: 0,
        failed: 0,
    };
    summary.reindexed = reindex_stale(&args.library, &store, &report.stale, args)?;
    summary.failed = summary.stale - summary.reindexed;
    Ok(summary)
}

/// 逐个重算被外部修改的素材并写回。返回成功条数。
fn reindex_stale(
    root: &Path,
    store: &Store,
    stale: &[String],
    args: &ReindexArgs,
) -> Result<usize, String> {
    if stale.is_empty() {
        return Ok(0);
    }
    let mut jobs: VecDeque<(String, PathBuf, PathBuf, Option<PathBuf>)> = VecDeque::new();
    let mut derived: Vec<ReindexedFields> = Vec::new();
    let mut dims: Vec<(String, u32, u32)> = Vec::new();
    let mut reindexed = 0usize;
    let total = stale.len();
    let mut done = 0usize;
    let mut last_reported = 0usize;
    let progress_step = (total / 100).max(1);

    let flush = |store: &Store,
                 derived: &mut Vec<ReindexedFields>,
                 dims: &mut Vec<(String, u32, u32)>,
                 reindexed: &mut usize|
     -> Result<(), String> {
        if !derived.is_empty() {
            let n = store
                .set_reindexed_batch(derived)
                .map_err(|e| e.to_string())?;
            *reindexed += n;
            derived.clear();
        }
        if !dims.is_empty() {
            let refs: Vec<(&str, u32, u32)> =
                dims.iter().map(|(u, w, h)| (u.as_str(), *w, *h)).collect();
            let n = store
                .set_dimensions_batch(&refs)
                .map_err(|e| e.to_string())?;
            logging::info!("reindex: 尺寸回写 {n} 行");
            dims.clear();
        }
        Ok(())
    };

    for uuid in stale {
        done += 1;
        let Ok(Some(meta)) = store.get_asset(uuid) else {
            // 对账与用户删除并发的窗口期：行没了就没什么可重算。
            continue;
        };
        let path = join_rel(root, &meta.rel_path);
        let ext = extension_of(&meta.rel_path);
        let kind = kind_of(&path);
        let Ok(md) = std::fs::metadata(&path) else {
            // 对账到重索引之间又被外部删除：留给下轮对账按 missing 处理。
            eprintln!("warn {uuid} ({}): 正本不可读，本轮跳过", path.display());
            continue;
        };
        let Some(fs_mtime) = mtime_secs(&md) else {
            eprintln!("warn {uuid} ({}): 读 mtime 失败，本轮跳过", path.display());
            continue;
        };
        let fs_size = md.len() as i64;

        match kind {
            AssetKind::Text => {
                // 正文重写走 set_text_content（FTS 触发器同步），派生批只带
                // size/mtime/摘要；正文读失败 = 保持旧正文（诚实降级）。
                match library::read_fts_content(&path) {
                    Some(content) => {
                        if let Err(e) = store.set_text_content(uuid, &content) {
                            eprintln!("failed {uuid}: 正文写回失败: {e}");
                            continue;
                        }
                    }
                    None => eprintln!("warn {uuid}: 正文读取失败，保留旧正文索引"),
                }
                let Ok(content_hash) = library::sha256_file(&path) else {
                    eprintln!("failed {uuid} ({}): 哈希失败", path.display());
                    continue;
                };
                derived.push(ReindexedFields {
                    uuid: uuid.clone(),
                    size_bytes: fs_size,
                    mtime: fs_mtime,
                    phash: None,
                    content_hash: Some(content_hash.to_vec()),
                });
            }
            AssetKind::Image | AssetKind::Video | AssetKind::Other => {
                // 摘要：非图片重算 SHA-256（等值去重的判死依据）；图片保持
                // None（图片走 pHash 感知去重，语义互不侵犯）。
                let content_hash = if kind == AssetKind::Image {
                    None
                } else {
                    match library::sha256_file(&path) {
                        Ok(h) => Some(h.to_vec()),
                        Err(e) => {
                            eprintln!("failed {uuid} ({}): 哈希失败: {e}", path.display());
                            continue;
                        }
                    }
                };
                let phash = if kind == AssetKind::Image {
                    // 全分辨率解码算 pHash（与导入侧同口径）；失败保旧值，
                    // 感知去重的相似提醒宁旧勿错。
                    match library::decode_for_phash(&path) {
                        Ok(img) => library::phash_of(&img),
                        Err(e) => {
                            eprintln!("warn {uuid}: 重算 pHash 失败，保留旧值: {e}");
                            meta.phash.clone()
                        }
                    }
                } else {
                    None
                };
                // 缩略图/paste.png 重派生交给 worker（视频走 Shell 抽帧）。
                if is_thumbnailable(&ext) {
                    let dest = absolutize(&root.join(Store::thumbnail_cache_path(uuid, "png")));
                    let paste_dest = if is_paste_derivable(&ext) {
                        Some(absolutize(&root.join(Store::paste_png_path(uuid))))
                    } else {
                        None
                    };
                    jobs.push_back((uuid.clone(), path.clone(), dest, paste_dest));
                }
                derived.push(ReindexedFields {
                    uuid: uuid.clone(),
                    size_bytes: fs_size,
                    mtime: fs_mtime,
                    phash,
                    content_hash,
                });
            }
        }

        if derived.len() >= WRITE_BATCH {
            flush(store, &mut derived, &mut dims, &mut reindexed)?;
        }
        if done == total || done - last_reported >= progress_step {
            println!("PROGRESS\t{done}\t{total}");
            last_reported = done;
        }
    }

    // worker 阶段：有界批处理（与 derive-thumbs 同款），结果只取尺寸。
    if !jobs.is_empty() {
        let exe = resolve_worker_exe(args.worker_exe.as_deref())?;
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        let priority = if args.fast {
            PoolPriority::ForegroundBelowNormal
        } else {
            PoolPriority::BackgroundIdle
        };
        let pool = WorkerPool::with_priority(&exe, workers, priority);
        let batch_size = workers * 4;
        while !jobs.is_empty() {
            let batch: Vec<_> = (0..batch_size.min(jobs.len()))
                .map(|_| jobs.pop_front().expect("已检查队列非空"))
                .collect();
            let mut receivers = Vec::with_capacity(batch.len());
            for (idx, (uuid, source, dest, paste_dest)) in batch.into_iter().enumerate() {
                let rx = pool.submit(JobRequest::ThumbnailPng {
                    job_id: idx as u64,
                    source,
                    dest,
                    max_edge: DEFAULT_MAX_EDGE,
                    paste_dest,
                    paste_max_edge: DEFAULT_PASTE_MAX_EDGE,
                });
                receivers.push((uuid, rx));
            }
            for (uuid, rx) in receivers {
                match rx.recv_timeout(JOB_TIMEOUT) {
                    Ok(result @ JobResult::Ok { .. }) => {
                        if let Some((w, h)) = result.dimensions() {
                            dims.push((uuid, w, h));
                        } else {
                            eprintln!("warn {uuid}: worker 未回报像素尺寸");
                        }
                    }
                    Ok(JobResult::Failed { reason, .. }) => {
                        eprintln!("failed {uuid}: 缩略图重派生失败: {reason}");
                    }
                    Err(e) => {
                        eprintln!("failed {uuid}: 等待 worker 结果超时/断开 ({e})");
                    }
                }
            }
        }
    }

    flush(store, &mut derived, &mut dims, &mut reindexed)?;
    Ok(reindexed)
}

/// 与 derive-thumbs 相同的档位常量：瀑布流 2 倍图 + paste.png 4096 上限。
const DEFAULT_MAX_EDGE: u32 = 320;
const DEFAULT_PASTE_MAX_EDGE: u32 = 4096;

fn extension_of(rel_path: &str) -> String {
    Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// worker 二进制定位：显式路径 > 本工具同目录（cargo 产物布局）。
pub fn resolve_worker_exe(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(format!("--worker-exe {} 不存在", path.display()));
        }
        return Ok(path.to_path_buf());
    }
    let here = std::env::current_exe().map_err(|e| format!("读取自身路径失败: {e}"))?;
    let sibling = here
        .parent()
        .ok_or("无法定位自身所在目录")?
        .join("decode-worker.exe");
    if sibling.is_file() {
        return Ok(sibling);
    }
    Err(format!(
        "未找到 decode-worker.exe（尝试过 {}）：请先 cargo build -p worker 或用 --worker-exe 指定",
        sibling.display()
    ))
}

#[cfg(test)]
mod reindex_spec {
    use super::{run, ReindexArgs};
    use std::fs;
    use std::path::Path;

    use library::consistency;
    use store::{AssetMeta, Store};

    fn meta(uuid: &str, rel_path: &str, size: i64, mtime: Option<i64>) -> AssetMeta {
        AssetMeta {
            uuid: uuid.to_string(),
            file_name: "a.txt".to_string(),
            rel_path: rel_path.to_string(),
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

    fn args(root: &Path) -> ReindexArgs {
        ReindexArgs {
            library: root.to_path_buf(),
            worker_exe: None,
            fast: false,
        }
    }

    /// 文本素材的完整闭环：外部修改 → 判脏 → 正文/摘要/size/mtime 重写落库，
    /// FTS 行随触发器同步（正文检索立即命中新文案）。
    #[test]
    fn text_stale_is_fully_reindexed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let store = Store::open(&root.join("meta.db")).unwrap();

        let raw = root.join("objects").join("u-1").join("raw.txt");
        fs::create_dir_all(raw.parent().unwrap()).unwrap();
        fs::write(&raw, "旧文案：请回复发货时间").unwrap();
        let md = fs::metadata(&raw).unwrap();
        store
            .upsert_asset(&meta(
                "u-1",
                "objects/u-1/raw.txt",
                4,
                Some(consistency::mtime_secs(&md).unwrap() + 999),
            ))
            .unwrap();
        store
            .set_text_content("u-1", "旧文案：请回复发货时间")
            .unwrap();

        // 外部修改正文。
        fs::write(&raw, "新文案：亲，已经为您加急发货啦，请注意查收物流信息").unwrap();

        let summary = run(&args(root)).unwrap();
        assert_eq!(summary.stale, 1);
        assert_eq!(summary.reindexed, 1);
        assert_eq!(summary.failed, 0);

        let asset = store.get_asset("u-1").unwrap().unwrap();
        assert_eq!(asset.size_bytes, fs::metadata(&raw).unwrap().len() as i64);
        assert_eq!(
            asset.mtime,
            Some(consistency::mtime_secs(&fs::metadata(&raw).unwrap()).unwrap())
        );
        assert!(asset.content_hash.is_some(), "非图片素材重算内容摘要");
        // FTS 正文随触发器刷新：新文案可检索、旧文案不再命中。
        assert_eq!(store.search("加急发货", 10).unwrap().len(), 1);
        assert!(store.search("请回复发货时间", 10).unwrap().is_empty());

        // 再跑一轮：已收敛，无动作。
        let summary = run(&args(root)).unwrap();
        assert_eq!(summary.stale, 0);
        assert_eq!(summary.reindexed, 0);
    }

    /// 缺失与基线与孤儿在 run() 里一并处置（对账半边）。
    #[test]
    fn run_reports_missing_baseline_and_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let store = Store::open(&root.join("meta.db")).unwrap();

        let raw = root.join("objects").join("u-live").join("raw.txt");
        fs::create_dir_all(raw.parent().unwrap()).unwrap();
        fs::write(&raw, "在库内容").unwrap();
        let md = fs::metadata(&raw).unwrap();
        store
            .upsert_asset(&meta(
                "u-live",
                "objects/u-live/raw.txt",
                md.len() as i64,
                None,
            ))
            .unwrap();
        store
            .upsert_asset(&meta("u-gone", "objects/u-gone/raw.txt", 1, Some(1)))
            .unwrap();
        // 刚建的孤儿目录在 600s 年龄护栏内：穿过 run() 也不会被清扫
        //（真实清扫路径在 library 一致性测试里以注入年龄 0 验证过）。
        let orphan = root.join("objects").join("orphan");
        fs::create_dir_all(&orphan).unwrap();

        let summary = run(&args(root)).unwrap();
        assert_eq!(summary.baselined, 1);
        assert_eq!(summary.missing, 1);
        assert_eq!(summary.orphans_reaped, 0);
        assert!(store.get_asset("u-gone").unwrap().unwrap().missing);
        assert!(orphan.exists());
    }

    /// 无库根目录是致命错误（与 derive-thumbs 的「下无 meta.db」同语义）。
    #[test]
    fn missing_meta_db_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(&args(dir.path())).is_err());
    }
}
