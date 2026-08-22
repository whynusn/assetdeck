//! 真实 SendInput 手动验收测试。
//!
//! 本地手动跑：`cargo test -p platform --test win32_manual -- --ignored`
//! CI 不跑真实注入（`#[ignore]` 默认跳过）。
//!
//! 流程：拉起记事本 → 轮询前台直至其成为前台窗口（最多 5 秒）→
//! 注入 'H'/'I' 两键（各按下+释放）→ 断言系统接受全部事件；
//! 实际输入效果请人工在记事本窗口确认。

#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use platform::win32::Win32Injector;
use platform::{KeyInjector, KEY_UP};

/// 查询当前前台窗口所属进程 pid；无前台窗口时返回 0。
fn foreground_pid() -> u32 {
    // 安全：只读查询；pid 经出参返回。
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return 0;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

fn cleanup(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
#[ignore = "本地手动跑;CI 不跑真实注入"]
fn real_sendinput_into_notepad() {
    let mut child = Command::new("notepad")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("无法启动 notepad(请在本地图形会话中手动运行本测试)");

    // 轮询：等记事本成为前台窗口，上限 5 秒。
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ready = false;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(500));
        if foreground_pid() == child.id() {
            ready = true;
            break;
        }
    }
    if !ready {
        cleanup(&mut child);
        panic!("5 秒内 notepad 未成为前台窗口(可能被其他窗口抢占)");
    }

    // 'H'/'I' 四个键事件：按下+释放相位以 KEY_UP 位标记。
    let seq: Vec<u16> = b"HI"
        .iter()
        .flat_map(|&k| [u16::from(k), u16::from(k) | KEY_UP])
        .collect();

    let mut injector = Win32Injector;
    let result = injector.inject(&seq);

    cleanup(&mut child);
    assert!(
        result.is_ok(),
        "SendInput 注入未全部送达: {:?}",
        result.err()
    );
}
