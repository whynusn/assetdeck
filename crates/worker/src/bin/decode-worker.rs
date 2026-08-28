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

// Shell 抽帧只服务于本进程，不进 worker 的 lib 公共面：宿主拿不到这条路径，
// 「解码只在 worker 进程内」这条红线就不靠自觉维持。
#[path = "../shell_thumb.rs"]
mod shell_thumb;

/// 交给 `image` crate 解码的扩展名。其余一律走 Shell 缩略图工厂。
const IMAGE_CRATE_EXTS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "bmp", "webp"];

fn main() {
    // D38：worker 日志（按进程名前缀轮换）；后台档镜像 stderr。
    logging::init_from_env("decode-worker", None, logging::Level::Info);
    logging::info!("decode-worker 启动 pid={}", std::process::id());

    // D37 前台高速档旗标：宿主（WorkerPool::with_priority）以 --foreground 拉起时，
    // 不自压 IO/内存优先级，让海量小文件随机 IO 保持正常档位。
    let foreground = std::env::args().any(|a| a == "--foreground");
    if !foreground {
        // D11：IO/内存优先级降为后台。不用进程级 PROCESS_MODE_BACKGROUND_BEGIN——
        // 它有未文档化的 32MiB 工作集封顶（Mozilla bug 1476365），且 GetPriorityClass
        // 读不回该标志；线程级模式同样把内存/IO 优先级压到 VERY_LOW 且无此副作用。
        // CPU 调度层面的 idle 由宿主侧 SetPriorityClass(IDLE_PRIORITY_CLASS) 兜底。
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
    }
    // Shell 缩略图工厂需要 COM；在协议循环之前一次性初始化。
    shell_thumb::init_com();
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
            width: None,
            height: None,
        },
        JobRequest::ThumbnailPng {
            job_id,
            source,
            dest,
            max_edge,
            paste_dest,
            paste_max_edge,
        } => match make_thumbnail(source, dest, *max_edge, paste_dest.as_deref(), *paste_max_edge) {
            Ok(done) => JobResult::Ok {
                job_id: *job_id,
                payload: done.dest,
                width: Some(done.width),
                height: Some(done.height),
            },
            Err(reason) => JobResult::Failed {
                job_id: *job_id,
                reason,
            },
        },
    }
}

/// 缩略图产出：落盘路径 + 源媒体**原始**像素尺寸（缩放前）。
struct Thumbnail {
    dest: String,
    width: u32,
    height: u32,
}

/// 解码 → 等比缩放（最长边 ≤ max_edge，小图不放大）→ PNG 落盘。
///
/// 解码器按扩展名分流：`image` crate 覆盖静态图片，其余（视频容器等）交给
/// Windows Shell 缩略图工厂抽一帧。图片格式若被 `image` 拒绝，也再给 Shell
/// 一次机会——扩展名骗人的素材比想象中常见。
/// 任何失败都以原因字符串回报，不 panic。
///
/// `paste_dest` 非空时，**同一份解码**再产出一份「上框用」PNG（D20）：
/// 一次解码两路输出，避免缩略图与 paste.png 各解一遍的重复开销。
/// 任何一路输出写盘失败都算整个 job 失败，调用方按缺省语义处理。
fn make_thumbnail(
    source: &Path,
    dest: &Path,
    max_edge: u32,
    paste_dest: Option<&Path>,
    paste_max_edge: u32,
) -> Result<Thumbnail, String> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if IMAGE_CRATE_EXTS.contains(&ext.as_str()) {
        match decode_with_image_crate(source, dest, max_edge, paste_dest, paste_max_edge) {
            Ok(thumb) => return Ok(thumb),
            Err(image_error) => {
                return decode_with_shell(source, dest, max_edge, paste_dest)
                    .map_err(|shell_error| {
                        format!("{image_error}；Shell 回退亦失败: {shell_error}")
                    })
            }
        }
    }
    decode_with_shell(source, dest, max_edge, paste_dest)
}

fn decode_with_image_crate(
    source: &Path,
    dest: &Path,
    max_edge: u32,
    paste_dest: Option<&Path>,
    paste_max_edge: u32,
) -> Result<Thumbnail, String> {
    // 按内容嗅探格式而非扩展名：`image::open` 信任路径后缀，PNG 内容挂 .jpg
    // 名会被塞进 JPEG 解码器报「Illegal start bytes」（导入侧已实测）。嗅探后
    // 伪装文件直接走对路解码，只有真解不动的才轮到 Shell 回退。
    let img = image::ImageReader::open(source)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|e| format!("读取失败 {source:?}: {e}"))
        .and_then(|reader| reader.decode().map_err(|e| format!("解码失败 {source:?}: {e}")))?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("零尺寸图像 {source:?}"));
    }
    write_scaled_png(&img, dest, max_edge, "缩略图")?;
    if let Some(paste) = paste_dest {
        // 与 dest 同源解码；paste 走更高上限，小图不放大。
        write_scaled_png(&img, paste, paste_max_edge, "上框 PNG")?;
    }
    Ok(Thumbnail {
        dest: dest.to_string_lossy().into_owned(),
        width: w,
        height: h,
    })
}

/// 等比缩放（最长边 ≤ max_edge，小图不放大）并以 PNG 落盘。
fn write_scaled_png(
    img: &DynamicImage,
    dest: &Path,
    max_edge: u32,
    what: &str,
) -> Result<(), String> {
    let (w, h) = img.dimensions();
    let scaled: DynamicImage = if w <= max_edge && h <= max_edge {
        img.clone()
    } else {
        img.resize(max_edge, max_edge, image::imageops::FilterType::Lanczos3)
    };
    ensure_parent(dest)?;
    // 显式强制 PNG 格式，与任务类型 ThumbnailPng 契约一致。
    scaled
        .save_with_format(dest, image::ImageFormat::Png)
        .map_err(|e| format!("写{what} {dest:?} 失败: {e}"))
}

/// Shell 抽帧 → PNG 落盘。尺寸回报的是帧尺寸（Shell 保持原始比例）。
///
/// `paste_dest` 非空时把同一帧原样再写一份到该路径——Shell 路径只服务
/// 视频/损坏图片，帧上限由 `max_edge` 决定，paste 拿不到更高分辨率，
/// 但「框里有图」仍好过「只复制 + 提示」。
fn decode_with_shell(
    source: &Path,
    dest: &Path,
    max_edge: u32,
    paste_dest: Option<&Path>,
) -> Result<Thumbnail, String> {
    let frame = shell_thumb::extract_frame(source, max_edge)?;
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .ok_or_else(|| format!("Shell 帧尺寸与像素数不符 {source:?}"))?;
    let png = DynamicImage::ImageRgba8(buffer);
    ensure_parent(dest)?;
    png.save_with_format(dest, image::ImageFormat::Png)
        .map_err(|e| format!("写缩略图 {dest:?} 失败: {e}"))?;
    if let Some(paste) = paste_dest {
        ensure_parent(paste)?;
        png.save_with_format(paste, image::ImageFormat::Png)
            .map_err(|e| format!("写上框 PNG {paste:?} 失败: {e}"))?;
    }
    Ok(Thumbnail {
        dest: dest.to_string_lossy().into_owned(),
        width: frame.width,
        height: frame.height,
    })
}

fn ensure_parent(dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录 {parent:?} 失败: {e}"))?;
    }
    Ok(())
}
