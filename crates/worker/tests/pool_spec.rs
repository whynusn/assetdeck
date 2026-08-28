//! M4 红灯测试集：协议 roundtrip / 池上限核数 / idle 优先级 / 监督重启 / 毒资产隔离。
//! 测试名与 TDD_PLAN M4 清单一一对应。

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, TerminateProcess, IDLE_PRIORITY_CLASS, PROCESS_TERMINATE,
};

use worker::{query_priority_class, Envelope, JobRequest, JobResult, PoolPriority, WorkerPool};

/// 把 worker 日志钉进本进程专属的临时目录（DSH_LOG_DIR 显式注入，不靠环境
/// 继承）：测试对源码树零日志污染，且目录随进程隔离、跑完可整体清理。
fn with_test_pool(size: usize) -> WorkerPool {
    static TEST_LOG_DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = TEST_LOG_DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("dsh_worker_test_logs_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    });
    let exe = std::env::var("CARGO_BIN_EXE_decode-worker")
        .expect("CARGO_BIN_EXE_decode-worker 未设置：请在 cargo 测试环境运行");
    WorkerPool::with_priority_and_log_dir(
        Path::new(&exe),
        size,
        PoolPriority::BackgroundIdle,
        Some(dir.clone()),
    )
}

/// 时间预算类断言统一给宽裕上界（CI 抖动安全），best-effort：实际目标亚秒。
const BUDGET: Duration = Duration::from_secs(10);

fn wait_result(rx: Receiver<JobResult>) -> JobResult {
    rx.recv_timeout(BUDGET).expect("等待任务结果超时")
}

fn expect_ok(result: JobResult, job_id: u64) -> String {
    match result {
        JobResult::Ok {
            job_id: id,
            payload,
            ..
        } => {
            assert_eq!(id, job_id);
            payload
        }
        other => panic!("job {job_id} 应成功，实际 {other:?}"),
    }
}

fn expect_failed(result: JobResult, job_id: u64) -> String {
    match result {
        JobResult::Failed { job_id: id, reason } => {
            assert_eq!(id, job_id);
            assert!(!reason.is_empty(), "失败原因不应为空");
            reason
        }
        other => panic!("job {job_id} 应失败，实际 {other:?}"),
    }
}

/// 真实杀死进程（TerminateProcess，禁止 mock 被测对象）。
fn kill_process(pid: u32) {
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        assert!(!handle.is_null(), "OpenProcess({pid}) 失败");
        assert_ne!(
            TerminateProcess(handle, 1),
            0,
            "TerminateProcess({pid}) 失败"
        );
        CloseHandle(handle);
    }
}

fn make_png(dir: &Path, name: &str, edge: u32) -> PathBuf {
    let path = dir.join(name);
    let img = image::GrayImage::from_fn(edge, edge, |_x, _y| image::Luma([128]));
    image::DynamicImage::ImageLuma8(img)
        .save(&path)
        .expect("写测试 PNG 失败");
    path
}

#[test]
fn job_result_roundtrips_over_ipc_protocol() {
    // 协议即契约：请求信封 NDJSON 序列化 → 反序列化必须无损。
    let req = Envelope::Request {
        v: worker::PROTOCOL_VERSION,
        req: JobRequest::ThumbnailPng {
            job_id: 42,
            source: PathBuf::from("C:/pics/raw.png"),
            dest: PathBuf::from("thumbs/a/ab/uuid.png"),
            max_edge: 256,
            paste_dest: None,
            paste_max_edge: 4096,
        },
    };
    let json = serde_json::to_string(&req).unwrap();
    // 版本字段存在且为 1（前向兼容留位）。
    assert!(json.contains(r#""v":1"#), "信封缺少版本字段: {json}");
    let back: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, req);

    // Echo 请求 + Ok 响应 + Failed 响应三种形态都要走通。
    let echo = Envelope::Request {
        v: worker::PROTOCOL_VERSION,
        req: JobRequest::Echo {
            job_id: 7,
            payload: "你好 worker".into(),
        },
    };
    let echo_json = serde_json::to_string(&echo).unwrap();
    assert_eq!(serde_json::from_str::<Envelope>(&echo_json).unwrap(), echo);

    let ok = Envelope::response(JobResult::Ok {
        job_id: 42,
        payload: "thumbs/a/ab/uuid.png".into(),
        width: Some(1920),
        height: Some(1080),
    });
    let ok_json = serde_json::to_string(&ok).unwrap();
    assert!(ok_json.contains(r#""v":1"#));
    assert_eq!(serde_json::from_str::<Envelope>(&ok_json).unwrap(), ok);

    // 尺寸是可选扩展位：旧 worker 不带 width/height 的响应仍须能解析，
    // 否则一次协议演进就会让在跑的 worker 变成哑巴。
    let legacy = r#"{"v":1,"res":{"type":"ok","job_id":9,"payload":"x.png"}}"#;
    let parsed: Envelope = serde_json::from_str(legacy).expect("旧格式 Ok 响应必须可解析");
    assert_eq!(
        parsed,
        Envelope::response(JobResult::Ok {
            job_id: 9,
            payload: "x.png".into(),
            width: None,
            height: None,
        })
    );

    let failed = Envelope::response(JobResult::Failed {
        job_id: 43,
        reason: "解码失败".into(),
    });
    let failed_json = serde_json::to_string(&failed).unwrap();
    assert!(failed_json.contains(r#""v":1"#));
    assert_eq!(
        serde_json::from_str::<Envelope>(&failed_json).unwrap(),
        failed
    );
}

#[test]
fn pool_size_capped_at_cpu_count() {
    let cpus = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let pool = with_test_pool(999);
    assert_eq!(
        pool.worker_pids().len(),
        cpus,
        "池大小请求值超过 CPU 核数时必须被钳制到核数"
    );
}

#[test]
fn idle_priority_set_on_worker_process() {
    let pool = with_test_pool(2);
    let pids = pool.worker_pids();
    assert_eq!(pids.len(), 2);
    for pid in pids {
        assert_eq!(
            wait_idle_class(pid),
            Some(IDLE_PRIORITY_CLASS),
            "worker 进程 {pid} 的优先级类应为 IDLE_PRIORITY_CLASS（实测，非仅设置成功）"
        );
    }
}

/// 实测进程优先级类（best-effort 上界 10s：给 OS 异步语义与 CI 抖动留余量）。
///
/// 注：design.md 原定断言 PROCESS_MODE_BACKGROUND_BEGIN，但 Win32 实测该模式
/// 不经 GetPriorityClass 读回（Chromium 内核测试同结论），且带 32MiB 工作集
/// 封顶陷阱；故以跨进程可设可测的 IDLE_PRIORITY_CLASS 承载同一红线意图，
/// IO/内存降级由 worker 入口 THREAD_MODE_BACKGROUND_BEGIN 兜底（不可外部观测）。
fn wait_idle_class(pid: u32) -> Option<u32> {
    let deadline = Instant::now() + BUDGET;
    loop {
        match query_priority_class(pid) {
            Some(class) if class == IDLE_PRIORITY_CLASS => return Some(class),
            Some(_) | None => {}
        }
        if Instant::now() >= deadline {
            return query_priority_class(pid);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn worker_crash_supervisor_respawns_within_budget() {
    let pool = with_test_pool(2);
    let old_pids = pool.worker_pids();
    assert_eq!(old_pids.len(), 2);

    kill_process(old_pids[0]);

    // 预算窗口 10s（CI 抖动安全上界，best-effort；实际目标亚秒）。
    let deadline = Instant::now() + BUDGET;
    loop {
        let pids = pool.worker_pids();
        let new_pid = pids.iter().find(|p| !old_pids.contains(p)).copied();
        if pids.len() == old_pids.len() {
            if let Some(new_pid) = new_pid {
                // 容量恢复 + 替补进程 idle 优先级正确（实测）。
                assert_eq!(
                    wait_idle_class(new_pid),
                    Some(IDLE_PRIORITY_CLASS),
                    "替补 worker {new_pid} 也必须以 idle 优先级运行"
                );
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "监督重启超预算（10s）：当前 pids {pids:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }

    // 重启后池仍可正常服务。
    let rx = pool.submit(JobRequest::Echo {
        job_id: 1,
        payload: "alive".into(),
    });
    expect_ok(wait_result(rx), 1);
}

#[test]
fn poison_asset_fails_job_not_pool() {
    let dir = tempfile::tempdir().unwrap();
    let pool = with_test_pool(2);

    // 坏资产一：不存在的路径 → 该 job Failed。
    let missing = dir.path().join("no_such_file.png");
    let rx = pool.submit(JobRequest::ThumbnailPng {
        job_id: 1001,
        source: missing,
        dest: dir.path().join("thumbs/out1.png"),
        max_edge: 64,
        paste_dest: None,
        paste_max_edge: 4096,
    });
    expect_failed(wait_result(rx), 1001);

    // 坏资产二：损坏文件 → 同样只失败该 job。
    let corrupt = dir.path().join("corrupt.png");
    std::fs::write(&corrupt, b"definitely not a png").unwrap();
    let rx = pool.submit(JobRequest::ThumbnailPng {
        job_id: 1002,
        source: corrupt,
        dest: dir.path().join("thumbs/out2.png"),
        max_edge: 64,
        paste_dest: None,
        paste_max_edge: 4096,
    });
    expect_failed(wait_result(rx), 1002);

    // 池与其余任务不受影响：Echo 成功。
    let rx = pool.submit(JobRequest::Echo {
        job_id: 1003,
        payload: "still-alive".into(),
    });
    assert_eq!(expect_ok(wait_result(rx), 1003), "still-alive");

    // 合法 PNG 缩略图成功落盘且最长边 ≤ max_edge。
    let src = make_png(dir.path(), "valid.png", 96);
    let dest = dir.path().join("thumbs/sub/dir/out.png");
    let rx = pool.submit(JobRequest::ThumbnailPng {
        job_id: 1004,
        source: src,
        dest: dest.clone(),
        max_edge: 64,
        paste_dest: None,
        paste_max_edge: 4096,
    });
    let result = wait_result(rx);
    // 回报的必须是**原始**尺寸而非缩放后尺寸——瀑布流要的是素材真实宽高比。
    assert_eq!(
        result.dimensions(),
        Some((96, 96)),
        "缩略图结果应携带源图原始像素尺寸，实际 {result:?}"
    );
    expect_ok(result, 1004);
    assert!(dest.exists(), "缩略图应已写盘");
    let dims = image::GenericImageView::dimensions(&image::open(&dest).unwrap());
    assert!(
        dims.0 <= 64 && dims.1 <= 64,
        "缩略图应等比缩放，实际 {dims:?}"
    );

    assert!(!pool.degraded(), "毒资产不得拖垮池");
}

/// D20 双路输出：同一份解码旁挂「上框用 paste.png」，缩略图与 paste 同时落盘。
/// 千牛一类目标只认 CF_PNG，jpg 必须拿到这份派生 PNG 才能上框而非退化为仅复制。
#[test]
fn thumbnail_job_with_paste_dest_writes_both_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let pool = with_test_pool(2);

    // 源图 256x256：缩略图上限 64 -> 等比 64x64；paste 上限 4096 -> 原尺寸透传。
    let src = make_png(dir.path(), "big.jpg", 256);
    // make_png 写的是 PNG 字节，扩展名骗 worker 走 image crate 也无妨（内容决定解码）。
    let thumb = dir.path().join("thumbs/ab/thumb.png");
    let paste = dir.path().join("objects/uuid/paste.png");
    let rx = pool.submit(JobRequest::ThumbnailPng {
        job_id: 1101,
        source: src,
        dest: thumb.clone(),
        max_edge: 64,
        paste_dest: Some(paste.clone()),
        paste_max_edge: 4096,
    });
    let result = wait_result(rx);
    assert_eq!(
        result.dimensions(),
        Some((256, 256)),
        "尺寸回报的必须是源图原始像素（缩放前），实际 {result:?}"
    );
    expect_ok(result, 1101);

    assert!(thumb.exists(), "缩略图应已写盘");
    assert!(paste.exists(), "上框用 paste.png 应已写盘");
    let (tw, th) = image::GenericImageView::dimensions(&image::open(&thumb).unwrap());
    assert!(
        tw <= 64 && th <= 64 && tw > 0 && th > 0,
        "缩略图应等比缩到最长边 64 内，实际 {tw}x{th}"
    );
    let (pw, ph) = image::GenericImageView::dimensions(&image::open(&paste).unwrap());
    assert_eq!(
        (pw, ph),
        (256, 256),
        "paste 走更高上限应保持原尺寸，实际 {pw}x{ph}"
    );
    assert!(!pool.degraded());
}

/// 回归：PNG 内容挂 .jpg 名。旧实现按扩展名进 JPEG 解码器报
/// 「Illegal start bytes: 89504e47…」，全靠 Shell 抽帧兜底才没失败（分辨率与
/// 动图都受损）；image crate 改按内容嗅探后应直接走对路解码。
#[test]
fn misnamed_png_with_jpg_extension_decodes_by_content() {
    let dir = tempfile::tempdir().unwrap();
    let pool = with_test_pool(1);

    let src = make_png(dir.path(), "伪装.jpg", 48);
    let dest = dir.path().join("thumbs/cd/uuid.png");
    let rx = pool.submit(JobRequest::ThumbnailPng {
        job_id: 1201,
        source: src,
        dest: dest.clone(),
        max_edge: 32,
        paste_dest: None,
        paste_max_edge: 4096,
    });
    let result = wait_result(rx);
    assert_eq!(
        result.dimensions(),
        Some((48, 48)),
        "伪装扩展名不影响尺寸回报，实际 {result:?}"
    );
    expect_ok(result, 1201);
    assert!(dest.exists());
    assert!(!pool.degraded());
}

/// 门 2 补充断言：重启预算耗尽后进入 degraded，后续 submit 直接 Failed。
#[test]
fn restart_budget_exhaustion_degrades_pool() {
    let pool = with_test_pool(1);
    let mut current = pool.worker_pids()[0];

    // 重启上限 3：前三次 kill 都应得到替补。
    for _ in 0..3 {
        kill_process(current);
        current = wait_new_pid(&pool, current);
    }
    assert!(!pool.degraded(), "重启计数未超上限前不得降级");

    // 第四次死亡：预算耗尽 → degraded，后续 submit 直接返回 Failed。
    kill_process(current);
    let deadline = Instant::now() + BUDGET;
    while !pool.degraded() {
        assert!(
            Instant::now() < deadline,
            "降级判定超时（best-effort 上界 10s）"
        );
        thread::sleep(Duration::from_millis(25));
    }
    let rx = pool.submit(JobRequest::Echo {
        job_id: 2001,
        payload: "rejected".into(),
    });
    expect_failed(wait_result(rx), 2001);
}

/// 等待单槽位池出现新 pid（旧进程死亡 → 监督替补完成）。
fn wait_new_pid(pool: &WorkerPool, old: u32) -> u32 {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(p) = pool.worker_pids().into_iter().find(|p| *p != old) {
            return p;
        }
        assert!(Instant::now() < deadline, "监督重启超时（旧 pid {old}）");
        thread::sleep(Duration::from_millis(25));
    }
}
