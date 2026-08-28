//! 派生「浏览用缩略图」与「上框用 PNG」：为库内每个可视素材生成
//! `thumbs/<分片>/<uuid>.png`，同时把源媒体的原始像素尺寸回写
//! `assets.width/height`；全部图片（含 PNG 原图）还会在同一份解码里旁挂
//! `objects/<uuid>/paste.png`（D20，千牛一类目标只认 CF_PNG；PNG 原图同样
//! 派生并以 4096 cap 封顶，避免原图尺寸直通压垮上框触发侧与 IM 解码尾段）。
//!
//! 为什么要有这道离线工序：UI 进程不解码（D11），素材管理器只能 `fs::read`
//! 现成的 PNG。没有这批派生文件，瓦片就只能显示占位色块——素材无法辨识；
//! 尺寸回写则决定瀑布流的真实版式：缺尺寸时布局只能用与画面无关的占位公式；
//! 缺 paste.png 时 jpg/webp 等落到千牛会退化为「只复制 + 提示」，无法上框。
//!
//! 覆盖范围：图片交给 `image` crate，视频等容器由 worker 内的 Windows Shell
//! 缩略图工厂抽帧，两者都在子进程完成。GUI 每次导入后都会跑本工具，
//! 因此新导入的图片（含 PNG 原图）自动获得封顶的 paste.png；对旧库重跑一次即可回填。
//!
//! 用法：
//! ```text
//! derive-thumbs [--library <root>] [--max-edge <px>] [--force] [--worker-exe <path>]
//! ```

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use store::Store;
use worker::{JobRequest, JobResult, PoolPriority, WorkerPool};

/// 单个素材的解码预算。worker 跑在 idle 优先级，视频抽帧还要等 Shell，给足余量。
const JOB_TIMEOUT: Duration = Duration::from_secs(90);
/// 缩略图最长边。瀑布流列宽约 150px，2 倍图足够应付高 DPI。
const DEFAULT_MAX_EDGE: u32 = 320;

/// paste.png 最长边上限。给足够大的值即等价原尺寸转码；4096 兼顾
/// IM 输入框粘贴体感与 worker 内存（与 derive-paste-png 默认一致）。
const DEFAULT_PASTE_MAX_EDGE: u32 = 4096;

/// CLI 档位（D37）：fast = 前台高速派生（BELOW_NORMAL，不压 IO 优先级）；
/// background = 维持 D11 idle+背景模式。缺省 background 保持旧观感。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliMode {
    Fast,
    Background,
}

struct Args {
    library: PathBuf,
    max_edge: u32,
    force: bool,
    worker_exe: Option<PathBuf>,
    mode: CliMode,
}

fn main() {
    // D38：日志初始化；stdout 是 PROGRESS/NOTICE 协议通道，日志只进文件。
    logging::init_from_env("derive-thumbs", None, logging::Level::Info);
    logging::info!(
        "derive-thumbs 启动：{:?}",
        std::env::args().collect::<Vec<_>>()
    );
    if let Err(error) = run() {
        logging::error!("derive-thumbs 失败: {error}");
        eprintln!("derive-thumbs 失败: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let db = args.library.join("meta.db");
    if !db.is_file() {
        return Err(format!("{} 下无 meta.db", args.library.display()));
    }
    let store = Store::open(&db).map_err(|e| e.to_string())?;

    // (uuid, 源文件绝对路径, 缩略图绝对路径, 可选 paste.png 绝对路径)
    let mut pending: Vec<(String, PathBuf, PathBuf, Option<PathBuf>)> = Vec::new();
    let mut skipped_kind = 0usize;
    let mut skipped_existing = 0usize;
    let mut missing_source = 0usize;
    let mut paste_needed = 0usize;

    store
        .for_each_asset(|meta| {
            let ext = extension_of(&meta.rel_path);
            // 缩略图能力判定收敛到 crates/media 注册表（综合分析报告
            // 「扩展性缺口 #2」）：新格式只需在 MEDIA_TYPES 加一行，本工具
            // 自动获得一致判定。文本类不在其中——它们有专门的文字瓦片表现。
            if !media::is_thumbnailable(&ext) {
                skipped_kind += 1;
                return;
            }
            let source = join_rel(&args.library, &meta.rel_path);
            if !source.is_file() {
                missing_source += 1;
                return;
            }
            let dest = absolutize(
                &args
                    .library
                    .join(Store::thumbnail_cache_path(&meta.uuid, "png")),
            );
            // 旁挂「上框用 paste.png」的扩展名同样来自 media 注册表：全部
            // 图片（含 PNG 原图）都派生，worker 以 4096 cap 封顶；视频走
            // HDROP（千牛粘贴即发送属已接受的边界，D18），无需派生。
            let paste_dest = if media::is_paste_derivable(&ext) {
                paste_needed += 1;
                Some(absolutize(
                    &args.library.join(Store::paste_png_path(&meta.uuid)),
                ))
            } else {
                None
            };
            // 已有缩略图但尺寸没回写过，仍要重跑——否则布局永远停在占位比例；
            // 同样，paste.png 缺失（旧库未回填）也要重跑，让 jpg 能在千牛上框。
            let paste_ready = paste_dest.as_ref().is_none_or(|p| p.is_file());
            if dest.is_file() && meta.aspect().is_some() && paste_ready && !args.force {
                skipped_existing += 1;
                return;
            }
            pending.push((meta.uuid, source, dest, paste_dest));
        })
        .map_err(|e| e.to_string())?;

    if pending.is_empty() {
        println!(
            "nothing to do: ready={skipped_existing} unsupported={skipped_kind} missing_source={missing_source}"
        );
        return Ok(());
    }

    let exe = resolve_worker_exe(args.worker_exe.as_deref())?;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    let priority = match args.mode {
        CliMode::Fast => PoolPriority::ForegroundBelowNormal,
        CliMode::Background => PoolPriority::BackgroundIdle,
    };
    let pool = WorkerPool::with_priority(&exe, workers, priority);
    // 有界批处理：不同时把上万任务全部塞进 worker 队列，避免内存和 IPC 积压。
    let batch_size = workers * 4;
    let total = pending.len();
    let mut queue: VecDeque<(String, PathBuf, PathBuf, Option<PathBuf>)> =
        pending.drain(..).collect();
    let mut next_job_id: u64 = 0;

    // 进度按 1% 粒度上报，避免上万素材时向 UI 发送过多事件。
    let progress_step = (total / 100).max(1);
    let mut last_reported = 0usize;
    let mut processed = 0usize;
    let mut derived = 0usize;
    let mut paste_derived = 0usize;
    let mut failed = 0usize;
    let mut sized = 0usize;

    // 尺寸批量回写缓冲（D37）：每行独立 UPDATE 在 Windows 上各付一次 fsync，
    // 攒 DIMS_BATCH 行共享一次事务；循环结束前必须冲刷收尾。
    const DIMS_BATCH: usize = 256;
    let mut dims_batch: Vec<(String, u32, u32)> = Vec::new();

    while !queue.is_empty() {
        let batch: Vec<_> = (0..batch_size.min(queue.len()))
            .map(|_| queue.pop_front().expect("已检查队列非空"))
            .collect();
        let mut receivers = Vec::with_capacity(batch.len());
        for (uuid, source, dest, paste_dest) in batch {
            let job_id = next_job_id;
            next_job_id += 1;
            let had_paste = paste_dest.is_some();
            let src_display = source.display().to_string();
            let rx = pool.submit(JobRequest::ThumbnailPng {
                job_id,
                source,
                dest,
                max_edge: args.max_edge,
                // 同一份解码旁挂「上框用」paste.png（D20）。
                paste_dest,
                paste_max_edge: DEFAULT_PASTE_MAX_EDGE,
            });
            receivers.push((uuid, src_display, had_paste, rx));
        }

        for (uuid, src_display, had_paste, rx) in receivers {
            match rx.recv_timeout(JOB_TIMEOUT) {
                Ok(result @ JobResult::Ok { .. }) => {
                    derived += 1;
                    // job 成功即两路输出都已落盘（worker 任何一路失败都返回 Failed）。
                    if had_paste {
                        paste_derived += 1;
                    }
                    match result.dimensions() {
                        Some((w, h)) => {
                            dims_batch.push((uuid.clone(), w, h));
                            if dims_batch.len() >= DIMS_BATCH {
                                flush_dims(&store, &mut dims_batch, &mut sized);
                            }
                        }
                        None => eprintln!("warn {uuid}: worker 未回报像素尺寸"),
                    }
                }
                Ok(JobResult::Failed { reason, .. }) => {
                    // 失败行带上源路径：uuid 对排查素材问题毫无帮助。
                    eprintln!("failed {uuid} ({}): {reason}", src_display);
                    failed += 1;
                }
                Err(e) => {
                    eprintln!(
                        "failed {uuid} ({}): 等待 worker 结果超时/断开 ({e})",
                        src_display
                    );
                    failed += 1;
                }
            }

            processed += 1;
            if processed == total || processed - last_reported >= progress_step {
                println!("PROGRESS\t{processed}\t{total}");
                last_reported = processed;
            }
        }
    }

    // 收尾冲刷残余批次（不足 DIMS_BATCH 也要落盘）。
    flush_dims(&store, &mut dims_batch, &mut sized);
    logging::info!(
        "派生完成 derived={derived} paste={paste_derived}/{paste_needed} sized={sized} failed={failed} ready={skipped_existing} mode={:?}",
        args.mode
    );

    println!(
        "done: derived={derived} paste={paste_derived}/{paste_needed} sized={sized} \
         failed={failed} ready={skipped_existing} unsupported={skipped_kind} \
         missing_source={missing_source} root={}",
        args.library.display()
    );
    if failed > 0 {
        return Err(format!("{failed} 个素材缩略图派生失败"));
    }
    Ok(())
}

/// 冲刷尺寸回写批次：共享一次事务；失败仅告警不中断整批（与单条语义一致）。
fn flush_dims(store: &Store, batch: &mut Vec<(String, u32, u32)>, sized: &mut usize) {
    if batch.is_empty() {
        return;
    }
    let refs: Vec<(&str, u32, u32)> = batch.iter().map(|(u, w, h)| (u.as_str(), *w, *h)).collect();
    match store.set_dimensions_batch(&refs) {
        Ok(n) => *sized += n,
        Err(e) => eprintln!("warn: 批量回写尺寸失败 {e}（{} 行未落）", refs.len()),
    }
    batch.clear();
}

fn parse_args() -> Result<Args, String> {
    let mut library = PathBuf::from("samples/library");
    let mut max_edge = DEFAULT_MAX_EDGE;
    let mut force = false;
    let mut worker_exe = None;
    // 缺省 background 维持旧观感；UI 前台导入显式传 fast（D37）。
    let mut mode = CliMode::Background;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--library" => library = PathBuf::from(it.next().ok_or("--library 缺少值")?),
            "--max-edge" => {
                max_edge = it
                    .next()
                    .ok_or("--max-edge 缺少值")?
                    .parse::<u32>()
                    .map_err(|e| format!("--max-edge 需为正整数: {e}"))?;
                if max_edge == 0 {
                    return Err("--max-edge 必须大于 0".into());
                }
            }
            "--worker-exe" => {
                worker_exe = Some(PathBuf::from(it.next().ok_or("--worker-exe 缺少值")?))
            }
            "--mode" => {
                let v = it.next().ok_or("--mode 缺少值")?;
                mode = match v.as_str() {
                    "fast" => CliMode::Fast,
                    "background" => CliMode::Background,
                    other => return Err(format!("未知 --mode {other}（可用：fast | background）")),
                };
            }
            "--force" => force = true,
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(Args {
        library,
        max_edge,
        force,
        worker_exe,
        mode,
    })
}

/// `rel_path` 以 '/' 分隔存储，必须逐段拼接（整串 join 会产出混合分隔路径）。
fn join_rel(root: &Path, rel_path: &str) -> PathBuf {
    let mut joined = root.to_path_buf();
    for segment in rel_path.split('/').filter(|s| !s.is_empty()) {
        joined.push(segment);
    }
    absolutize(&joined)
}

fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn extension_of(rel_path: &str) -> String {
    Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// worker 二进制定位：显式路径 > 本工具同目录（cargo 产物布局）。
/// `WorkerPool::with_size` 依赖 `CARGO_BIN_EXE_*`，生产路径不可用。
fn resolve_worker_exe(explicit: Option<&Path>) -> Result<PathBuf, String> {
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
