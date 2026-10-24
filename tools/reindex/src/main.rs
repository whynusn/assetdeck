//! reindex 进程入口：内容一致性对账 + 被外部修改素材的重索引（D91）。
//!
//! stdout 是行协议通道（MARK/FIXED/PROGRESS/done），日志只进文件（D38）。
//! 用法：
//! ```text
//! reindex [--library <root>] [--worker-exe <path>] [--mode fast|background]
//! ```

use reindex::{run, ReindexArgs};

fn main() {
    logging::init_from_env("reindex", None, logging::Level::Info);
    logging::info!("reindex 启动：{:?}", std::env::args().collect::<Vec<_>>());
    match parse_args() {
        Ok(args) => match run(&args) {
            Ok(summary) => {
                logging::info!("reindex 完成：{summary:?}");
                println!(
                    "done: reindexed={} failed={} missing={} recovered={} stale={} \
                     baselined={} orphan={} root={}",
                    summary.reindexed,
                    summary.failed,
                    summary.missing,
                    summary.recovered,
                    summary.stale,
                    summary.baselined,
                    summary.orphans_reaped,
                    args.library.display()
                );
            }
            Err(error) => {
                logging::error!("reindex 失败: {error}");
                eprintln!("reindex 失败: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("reindex 参数错误: {error}");
            std::process::exit(2);
        }
    }
}

fn parse_args() -> Result<ReindexArgs, String> {
    // 缺省与 derive-thumbs 同款约定（CLI 单跑可用）；宿主总是显式传 --library。
    let mut args = ReindexArgs {
        library: std::path::PathBuf::from("samples/library"),
        worker_exe: None,
        fast: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--library" => {
                args.library = std::path::PathBuf::from(it.next().ok_or("--library 缺少值")?)
            }
            "--worker-exe" => {
                args.worker_exe = Some(std::path::PathBuf::from(
                    it.next().ok_or("--worker-exe 缺少值")?,
                ))
            }
            "--mode" => {
                let v = it.next().ok_or("--mode 缺少值")?;
                args.fast = match v.as_str() {
                    "fast" => true,
                    "background" => false,
                    other => return Err(format!("未知 --mode {other}（可用：fast | background）")),
                };
            }
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(args)
}
