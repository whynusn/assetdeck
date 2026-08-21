# AGENTS.md

## 项目状态与事实来源

- 本仓库为 greenfield（尚无代码、无 Cargo.toml）。**唯一规范来源是 `DECISIONS.md`**——动工前先读它；本文件只提炼其中最容易违反的硬约束。
- 产品：素材管理器（类 Eagle）+ IM 粘贴发送器，双线并行 MVP。

## 技术栈硬约束

- **Rust + Slint**。禁止引入 Electron/Tauri/egui/iced/Qt 等替代 UI 方案——内存预算（见下）是选型的根本理由。
- ⚠️ Slint 免费版为 **GPLv3**。引入任何新依赖前检查许可证兼容性（闭源商业化问题未决，见 `DECISIONS.md` 行动项 A1）。
- 目标平台 **仅 Windows**（v1）。不要使用 Unix-only API；平台相关代码（剪贴板、输入注入、窗口枚举）收拢到独立模块，v2 要接 Linux 分层方案（归档在 `DECISIONS.md` 第四节）。

## 性能预算 = 验收线，不是愿望

- 空闲 RSS ≤ 100MB；浏览 10 万条 ≤ 250MB。
- 设计目标规模：**10–100 万条资产必须丝滑**。任何全量载入内存的实现（如把全部元数据读进 Vec）都违反 D3/D4。
- 内存回归监控要进 CI（行动项 A3）；在监控落地前，新增依赖或缓存时手动评估常驻内存增量。

## 架构红线

- **进程模型**：UI 主进程单实例，永不执行缩略图生成/视频抽帧/pHash 计算——这些全部放独立 worker 进程池（核数封顶、IO idle 优先级）。
- **检索**：分类/属性过滤用 RoaringBitmap 位图交集，全文用 SQLite FTS5。**v1 禁止引入向量检索/向量数据库依赖**（架构上预留候选集抽象层即可）。
- **导入去重必做**：pHash（每图 8 字节）。入库模型是复制入库（双倍磁盘），不去重就是重复占盘。
- **auto-send 解耦**：粘贴管线末端「合成 Enter」必须是独立开关且**默认关**。双击素材的语义止步于「进入输入框」，任何改动不得把回车直发合并进默认路径。
- **焦点校验**：注入 Ctrl+V 前必须校验目标窗口存活（记录唤起面板时的前一前台窗口），失败降级为仅复制。

## v1 明确不做（不要顺手实现）

- 向量检索 / 以图搜图
- 视频悬停 scrub 预览（v1 只做缩略图 + 时长 + 点击播放）
- Linux / Wayland 支持
- 自动打标模型（分类靠用户导入时手动选 + 「待分类」收件箱）

## 工具链与环境（踩坑记录，勿动）

- 本地工具链：rustup `stable-x86_64-pc-windows-gnu`（位于 `~/.rustup`）；cargo/rustc/clippy 等经 **scoop shims**（`~/scoop/shims`）暴露。CI 用 runner 自带 MSVC——源码级兼容。
- **MinGW 依赖**：scoop `gcc` 包提供 binutils；`dlltool`/`gcc`/`ld` 的 shims 是 raw-dylib（windows-* crate 族）硬依赖，**禁止删除**。缺 dlltool 时构建报 "error calling dlltool"。
- `.cargo/config.toml` 为 gnu target 注入 `-L native=<mingw lib>`（解决 nuwen 发行版导入库搜索路径问题），勿删。
- rustup 镜像已配用户级环境变量 `RUSTUP_DIST_SERVER`/`RUSTUP_UPDATE_ROOT` → TUNA。
- Slint 以 `default-features = false` 使用时**必须带 `compat-1-2` feature**，否则编译期 compile_error!。

## 大文件自建清单（Slint 生态缺口，评估工期时计入）

- 百万级变宽高比瀑布流虚拟化网格（Slint ListView 不够用）
- 视频纹理管线（解码帧 → GPU 上传）
