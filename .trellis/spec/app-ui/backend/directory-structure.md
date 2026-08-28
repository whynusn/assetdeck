# Directory Structure — app-ui

## 布局

```
crates/app-ui/
├── Cargo.toml          # bin: asset-manager; deps: ui-viewmodels + slint
├── build.rs            # slint_build::compile("ui/appwindow.slint")
├── src/main.rs         # 薄壳：VM/目标路由装配 + Slint 事件循环
├── tests/deps_guard.rs # 依赖红线守卫测试
└── ui/
    └── appwindow.slint
```

## 模块组织规则

- main.rs 保持薄：初始化（VM 装配、subscriber、单实例锁）+ 事件循环，无业务逻辑。
- M5 后 `.slint` 组件增多时：`ui/` 下按组件拆文件；第三方组件以源码形式放 `app-ui/components/` 并逐个冒烟实例化测试。
- 二进制名 `asset-manager`（`[[bin]]` 显式声明）。

## 工具链注意（踩坑沉淀）

- Slint 必须 `default-features = false` + **`compat-1-2` feature**（缺它 compile_error!）+ std/backend-winit/renderer-femtovg/renderer-software。
- windows-gnu 目标下 slint 编译链接已验证通过（M0）；`.cargo/config.toml` 的 `-L native=` 注入勿删。

### Slint 1.17 语法踩坑（M5 实录）

- **MouseArea 已移除**：点击/双击手势一律用 `TouchArea`；双击是 `double-clicked => {}` 回调。
- **元素命名用 `name := Element` 实例化语法**（如 `btn := TouchArea {}`）。
- **属性必须写可见性前缀**：`in` / `out` / `in-out`，裸 `property <T>` 无法被壳层读写。
- **Flickable 滚动语义**：`viewport-y <=> content-y` 双向绑定后，向下滚动时 `content-y` 为负值（0 在顶部）。
- **内容总高必须回填**：`viewport-height: Math.max(content-height, self.height)` 依赖壳层写入 content-height。
- **`changed <property> => {}`** 可监听属性变化转发回调，适合把 viewport-y 变化推回壳层。

## M8 装配边界

- main.rs 创建 `TargetRoutingRuntime`，以 `BUILTIN_PROFILES` 和当前 `None` 用户 profile 初始化。
- 目标条属性、选择回调、固定切换与 750ms `Timer` 都在 main.rs 桥接，状态决策在 VM。
- `.slint` 只声明 TargetChoiceData、属性与回调，不写匹配或状态机逻辑。
- 当前 `TargetRoutingRuntime` 在 ui-viewmodels 内直接持有 Win32 实现，属于平台装配边界违规；后续应把具体平台类型迁入本二进制入口。
