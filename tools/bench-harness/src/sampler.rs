//! RSS 采样器：Win32 `GetProcessMemoryInfo` 读子进程 WorkingSet。
//!
//! 测量失败=红（spec error-handling）：进程消失/API 失败一律 [`SamplerError`]
//! 向上传播，由调用方转非零退出码——禁止静默跳过让预算检查形同虚设。

use std::fmt;
use std::time::{Duration, Instant};

/// 一次测量窗口的有效样本报告（预热样本已丢弃）。
#[derive(Debug, Clone, Copy)]
pub struct SampleReport {
    pub median_bytes: u64,
    /// 有效样本数（不含预热）。
    pub samples: usize,
}

#[derive(Debug)]
pub enum SamplerError {
    /// 进程已退出或不可打开。
    ProcessGone,
    /// 采样 API 调用失败（含非 Windows 平台不支持）。
    ApiFailed(String),
}

impl fmt::Display for SamplerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SamplerError::ProcessGone => write!(f, "子进程已消失"),
            SamplerError::ApiFailed(msg) => write!(f, "采样 API 失败: {msg}"),
        }
    }
}

impl std::error::Error for SamplerError {}

/// 单次 WorkingSet 采样；失败语义见 [`working_set_checked`]。
pub fn working_set_bytes(pid: u32) -> Option<u64> {
    working_set_checked(pid).ok()
}

/// 区分「进程没了」与「API 失败」：前者可能是 browse 子进程自然退出的正常终点，
/// 后者永远是异常。两者在断言路径上都按红处理。
///
/// 「没了」的判定不能只看 OpenProcess：harness 持有子进程句柄期间，已退出
/// 进程的内核对象仍可打开，且 GetProcessMemoryInfo 会返回残留值（实测恒
/// 32768 字节）——必须叠加 GetExitCodeProcess 终止态检测。
pub fn working_set_checked(pid: u32) -> Result<u64, SamplerError> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        /// STILL_ACTIVE（STATUS_PENDING）：进程尚未终止时的退出码哨兵值。
        const STILL_ACTIVE: u32 = 259;

        // 安全：pid 来自本 harness 刚 spawn 的子进程；句柄用后即关。
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err(SamplerError::ProcessGone);
        }
        let mut exit_code: u32 = 0;
        let got_code = GetExitCodeProcess(handle, &mut exit_code);
        if got_code == 0 {
            CloseHandle(handle);
            return Err(SamplerError::ApiFailed("GetExitCodeProcess 返回 0".into()));
        }
        if exit_code != STILL_ACTIVE {
            // 已终止（哪怕句柄未关、内核对象尚在）：按进程消失收窗。
            CloseHandle(handle);
            return Err(SamplerError::ProcessGone);
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = GetProcessMemoryInfo(handle, &mut counters, counters.cb);
        CloseHandle(handle);
        if ok == 0 {
            return Err(SamplerError::ApiFailed(
                "GetProcessMemoryInfo 返回 0".into(),
            ));
        }
        Ok(counters.WorkingSetSize as u64)
    }

    #[cfg(not(windows))]
    {
        let _ = pid;
        Err(SamplerError::ApiFailed(
            "RSS 采样仅支持 Windows（v1 平台范围）".into(),
        ))
    }
}

/// 连续采样：每 `poll_ms` 一次，直至 `hold_ms` 窗口结束或进程终止；
/// 丢弃前 `warmup` 个预热样本后取中位数。
///
/// 宽裕采样纪律（PRD）：预热丢弃前段（启动瞬态）+ 窗口内多轮样本取中位数。
/// 进程在预热完成前终止 = 提前退出 → `ProcessGone`（测量失败=红）；
/// 预热完成后自然退出属合法收窗终点（browse 子进程跑完脚本即退），
/// 中位数只统计存活期样本。
pub fn sample_median(
    pid: u32,
    poll_ms: u64,
    warmup: usize,
    hold_ms: u64,
) -> Result<SampleReport, SamplerError> {
    let started = Instant::now();
    let mut samples: Vec<u64> = Vec::new();
    loop {
        match working_set_checked(pid) {
            Ok(ws) => samples.push(ws),
            // ApiFailed 无条件上抛；ProcessGone 允许提前收窗（browse 子进程自然退出）
            Err(e @ SamplerError::ApiFailed(_)) => return Err(e),
            Err(SamplerError::ProcessGone) => break,
        }
        if started.elapsed() >= Duration::from_millis(hold_ms) {
            break;
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
    }

    if samples.len() <= warmup {
        return Err(SamplerError::ProcessGone);
    }
    let mut tail = samples.split_off(warmup);
    tail.sort_unstable();
    let n = tail.len();
    let median = if n % 2 == 1 {
        tail[n / 2]
    } else {
        (tail[n / 2 - 1] + tail[n / 2]) / 2
    };
    Ok(SampleReport {
        median_bytes: median,
        samples: n,
    })
}
