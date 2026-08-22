//! 红灯测试 2/3：`idle_rss_under_100mb` / `browse_100k_rss_under_250mb`
//! （PRD 需求 2/3，D10 合同数字）。
//!
//! #[ignore]：由 mem-regression job 显式跑或本地手动
//! `cargo test -p bench-harness --release -- --ignored --nocapture`。
//!
//! 测量失败=红：exe 缺失、spawn 失败、采样失败、browse 子进程异常退出
//! 一律 panic——禁止静默跳过让预算检查形同虚设。
//!
//! profile 分支决策（dispatch 步骤 6）：debug 二进制体积/分配行为与 release
//! 不可比，browse debug 档超预算属预期——debug 仅打印数字不断言；idle 两档
//! 都硬断言。CI mem-regression job 以 release 出正式数字。

#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Stdio};

use bench_harness::generate::generate_library;
use bench_harness::sampler::sample_median;

/// D10 合同数字（DECISIONS.md）：空闲 RSS ≤ 100MB。
const IDLE_BUDGET_BYTES: u64 = 100 * 1024 * 1024;
/// D10 合同数字：浏览 10 万条 ≤ 250MB。
const BROWSE_BUDGET_BYTES: u64 = 250 * 1024 * 1024;
const POLL_MS: u64 = 250;
const WARMUP: usize = 8;
/// 静置窗口 ≥10s（spec directory-structure）；browse 留足浏览脚本耗时余量。
const IDLE_HOLD_MS: u64 = 12_000;
const BROWSE_HOLD_MS: u64 = 25_000;
const ROWS: u64 = 100_000;

fn locate_app_exe() -> PathBuf {
    // CARGO_MANIFEST_DIR = <workspace>/tools/bench-harness → 上溯两级到 workspace 根；
    // profile 按 cfg!(debug_assertions)：ignored 测试走 target/debug（本地 cargo test
    // 无 release 产物），CI job 用 --release 出正式数字（design 权衡记录）。
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .ancestors()
        .nth(2)
        .expect("manifest 目录层级异常，无法上溯到 workspace 根");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let exe = workspace
        .join("target")
        .join(profile)
        .join("asset-manager.exe");
    assert!(
        exe.exists(),
        "测量失败=红: asset-manager.exe 不存在({})——先 cargo build --workspace",
        exe.display()
    );
    exe
}

#[test]
#[ignore = "mem-regression job 与本地手动跑"]
fn idle_rss_under_100mb() {
    let exe = locate_app_exe();
    // idle 模式无参启动（design 契约）：真实 GUI 路径含渲染器驻留
    let mut child = Command::new(&exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("测量失败=红: 拉起 asset-manager 失败");
    let report = sample_median(child.id(), POLL_MS, WARMUP, IDLE_HOLD_MS)
        .expect("测量失败=红: idle 采样中止");
    // idle 进程在窗口内自行退出 = 提前退出 = 测量失败（PRD 红线）：
    // 部分窗口样本不代表稳态空闲，禁止当有效数据放行。
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            let _ = child.wait();
            panic!("测量失败=红: idle 子进程在采样窗口内自行退出({status})");
        }
        Err(e) => panic!("测量失败=红: wait 失败: {e}"),
    }
    let _ = child.kill();
    let _ = child.wait();

    print_report("idle", report.median_bytes, report.samples);
    assert!(
        report.median_bytes <= IDLE_BUDGET_BYTES,
        "idle 中位数 {} 字节超 D10 预算 {IDLE_BUDGET_BYTES} 字节",
        report.median_bytes
    );
}

#[test]
#[ignore = "mem-regression job 与本地手动跑"]
fn browse_100k_rss_under_250mb() {
    // 先生成 10 万条合成库（确定性生成器；tempdir 生命周期覆盖整个测试）
    let dir = tempfile::tempdir().expect("tempdir 失败");
    let lib = dir.path().join("lib");
    generate_library(&lib, ROWS, 0).expect("合成库生成失败");

    let exe = locate_app_exe();
    // browse 模式传 --bench DIR（design 契约：子进程 Store::open 读库→建索引→
    // 脚本化浏览）；不开窗，浏览脚本跑完进入 hold 后自然退出
    let mut child = Command::new(&exe)
        .arg("--bench")
        .arg(&lib)
        .arg("--bench-hold-ms")
        .arg((BROWSE_HOLD_MS / 2).to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("测量失败=红: 拉起 asset-manager(--bench) 失败");

    let report = sample_median(child.id(), POLL_MS, WARMUP, BROWSE_HOLD_MS)
        .expect("测量失败=红: browse 采样中止");

    // 子进程必须干净退出（异常退出 = 样本不可信 = 红）
    match child.try_wait() {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(status)) => panic!("测量失败=红: browse 子进程异常退出({status})"),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("测量失败=红: browse 子进程未在采样窗口内完成浏览脚本");
        }
        Err(e) => panic!("测量失败=红: wait 失败: {e}"),
    }

    print_report("browse", report.median_bytes, report.samples);
    if cfg!(debug_assertions) {
        // debug 档仅打印不断言：二进制体积/分配行为不同，browse 超预算属预期
        // （dispatch 步骤 6 决策）；正式判定以 CI release 档为准。
        println!("RSS[browse] debug 档不断言（release 硬断言 {BROWSE_BUDGET_BYTES} 字节）");
    } else {
        assert!(
            report.median_bytes <= BROWSE_BUDGET_BYTES,
            "browse 中位数 {} 字节超 D10 预算 {BROWSE_BUDGET_BYTES} 字节",
            report.median_bytes
        );
    }
}

fn print_report(label: &str, median_bytes: u64, samples: usize) {
    let budget = if label == "idle" {
        IDLE_BUDGET_BYTES
    } else {
        BROWSE_BUDGET_BYTES
    };
    let usage = median_bytes as f64 / budget as f64 * 100.0;
    let margin = 100.0 - usage;
    println!(
        "RSS[{label}] 中位数={median_bytes}B({}KiB) 样本={samples} 预算占用={usage:.1}% 余量={margin:.1}%",
        median_bytes / 1024
    );
}
