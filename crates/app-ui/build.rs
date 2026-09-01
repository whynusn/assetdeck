fn main() {
    // 样式钉在 fluent（暗色常量表）：不跟随系统亮暗，避免与 theme.slint 令牌漂移。
    // 运行时明暗切换不走换样式（编译期烘焙），而是壳层写内置 Palette 的
    // color-scheme——fluent 的每个控件颜色都是「scheme==Dark ? 暗 : 亮」的活绑定
    // （见生成代码），翻 scheme 即整体切换（D37）。
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/appwindow.slint", config).unwrap();

    // 品牌图标（exe 资源）：Explorer/任务栏/快捷方式显示用，由 scripts/gen-icon.py
    // 从仓库根 assets/logo.png 生成。winresource 自动择路 gnu=windres / msvc=rc.exe；
    // FileVersion/ProductVersion 默认取 CARGO_PKG_* 环境变量。窗口内图标（标题栏）
    // 不在这里，走 appwindow.slint 的 Window icon 属性。
    #[cfg(target_os = "windows")]
    {
        let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let icon = manifest.join("../../assets/app-icon.ico");

        // windres 内部拼 gcc 预处理命令行时不给含空格路径加引号——本仓库路径含空格
        // （Documents\Default Project）会炸 preprocessing。gnu 下把 winresource 触碰
        // 的路径（OUT_DIR / CARGO_MANIFEST_DIR / 图标）统一换 8.3 短路径规避；msvc
        // 的 rc.exe 无此问题。installer/build.rs 有一份镜像实现，改动需同步。
        #[cfg(target_env = "gnu")]
        {
            use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

            // canonicalize 归一 ".." 并剥 \\?\ 前缀；未启用 8.3 的卷
            // GetShortPathNameW 会原样返回长路径（本机构建卷 C: 已验证启用）。
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
                .set("FileDescription", "素材管理器")
                .set("ProductName", "素材管理器")
                .compile()
                .expect("嵌入 exe 资源图标失败");
        }

        #[cfg(not(target_env = "gnu"))]
        {
            winresource::WindowsResource::new()
                .set_icon(icon.to_str().unwrap())
                .set("FileDescription", "素材管理器")
                .set("ProductName", "素材管理器")
                .compile()
                .expect("嵌入 exe 资源图标失败");
        }
    }
}
