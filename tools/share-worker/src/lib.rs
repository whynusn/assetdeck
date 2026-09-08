//! share-worker 库面（D80）：模块拆分供 bin 与集成测试共用。
//!
//! 进程模型与协议总览见各模块文档；二进制入口在 main.rs。

pub mod engine;
pub mod lines;
pub mod mdns;
pub mod proto;
