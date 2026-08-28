//! 导入素材到真实的 .library 库，或把 .library 导出为素材包。
//! 默认用法：`sample-library [inbox] [out]`；导出：`sample-library export <root> <out.emo>`。
//! 包格式支持通过 `packages::PackageRegistry` 注册（当前内置目录与千牛 .emo），
//! 新增 zip/eagle 等格式不再改动本文件（综合分析报告「三.1」）。

mod packages;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use library::{CopyState, EnqueueOutcome, ImportMode, ImportRequest, Library};
use packages::{DirectoryReader, EmoReader, EmoWriter, ImportedAsset, PackageRegistry};

fn main() {
    // D38：日志初始化（子进程由 UI 注入 DSH_LOG_DIR/DSH_LOG_LEVEL；直跑回落）。
    // stdout 是协议通道，日志只进文件 + 低等级镜像 stderr，绝不污染协议。
    logging::init_from_env("sample-library", None, logging::Level::Info);
    logging::info!("sample-library 启动：{:?}", std::env::args().collect::<Vec<_>>());
    if let Err(error) = run() {
        logging::error!("sample-library failed: {error}");
        eprintln!("sample-library failed: {error}");
        std::process::exit(1);
    }
}

/// CLI 档位参数：--mode fast（默认）| background。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliMode {
    Fast,
    Background,
}

impl CliMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "fast" => Ok(CliMode::Fast),
            "background" => Ok(CliMode::Background),
            other => Err(format!("未知 --mode {other}（可用：fast | background）")),
        }
    }

    fn to_library_mode(self) -> ImportMode {
        match self {
            CliMode::Fast => ImportMode::Fast,
            CliMode::Background => ImportMode::Background,
        }
    }
}

struct RunOptions {
    inbox: PathBuf,
    out: PathBuf,
    mode: CliMode,
}

fn parse_run_options(args: &[String]) -> Result<RunOptions, String> {
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut mode = CliMode::Fast;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--mode" => {
                let v = it.next().ok_or("--mode 缺少值")?;
                mode = CliMode::parse(v)?;
            }
            _ => {
                // 兼容旧调用方按位置传 [inbox] [out]；export 分支已提前拦截。
                positional.push(PathBuf::from(arg));
            }
        }
    }
    Ok(RunOptions {
        inbox: positional.first().cloned().unwrap_or_else(|| PathBuf::from("samples/inbox")),
        out: positional.get(1).cloned().unwrap_or_else(|| PathBuf::from("samples/library")),
        mode,
    })
}

fn run() -> Result<(), String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("export") {
        if raw.len() < 3 {
            return Err("usage: sample-library export <library-root> <output.emo>".into());
        }
        return export_package(Path::new(&raw[1]), Path::new(&raw[2]));
    }

    let opts = parse_run_options(&raw)?;
    import_package(&opts.inbox, &opts.out, opts.mode)
}

/// 导入路径：registry.reader_for(inbox) → read → 有界并发 enqueue →
/// 最后删除 read 返回的 cleanup 临时目录。
fn import_package(inbox: &Path, out: &Path, mode: CliMode) -> Result<(), String> {
    let started = Instant::now();

    let mut registry = PackageRegistry::new();
    registry
        .register_reader(Box::new(EmoReader::new()))
        .register_reader(Box::new(DirectoryReader));

    let reader = registry
        .reader_for(inbox)
        .ok_or_else(|| format!("不支持导入的来源: {}", inbox.display()))?;
    let extract_started = Instant::now();
    let package = reader.read(inbox).map_err(|e| e.to_string())?;
    let extract_elapsed = extract_started.elapsed();

    let result = run_import(&package.assets, out, mode);
    let total_elapsed = started.elapsed();

    if let Some(cleanup) = package.cleanup {
        let _ = std::fs::remove_dir_all(cleanup);
    }

    // 阶段耗时诊断行（D37）：解析器只认 PROGRESS/NOTICE 前缀，本行直落 stdout
    // 不干扰协议；需要定位瓶颈时看这一行就知道包解压占了多大比例。
    println!(
        "timing	extract={:?} total={:?} assets={}",
        extract_elapsed,
        total_elapsed,
        package.assets.len()
    );
    logging::info!(
        "导入阶段结束 mode={mode:?} assets={} extract={:?} total={:?} 结果={}",
        package.assets.len(),
        extract_elapsed,
        total_elapsed,
        result.as_ref().map(|_| "ok").unwrap_or("err")
    );

    result
}

/// 单文件等待终态的超时：老实现 15s 是逐个串行语境下的妥协；并发流水线里
/// 允许大视频长时间拷贝，与 derive-thumbs 的单任务预算保持同一量级。
const TICKET_TIMEOUT: Duration = Duration::from_secs(90);
/// 失败清单内存上限：超过后只留条数，明细由「等 N 个」汇总兜底，
/// 数万条打包里几十万个坏文件也不会撑爆聚合内存。
const FAILURE_LIST_CAP: usize = 256;
/// 进度行粒度：1%（至少每件必报一次收尾）。
fn progress_step(total: usize) -> usize {
    (total / 100).max(1)
}

/// 导入汇总：全部跨线程原子/互斥访问。
struct Summary {
    imported: AtomicUsize,
    skipped: AtomicUsize,
    failed_total: AtomicUsize,
    failures: Mutex<Vec<String>>,
    done: AtomicUsize,
    /// 进度行步长（1% 粒度），由 run_import 按总量算好注入。
    step: usize,
}

impl Summary {
    fn record_failure(&self, source: &Path, reason: &str) {
        println!("failed {} : {reason}", source.display());
        self.failed_total.fetch_add(1, Ordering::Relaxed);
        let mut list = self.failures.lock().unwrap();
        if list.len() < FAILURE_LIST_CAP {
            list.push(format!("{}：{reason}", source.display()));
        }
    }
}

/// 有界并发导入主体（D37）：W 个工作线程从有界通道领任务——每个任务内
/// 完成 解码→pHash→去重→入队拷贝→等终态 全链路；解码（CPU 密集）在多
/// 线程重叠进行，拷贝走库内的并发磁盘线程池，两侧天然流水线化。整体
/// 未完成数量受通道容量 + 库背压双重钳制，不会积压到内存失守。
fn run_import(assets: &[ImportedAsset], out: &Path, mode: CliMode) -> Result<(), String> {
    let library = Arc::new(
        Library::open_with_mode(mode.to_library_mode(), out).map_err(|e| e.to_string())?
    );
    let total = assets.len();
    if total == 0 {
        let count = library.store().all_assets_count().map_err(|e| e.to_string())?;
        println!("done: imported=0 skipped=0 failed=0 total={count} root={}", out.display());
        return Ok(());
    }

    let workers = match mode {
        CliMode::Fast => {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2);
            cores.clamp(2, 8).min(library.capacity().max(1))
        }
        CliMode::Background => 2.min(library.capacity().max(1)),
    };

    let step = progress_step(total);
    let summary = Arc::new(Summary {
        imported: AtomicUsize::new(0),
        skipped: AtomicUsize::new(0),
        failed_total: AtomicUsize::new(0),
        failures: Mutex::new(Vec::new()),
        done: AtomicUsize::new(0),
        step,
    });

    // 有界任务通道：容量即背压窗口，生产端塞满就阻塞，比手工信号量直观。
    let (task_tx, task_rx) =
        std::sync::mpsc::sync_channel::<&ImportedAsset>(library.capacity().max(workers));
    let task_rx = Arc::new(Mutex::new(task_rx));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let rx = Arc::clone(&task_rx);
            let library = Arc::clone(&library);
            let summary = Arc::clone(&summary);
            scope.spawn(move || loop {
                // recv 空闲返回 Err 视作关闸退场（生产端 drop 即发生）。
                let item = {
                    let guard = rx.lock().unwrap();
                    guard.recv()
                };
                match item {
                    Ok(asset) => import_one(&library, &summary, asset),
                    Err(_) => break,
                }
            });
        }

        for asset in assets {
            if task_tx.send(asset).is_err() {
                break; // 消费端全灭（异常退出场景），停止投递
            }
        }
        drop(task_tx);
        // scope 结束隐式 join 所有 worker：此时所有票均已终态。
    });

    let imported = summary.imported.load(Ordering::Relaxed);
    let skipped = summary.skipped.load(Ordering::Relaxed);
    let failed_total = summary.failed_total.load(Ordering::Relaxed);
    let failures = summary.failures.lock().unwrap();

    // 进度收尾必须打满：节流可能让最后一段停在 99%。
    println!("PROGRESS\t{total}\t{total}");

    let count = library
        .store()
        .all_assets_count()
        .map_err(|e| e.to_string())?;
    println!(
        "done: imported={imported} skipped={skipped} failed={failed_total} total={count} root={}",
        out.display()
    );
    if failed_total > 0 || !failures.is_empty() {
        // NOTICE 行：UI 侧 task_runner 解析后弹提示，失败不再「默默吞掉」。
        println!("NOTICE\t有 {failed_total} 个素材导入失败：{}", summarize_failures(&failures));
    }
    Ok(())
}

/// 单素材处理：enqueue → 等终态 → 记账。任何形态的失败都只记账继续。
fn import_one(library: &Library, summary: &Summary, asset: &ImportedAsset) {
    let request = ImportRequest {
        source: asset.source.clone(),
        category: asset.category.clone(),
        tags: asset.tags.clone(),
    };
    let outcome = library.enqueue(request);
    match outcome {
        Ok(EnqueueOutcome::Ticket(ticket)) => {
            match library.wait_terminal(&ticket, TICKET_TIMEOUT) {
                Some(CopyState::Done) => {
                    match library.store().get_asset(&ticket.uuid) {
                        Ok(Some(meta)) => {
                            println!(
                                "imported {} => {} ({} bytes)",
                                asset.source.display(),
                                meta.rel_path,
                                meta.size_bytes
                            );
                            summary.imported.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(None) => {
                            summary.record_failure(&asset.source, "拷贝完成后元数据缺失");
                        }
                        Err(e) => {
                            summary.record_failure(
                                &asset.source,
                                &format!("查询元数据失败：{e}"),
                            );
                        }
                    }
                }
                Some(CopyState::Failed(reason)) => {
                    // 拷贝/落库失败已在库侧回滚（无半成品），这里只记账继续。
                    summary.record_failure(&asset.source, &reason);
                }
                other @ (Some(CopyState::Pending) | Some(CopyState::Copying { .. }) | None) => {
                    let _ = other;
                    summary.record_failure(&asset.source, "等待拷贝完成超时或状态异常");
                }
            }
        }
        Ok(EnqueueOutcome::Duplicate { existing_uuid }) => {
            println!("duplicate {existing_uuid} <= {}", asset.source.display());
            summary.skipped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(EnqueueOutcome::Unsupported { reason }) => {
            summary.record_failure(&asset.source, &reason);
        }
        Ok(EnqueueOutcome::Backpressure) => {
            summary.record_failure(&asset.source, "导入队列背压");
        }
        Err(e) => {
            summary.record_failure(&asset.source, &format!("{e}"));
        }
    }

    let done = summary.done.fetch_add(1, Ordering::Relaxed) + 1;
    // 逐件与收尾保证进度可见；中间按步长节流避免数万行管道噪音。
    if done == usize::MAX || done % summary.step == 0 {
        println!("PROGRESS\t{done}\t{}", summary.done.load(Ordering::Relaxed).max(done));
    }
}

/// 汇总失败清单给 NOTICE 行：最多点名 3 个路径，其余以「等 N 个」收尾；
/// 明细超出 [FAILURE_LIST_CAP] 时退化为纯计数。
fn summarize_failures(failures: &[String]) -> String {
    const MAX_NAMED: usize = 3;
    if failures.is_empty() {
        return "明细已省略".to_string();
    }
    if failures.len() <= MAX_NAMED {
        return failures.join("；");
    }
    format!("{} 等", failures[..MAX_NAMED].join("；"))
}

/// 导出路径：registry.writer_for(output) → write(&library, output)。
fn export_package(root: &Path, output: &Path) -> Result<(), String> {
    let library = Library::open(root).map_err(|e| e.to_string())?;
    let mut registry = PackageRegistry::new();
    registry.register_writer(Box::new(EmoWriter::new(root.to_path_buf())));
    let writer = registry
        .writer_for(output)
        .ok_or_else(|| format!("不支持的导出目标: {}", output.display()))?;
    writer.write(&library, output).map_err(|e| e.to_string())
}


