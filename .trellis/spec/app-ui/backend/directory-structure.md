# Directory Structure — app-ui

## 布局

```
crates/app-ui/
├── Cargo.toml          # bin: asset-manager; deps: ui-viewmodels + slint
├── build.rs            # slint_build::compile("ui/appwindow.slint")
├── src/main.rs         # 薄壳：include_modules! + AppWindow::new().run()
├── tests/deps_guard.rs # 依赖红线守卫测试
└── ui/
    └── appwindow.slint
```

## 模块组织规则

- main.rs 保持薄：初始化（VM 装配、subscriber、单实例锁）+ 事件循环，无业务逻辑。
- M5 后 `.slint` 组件增多时：`ui/` 下按组件拆文件；slintcn 等第三方组件**以源码形式**放 `app-ui/components/` 并逐个冒烟实例化测试。
- 二进制名 `asset-manager`（[[bin]] 显式声明）。

## 工具链注意（踩坑沉淀）

- Slint 必须 `default-features = false` + **`compat-1-2` feature**（缺它 compile_error!）+ std/backend-winit/renderer-femtovg/renderer-software。
- windows-gnu 目标下 slint 编译链接已验证通过（M0）；`.cargo/config.toml` 的 `-L native=` 注入勿删。
