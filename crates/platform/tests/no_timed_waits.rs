//! 守卫（AC1）：`win32.rs` 的产品路径不得再用时钟等待冒充「目标已就绪」。
//!
//! 为什么要源码级守卫：`sleep(60)` 这类调用在单元测试里看不出问题——它总会「成功」，
//! 只是慢。回归会悄无声息地把事件等待改回固定睡眠，所以在源文本层面钉死。
//!
//! 放行方式：在同一行末尾加 `// sleep-allowed(<理由>)`。当前唯一白名单是剪贴板
//! 打开失败后的退避重试——那里没有可订阅的系统事件，退避是唯一手段。

const SOURCE: &str = include_str!("../src/win32.rs");
const ALLOW_MARK: &str = "sleep-allowed(";

/// 只检查产品路径：`#[cfg(test)]` 之后是同文件内的单元测试，
/// 它们用时钟断言「真的等满了上限」，那是被测行为本身。
fn production_lines() -> impl Iterator<Item = (usize, &'static str)> {
    SOURCE
        .lines()
        .take_while(|line| line.trim() != "#[cfg(test)]")
        .enumerate()
        .map(|(index, line)| (index + 1, line))
}

#[test]
fn win32_production_paths_do_not_sleep() {
    let offenders: Vec<String> = production_lines()
        .filter(|(_, line)| {
            let code = line.split("//").next().unwrap_or(line);
            code.contains("thread::sleep") || code.contains("Sleep(")
        })
        .filter(|(_, line)| !line.contains(ALLOW_MARK))
        .map(|(number, line)| format!("{number}: {}", line.trim()))
        .collect();
    assert!(
        offenders.is_empty(),
        "win32.rs 产品路径出现固定睡眠；请改成事件等待，\
         确实无事件可订阅时在行尾加 // sleep-allowed(<理由>)：\n{}",
        offenders.join("\n")
    );
}

#[test]
fn win32_production_paths_do_not_step_a_clock_forward() {
    // `Instant` 本身合法（`WaitOutcome::Observed{elapsed_ms}` 要记账），
    // 不合法的是「拿它当轮询节拍」——deadline 循环。
    let offenders: Vec<String> = production_lines()
        .filter(|(_, line)| {
            let code = line.split("//").next().unwrap_or(line);
            code.contains("deadline") || code.contains("Instant::now() >=")
        })
        .filter(|(_, line)| !line.contains(ALLOW_MARK))
        .map(|(number, line)| format!("{number}: {}", line.trim()))
        .collect();
    assert!(
        offenders.is_empty(),
        "win32.rs 产品路径出现基于时钟的轮询循环；请改成 EventWait：\n{}",
        offenders.join("\n")
    );
}

#[test]
fn guard_allowlist_stays_minimal() {
    // 白名单只应有剪贴板退避那一条。多出来一条就要在评审里被看见。
    let allowed: Vec<&str> = production_lines()
        .filter(|(_, line)| line.contains(ALLOW_MARK))
        .map(|(_, line)| line.trim())
        .collect();
    assert_eq!(
        allowed.len(),
        1,
        "sleep-allowed 白名单发生变化，请确认每一条都真的无事件可订阅：\n{}",
        allowed.join("\n")
    );
    assert!(
        allowed[0].contains("CLIPBOARD_RETRY_DELAY_MS"),
        "唯一白名单应是剪贴板退避，实际是：{}",
        allowed[0]
    );
}
