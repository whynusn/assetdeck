fn main() {
    // 品牌图标：安装器 exe 与主程序同源——仓库根 assets/app-icon.ico（跨 workspace
    // 相对路径引用，单一事实源；scripts/gen-icon.py 生成）。用户下载后在
    // Explorer/运行对话框里看到的第一眼就是它。winresource 自动择路 gnu=windres /
    // msvc=rc.exe；FileVersion/ProductVersion 默认取 CARGO_PKG_* 环境变量。
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join("../assets/app-icon.ico");

    // windres 内部拼 gcc 预处理命令行时不给含空格路径加引号——本仓库路径含空格
    // （Documents\Default Project）会炸 preprocessing。gnu 下把 winresource 触碰的
    // 路径（OUT_DIR / CARGO_MANIFEST_DIR / 图标）统一换 8.3 短路径规避；msvc 的
    // rc.exe 无此问题。crates/app-ui/build.rs 有一份镜像实现，改动需同步。
    #[cfg(target_env = "gnu")]
    {
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

        fn short(p: &std::path::Path) -> String {
            let canonical = p
                .canonicalize()
                .expect("资源路径不存在")
                .to_str()
                .unwrap()
                .trim_start_matches(r"\\?\")
                .to_string();
            let wide: Vec<u16> = canonical.encode_utf16().chain(Some(0)).collect();
            unsafe {
                let n = GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0);
                assert!(n > 0, "GetShortPathNameW 失败: {}", canonical);
                let mut buf = vec![0u16; n as usize];
                GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), n);
                String::from_utf16_lossy(&buf[..(n as usize - 1)])
            }
        }

        std::env::set_var("CARGO_MANIFEST_DIR", short(&manifest));
        std::env::set_var(
            "OUT_DIR",
            short(std::path::Path::new(&std::env::var("OUT_DIR").unwrap())),
        );
        winresource::WindowsResource::new()
            .set_icon(&short(&icon))
            .set("FileDescription", "素材管理器 安装程序")
            .set("ProductName", "素材管理器")
            .compile()
            .expect("嵌入安装器资源图标失败");
    }

    #[cfg(not(target_env = "gnu"))]
    {
        winresource::WindowsResource::new()
            .set_icon(icon.to_str().unwrap())
            .set("FileDescription", "素材管理器 安装程序")
            .set("ProductName", "素材管理器")
            .compile()
            .expect("嵌入安装器资源图标失败");
    }
}
