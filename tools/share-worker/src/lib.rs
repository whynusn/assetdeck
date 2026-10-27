//! share-worker 库面（D80）：模块拆分供 bin 与集成测试共用。
//!
//! 进程模型与协议总览见各模块文档；二进制入口在 main.rs。

pub mod dial;
pub mod engine;
pub mod lines;
pub mod mdns;
pub mod proto;
pub mod sync;

/// 引擎层无日志门面依赖时的留痕出口（main.rs 装配真实日志，测试丢弃）。
pub fn log_line(line: String) {
    if let Some(hook) = LOG_HOOK.lock().unwrap().as_ref() {
        hook(line);
    }
}

/// 日志钩子类型（main.rs 装配真实日志，测试可替换观察）。
type LogHook = std::sync::Arc<dyn Fn(String) + Send + Sync>;

static LOG_HOOK: std::sync::Mutex<Option<LogHook>> = std::sync::Mutex::new(None);

pub fn set_log_hook(hook: LogHook) {
    *LOG_HOOK.lock().unwrap() = Some(hook);
}
