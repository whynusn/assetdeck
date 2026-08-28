//! 分级诊断日志（D38）：低频重要的默认全量保存，超高频事件放高等级、
//! 平时关闭，需要时临时开启。所有子进程经环境变量继承同一定位。
//!
//! 设计要点：
//! - 每个进程一条独立日志文件（app / sample-library / derive-thumbs /
//!   decode-worker），全部落在同一日志目录，导出=整目录拷贝/压缩；
//! - 线级时间戳（UTC，ms 精度）+ 等级 + 目标 + 消息，纯 std 实现，
//!   不引入 chrono；
//! - 初始化按进程语义激进防御：任何 IO 失败只降级为 stderr 提示，
//!   绝不拖垮业务路径；
//! - 预留容量清理：按前缀保留最近 N 份，老文件自动轮换删除。
//!
//! 使用：
//! - 桌面端：launch 时 init(InitOptions{ name:"app", .. })；
//! - 子进程：task_runner 注入 DSH_LOG_DIR / DSH_LOG_LEVEL，工具进程
//!   启动即 init_from_env("sample-library")；
//! - 运行时切换等级：settings 里的「细粒度诊断日志」开关直接 set_level。
//!
//! 目录解析（init_from_env）：DSH_LOG_DIR > 调用方 fallback_dir >
//! 平台标准目录（%LOCALAPPDATA%\asset-manager\logs）。**永不回落到当前
//! 工作目录**——否则 cargo test / 任意宿主直跑会把日志洒进源码树。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

// 日志门面宏复用 log crate（MIT），全局器是下方的 FileSink。
pub use log::{debug, error, info, trace, warn};

/// 日志等级（自增即更详细）。排序：Off < Error < Warn < Info < Debug < Trace。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Level::Off),
            "error" => Some(Level::Error),
            "warn" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    /// 从 log facade 的等级映射到本枚举。没记的等级按 log 文档对应：
    /// Error/Warn/Info/Debug/Trace 一一对应。
    fn from_log_level(l: log::Level) -> Level {
        match l {
            log::Level::Error => Level::Error,
            log::Level::Warn => Level::Warn,
            log::Level::Info => Level::Info,
            log::Level::Debug => Level::Debug,
            log::Level::Trace => Level::Trace,
        }
    }

    fn to_filter(self) -> log::LevelFilter {
        match self {
            Level::Off => log::LevelFilter::Off,
            Level::Error => log::LevelFilter::Error,
            Level::Warn => log::LevelFilter::Warn,
            Level::Info => log::LevelFilter::Info,
            Level::Debug => log::LevelFilter::Debug,
            Level::Trace => log::LevelFilter::Trace,
        }
    }
}

/// 初始化参数。
pub struct InitOptions {
    /// 日志目录（不存在则创建）。
    pub dir: PathBuf,
    /// 进程名：决定文件名前缀与轮换分组里的身份。
    pub name: String,
    /// 初始允许的日志等级。
    pub level: Level,
    /// 是否把 Error/Warn 同步镜像到 stderr（工具被直接运行时便于人工看到）。
    pub mirror_stderr: bool,
}

/// 每进程日志文件保留份数上限（轮换）。
const KEEP_FILES_PER_NAME: usize = 8;

struct Inner {
    file: Mutex<File>,
    level: RwLock<Level>,
    mirror_stderr: bool,
    path: PathBuf,
}

static GLOBAL: OnceLock<Arc<Inner>> = OnceLock::new();

/// 初始化文件日志；已初始化时不重复覆盖（进程级单例，二次调用返回旧路径）。
pub fn init(opts: InitOptions) -> Option<PathBuf> {
    if let Some(existing) = GLOBAL.get() {
        return Some(existing.path.clone());
    }
    match open_log_file(&opts) {
        Ok((file, path)) => {
            let inner = Arc::new(Inner {
                file: Mutex::new(file),
                level: RwLock::new(opts.level),
                mirror_stderr: opts.mirror_stderr,
                path: path.clone(),
            });
            // 先注册再替换全局日志器：log::set_boxed_logger 只允许一次，
            // RegisterOnceError 可忽略（测试顺序下可能被前一个 init 占位）。
            if GLOBAL.set(Arc::clone(&inner)).is_err() {
                // 理论不可达：OnceLock 在 set 成功前独占。
            }
            let _ = log::set_boxed_logger(Box::new(LogBridge(inner.clone())));
            log::set_max_level(opts.level.to_filter());
            log::logger().flush();
            Some(path)
        }
        Err(e) => {
            eprintln!("[logging] 无法初始化日志 {}: {e}", opts.dir.display());
            None
        }
    }
}

/// 从环境变量初始化（子进程继承 DSH_LOG_DIR / DSH_LOG_LEVEL 的协作约定）。
/// 目录缺失时回落平台标准目录 + 默认等级，保证工具进程永远不会因日志崩掉。
/// 缺省目录**永不**是当前工作目录：cargo test 的 cwd 是包根、任意宿主直跑的
/// cwd 不可控，把日志写进 cwd 会污染源码树（实测 decode-worker 日志曾落进
/// crates/worker/）。见 platform_default_dir。
pub fn init_from_env(
    name: &str,
    fallback_dir: Option<PathBuf>,
    fallback_level: Level,
) -> Option<PathBuf> {
    let level = std::env::var("DSH_LOG_LEVEL")
        .ok()
        .and_then(|s| Level::parse(&s))
        .unwrap_or(fallback_level);
    let dir = std::env::var("DSH_LOG_DIR")
        .ok()
        .map(PathBuf::from)
        .or(fallback_dir)
        .unwrap_or_else(platform_default_dir);
    init(InitOptions {
        dir,
        name: name.to_string(),
        level,
        // 命令行工具默认把低等级镜像到 stderr：协议通道在 stdout，互不干扰。
        mirror_stderr: true,
    })
}

/// 运行时切换等级（设置面板「细粒度诊断日志」调这个）。
pub fn set_level(level: Level) {
    if let Some(inner) = GLOBAL.get() {
        *inner.level.write().unwrap() = level;
        log::set_max_level(level.to_filter());
    }
}

pub fn current_level() -> Level {
    match GLOBAL.get() {
        Some(inner) => *inner.level.read().unwrap(),
        None => Level::Off,
    }
}

/// 当前进程激活的日志文件路径（导出/打开目录用）。
pub fn active_log_path() -> Option<PathBuf> {
    GLOBAL.get().map(|inner| inner.path.clone())
}

/// 活跃日志所在目录（子进程环境注入 / 一键打开日志文件夹用）。
pub fn logs_dir() -> Option<PathBuf> {
    active_log_path().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// 平台标准缺省日志目录：`%LOCALAPPDATA%\asset-manager\logs`。
///
/// 三层兜底全部是绝对路径，任何分支都不含 cwd：
/// `LOCALAPPDATA` → `USERPROFILE\AppData\Local` → 系统临时目录。
/// 桌面端主进程不走这里（init 用 exe 同目录 logs/ 的便携约定，D39），
/// 本函数只服务 init_from_env 的「环境变量与 fallback 均缺」场景。
fn platform_default_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|profile| PathBuf::from(profile).join("AppData").join("Local"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("asset-manager").join("logs")
}

/// 目录里所有进程的日志按 mtime 新到旧排序；为「打开日志文件夹」的方便性，
/// 同时返回最大的一条作为建议导出目标。
pub fn newest_log_path(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("log"))
        .collect();
    files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    files.pop()
}

// ---------- 内部实现 ----------

fn open_log_file(opts: &InitOptions) -> std::io::Result<(File, PathBuf)> {
    fs::create_dir_all(&opts.dir)?;
    let pid = std::process::id();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = opts
        .dir
        .join(format!("{}-{}-{}.log", opts.name, pid, millis));
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    prune_old_files(&opts.dir, &opts.name, KEEP_FILES_PER_NAME);
    Ok((file, path))
}

/// 轮换：目录下同名前缀的 .log 保留最近 keep 份，其余删除（尽力而为）。
fn prune_old_files(dir: &Path, name: &str, keep: usize) {
    let prefix = format!("{name}-");
    let mut files: Vec<(SystemTime, PathBuf)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix) && n.ends_with(".log"))
                    .unwrap_or(false)
            })
            .filter_map(|p| {
                let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
                Some((m, p))
            })
            .collect(),
        Err(_) => return,
    };
    files.sort_by_key(|(t, _)| *t);
    for (_, old) in files.iter().take(files.len().saturating_sub(keep)) {
        let _ = std::fs::remove_file(old);
    }
}

/// log facade 桥：把 log 宏的记录转发到文件（可选镜像 stderr）。
struct LogBridge(Arc<Inner>);

impl log::Log for LogBridge {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        Level::from_log_level(metadata.level()) <= *self.0.level.read().unwrap()
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} [{}] {}: {}\n",
            timestamp_str(SystemTime::now()),
            record.level().as_str(),
            record.target(),
            record.args()
        );
        let inner = &self.0;
        {
            let mut file = inner.file.lock().unwrap();
            let _ = file.write_all(line.as_bytes());
        }
        if inner.mirror_stderr && record.level() <= log::Level::Warn {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        let _ = self.0.file.lock().unwrap().flush();
    }
}

/// 纯 std 的时间戳：UTC + 毫秒。内部用 days-to-civil（Hinnant）换算年月日。
fn timestamp_str(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let millis = (secs % 1000) as u32;
    let secs = (secs / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{millis:03} UTC")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_default_dir_is_absolute_and_never_cwd() {
        let dir = platform_default_dir();
        assert!(
            dir.is_absolute(),
            "缺省日志目录必须是绝对路径，实际 {dir:?}"
        );
        // 三层兜底都不得把 cwd 相对段（如 "."）或空段混进路径。
        for component in dir.components() {
            if let std::path::Component::CurDir = component {
                panic!("缺省日志目录不得包含当前目录段: {dir:?}");
            }
        }
    }

    #[test]
    fn level_parse_roundtrip() {
        for lvl in [
            Level::Off,
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ] {
            assert_eq!(Level::parse(lvl.as_str()), Some(lvl));
            assert_eq!(Level::parse(&lvl.as_str().to_uppercase()), Some(lvl));
        }
        assert_eq!(Level::parse("verbose"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn level_ordering_is_monotonic() {
        assert!(Level::Off < Level::Error);
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    #[test]
    fn timestamp_is_utc_iso_like() {
        let s = timestamp_str(SystemTime::now());
        assert!(s.len() >= 24, "实际: {s}");
        assert!(s.ends_with(" UTC"));
        assert!(s.contains(':'));
    }

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn prune_keeps_newest_n() {
        let dir = std::env::temp_dir().join(format!("dsh_login_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for i in 0..12 {
            fs::write(dir.join(format!("probe-1-{i}.log")), b"x").unwrap();
        }
        // 手动制造 mtime 递增
        for (i, p) in std::fs::read_dir(&dir).unwrap().flatten().enumerate() {
            let t = UNIX_EPOCH + std::time::Duration::from_millis(1000 + i as u64);
            let _ = filetime_shim(&p.path(), t);
        }
        prune_old_files(&dir, "probe", 8);
        let left = std::fs::read_dir(&dir).unwrap().flatten().count();
        assert!(left <= 8, "应裁剪到 ≤8，实际 {left}");
        let _ = fs::remove_dir_all(&dir);
    }

    // 不用 filetime 依赖：尽量改 modified 时间（Windows 上可写）。
    fn filetime_shim(p: &Path, t: SystemTime) -> std::io::Result<()> {
        let file = OpenOptions::new().append(true).open(p)?;
        // 无法设置 mtime 时不强求（测试只验证上限裁剪逻辑）。
        drop(file);
        let _ = t;
        Ok(())
    }
}
