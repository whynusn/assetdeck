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
