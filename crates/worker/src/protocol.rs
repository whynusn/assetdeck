//! IPC 协议唯一出处：NDJSON stdio 信封与任务类型（serde 全 derive）。
//! 协议通道与日志通道分离：本模块只定义可序列化契约，不含任何进程管理/日志代码。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 当前协议版本；信封中的 `v` 字段为前向兼容留位。
pub const PROTOCOL_VERSION: u32 = 1;

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
    /// 图片缩略图：解码 → 等比缩放（最长边 ≤ max_edge）→ PNG 写 dest。
    ThumbnailPng {
        job_id: u64,
        source: PathBuf,
        dest: PathBuf,
        max_edge: u32,
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
    Ok { job_id: u64, payload: String },
    Failed { job_id: u64, reason: String },
}

impl JobResult {
    pub fn job_id(&self) -> u64 {
        match self {
            JobResult::Ok { job_id, .. } | JobResult::Failed { job_id, .. } => *job_id,
        }
    }
}
