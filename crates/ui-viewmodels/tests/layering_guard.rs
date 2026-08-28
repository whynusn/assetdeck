//! 机械守卫：VM 层不得引用任何平台具体实现。
//!
//! 只靠人眼 review 守不住这条线（历史上 `target_runtime.rs` 就直接 use 了 Win32 结构体），
//! 所以用源码扫描把它变成会红的测试。

use std::fs;
use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("读取源码目录失败") {
        let path = entry.expect("读取目录项失败").path();
        if path.is_dir() {
            files.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files
}

#[test]
fn vm_sources_never_reference_platform_implementations() {
    for file in rust_sources(&src_dir()) {
        let text = fs::read_to_string(&file).expect("读取源码失败");
        for banned in ["platform::win32", "Win32"] {
            assert!(
                !text.contains(banned),
                "红线违规：{} 引用了平台具体实现 `{banned}`，Win32 只能在二进制入口装配",
                file.display()
            );
        }
    }
}

#[test]
fn vm_sources_have_no_platform_conditional_gates() {
    for file in rust_sources(&src_dir()) {
        let text = fs::read_to_string(&file).expect("读取源码失败");
        assert!(
            !text.contains("cfg(windows)"),
            "红线违规：{} 出现平台条件门；VM 必须在任意平台可编译可测",
            file.display()
        );
    }
}
