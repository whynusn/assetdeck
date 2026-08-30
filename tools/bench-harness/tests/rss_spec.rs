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
use std::thread;
use std::time::{Duration, Instant};

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
/// browse 采样窗只是**上界**：sampler 在子进程自然退出时提前收窗
/// （ProcessGone → break），健康跑机无代价。CI 2vCPU 慢机的装载段耗时波动
/// 极大：25s 窗与 60s 窗各被击穿过一次（run 33291431999 / 33302128595——
/// 宽限期满子进程仍存活，装载+浏览慢机可超 35s；不是内存超支，是时限抖动），
/// 放大到 120s。子进程驻留 = 窗口一半（30s），采样中位数以稳态段为主。
const BROWSE_HOLD_MS: u64 = 120_000;
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
    // idle 模式无参启动（design 契约）：真实 GUI 路径含渲染器驻留。
    // stderr 落盘：若启动即退，把 panic 现场带回失败信息——「测量失败=红」
    // 必须可归因，不允许黑洞退出。
    let stderr_path = std::env::temp_dir().join("asset-manager-idle-stderr.log");
    // 首拉预设回退哨兵：禁用应用内「GL 失败自愈重启」，避免首拉进程悄悄换出
    // 孤儿子进程污染测量；由本测试显式决定是否以软件档重测。
    let mut child = spawn_idle(&exe, false);
    let first_pid = child.id();
    let report = match sample_median(child.id(), POLL_MS, WARMUP, IDLE_HOLD_MS) {
        Ok(report) => report,
        Err(first) => {
            // 首拉即退（无 GL 环境：femtovg 初始化失败）。以软件渲染档重测一次
            // ——测的就是「这台机器上应用实际跑起来的形态」（D10 预算对最坏
            // 渲染路径守门）。重测再失败才判红，并带回两轮证据。
            let _ = child.wait();
            let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            let child2 = spawn_idle(&exe, true);
            let retry_pid = child2.id();
            match sample_median(child2.id(), POLL_MS, WARMUP, IDLE_HOLD_MS) {
                Ok(report2) => {
                    child = child2;
                    report2
                }
                Err(second) => panic!(
                    "测量失败=红: idle 采样中止(首拉={first} 重测={second})\n首拉pid={first_pid} 重测pid={retry_pid}\n== 首拉(stderr) ==\n{stderr}\n== 重测(pid {retry_pid}) app 日志 ==\n{}\n== 最新 app 日志 ==\n{}",
                    dump_app_log_for(retry_pid),
                    dump_newest_app_log()
                ),
            }
        }
    };
    let retry_pid = child.id();
    // idle 进程在窗口内自行退出 = 提前退出 = 测量失败（PRD 红线）：
    // 部分窗口样本不代表稳态空闲，禁止当有效数据放行。
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            let _ = child.wait();
            let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            panic!(
                "测量失败=红: idle 子进程在采样窗口内自行退出({status}) pid={retry_pid}\n== 子进程 stderr ==\n{stderr}\n== pid {retry_pid} app 日志(头部含渲染档决策) ==\n{}",
                dump_app_log_for(retry_pid)
            );
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

/// 拉起 idle 测量子进程。force_software 时显式钉软件渲染档（重测路径）；
/// 否则预设 ASSETDECK_RENDER_FALLBACK 哨兵（首拉不做应用内自愈重启）。
fn spawn_idle(exe: &PathBuf, force_software: bool) -> std::process::Child {
    let stderr_path = std::env::temp_dir().join("asset-manager-idle-stderr.log");
    let mut cmd = Command::new(exe);
    if force_software {
        // 三保险：env 档 + 程序化强制软件档（ASSETDECK_FORCE_SOFTWARE 走应用自己的
        // BackendSelector，backend/renderer 分开传，不再依赖 Slint 的 env 解析黑盒）
        // + 哨兵（禁用应用内自愈重启，防换进程污染测量）。
        cmd.env("SLINT_BACKEND", "winit-software")
            .env("ASSETDECK_FORCE_SOFTWARE", "1")
            .env("ASSETDECK_RENDER_FALLBACK", "1");
    } else {
        cmd.env("ASSETDECK_RENDER_FALLBACK", "1");
    }
    cmd.stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(&stderr_path).expect("stderr 落盘失败"),
        ))
        .spawn()
        .expect("测量失败=红: 拉起 asset-manager 失败")
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
    // 失败归因插桩：窗终时刻的样本概况（中位数/样本数可判子进程死在哪个阶段）。
    eprintln!(
        "[diag] 采样窗结束 median={}B samples={}",
        report.median_bytes, report.samples
    );

    // 子进程必须干净退出（异常退出 = 样本不可信 = 红）。
    // 退出摘除竞态（CI 实测，run 33291431999）：ExitProcess 写回真实退出码
    // （≠STILL_ACTIVE）到内核对象置信号之间有毫秒级窗口——sampler 恰在此窗
    // 收到 ProcessGone 收窗后，紧邻的 try_wait 仍可能 WAIT_TIMEOUT => 误判
    // 「未在窗口内完成」。ProcessGone 本身就是完成信号，这里给 5s 宽限期
    // 复查退出状态；真挂死的子进程超期后 kill 判红，语义不变。
    let mut status = child.try_wait().expect("测量失败=红: wait 失败");
    let grace = Instant::now() + Duration::from_secs(5);
    while status.is_none() && Instant::now() < grace {
        thread::sleep(Duration::from_millis(50));
        status = child.try_wait().expect("测量失败=红: wait 失败");
    }
    match status {
        Some(s) if s.success() => {}
        Some(s) => panic!("测量失败=红: browse 子进程异常退出({s})"),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("测量失败=红: browse 子进程未在采样窗口内完成浏览脚本");
        }
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

/// 失败归因：按 pid 精确抓取对应子进程的 app-<pid>-*.log（头部 + 尾部）。
/// 头部必含「渲染档」决策行——判定该进程到底跑的哪个渲染器。
fn dump_app_log_for(pid: u32) -> String {
    let logs_dir = locate_app_exe()
        .parent()
        .map(|dir| dir.join("logs"))
        .expect("exe 路径异常");
    let prefix = format!("app-{pid}-");
    let path = std::fs::read_dir(&logs_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(&prefix) && name.ends_with(".log")
                })
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        })
        .map(|e| e.path());
    match path {
        Some(path) => {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = content.lines().collect();
            let head: Vec<&str> = lines.iter().take(12).copied().collect();
            let tail: Vec<&str> = lines.iter().rev().take(40).copied().collect();
            format!(
                "[{} 共 {} 行；头部 12 行]\n{}\n[尾部 40 行]\n{}",
                path.display(),
                lines.len(),
                head.join("\n"),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            )
        }
        None => format!(
            "(pid {pid} 无日志: {} 下无 {prefix}*.log)",
            logs_dir.display()
        ),
    }
}

/// 失败归因：抓取 exe 旁 logs/ 下最新的 app-*.log 尾部（D39 缺省日志位置）。
fn dump_newest_app_log() -> String {
    let logs_dir = locate_app_exe()
        .parent()
        .map(|dir| dir.join("logs"))
        .expect("exe 路径异常");
    let newest = std::fs::read_dir(&logs_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with("app-") && name.ends_with(".log")
                })
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        })
        .map(|e| e.path());
    match newest {
        Some(path) => {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let tail: Vec<&str> = content.lines().rev().take(40).collect();
            format!(
                "[{} 尾部 40 行]\n{}",
                path.display(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            )
        }
        None => format!("(无 app 日志: {} 不存在或为空)", logs_dir.display()),
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
