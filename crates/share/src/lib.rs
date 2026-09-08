//! 素材共享域模型（D80）：纯类型与校验，零 IO、零网络。
//!
//! 传输引擎在 tools/share-worker（独立按需进程，iroh/tokio 只进该编译单元）；
//! 本 crate 是发送 / 接收 / 记录三侧共享的契约层。

pub mod events;
pub mod manifest;
pub mod record;
pub mod request;

pub use events::{parse_worker_line, DoneState, ItemRole, WorkerEvent};
pub use manifest::{
    safe_ext, ManifestError, ManifestItem, TransferManifest, FILE_NAME_MAX_CHARS,
    MANIFEST_MAX_ITEMS,
};
pub use record::{ShareDirection, ShareOutcome, ShareRecord};
pub use request::{DeviceEntry, RequestError, SendItem, SendRequest, ALPN, MDNS_SERVICE};
