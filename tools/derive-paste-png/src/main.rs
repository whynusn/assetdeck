//! 派生「上框用 PNG」：为库内图片生成 `objects/<uuid>/paste.png`。
//!
//! 为什么需要这道工序：千牛一类目标把 `CF_HDROP` 粘贴当「直接发送文件」，
//! 协商只能返回 `WouldSend` 从而退化为仅复制。给图片旁挂一份等价 PNG
//! （worker 以 4096 cap 派生，PNG 原图同样派生），上框链路就能走 `CF_PNG`
//! 落进输入框而不发送，且载荷尺寸被封顶。
//!
//! 注：GUI 导入管线已通过 `derive-thumbs`（同一份解码旁挂 paste.png，D20）自动
//! 产出该文件，本工具保留为无 GUI 场景下的命令行回填/强制重生成入口。
//!
//! 分层纪律（D11）：本工具不依赖 `image`，全部解码提交给 `decode-worker` 子进程。
//!
//! 用法：
//! ```text
//! derive-paste-png [--library <root>] [--max-edge <px>] [--force] [--worker-exe <path>]
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use store::Store;
use worker::{JobRequest, JobResult, WorkerPool};

/// 单张图片的解码预算：worker 是 idle 优先级，给足余量。
const JOB_TIMEOUT: Duration = Duration::from_secs(60);
/// 默认最长边上限。给足够大的值即等价原尺寸转码；4096 兼顾输入框粘贴体感与内存。
const DEFAULT_MAX_EDGE: u32 = 4096;
/// 需要派生的图片扩展名：全部图片（含 PNG 原图，统一 4096 cap 封顶）。
const DERIVABLE: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];

struct Args {
    library: PathBuf,
    max_edge: u32,
    force: bool,
    worker_exe: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("derive-paste-png 失败: {error}");
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

    // (uuid, 源文件绝对路径, 派生目标绝对路径)
    let mut pending: Vec<(String, PathBuf, PathBuf)> = Vec::new();
    let mut skipped_existing = 0usize;
    let mut skipped_kind = 0usize;
    let mut missing_source = 0usize;

    store
        .for_each_asset(|meta| {
            let ext = extension_of(&meta.rel_path);
            if !DERIVABLE.contains(&ext.as_str()) {
                skipped_kind += 1;
                return;
            }
            let source = join_rel(&args.library, &meta.rel_path);
            if !source.is_file() {
                missing_source += 1;
                return;
            }
            // 绝对化：worker 是独立子进程，相对 dest 会按它自己的工作目录解析。
            let dest = absolutize(&args.library.join(Store::paste_png_path(&meta.uuid)));
            if dest.is_file() && !args.force {
                skipped_existing += 1;
                return;
            }
            pending.push((meta.uuid, source, dest));
        })
        .map_err(|e| e.to_string())?;

    if pending.is_empty() {
        println!(
            "nothing to do: derived={skipped_existing} non_image={skipped_kind} missing_source={missing_source}"
        );
        return Ok(());
    }

    let exe = resolve_worker_exe(args.worker_exe.as_deref())?;
    let pool = WorkerPool::with_exe(&exe, 2);
    let mut receivers = Vec::with_capacity(pending.len());
    for (job_id, (uuid, source, dest)) in pending.iter().enumerate() {
        let rx = pool.submit(JobRequest::ThumbnailPng {
            job_id: job_id as u64,
            source: source.clone(),
            dest: dest.clone(),
            max_edge: args.max_edge,
            // 本工具只产出 paste.png 单路输出（dest 即 paste 路径）。
            paste_dest: None,
            paste_max_edge: args.max_edge,
        });
        receivers.push((uuid.clone(), rx));
    }

    let mut derived = 0usize;
    let mut failed = 0usize;
    for (uuid, rx) in receivers {
        match rx.recv_timeout(JOB_TIMEOUT) {
            Ok(JobResult::Ok { payload, .. }) => {
                println!("derived {uuid} => {payload}");
                derived += 1;
            }
            Ok(JobResult::Failed { reason, .. }) => {
                eprintln!("failed {uuid}: {reason}");
                failed += 1;
            }
            Err(e) => {
                eprintln!("failed {uuid}: 等待 worker 结果超时/断开 ({e})");
                failed += 1;
            }
        }
    }

    println!(
        "done: derived={derived} failed={failed} already={skipped_existing} non_image={skipped_kind} missing_source={missing_source} root={}",
        args.library.display()
    );
    if failed > 0 {
        return Err(format!("{failed} 个素材派生失败"));
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut library = PathBuf::from("samples/library");
    let mut max_edge = DEFAULT_MAX_EDGE;
    let mut force = false;
    let mut worker_exe = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--library" => {
                library = PathBuf::from(it.next().ok_or("--library 缺少值")?);
            }
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
                worker_exe = Some(PathBuf::from(it.next().ok_or("--worker-exe 缺少值")?));
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
/// `WorkerPool::with_size` 依赖 `CARGO_BIN_EXE_*`，生产路径不可用，故必须显式给 exe。
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
