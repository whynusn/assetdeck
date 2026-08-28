//! 解码 worker 进程池：IPC 协议、监督重启、背压。
//!
//! D11 红线：UI 主进程永不执行缩略图生成/视频抽帧/pHash 计算，
//! 这些全部隔离在本 crate 管理的独立 worker 进程中。
//!
//! 池模型（design.md）：
//! - 每个 worker 一对线程——writer（stdin 写入）+ reader（逐行读响应并按 job_id 路由）；
//! - 监督：reader 检测 EOF/解析失败即判定该进程死亡，其 pending 全部立即以
//!   `Failed` 回报（不重试），随后在重启预算内拉起替补；预算耗尽进入 degraded；
//! - IO idle 优先级：宿主侧设 `IDLE_PRIORITY_CLASS`（可实测读回），worker 入口
//!   自设线程级背景模式压低内存/IO 优先级。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    GetPriorityClass, OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    IDLE_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub mod protocol;

pub use protocol::{Envelope, JobRequest, JobResult, PROTOCOL_VERSION};

/// 每池重启上限：超过后进入 degraded，拒绝新任务而非陷入无限快速重启循环。
const MAX_RESTARTS_PER_POOL: usize = 3;

/// 单个 worker 进程槽位。
struct WorkerSlot {
    /// NDJSON 行通道，writer 线程消费后写入子进程 stdin。
    writer: Sender<String>,
    pid: u32,
    /// 保活进程句柄；drop 只关句柄不杀进程——子进程经 stdin EOF 自行 exit(0)。
    _child: Child,
}

struct PoolState {
    /// None = 该槽位死亡且不再替补（仅出现在降级后）。
    slots: Vec<Option<WorkerSlot>>,
    /// pending 表：(worker 下标, job_id) → 结果回传 sender。job_id 由调用方保证唯一。
    pending: HashMap<(usize, u64), Sender<JobResult>>,
    rr: usize,
    restarts: usize,
    degraded: bool,
}

struct PoolInner {
    exe: PathBuf,
    priority: PoolPriority,
    /// 显式注入的 worker 日志目录（DSH_LOG_DIR）；None = 依赖环境继承。
    log_dir: Option<PathBuf>,
    state: Mutex<PoolState>,
    shutdown: AtomicBool,
}

/// worker 进程调度档位（D37）：前台高速导入时放弃 idle 压制换取吞吐；
/// 后台/浏览补图维持 D11 的 idle 语义不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolPriority {
    /// D11 默认：IDLE_PRIORITY_CLASS，worker 内部线程自设背景模式（IO/内存
    /// 优先级 VERY_LOW）。供「不惊扰用户」的场景。
    BackgroundIdle,
    /// 用户显式发起的导入：BELOW_NORMAL_PRIORITY_CLASS + 不启用内部背景模式，
    /// 页缓存不被立即驱逐，海量小文件的随机 IO 不再被压到最低档。
    ForegroundBelowNormal,
}

impl PoolPriority {
    fn win_priority_class(self) -> u32 {
        match self {
            PoolPriority::BackgroundIdle => IDLE_PRIORITY_CLASS,
            PoolPriority::ForegroundBelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        }
    }
}

/// 解码 worker 进程池。
///
/// UI 主进程只经本结构提交任务；所有解码发生在独立子进程中。
pub struct WorkerPool {
    inner: Arc<PoolInner>,
}

impl WorkerPool {
    /// 测试/默认构造：经 cargo 的 `CARGO_BIN_EXE_decode-worker` 定位 worker 二进制。
    /// 生产装配请用 [`WorkerPool::with_exe`] 显式指定路径。默认后台 idle 档。
    pub fn with_size(size: usize) -> Self {
        let exe = std::env::var("CARGO_BIN_EXE_decode-worker")
            .expect("CARGO_BIN_EXE_decode-worker 未设置：请在 cargo 测试环境运行，或改用 with_exe");
        Self::with_exe(Path::new(&exe), size)
    }

    /// 便携/生产构造：从当前可执行文件同目录找 `decode-worker.exe`（idle 档）。
    ///
    /// 这是打包后双击主 exe 的路径约定；找不到时仍然会构造池，但所有槽位
    /// 以初始拉起失败进入 `degraded`，调用方可通过 [`WorkerPool::degraded`] 感知。
    pub fn with_sibling_exe(size: usize) -> Self {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|dir| dir.join("decode-worker.exe")))
            .unwrap_or_else(|| PathBuf::from("decode-worker.exe"));
        Self::with_exe(&exe, size)
    }

    /// 以显式二进制路径构造池（D11 后台 idle 档）。启动即拉满 n 个子进程，
    /// n 被钳制到 CPU 核数（红线：池大小按核数封顶），下限 1。
    pub fn with_exe(exe: &Path, size: usize) -> Self {
        Self::with_priority(exe, size, PoolPriority::BackgroundIdle)
    }

    /// 显式档位构造（D37）：前台导入走 ForegroundBelowNormal。
    pub fn with_priority(exe: &Path, size: usize, priority: PoolPriority) -> Self {
        Self::with_priority_and_log_dir(exe, size, priority, None)
    }

    /// 显式注入日志目录的构造：每个 worker（含替补拉起）spawn 时都带上
    /// DSH_LOG_DIR，不依赖环境继承——测试/独立宿主可把日志钉到指定目录
    /// （logging 的目录约定：DSH_LOG_DIR > fallback > 平台标准目录，永不 cwd）。
    /// 传 None 与 [`WorkerPool::with_priority`] 等价。
    pub fn with_priority_and_log_dir(
        exe: &Path,
        size: usize,
        priority: PoolPriority,
        log_dir: Option<PathBuf>,
    ) -> Self {
        let cpus = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let size = size.max(1).min(cpus);

        let mut slots = Vec::with_capacity(size);
        let mut readers: Vec<(usize, ChildStdout)> = Vec::new();
        let mut degraded = false;
        for idx in 0..size {
            match spawn_worker(exe, priority, log_dir.as_deref()) {
                Ok((slot, stdout)) => {
                    slots.push(Some(slot));
                    readers.push((idx, stdout));
                }
                // 拉起失败按降级处理：池不可用但主进程不受影响。
                Err(_) => {
                    degraded = true;
                    slots.push(None);
                }
            }
        }

        let inner = Arc::new(PoolInner {
            exe: exe.to_path_buf(),
            priority,
            log_dir,
            state: Mutex::new(PoolState {
                slots,
                pending: HashMap::new(),
                rr: 0,
                restarts: 0,
                degraded,
            }),
            shutdown: AtomicBool::new(false),
        });
        for (idx, stdout) in readers {
            start_reader(&inner, idx, stdout);
        }
        WorkerPool { inner }
    }

    /// 提交一个任务，返回结果接收端。job_id 由调用方保证唯一。
    pub fn submit(&self, req: JobRequest) -> Receiver<JobResult> {
        let job_id = req.job_id();
        let (tx, rx) = channel();
        let mut st = self.inner.state.lock().unwrap();

        if st.degraded {
            drop(st);
            let _ = tx.send(JobResult::Failed {
                job_id,
                reason: "worker 池已降级（重启预算耗尽），任务被拒绝".into(),
            });
            return rx;
        }

        // 轮询选槽。
        let count = st.slots.len().max(1);
        let idx = st.rr % count;
        st.rr = (st.rr + 1) % count;

        // 先取 writer 通道快照（clone），避免持 slots 借用时再可变借用 pending 表。
        // 用 get 防御：池关闭中槽位可能已被清空（Drop 竞态）。
        let writer = match st.slots.get(idx).and_then(|s| s.as_ref()) {
            Some(slot) => slot.writer.clone(),
            None => {
                drop(st);
                let _ = tx.send(JobResult::Failed {
                    job_id,
                    reason: format!("worker #{idx} 不可用"),
                });
                return rx;
            }
        };

        let line = serde_json::to_string(&Envelope::request(req)).expect("协议类型序列化不可失败");
        // 先登记 pending 再发送：防响应先于注册到达而丢路由。
        st.pending.insert((idx, job_id), tx);
        if writer.send(line).is_err() {
            if let Some(tx) = st.pending.remove(&(idx, job_id)) {
                let _ = tx.send(JobResult::Failed {
                    job_id,
                    reason: "worker 写入通道断开".into(),
                });
            }
        }
        rx
    }

    /// 测试钩子：当前存活 worker 的 pid 快照（崩溃测试用外部 kill）。
    pub fn worker_pids(&self) -> Vec<u32> {
        self.inner
            .state
            .lock()
            .unwrap()
            .slots
            .iter()
            .filter_map(|s| s.as_ref())
            .map(|s| s.pid)
            .collect()
    }

    /// 测试钩子：池是否已降级（重启预算耗尽或初始拉起失败）。
    pub fn degraded(&self) -> bool {
        self.inner.state.lock().unwrap().degraded
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        // 清空槽位：Sender 掉线使 writer 线程退场并关闭 stdin，
        // 子进程收到 EOF 后 exit(0) 自行结束，无需强杀。
        self.inner.state.lock().unwrap().slots.clear();
    }
}

/// 测试钩子辅助：按 pid 实测进程优先级类（非仅“设置成功”）。
pub fn query_priority_class(pid: u32) -> Option<u32> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let class = GetPriorityClass(handle);
        CloseHandle(handle);
        Some(class)
    }
}

/// 拉起单个 worker 子进程并配置好 writer 线程，返回槽位与待接管的 stdout。
fn spawn_worker(
    exe: &Path,
    priority: PoolPriority,
    log_dir: Option<&Path>,
) -> std::io::Result<(WorkerSlot, ChildStdout)> {
    let mut child = Command::new(exe);
    // 前台档通过旗标告知子进程别自压 IO/内存优先级（decode-worker 入口解析）。
    if matches!(priority, PoolPriority::ForegroundBelowNormal) {
        child.arg("--foreground");
    }
    // 显式注入优先于环境继承：宿主对 worker 日志落点负全责，测试据此钉死
    // 临时目录，源码树不出现任何日志文件。
    if let Some(dir) = log_dir {
        child.env("DSH_LOG_DIR", dir);
    }
    let mut child = child
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // 日志通道与协议通道分离：不留 stderr 混流入口
        .creation_flags(0x08000000) // CREATE_NO_WINDOW：避免 GUI 启动时带出黑框控制台
        .spawn()?;
    // 优先级档位（D11/D37）：后台档维持 IDLE_PRIORITY_CLASS；前台高速导入换
    // BELOW_NORMAL_PRIORITY_CLASS——仍让位于用户普通操作，但不再把海量
    // 小文件随机 IO 压到 VERY_LOW（THREAD_MODE_BACKGROUND_BEGIN 由子进程
    // 收到 --foreground 时跳过）。GetPriorityClass 可实测读回。
    unsafe {
        SetPriorityClass(child.as_raw_handle(), priority.win_priority_class());
    }
    let pid = child.id();
    let stdin = child.stdin.take().expect("stdin 已 piped");
    let stdout = child.stdout.take().expect("stdout 已 piped");

    let (tx, rx) = channel::<String>();
    thread::spawn(move || writer_loop(rx, stdin));
    Ok((
        WorkerSlot {
            writer: tx,
            pid,
            _child: child,
        },
        stdout,
    ))
}

fn start_reader(inner: &Arc<PoolInner>, idx: usize, stdout: ChildStdout) {
    let inner = Arc::clone(inner);
    thread::spawn(move || reader_loop(inner, idx, stdout));
}

/// 宿主侧 stdin 写入线程：通道关闭即退出并关闭 stdin（子进程的 EOF 信号）。
fn writer_loop(rx: Receiver<String>, mut stdin: ChildStdin) {
    for line in rx.iter() {
        if writeln!(stdin, "{line}").is_err() || stdin.flush().is_err() {
            break; // 子进程 stdin 断开
        }
    }
}

/// 宿主侧响应读取线程：逐行解析信封并按 (worker, job_id) 路由到 pending 表。
fn reader_loop(inner: Arc<PoolInner>, idx: usize, stdout: ChildStdout) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF：子进程消亡
            Ok(_) => {}
            Err(_) => break,
        }
        match serde_json::from_str::<Envelope>(line.trim()) {
            Ok(Envelope::Response { res, .. }) => route_response(&inner, idx, res),
            _ => break, // 解析失败 = 协议损坏，判定该 worker 死亡
        }
    }
    supervise_death(&inner, idx);
}

fn route_response(inner: &PoolInner, idx: usize, res: JobResult) {
    let key = (idx, res.job_id());
    if let Some(tx) = inner.state.lock().unwrap().pending.remove(&key) {
        let _ = tx.send(res);
    }
}

/// 死亡处理：pending 全部 Failed 回报 + 替补拉起（受重启预算约束，超限降级）。
fn supervise_death(inner: &Arc<PoolInner>, idx: usize) {
    if inner.shutdown.load(Ordering::SeqCst) {
        return; // 池关闭中的自然 EOF，不重启
    }
    let mut st = inner.state.lock().unwrap();
    if st.degraded {
        return;
    }
    // 池关闭中槽位可能已被清空（Drop 竞态）：不再登记/替补。
    if idx >= st.slots.len() {
        return;
    }

    // 该 worker 全部 pending 立即 Failed——坏进程的半成品状态不可信，不做池内黑盒重试。
    let dead_keys: Vec<(usize, u64)> = st
        .pending
        .keys()
        .filter(|key| key.0 == idx)
        .copied()
        .collect();
    for key in dead_keys {
        if let Some(tx) = st.pending.remove(&key) {
            let _ = tx.send(JobResult::Failed {
                job_id: key.1,
                reason: format!("worker #{idx} 进程死亡，任务未完成"),
            });
        }
    }

    if st.restarts >= MAX_RESTARTS_PER_POOL {
        st.degraded = true;
        st.slots[idx] = None;
        return;
    }
    st.restarts += 1;
    match spawn_worker(&inner.exe, inner.priority, inner.log_dir.as_deref()) {
        Ok((slot, stdout)) => {
            st.slots[idx] = Some(slot);
            // 持锁期间拉 reader 线程安全：新线程先读管道，与宿主持锁无线程等待环。
            start_reader(inner, idx, stdout);
        }
        // 替补拉起失败同样直接降级，禁止无限重试循环。
        Err(_) => {
            st.degraded = true;
            st.slots[idx] = None;
        }
    }
}
