# Implement — 通用导入 + 导入归类弹窗

> 依赖：crud 子任务已合并（`set_category` 通路、库写子命令模式、`for_each_asset_active`）。
> 节奏：红灯先行；每阶段末局部测试，收尾全量三道门。

## 阶段 0 — Spike（先做，结论回写 design.md 对应节）

- [x] S1 Slint 1.17 OS 文件拖入能力查证（design §4）：跑最小样本确认是否有 drop 事件；无 → 定 platform `IDropTarget` 兜底路线（子类化 vs 注册的共存结论一并定）。
- [x] S2 「统一归入 ▼」控件选型：ComboBox 不吃自由文本 → 验证 PopupMenu+输入行组合的可行最小实现。

## 阶段 1 — sample-library 收集层 + CLI

- [ ] 1.1 红灯（tools/sample-library/tests）：`--import-paths` 三分来源解析；混选（f:+p:）→ 包内分类落库 + 散文件走 override；`--category-override` 使 explicit 胜过 groupName（RuleChain 语义）；`--force-inbox`；取消路径零文件进库（CLI 无 UI，测=「未传 cmd 则不写库」）。
- [ ] 1.2 实现 `ImportSource` 解析 + `run()` 分发；分类数预扫描函数 `probe_source_categories`（EmoReader 清单 / 目录子项数，只读结构）。

## 阶段 2 — 归类弹窗 VM（ui-viewmodels，纯函数）

- [ ] 2.1 红灯 `classify_spec`：来源三分 × 选项表穷举（D50 表逐条：默认项、选项集、N 标注）；混选分组行数；`plan_groups` 对未知扩展名过滤=Unsupported 跳过语义一致。
- [ ] 2.2 实现 `classify.rs`（SourceKind/ClassifyMode/SourceGroup/plan_groups）；记忆结构 `RememberedClassify` + settings 字段 + SETTING_SPECS 新行「导入时询问归类」（describe/toggle 走 D28 机制，含旧 TOML 缺字段兼容读）。
- [ ] 2.3 红灯：记忆命中 → 不弹窗直接套方式；「统一归入」记忆含分类名；恢复询问 toggle 生效。

## 阶段 3 — 弹窗 UI + 三入口接线（app-ui）

- [ ] 3.1 ClassifyDialog 组件（appwindow.slint）：分组行 + 单选 + 条件性统一归入下拉（S2 结论）+ 记住勾选 + 导入/取消；入场下一帧翻转、出场两段式（按 motion 的既定模式先行实现）。
- [ ] 3.2 main.rs：三入口（主导入按钮 → 文件多选对话框（含 .emo 过滤）、导入文件夹、导入 .emo）汇流 `open_classify_dialog(paths)`；确认后拼 `--import-paths` 临时清单文件 → 现有 `spawn_import_pipeline`（阶段失败不触发重载，D36② 纪律保持）；取消 = 直接 return（零副作用）。
- [ ] 3.3 文件对话框多选：`FileDialogs` trait 扩 `pick_open_files`（FOS_ALLOWMULTISELECT，platform win32 增量；D16 装配不变）。
- [ ] 3.4 冒烟：samples/inbox 手工走三入口 + 混选；取消后库计数不变。

## 阶段 4 — 拖拽导入（按 S1 路线二选一）

- [ ] 4.1a（原生可行）：slint drop 回调 → 转路径清单进 `open_classify_dialog`。
- [ ] 4.1b（兜底）：`platform::win32::dragdrop`（IDropTarget + 路径提取 + 超时保护）；trait `FileDropSink` 进 lib 层；main.rs 装配进 `win32_runtime_deps()`，drop 事件经 channel → `invoke_from_event_loop`。
- [ ] 4.2 红灯/守卫：拖入含不支持类型文件 → 该文件跳过、其余正常进弹窗（mock sink 单测）。

## 阶段 5 — 收口

- [ ] 5.1 三道门全绿（含 layering_guard/deps_guard；若 4.1b 落地，platform 新模块须过 deps 白名单评审）。
- [ ] 5.2 D49/D50 回写 DECISIONS.md（含 spike 结论、记忆边界）；新约定进 spec（导入入口单弹窗纪律、库写子命令复用）走 trellis-update-spec。
