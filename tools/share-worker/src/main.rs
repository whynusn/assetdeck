//! D80 传输引擎地基（M0 起步）：iroh 在本仓 windows-gnu 工具链上的编译与绑定探针。
//!
//! 完整引擎（清单流、接收双重确认、进度行协议）随后续批次落地；本探针先钉三件事：
//! ① gnu 工具链整树可编译（iroh + tokio + noq/rustls/ring）——D80 最大的技术风险点；
//! ② `presets::N0DisableRelay`（pkarr 第三方信令 + relay 整体关闭）下端点可绑定并取到直连地址
//!    ——红线 2（无中继）+ 原则 ①（第三方免费信令）在 API 层同时成立；
//! ③ 端口映射组件随默认树在位（portmapper，UPnP/PCP/NAT-PMP），见 D80 钉子 2。

use iroh::endpoint::presets;
use iroh::Endpoint;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let endpoint = Endpoint::builder(presets::N0DisableRelay)
        .bind()
        .await
        .expect("share-worker: endpoint 绑定失败");
    println!("ENDPOINT\t{:?}", endpoint.addr());
    endpoint.close().await;
}
