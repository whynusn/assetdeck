fn main() {
    // 样式钉在 fluent（暗色常量表）：不跟随系统亮暗，避免与 theme.slint 令牌漂移。
    // 运行时明暗切换不走换样式（编译期烘焙），而是壳层写内置 Palette 的
    // color-scheme——fluent 的每个控件颜色都是「scheme==Dark ? 暗 : 亮」的活绑定
    // （见生成代码），翻 scheme 即整体切换（D37）。
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/appwindow.slint", config).unwrap();
}
