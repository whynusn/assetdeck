# TDD_PLAN.md — 测试驱动开发计划

> 依据：`DECISIONS.md`（D1–D12）+ `AGENTS.md`（硬约束红线）
> 制定日期：2026-08-21。仓库尚无代码，本计划同时充当脚手架蓝图。
> 假设：CI 为 GitHub Actions（windows-latest）；若换平台需同步修改第八节。

---

## 一、总纲：为什么 TDD 在这个项目可行

Slint 的 `.slint` 声明式代码难以直接 TDD。因此本项目的第一原则：

> **所有业务逻辑住在纯 Rust crate 里，`.slint` 只做哑渲染层。**

- 核心 crate（索引/检索/库管理/管线）**零 Slint 依赖**，纯 `cargo test` 驱动；
- UI 层通过 ViewModel（普通 Rust struct + trait 接口）与核心通信，ViewModel 全覆盖单测；
- `.slint` 文件本身只做冒烟级验证，不做单测。

这是整个计划成立的前提，违反它 = TDD 计划作废。

## 二、工作区结构与测试职责

```
crates/
  domain/        # 实体与查询模型：Asset、Category、Filter、Sorter（纯函数，零 IO）
  index/         # RoaringBitmap 分面索引 + facet 计数缓存
  store/         # SQLite 持久化、FTS5、迁移、smart folder 序列化
  library/       # .library 管理、异步拷贝队列、导入编排
  phash/         # pHash 计算与汉明距离匹配
  media/         # 缩略图/抽帧（仅接口定义在此，实现在 worker）
  pipeline/      # 粘贴管线：格式协商→剪贴板→焦点校验→注入→[auto-send]
  targets/       # 多 IM 目标册：profile 加载/覆盖、窗口匹配打分、TargetTracker 粘性状态机、L0–L3 体检编排（纯函数，零 IO、零平台依赖）
  platform/      # Win32 实现：剪贴板/SendInput/前台窗口（trait + win32 impl）
  worker/        # 解码 worker 进程池：协议、监督重启、背压
  ui-viewmodels/ # ViewModel 层（纯 Rust，可全量单测）
  app-ui/        # Slint UI（薄壳）+ app 二进制入口
tools/
  bench-harness/ # 内存/帧率测量夹具（合成库生成器 + RSS 采样器）
```

依赖方向强制单向：`app-ui → ui-viewmodels → {domain,index,store,library,pipeline} → targets → platform(trait)`。
**守卫测试**：`cargo-deny` bans 配置禁止 UI crate 直接依赖 media/phash/worker 实现 crate（红线「UI 进程不解码」的编译期守卫），并 ban 向量检索类依赖（faiss/usearch/torch 等，红线 D4）。

## 三、工具链约定

| 用途 | 选择 | 备注 |
|---|---|---|
| 单测/集成测 | `cargo test`（建议叠加 `cargo-nextest` 加速） | 标准 `#[test]`，不用额外测试框架 |
| 属性测试 | `proptest` | 仅用于索引正确性（对拍 oracle） |
| 基准 | `criterion` | 网格布局数学、位图交集、排序器 |
| Lint | `clippy -D warnings` + `rustfmt` | CI 必过 |
| 依赖审计 | `cargo-deny` | bans + licenses 双重检查 |
| 截图回归 | 暂缓 | Slint testing backend 可行后再评估 |

命令顺序（CI 与本地一致）：`fmt --check → clippy → deny check → test`。

## 四、里程碑与首批红灯测试

每个里程碑按 Red→Green→Refactor 推进；下列测试名是**每个里程碑最先写的失败测试**。

### M0 脚手架（0.5 周）✅ 已完成 2026-08-22
- [x] workspace 可 `cargo build`，CI 三件套绿（fmt/clippy -D warnings/test 本地全绿；ci.yml 就位）
- [x] `deny_check_bans_vector_and_ui_media_deps` —— 守卫测试通过（`deny_toml_bans_required_vector_entries` + `ui_cargo_toml_has_no_decode_layer_deps`）
- [x] 附加消险：Slint 1.17 在 windows-gnu 编译链接通过；窗口运行时渲染验证（空闲 WorkingSet 77.8MB < 100MB 预算）

### M1 查询模型与分面索引（1.5 周）✅ 已完成 2026-08-22
domain + index crate：
- [x] `filter_by_single_category_returns_matching_ids`
- [x] `intersect_two_facets_returns_conjunction`
- [x] `negated_filter_excludes_ids`
- [x] `facet_counts_match_bruteforce_oracle`（proptest 对拍；已抓到「单资产重复标签」领域不变量违规并修复）
- [x] `facet_count_cache_invalidates_on_tag_mutation`
- [x] `sorter_recency_then_name_is_stable_multisort`
- [x] `sorter_decoupled_from_filter_pipeline_order`
- [x] `empty_filter_returns_all_within_budget_1ms_at_1m`（✅ criterion 基线 @1M：交集 126µs / 单面 3.2µs / 全集 11.8µs，debug+release 双档断言通过）
- [x] `smart_folder_serde_roundtrip_preserves_filter_sorter`

> 实现备注：FacetIndex 含 assets HashMap + by_category/by_tag 位图 + 全集位图 + tag 计数缓存（变更即失效）；insert 为 upsert 语义。

### M2 存储层（1 周）✅ 已完成 2026-08-22
store crate：
- [x] `migration_v1_creates_assets_fts_tags_tables`
- [x] `fts_search_chinese_filename_hits_trigram`（⚠️ 关键决策已锁定：trigram tokenizer；**查询须为连续子串且 ≥3 字符**，2 字中文查询返回空；用户查询统一引号短语包裹——踩坑已沉淀至 `.trellis/spec/store/backend/database-guidelines.md`）
- [x] `metadata_roundtrip_survives_reopen`
- [x] `schema_version_rejects_newer_db_file`
- [x] `thumbnail_cache_path_stable_per_asset_id`

### M3 库管理与导入管线（1.5 周）✅ 已完成 2026-08-22
library + phash crate：
- [x] `import_copies_file_into_library_layout`
- [x] `duplicate_phash_rejected_no_second_copy`（红线：pHash 先算后拷，重复零磁盘代价）
- [x] pHash 质量属性四测（相同=0 / 微扰≤10 实测0 / 无关≥16 / 已知值）；golden 夹具改为**程序化结构化图案**——纯色/棋盘属退化图，pHash 会落在浮点噪声区（教训已沉淀 spec）
- [x] `async_copy_metadata_visible_before_copy_done`（D7 体感瞬时入库；暂停钩子保证确定性）
- [x] `copy_queue_respects_backpressure_cap`
- [x] 手动分类 / 未分类→「待分类」收件箱（D5）
- [x] 视频导入派发 media job（D6 前半）；时长/缩略图实装随 M4 worker（断言 UI 进程路径无解码调用）

### M4 Worker 进程池（1 周）✅ 已完成 2026-08-22
worker crate：
- [x] `job_result_roundtrips_over_ipc_protocol`
- [x] `worker_crash_supervisor_respawns_within_budget`
- [x] `pool_size_capped_at_cpu_count`
- [x] `idle_priority_set_on_worker_process`（⚠️ M4 裁决：BACKGROUND_BEGIN 仅限当前进程自设且有 32MiB 工作集封顶副作用 → 宿主设 IDLE_PRIORITY_CLASS + worker 入口自设 THREAD_MODE_BACKGROUND_BEGIN；GetPriorityClass 实测断言。已沉淀 worker spec）
- [x] `poison_asset_fails_job_not_pool`（坏文件隔离）
- [x] 附加：`restart_budget_exhaustion_degrades_pool`（重启超限 → degraded，submit 直接 Failed）

> 实现备注：stdio + NDJSON 协议（信封 `{"v":1,"req"/"res":…}`）；任务 Echo/ThumbnailPng；替补上限 3 次/池；视频抽帧实装因解码栈选型未决（ffmpeg sidecar vs 纯 Rust）另立任务，见任务 prd 范围外。

### M5 UI 壳与虚拟化网格（2–3 周，最大风险项）✅ 已完成 2026-08-22（自动化部分）
ui-viewmodels + app-ui：
- [x] `viewmodel_window_of_100k_model_loads_only_visible_slice`（内存守卫：100k 数据 + capacity 注入式 LRU，窗外零缩略图驻留；load 调用数有界断言）
- [x] `grid_layout_math_variable_aspect_no_overlap`（criterion @10k：173µs，spike 远优于预算，未触发等宽回退预案）
- [x] `scroll_jump_10k_items_keeps_frame_budget`（软件渲染近似，50ms 宽裕上界，best-effort 标注）
- [x] `selection_double_click_emits_open_asset_event`
- [x] `filter_panel_changes_propagate_to_viewmodel_query`
- [ ] slintcn 组件以源码形式进 `app-ui/components/`，逐个冒烟实例化测试 → **推迟**：网络不可用，v1 先用自写最小组件（哑渲染已够 M5 闭环）
- [ ] 📋 手工验收清单：120fps 滚动体感、IME 中文输入、DPI 缩放

> 实现备注：masonry 固定列数布局（最短列放置）；VM 全量预计算 Rect 表（O(1) 跳转，@1M≈32MB 待 M7 实测）；Slint 1.17 踩坑（MouseArea 移除/TouchArea double-clicked/name := 语法/property 可见性）已沉淀 app-ui spec。

### M6 粘贴管线（1.5 周）✅ 已完成 2026-08-22
pipeline + platform crate：
- [x] `format_negotiation_table_image_video_text`（表驱动 match；新增 AssetKind::Other 使「未知组合→None」可测）
- [x] `paste_writes_clipboard_before_focus_switch`（Op 日志下标精确断言）
- [x] `focus_check_failure_degrades_to_copy_only`（mock 死窗口，红线 D8）
- [x] `auto_send_flag_defaults_off`（快照全等 `{"auto_send":false}`，红线）
- [x] `auto_send_off_never_synththesizes_enter`（off 组零 0x0D / on 对照组含，注入序列 [VK_CONTROL,'V','V|KEY_UP',VK_CONTROL|KEY_UP]）
- [x] `previous_foreground_window_recorded_on_panel_invoke`
- [x] `real_sendinput_into_notepad`（`#[ignore]` 就位，本地手动跑）

> 实现备注：VK 相位协议 KEY_UP=0x8000 归 platform::lib 契约所有（低 15 位 VK 无碰撞）；win32 unsafe 审计无泄漏/double-free（Set 成功后所有权移交系统）；windows-sys 0.59 API 形态踩坑已沉淀 platform spec。

### M7 内存回归与闭环验收（1 周，行动项 A2/A3 收口）✅ 已完成 2026-08-22
tools/bench-harness：
- [x] `synthetic_library_generator_produces_100k_metadata_rows`（确定性：uuid 字符串方案/固定 created_at；批量事务秒级）
- [x] `idle_rss_under_100mb`（release 实测中位 62.8MB，余量 40%；debug 档硬断言同样通过）
- [x] `browse_100k_rss_under_250mb`（release 实测中位 29.9MB，余量 89%；debug 档仅打印不断言——分配行为失真）
- [x] `closed_loop_doubleclick_to_input_box_under_500ms`（自动化部分：VM.double_click→negotiate→真实 Win32 剪贴板写+CF_UNICODETEXT 读回逐字比对→焦点死降级 CopiedOnly；真实注入由 real_sendinput_into_notepad 人工补全）
- [x] CI 新增 `mem-regression` job，预算超标即红；趋势产物存 artifact（含每日 cron）

> 实现备注：采样器防御了 GetProcessMemoryInfo 对已退出进程返回恒定残留值(32KB)的陷阱（叠加 GetExitCodeProcess 判活）；idle 提前退出=测量失败=红；store 新增 upsert_assets 批量与 for_each_asset 流式枚举（uuid 升序，禁全量物化）；ui-viewmodels 新增 catalog_loader(uuid→顺序 AssetId 装配)。踩坑已沉淀 bench-harness spec。

### M8 多 IM 目标路由（2 周，落地 D13）📋 部分实现，未交付

新增 `crates/targets`（纯 Rust，零 IO / 零平台依赖）：目标册加载/覆盖、窗口匹配打分、`TargetTracker` 粘性状态机、体检编排。
`platform` 新增 trait：`WindowEnumerator` / `WindowActivator` / `ForegroundObserver`(WinEvent 钩子) / `ReadinessProbe`(P0 纯 Win32 + P1 UIA 独立 COM 线程 + 超时)，实现进 `win32.rs`，非 Windows 走零平台 import。
`pipeline` 改造：`negotiate()` 吃 profile 有序格式回落；`PasteSession.previous_foreground` → `TargetBinding`；新增就绪度阶段与 `PasteFeedback` 收敛层；`PasteOutcome::Injected` 携带 `verified: bool`；auto-send 挪到链路外的独立可选步。

先写的失败测试（按优先级分组）：

P0 核心链路：
- [x] `core_upload_path_never_synthesizes_enter`（**新红线**：核心上框注入序列绝无 0x0D）
- [x] `unknown_exe_falls_back_to_generic_profile`（长尾兜底）
- [x] `negotiate_honors_profile_ordered_format_fallback`（只吃文件的 IM 回落到 hdrop）
- [x] `not_ready_no_conversation_never_injects`（就绪度否证即止，降级 CopiedOnly）
- [x] `unknown_readiness_injects_but_marks_unverified`（Unknown 中间档 → verified:false）
- [x] `probe_timeout_falls_back_to_unknown_not_notready`（**Mock 契约**：UIA 超时映射为 Inconclusive→verified:false；真实 UIA 超时未实现）
- [x] `l3_selftest_reads_back_sentinel_and_cleans_up`（**仅 Mock 报告判定**：SelfTestReport 读回+清场+无 Enter；真实哨兵写入/读回/清场未实现）
- [x] `custom_target_requires_l0_l2_before_enabling`（自定义目标未过体检不得启用）

P1 精准/粘性（`TargetTracker` 纯函数状态机）：
- [x] `eligible_target_foreground_rewrites_hot_target`（唯一改写路径）
- [x] `unrelated_foreground_does_not_change_hot_target`（铁律 A）
- [x] `own_panel_foreground_is_ignored_by_tracker`（自身不沾染）
- [x] `hot_target_has_no_ttl` / `pinned_target_not_overwritten`（无衰减 / 图钉冻结）
- [ ] `hot_target_survives_close_to_tray_and_reopen`（部分：同 profile 唯一候选可重绑；**仍未证明同一账号/会话/窗口实例**）
- [x] `resolve_two_wechat_windows_returns_ambiguous`（多开不静默选择一个）
- [ ] `readonly_conversation_detected_and_blocked`（UIA 只读会话：未实现）
- [x] `foreground_drift_before_inject_aborts`（注入前最后一次前台校验，铁律 B）
- [x] `health_grade_downgrades_to_yellow_when_readiness_unprobeable`（四色语义：黄≠绿，绿只来自 L3）
- [x] `window_not_running_is_unknown_not_red`（回归：休眠=灰，不是 L2 失败的红）

反馈完备性：
- [x] `every_not_ready_reason_maps_to_nonempty_feedback`（枚举穷举防漏）
- [x] `feedback_headline_contains_target_label`（回显目标名）
- [x] `all_degraded_feedback_mentions_clipboard_copied`（先说已复制）

P2 增强（不阻塞核心）：
- [x] `auto_send_off_never_synthesizes_enter`（沿用 M6 序列断言，开关独立）

载荷正确性（D14 根因回归守卫，2026-08-23 新增）：
- [x] `hdrop_promotes_relative_paths_to_absolute`（相对路径必须被提升，不能原样写进 CF_HDROP）
- [x] `hdrop_keeps_absolute_paths_and_terminates_list`（绝对路径不改写 + 双 NUL 终止布局）
- [x] `hdrop_rejects_empty_path_list`
- [x] `materialized_source_path_is_absolute_for_relative_library_root`（相对库 root + `/` 分隔 rel_path → 绝对且存在）
- [x] `video_payload_keeps_absolute_file_path_and_no_inline_bytes`

键盘焦点送进输入框（D21 `InputFocuser` 三级降级，2026-08-24 新增）：
- [x] `focus_step_runs_between_activate_and_probe`（顺序 write → activate → focus_input → probe → 前台复核 → Ctrl+V）
- [x] `focus_step_never_injects_keys_before_paste_chord`（焦点步只允许鼠标/UIA，绝不先合成按键）
- [x] `focus_unavailable_still_injects_but_marks_unverified`（`Unavailable` 不降级为仅复制，注入并标 `verified:false`）
- [x] `confirmed_focus_upgrades_inconclusive_probe_to_verified`（焦点确证可把 `Inconclusive` 升格为已验证）
- [x] `uia_strict_aborts_when_focus_unavailable`（严格画像才在拿不到焦点时中止）
- [x] `profile_anchor_is_forwarded_to_focuser_verbatim` / `profile_without_anchor_forwards_none`（锚点原样透传，缺锚点不臆造点击）
- [x] `input_anchor_is_parsed_and_exposed_as_focus_anchor` / `profile_without_input_anchor_yields_no_click_target` / `out_of_range_anchor_is_rejected_instead_of_clamped` / `user_profile_can_retune_input_anchor`（`crates/targets` 锚点解析：越界报错不夹紧，用户覆盖可重调）

`paste_sends` 画像能力（D18，粘贴即发送的 IM 不得触发发送）：
- [x] `negotiate_skips_paste_sends_formats`（协商阶段就跳过会自发的格式）
- [x] `paste_sends_falls_back_to_safe_format_when_available`（有安全格式则回落）
- [x] `paste_sends_format_copies_without_injecting`（无安全格式则只复制，绝不注入）
- [x] `paste_sends_feedback_tells_user_to_paste_manually`（提示用户手动粘贴）
- [x] `unsupported_and_would_send_are_distinct_results`（「不支持」与「会自发」是两种结果，不能混为一谈）
- [x] `paste_sends_is_per_kind_not_per_format`（D22 修订：`paste_sends` 按「类别 × 格式」声明，千牛只有视频 HDROP 会即发）
- [x] `legacy_flat_paste_sends_still_covers_every_kind`（旧的扁平数组写法仍解析为「所有类别」）
- [x] `builtin_image_route_prefers_file_reference_over_full_png`（微信/千牛图片首选 `files`，`png` 退为末位兜底）
- [x] `builtin_qianniu_sends_only_video_hdrop_not_image_hdrop`

> M8 另外新增并通过：`targeted_pipeline_order_is_write_activate_probe_validate_inject`、`selected_cold_target_reaches_exact_hwnd_and_never_synthesizes_enter`、`no_selected_target_still_copies_before_friendly_feedback`、`same_profile_windows_are_selected_by_unique_window_key`、`chip_shows_hot_target_without_user_click`、`ambiguous_expands_picker`、`fallback_target_requires_first_use_confirm`、`pin_toggle_freezes_chip`、`l3_selftest_sequence_contains_no_enter`。

> 当前 M8 已跑通真实双目标闭环：微信 2163916（文件传输助手）的文本/图片/视频、千牛 721614（接待中心）的文本/图片都通过产品路径进入输入框，全程无 Enter，截图取证在 `Default_Project_probe/`。千牛的视频例外——`paste_sends=["files"]` 意味着粘贴文件会被千牛当场发出，按 D18 停在「只复制 + 提示手动 Ctrl+V」，这是为守住「不替用户发消息」而刻意保留的边界。上一轮「jpg/mp4 上不了框」的根因是 CF_HDROP 写了相对路径被 IM 静默丢弃（DECISIONS D14），已修复并补齐上述五条守卫；就绪策略同时翻转为「否证阻塞才不注入」（D15）。

> 2026-08-24 补验：`asset-manager.exe`（`--library-root samples\library`）双击真实素材 `dog.jpg`，**全程不手工点击 IM 输入框**，微信与千牛都由 `Win32InputFocuser` 自行把键盘焦点送进输入框后落框——微信提示「已粘贴到 微信 (4.0) · 微信，请确认输入框内容」，千牛提示「已上框到 千牛 · tb940472610424-接待中心」，截图 `Default_Project_probe/r14-prod-wechat-1.png`、`r14-prod-qianniu-1.png` 确认素材停在输入框待发区、发送按钮未被触发。千牛的 mp4 因 `paste_sends=["files"]` 按 D18 停在「只复制 + 提示手动粘贴」，这是有意的边界。真实 PDD/Telegram（缺会话）、热键唤起、自定义目标持久化、L3 真实执行器和 WinEvent/PrintWindow 收口仍未完成，任务状态保持 `in_progress`。

> 2026-08-25 修订（D22）：上条里「图片走 `CF_PNG`」的表述已过期。微信与千牛的图片主路径改为
> `files`（`CF_HDROP` 交文件引用），`png` 退为末位兜底；真机实测端到端从 2061~3346ms 降到
> 587~1693ms，对端进程 CPU 从 1859~2234ms 降到 156~312ms，取证 `r17-wx-prodpath-files.png`、
> `r17-qn-prodpath-files.png`（输入框内是真缩略图，非文件名卡片）。「千牛 mp4 只复制不注入」仍成立，
> 但依据收窄为「千牛**仅视频** HDROP 即发」，图片 HDROP 实测停在输入框。

> 人工验证边界（见第六节）：真实 IM 的 exe/类名/标题/UIA 可用性、Electron 壳 PrintWindow 行为、WinEvent 常驻内存，均须本机实测（行动项 A5/A6），配方化后每次只落一行 TOML + 一次体检，而非发版重编。微信/千牛已取得本机实测部分，PDD/Telegram 仍待补。

> 任务级分解（开工闸门 / T1–T10 交付物 / D1–D10 排期 / 风险回退 / DoD）见 Trellis 任务目录 `.trellis/tasks/08-23-m8-target-routing/`（`prd.md` / `design.md` / `implement.md`，原始推导底稿在 `research/m8-plan-draft.md`）。

## 五、性能与内存回归方案（D10 落地）

1. **合成库生成器**：确定性生成 N 条元数据 + 渐变占位缩略图（无版权、可复现）；
2. **RSS 采样**：harness 以子进程拉起 app（`--bench` 模式），Win32 `GetProcessMemoryInfo` 采样 WorkingSet，静置 10s 取中位数；
3. **判定**：超预算即 CI 红，不允许「下次再修」；
4. **帧率**：布局数学用 criterion 硬测；真实渲染帧率暂为手工验收项（诚实标注，不假装自动化）。

## 六、无法自动化、必须人工验证的边界

诚实清单，写进每个 release 的检查单：

- 真实 IM 目标（微信/QQ/千牛/Telegram）的粘贴行为兼容矩阵；
- D13 目标册 builtin 数值（exe/窗口类名/标题模板/可接受剪贴板格式/settle_ms）与各 IM 的 UIA 树可用性（Electron 壳是否需 `--force-renderer-accessibility`）；
- 各 IM 自我会话的 L3 端到端自证（微信文件传输助手 / QQ 我的电脑 / Telegram Saved Messages）；
- `PrintWindow + PW_RENDERFULLCONTENT` 对 Electron 壳的悬停快照表现（全黑/耗时/挂线程）；
- UIPI：管理员权限窗口收不到 SendInput 的降级表现；
- 多显示器/DPI 变化下的浮层定位；
- Wayland 相关一切（v2 才涉及，见 `DECISIONS.md` 第四节归档）。

## 七、夹具策略

- `fixtures/images/`：程序化生成的确定性图片（渐变/几何图形），带 golden pHash；
- `fixtures/video/`：单个 <1MB 的手工压制 mp4（多平台 CI 均可解）；
- `fixtures/library/`：预置 mini .library 包（100 条）供 store/library 集成测；
- 禁止用网络下载或非确定性素材做测试夹具。

## 八、CI 流水线（windows-latest）

```
job lint:        fmt --check + clippy -D warnings + deny check
job test:        cargo nextest run（全部单元+集成，含 proptest 200 例）
job mem-regression: bench-harness 跑 M7 两项 RSS 断言（每日定时 + PR 触发）
job ignore-tests:  不跑（真实注入类留本地）
```

缓存 `~/.cargo` 与 `target`；Rust stable 工具链固定于 `rust-toolchain.toml`。

## 九、红线 ↔ 守卫测试映射（速查）

| AGENTS.md 红线 | 守卫 |
|---|---|
| 内存预算 D10 | M7 RSS 断言 + mem-regression job |
| UI 进程不解码 | cargo-deny 依赖禁令 + M3 worker-only 断言 |
| v1 禁向量检索 | cargo-deny bans |
| pHash 去重必做 | M3 duplicate_rejected 测试 |
| auto-send 默认关 | M6 默认值快照 + 注入序列断言 |
| 核心上框链路绝不合成回车（D13） | M8 `core_upload_path_never_synthesizes_enter` |
| 不确定就降级仅复制（D13 铁律 B） | M8 `foreground_drift_before_inject_aborts` + `not_ready_no_conversation_never_injects` |
| 上框止步输入框，粘贴不得触发发送（D18） | M8 `paste_sends_format_copies_without_injecting` + `negotiate_skips_paste_sends_formats` |
| 焦点注入只用鼠标/UIA，不得先合成按键（D21） | M8 `focus_step_never_injects_keys_before_paste_chord` + `focus_step_runs_between_activate_and_probe` |
| 焦点校验降级 | M6 mock 死窗口测试 |
| 仅 Windows | CI 仅 windows runner + platform crate cfg 门 |

## 十、里程碑顺序与工期合计

M0(0.5) → M1(1.5) → M2(1) → M3(1.5) → M4(1) → M5(2–3) → M6(1.5) → M7(1) → M8(2) ≈ **12–13 周**。
关键路径在 M5（瀑布流网格）；若 M5 spike 两周内达不到帧预算，回退方案：v1 先做等宽网格（布局数学简单一个数量级），变宽高比瀑布流降级为 v1.1。
M8（多 IM 目标路由）依赖 A5/A6 的本机实测数据，纯逻辑部分（`crates/targets` 状态机与体检编排）可先行 TDD，平台探测与 builtin 数值待实测填充。
