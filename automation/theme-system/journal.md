# theme-system 运行日志

> 每次运行由 `state.mjs complete/abort` 自动追加。手工补记请保持同一格式（见 runbook §8）。

## 初始化 · harness 建立

- 分支：`theme-system`（工作树 `.theme-system/worktree`，主工作树未触碰）
- 判据：`automation/theme-system/theme-spec.md`（v1，把 D55「外置 TOML 主题包」展开为可执行规格）
- 路线图：T0 主题系统基座（8 个组件）+ T1 首发主题包（6 个组件），共 14 个
- 已建工具：`state.mjs`（状态机/锁/日志/主题台账）、`preview.mjs`（主题 × 状态截图）、
  `license-audit.mjs`（许可/体积/IP 关键词门禁）、`cargo.sh`（本机构建包装）
- 状态夹具复用 `automation/ux-audit/states/fixtures.json`（42 状态，单一事实来源，避免两套漂移）
- 已知硬边界（theme-spec §1）：Slint 1.17.1 无运行时字体注册（字体族仅限系统已装）；
  std-widgets 只认明暗两档；色值只能经 Rust 注入，不得落入 `.slint` 源文件

## 初始化 · 夹具漂移修复（阻塞性）

- 现象：`preview.mjs` 与 ux-audit 的 `capture.mjs` 在**所有含 tiles 的状态**上 panic：
  `First argument not an array: StructFieldAccess ... name: "search-segments"`
- 根因：D87/D88 给 `TileData` 新增 `search-segments: [TextSegment]`，但
  `automation/ux-audit/states/fixtures.json` 的 20 个 tile 从未补该字段；ux-audit 定时任务已过期，
  该漂移一直没被捕获。两条自动化线共用同一夹具，因此同时被卡住。
- 修复：给 6 个状态、20 个 tile 补 `"search-segments": []`（空模型 = 默认头部预览，与实际默认态一致）。
- 验证：`preview --check` 通过（42 状态）；暗/亮两套实际渲染成功，像素取样与声明令牌逐值吻合：
  暗 `bg-bar #252525` / `bg-app #1c1c1c`，浅 `bg-bar #ececf0` / `bg-app #f7f7f8`。
- 说明：本修复落在 theme-system 分支；main 与 ux-audit 分支仍带此缺陷，合并后一并生效。

## 初始化 · D91 漂移与 D90 废弃面板（阻塞性）

- 现象（漂移）：merge main（D91 内容一致性对账）后，含 tiles 的状态改报 `unsupported Value::Void | Value::Void`。
- 根因：D91 给 `TileData` 新增 `missing` / `stale` 两个 bool；夹具 40 个 tile 字段未跟上。
  解释器对缺字段只在渲染期 panic，报错不含状态名，定位成本极高（这是第二次同类事故）。
- 修复：40 处补 `"missing": false, "stale": false`；并在 `preview.mjs` 增加**方向五：结构体漂移检查**——
  解析 `crates/app-ui/ui/*.slint` 的 `export struct` 字段与 `property <[Struct]>` 映射，逐元素做双向比对
  （缺字段 → 报 `Value::Void` 风险；多字段 → 报未知字段）。今后漂移在 `--check` 阶段以可读错误暴露。
- 现象（废弃）：`share-domains` / `share-ctl` / `share-request` 全量采集失败，报
  `Warning: ignoring unknown property: share-ctl-devices`。
- 根因：D90 用空间模型取代了域/成员/发现/索取控制面，旧面板属性在现代码里已不存在。
- 处置：**不伪造属性映射**（那会产出看似正常的假截图）。改为在 fixture 上标 `deprecated` 并写明迁移目标
  （`space-ctl-open` / `space-add-open` / `space-rows` / `space-ctl-invite`）；工具侧 `--all` 自动跳过、
  `--list` 展示去向、显式点名或组件引用则报错。
- 验证：`preview --check` = 39 个可采集 + 跳过 3 个废弃；`builtin-dark` 39/39、`builtin-light` 39/39 全绿。
- 说明：D90 迁移去向的**新状态**（如 `space-ctl-open`）尚未加入夹具；等 T0-C5/T1 需要时按需补真实夹具。
## run-20261025-132431 · T0-C1 · done

- 组件：主题包 schema 与解析器
- 结论：完成主题包解析器与完整可用链路，实际支持范围见 themes/README.md
- Findings：F-001(open) ThemeTokens.id 是 &'static str，外置主题包 id（运行时 String）无法表达
- 门禁：定向 cargo test 首轮通过；Slint check、preview --check、license-audit 通过；最终 Clippy/新回归测试进行中
- 截图：target/theme-system/shots/_states/final-review
- 基线：6682098

## 2026-10-25 · 用户请求的完整主题基座与首个可爱主题

- 范围：严格主题包 schema、校验/目录加载、运行时 token 注入、选择/持久化/重载与回退、PNG 装饰、桃桃猫主题、素材卡语义徽标及文本行对齐。实现范围与暂不支持项见 `themes/README.md`；未将原路线图所有主题标为完成。
- 修复：ComboBox 双向绑定及 last-applied 索引保证选择失败时正确回退；首次导入后迁移旧程序目录设置，避免主题丢失；背景只按可见视口绘制，窗口缩放使用当前主题间距，去掉随机 Rust 兜底色。
- 验证：`cargo test -p ui-viewmodels -p app-ui --quiet` = 287 passed / 0 failed；包含启动/切换/重启/重载/坏资源回退、设置迁移和几何恢复。`ui_hygiene`/`deps_guard` 随定向测试通过，基线未放宽。定向 Clippy `--all-targets -- -D warnings` 最终增量复核通过（0 warnings）。
- 视觉：三主题 × 网格/空库/设置/文本状态/960px 窄窗/导入归类/共享确认，共 21 张真实 Slint 软件渲染截图；关键图逐一目视检查。最新桃桃猫总览/设置/文本窄窗又重新采集确认，路径 `target/theme-system/shots/_states/final-review/`。静态截图未覆盖下拉菜单展开态；运行时测试覆盖选择回调与持久化/回退。
- 分发：`package.ps1` 随包复制主题并检查主题/PNG/署名/许可证，脚本语法与独立主题复制 SHA256 验证通过。未执行 release 安装包构建。CI 增加 preview 配置检查与素材许可门禁。
- 素材：在线获取 Microsoft Fluent Emoji Cat face 3D（MIT，256×256，37,653 bytes），原许可证和来源在主题包内。没有新增 crate。
