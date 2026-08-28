//! 子进程任务运行器（综合分析报告「三.10」）：导入/导出/缩略图派生共用一套
//! 「起进程 → 读 PROGRESS\t / NOTICE\t 行 → 收集 stderr → 回调 finished」编排，
//! 消除 main.rs 里三份几乎相同的重复代码。
//!
//! 协议线：子进程 stdout 逐行输出（UTF-8），以 `PROGRESS\t<done>\t<total>` 前缀
//! 上报进度（sample-library / derive-thumbs 均遵循），以 `NOTICE\t<文本>` 上报
//! 「任务整体成功但需要用户知情」的局部失败摘要；stderr 视为诊断文本，
//! 失败时拼进 finished 回调的消息。

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::Stdio;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 一个待执行的后台子进程任务。回调在**工作线程**上触发，
/// 调用方负责 `slint::invoke_from_event_loop` 弹回 UI 线程。
pub struct ChildTask {
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// 注入到子进程环境里的额外键值对（D38 日志传递：DSH_LOG_DIR / DSH_LOG_LEVEL）。
    pub envs: Vec<(String, String)>,
    /// 是否丢弃子进程窗口（CREATE_NO_WINDOW）；默认 true（后台工具不应弹窗）。
    pub hide_window: bool,
    pub on_progress: Box<dyn Fn(u32, u32) + Send>,
    /// 子进程 NOTICE\t 行回调：整体成功但局部失败需要用户知情（默认吞掉）。
    pub on_notice: Box<dyn Fn(String) + Send>,
    pub on_finished: Box<dyn Fn(bool, String) + Send>,
}

impl ChildTask {
    /// 常见形态的构造：可见窗口、布尔「隐藏窗口」默认 true。
    pub fn new(exe: PathBuf, args: Vec<String>) -> Self {
        Self {
            exe,
            args,
            envs: Vec::new(),
            hide_window: true,
            on_progress: Box::new(|_, _| {}),
            on_notice: Box::new(|_| {}),
            on_finished: Box::new(|_, _| {}),
        }
    }

    /// 注入子进程环境变量（日志联动用）。
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.envs.push((key.to_string(), value.to_string()));
        self
    }

    pub fn with_progress(mut self, on_progress: impl Fn(u32, u32) + Send + 'static) -> Self {
        self.on_progress = Box::new(on_progress);
        self
    }

    pub fn with_notice(mut self, on_notice: impl Fn(String) + Send + 'static) -> Self {
        self.on_notice = Box::new(on_notice);
        self
    }

    pub fn with_finished(mut self, on_finished: impl Fn(bool, String) + Send + 'static) -> Self {
        self.on_finished = Box::new(on_finished);
        self
    }

    /// 派生一个工作线程执行：起进程 → 流式读 stdout → 收集 stderr → wait → 回调。
    /// 返回值为「子进程是否成功启动」（启动失败时 finished(false, 消息) 已被调用）。
    pub fn run_in_background(self) -> bool {
        let exe_name = self.exe.display().to_string();
        let mut command = std::process::Command::new(&self.exe);
        command
            .args(&self.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        #[cfg(windows)]
        {
            if self.hide_window {
                command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                (self.on_finished)(false, format!("无法启动 {exe_name}: {error}"));
                return false;
            }
        };

        std::thread::spawn(move || {
            // stderr 收集线程：读满再拼进完成消息（失败诊断）。
            let stderr_thread = {
                let stderr = child.stderr.take();
                std::thread::spawn(move || {
                    let mut text = String::new();
                    if let Some(mut stderr) = stderr {
                        let _ = stderr.read_to_string(&mut text);
                    }
                    text
                })
            };

            if let Some(stdout) = child.stdout.take() {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if let Some(rest) = line.strip_prefix("PROGRESS\t") {
                        let mut parts = rest.split('\t');
                        let done = parts.next().and_then(|v| v.parse::<u32>().ok());
                        let total = parts.next().and_then(|v| v.parse::<u32>().ok());
                        if let (Some(done), Some(total)) = (done, total) {
                            (self.on_progress)(done, total);
                        }
                    } else if let Some(rest) = line.strip_prefix("NOTICE\t") {
                        let text = rest.trim();
                        if !text.is_empty() {
                            (self.on_notice)(text.to_string());
                        }
                    }
                }
            }

            let status = child.wait();
            let stderr_text = stderr_thread.join().unwrap_or_default();
            let (success, exit_code) = match status {
                Ok(status) => (status.success(), status.code().unwrap_or(-1)),
                Err(_) => (false, -1),
            };
            let message = if success {
                String::new() // 成功时消息由调用方自行组织。
            } else if stderr_text.trim().is_empty() {
                format!("{exe_name} 执行失败（exit={exit_code}）")
            } else {
                format!(
                    "{exe_name} 执行失败（exit={exit_code}）：{}",
                    stderr_text.trim()
                )
            };
            (self.on_finished)(success, message);
        });
        true
    }
}
