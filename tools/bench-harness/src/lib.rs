//! bench-harness 库目标：D10 验收的执行者——没有监控的预算等于没定。
//!
//! 模块划分：确定性生成器 / RSS 采样器 / 闭环探针。CLI 分发见 `main.rs`。

pub mod generate;
pub mod sampler;

#[cfg(windows)]
pub mod closed_loop;
