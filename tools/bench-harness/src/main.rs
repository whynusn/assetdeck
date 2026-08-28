//! bench-harness CLI：generate / measure-rss / closed-loop 三子命令。
//!
//! 手写参数解析（design 权衡：不引 clap）。退出码契约（error-handling spec：
//! 测量失败=红）：0 达标 · 1 超预算 · 2 测量失败（spawn 失败/采样失败/子进程
//! 异常退出）· 64 用法错误。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

use bench_harness::generate::generate_library;
use bench_harness::sampler::{sample_median, SampleReport, SamplerError};

/// 采样节奏与预热（design 契约：每 250ms 一采、丢弃前 8 个样本）。
const POLL_MS: u64 = 250;
const WARMUP: usize = 8;
/// 默认采样窗口：静置 ≥10s（spec directory-structure），留出 browse 脚本耗时余量。
const DEFAULT_HOLD_MS: u64 = 15_000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("generate") => cmd_generate(&args[1..]),
        Some("measure-rss") => cmd_measure_rss(&args[1..]),
        Some("closed-loop") => cmd_closed_loop(),
        _ => {
            eprintln!("{USAGE}");
            64
        }
    };
    // 显式 flush：stdout 接管道时为块缓冲，exit 不保证冲刷单行 JSON
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

const USAGE: &str = "用法:
  bench-harness generate --rows N --out DIR [--thumbs M]
  bench-harness measure-rss --exe PATH --library DIR --mode idle|browse --budget-mb B [--hold-ms H] [--json-out FILE]
  bench-harness closed-loop";

// ---------------------------------------------------------------------------
// generate
// ---------------------------------------------------------------------------

fn cmd_generate(args: &[String]) -> i32 {
    let Some(out) = flag_value(args, "--out") else {
        eprintln!("generate 缺少 --out DIR");
        return 64;
    };
    let rows: u64 = match flag_value(args, "--rows") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("--rows 不是数字: {v}");
                return 64;
            }
        },
        None => {
            eprintln!("generate 缺少 --rows N");
            return 64;
        }
    };
    let thumbs: usize = match flag_value(args, "--thumbs") {
        Some(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("--thumbs 不是数字: {v}");
            std::process::exit(64);
        }),
        None => 2_000,
    };

    match generate_library(Path::new(&out), rows, thumbs) {
        Ok(()) => {
            println!(
                "{}",
                json!({"command": "generate", "rows": rows, "thumbs": thumbs, "out": out})
            );
            eprintln!("合成库就绪: {out}（{rows} 行元数据 / {thumbs} 张缩略图）");
            0
        }
        Err(e) => {
            eprintln!("生成失败（测量准备失败=红）: {e}");
            2
        }
    }
}

// ---------------------------------------------------------------------------
// measure-rss
// ---------------------------------------------------------------------------

struct MeasureArgs {
    exe: PathBuf,
    library: String,
    mode: String,
    budget_bytes: u64,
    hold_ms: u64,
    json_out: Option<String>,
}

fn cmd_measure_rss(args: &[String]) -> i32 {
    let (Some(exe), Some(library), Some(mode), Some(budget_mb)) = (
        flag_value(args, "--exe"),
        flag_value(args, "--library"),
        flag_value(args, "--mode"),
        flag_value(args, "--budget-mb"),
    ) else {
        eprintln!("measure-rss 需要 --exe/--library/--mode/--budget-mb");
        return 64;
    };
    if !matches!(mode.as_str(), "idle" | "browse") {
        eprintln!("--mode 仅支持 idle|browse，得到: {mode}");
        return 64;
    }
    let Ok(budget_bytes) = budget_mb.parse::<u64>() else {
        eprintln!("--budget-mb 不是数字: {budget_mb}");
        return 64;
    };
    let hold_ms = match flag_value(args, "--hold-ms") {
        Some(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("--hold-ms 不是数字: {v}");
            std::process::exit(64);
        }),
        None => DEFAULT_HOLD_MS,
    };
    let margs = MeasureArgs {
        exe: PathBuf::from(exe),
        library,
        mode,
        budget_bytes: budget_bytes * 1024 * 1024, // MiB → 字节（D10 合同口径）
        hold_ms,
        json_out: flag_value(args, "--json-out"),
    };

    if !margs.exe.exists() {
        eprintln!("测量失败=红: 可执行文件不存在 {}", margs.exe.display());
        return 2;
    }

    // browse 模式传 --bench；子进程 hold 取父窗口一半，保证在父采样窗内自然退出
    let mut command = Command::new(&margs.exe);
    let idle = margs.mode == "idle";
    if !idle {
        command
            .arg("--bench")
            .arg(&margs.library)
            .arg("--bench-hold-ms")
            .arg((margs.hold_ms / 2).max(2_000).to_string());
    } else {
        // 首拉预设回退哨兵：禁用应用内「GL 失败自愈重启」，防孤儿换进程污染采样；
        // 首拉即退由下方软件档重测接管（与 rss_spec 同一策略）。
        command.env("ASSETDECK_RENDER_FALLBACK", "1");
    }
    let mut child = match command
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("测量失败=红: 拉起子进程失败: {e}");
            return 2;
        }
    };
    let pid = child.id();

    let mut sampled = sample_median(pid, POLL_MS, WARMUP, margs.hold_ms);
    if idle {
        if let Err(first) = &sampled {
            // 首拉即退（典型：无 GL 环境下 GPU 档事件循环起不来，见 rss_spec 同款
            // 处理）。以软件渲染档重测一次——测的就是「这台机器上应用实际跑起来
            // 的形态」（D10 对最坏渲染路径守门）；重测再失败才判红。
            eprintln!("idle 首拉采样中止({first})，以软件渲染档重测");
            let _ = child.kill();
            let _ = child.wait();
            let retry = match Command::new(&margs.exe)
                .env("SLINT_BACKEND", "winit-software")
                .env("ASSETDECK_FORCE_SOFTWARE", "1")
                .env("ASSETDECK_RENDER_FALLBACK", "1")
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("测量失败=红: 软件渲染档重测拉起失败: {e}");
                    return 2;
                }
            };
            let pid = retry.id();
            sampled = sample_median(pid, POLL_MS, WARMUP, margs.hold_ms);
            child = retry;
        }
    }
    let report = match sampled {
        Ok(r) => r,
        Err(e @ SamplerError::ApiFailed(_)) | Err(e @ SamplerError::ProcessGone) => {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("测量失败=红: 采样中止: {e}");
            return 2;
        }
    };

    // 收尾：idle 子进程由 harness 终止；browse 应已自然退出——异常退出按测量失败
    if margs.mode == "browse" {
        if let Err(e) = wait_browse_exit(&mut child) {
            // 先收尸再报红：不许把仍存活的子进程泄漏给后续步骤
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("{e}");
            return 2;
        }
    } else {
        // idle 模式下进程任何「窗口内自行退出」都是提前退出（PRD：测量失败=红）：
        // 部分窗口样本不代表稳态空闲，禁止当有效数据放行。
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = child.wait();
                eprintln!("测量失败=红: idle 子进程在采样窗口内自行退出({status})，样本不可信");
                return 2;
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("测量失败=红: wait 失败: {e}");
                return 2;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    emit_report(&margs, &report)
}

fn wait_browse_exit(child: &mut std::process::Child) -> Result<(), String> {
    match child.try_wait() {
        Ok(Some(status)) if status.success() => Ok(()),
        Ok(Some(status)) => Err(format!(
            "测量失败=红: browse 子进程异常退出({status})，样本不可信"
        )),
        Ok(None) => Err("测量失败=红: browse 子进程未在采样窗口内完成浏览脚本(仍存活)".into()),
        Err(e) => Err(format!("测量失败=红: wait 失败: {e}")),
    }
}

fn emit_report(margs: &MeasureArgs, report: &SampleReport) -> i32 {
    // 单行 JSON 面向 CI 解析（spec logging-guidelines：时间戳 + RSS 字节 + 阶段标签）；
    // 人类摘要走 stderr。时间戳属遥测元数据，不受生成器确定性红线约束。
    let ts_unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = json!({
        "mode": margs.mode,
        "median_bytes": report.median_bytes,
        "samples": report.samples,
        "budget_bytes": margs.budget_bytes,
        "ts_unix_secs": ts_unix_secs,
    });
    println!("{line}");
    let _ = std::io::stdout().flush();

    if let Some(path) = &margs.json_out {
        if let Err(e) = std::fs::write(path, format!("{line}\n")) {
            eprintln!("警告: 趋势 JSON 写盘失败({path}): {e}");
        }
    }

    let usage_pct = report.median_bytes as f64 / margs.budget_bytes as f64 * 100.0;
    if report.median_bytes > margs.budget_bytes {
        eprintln!(
            "超预算(D10): {} 中位数 {} 字节 > 预算 {} 字节（{usage_pct:.1}%）",
            margs.mode, report.median_bytes, margs.budget_bytes
        );
        1
    } else {
        eprintln!(
            "达标: {} 中位数 {} 字节 ≤ 预算 {} 字节（{usage_pct:.1}%）",
            margs.mode, report.median_bytes, margs.budget_bytes
        );
        0
    }
}

// ---------------------------------------------------------------------------
// closed-loop
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn cmd_closed_loop() -> i32 {
    match bench_harness::closed_loop::run_closed_loop_probe() {
        Ok(report) => {
            println!(
                "{}",
                json!({
                    "command": "closed-loop",
                    "elapsed_ms": report.elapsed_ms,
                    "copied_only_reason": report.copied_only_reason,
                })
            );
            eprintln!(
                "闭环自动化段完成: {} ms（<500ms 为 best-effort 预算，D10/A2）",
                report.elapsed_ms
            );
            0
        }
        Err(e) => {
            eprintln!("闭环探针失败: {e}");
            1
        }
    }
}

#[cfg(not(windows))]
fn cmd_closed_loop() -> i32 {
    eprintln!("closed-loop 仅支持 Windows");
    2
}

// ---------------------------------------------------------------------------
// 参数工具
// ---------------------------------------------------------------------------

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
