//! decode-worker 进程入口：stdin 逐行读 NDJSON 请求 → 处理 → stdout 写响应。
//!
//! 协议契约见 protocol.rs：
//! - EOF(stdin 关闭)= 退出信号，进程以 exit code 0 正常返回；
//! - 单 job 失败回 `Failed { reason }`，绝不 panic（坏资产隔离在 job 级）；
//! - 日志禁止混入 stdout/stderr（协议通道与日志通道分离，worker spec/logging-guidelines）。

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

use image::{DynamicImage, GenericImageView};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
};
use worker::{Envelope, JobRequest, JobResult};

fn main() {
    // D11：IO/内存优先级降为后台。不用进程级 PROCESS_MODE_BACKGROUND_BEGIN——
    // 它有未文档化的 32MiB 工作集封顶（Mozilla bug 1476365），且 GetPriorityClass
    // 读不回该标志；线程级模式同样把内存/IO 优先级压到 VERY_LOW 且无此副作用。
    // CPU 调度层面的 idle 由宿主侧 SetPriorityClass(IDLE_PRIORITY_CLASS) 兜底。
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF = 退出信号
            Ok(_) => {}
            Err(_) => break, // stdin 读错误视同关闭
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 无法解析的行无 job_id 可路由，跳过且不污染协议通道。
        if let Ok(Envelope::Request { req, .. }) = serde_json::from_str(trimmed) {
            let response = Envelope::response(handle(&req));
            if let Ok(json) = serde_json::to_string(&response) {
                if writeln!(out, "{json}").is_err() {
                    break; // stdout 断开 = 宿主已死
                }
                let _ = out.flush();
            }
        }
    }
    // EOF → main 正常返回即 exit(0)。
}

fn handle(req: &JobRequest) -> JobResult {
    match req {
        JobRequest::Echo { job_id, payload } => JobResult::Ok {
            job_id: *job_id,
            payload: payload.clone(),
        },
        JobRequest::ThumbnailPng {
            job_id,
            source,
            dest,
            max_edge,
        } => match make_thumbnail(source, dest, *max_edge) {
            Ok(dest_str) => JobResult::Ok {
                job_id: *job_id,
                payload: dest_str,
            },
            Err(reason) => JobResult::Failed {
                job_id: *job_id,
                reason,
            },
        },
    }
}

/// 解码 → 等比缩放（最长边 ≤ max_edge，小图不放大）→ PNG 落盘。
/// 任何失败都以原因字符串回报，不 panic。
fn make_thumbnail(source: &Path, dest: &Path, max_edge: u32) -> Result<String, String> {
    let img = image::open(source).map_err(|e| format!("解码失败 {source:?}: {e}"))?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("零尺寸图像 {source:?}"));
    }
    let scaled: DynamicImage = if w <= max_edge && h <= max_edge {
        img
    } else {
        img.resize(max_edge, max_edge, image::imageops::FilterType::Lanczos3)
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录 {parent:?} 失败: {e}"))?;
    }
    // 显式强制 PNG 格式，与任务类型 ThumbnailPng 契约一致。
    scaled
        .save_with_format(dest, image::ImageFormat::Png)
        .map_err(|e| format!("写缩略图 {dest:?} 失败: {e}"))?;
    Ok(dest.to_string_lossy().into_owned())
}
