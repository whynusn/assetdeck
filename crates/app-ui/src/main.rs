slint::include_modules!();

fn main() {
    let ui = AppWindow::new().expect("AppWindow 创建失败");
    ui.run().expect("Slint 事件循环异常退出");
}
