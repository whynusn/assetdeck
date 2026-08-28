//! IPC 协议唯一出处：NDJSON stdio 信封与任务类型（serde 全 derive）。
//! 协议通道与日志通道分离：本模块只定义可序列化契约，不含任何进程管理/日志代码。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 当前协议版本；信封中的 `v` 字段为前向兼容留位。
pub const PROTOCOL_VERSION: u32 = 1;

/// `ThumbnailPng.paste_max_edge` 缺省值：派生「上框用 PNG」的最长边上限。
/// 给足够大的值即等价原尺寸转码；4096 兼顾 IM 输入框粘贴体感与 worker 内存。
pub const DEFAULT_PASTE_MAX_EDGE: u32 = 4096;

/// `paste_max_edge` 字段的 serde 缺省函数（旧请求不携带该字段时取此值）。
fn default_paste_max_edge() -> u32 {
    DEFAULT_PASTE_MAX_EDGE
}

/// NDJSON 信封：请求 `{ "v": 1, "req": ... }`，响应 `{ "v": 1, "res": ... }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Envelope {
    Request { v: u32, req: JobRequest },
    Response { v: u32, res: JobResult },
}

impl Envelope {
    /// 以当前协议版本构造请求信封。
    pub fn request(req: JobRequest) -> Self {
        Envelope::Request {
            v: PROTOCOL_VERSION,
            req,
        }
    }

    /// 以当前协议版本构造响应信封。
    pub fn response(res: JobResult) -> Self {
        Envelope::Response {
            v: PROTOCOL_VERSION,
            res,
        }
    }
}

/// 任务请求 v1。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobRequest {
    /// 协议测试/健康检查：原样回显 payload。
    Echo { job_id: u64, payload: String },
    /// 缩略图：解码 → 等比缩放（最长边 ≤ max_edge）→ PNG 写 dest。
    ///
    /// 「用什么解码器」是 worker 的内部策略，不是调用方的知识（D11）：
    /// 图片走 `image` crate，视频容器走 Windows Shell 缩略图工厂。
    /// 调用方只说「给这个文件做一张缩略图」。
    ///
    /// 可选第二输出 `paste_dest`：同一份解码再产出一份「上框用」PNG
    /// （D20——千牛一类目标只认 `CF_PNG`，jpg/webp 原字节写剪贴板不被识别）。
    /// 全部图片素材携带（PNG 原图同样派生，`paste_max_edge` 默认 4096 封顶）；
    /// 视频等传 `None`。serde default 保证不携带该字段的旧请求依旧可解析。
    ThumbnailPng {
        job_id: u64,
        source: PathBuf,
        dest: PathBuf,
        max_edge: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        paste_dest: Option<PathBuf>,
        #[serde(default = "default_paste_max_edge")]
        paste_max_edge: u32,
    },
}

impl JobRequest {
    pub fn job_id(&self) -> u64 {
        match self {
            JobRequest::Echo { job_id, .. } | JobRequest::ThumbnailPng { job_id, .. } => *job_id,
        }
    }
}

/// 任务结果。失败形态为 `Failed { reason }`，与 worker 错误处理规范对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobResult {
    Ok {
        job_id: u64,
        payload: String,
        /// 源媒体的**原始**像素尺寸（缩放前）。缩略图任务才有值，Echo 恒 None。
        ///
        /// 为什么由 worker 回传而不是调用方自己量：宿主进程不许解码（D11），
        /// 而瀑布流布局需要真实宽高比，否则只能用占位公式排出与画面无关的版式。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    Failed {
        job_id: u64,
        reason: String,
    },
}

impl JobResult {
    pub fn job_id(&self) -> u64 {
        match self {
            JobResult::Ok { job_id, .. } | JobResult::Failed { job_id, .. } => *job_id,
        }
    }

    /// 结果携带的原始像素尺寸；缺任一维即视为未知。
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match self {
            JobResult::Ok {
                width: Some(w),
                height: Some(h),
                ..
            } => Some((*w, *h)),
            _ => None,
        }
    }
}
