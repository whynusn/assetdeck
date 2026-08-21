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
  platform/      # Win32 实现：剪贴板/SendInput/前台窗口（trait + win32 impl）
  worker/        # 解码 worker 进程池：协议、监督重启、背压
  ui-viewmodels/ # ViewModel 层（纯 Rust，可全量单测）
  app-ui/        # Slint UI（薄壳）+ app 二进制入口
tools/
  bench-harness/ # 内存/帧率测量夹具（合成库生成器 + RSS 采样器）
```

依赖方向强制单向：`app-ui → ui-viewmodels → {domain,index,store,library,pipeline} → platform(trait)`。
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

### M1 查询模型与分面索引（1.5 周）— 纯 TDD 主战场 🔄 进行中
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

### M2 存储层（1 周）
store crate：
- [ ] `migration_v1_creates_assets_fts_tags_tables`
- [ ] `fts_search_chinese_filename_hits_trigram`（⚠️ 关键决策：FTS5 默认 unicode61 不切中文，**必须用 trigram tokenizer**——此测试先行锁定该决策）
- [ ] `metadata_roundtrip_survives_reopen`
- [ ] `schema_version_rejects_newer_db_file`
- [ ] `thumbnail_cache_path_stable_per_asset_id`

### M3 库管理与导入管线（1.5 周）
library + phash crate：
- [ ] `import_copies_file_into_library_layout`
- [ ] `duplicate_phash_rejected_no_second_copy`（红线：去重必做）
- [ ] `phash_hamming_distance_under_threshold_matches_golden`（确定性渐变图 golden 夹具）
- [ ] `async_copy_state_machine_preview_available_before_copy_done`（D7 连带义务）
- [ ] `copy_queue_respects_backpressure_cap`
- [ ] `manual_category_assigned_on_import` / `uncategorized_goes_to_inbox`（D5）
- [ ] `video_import_extracts_duration_and_thumbnail_job_dispatched_to_worker_only`（断言 UI 进程路径无解码调用）

### M4 Worker 进程池（1 周）
worker crate：
- [ ] `job_result_roundtrips_over_ipc_protocol`
- [ ] `worker_crash_supervisor_respawns_within_budget`
- [ ] `pool_size_capped_at_cpu_count`
- [ ] `idle_priority_set_on_worker_process`（Windows: PROCESS_MODE_BACKGROUND_BEGIN 断言）
- [ ] `poison_asset_fails_job_not_pool`（坏文件隔离）

### M5 UI 壳与虚拟化网格（2–3 周，最大风险项）
ui-viewmodels + app-ui：
- [ ] `viewmodel_window_of_100k_model_loads_only_visible_slice`（内存守卫：可见窗口外零缩略图驻留）
- [ ] `grid_layout_math_variable_aspect_no_overlap`（criterion：布局数学独立基准）
- [ ] `scroll_jump_10k_items_keeps_frame_budget`（软件渲染下近似测量，标注 best-effort）
- [ ] `selection_double_click_emits_open_asset_event`
- [ ] `filter_panel_changes_propagate_to_viewmodel_query`
- [ ] slintcn 组件以源码形式进 `app-ui/components/`，逐个冒烟实例化测试
- [ ] 📋 手工验收清单：120fps 滚动体感、IME 中文输入、DPI 缩放

### M6 粘贴管线（1.5 周）
pipeline + platform crate：
- [ ] `format_negotiation_table_image_video_text`（表驱动：资产类型×目标 profile→CF_HDROP/PNG/DIB/text）
- [ ] `paste_writes_clipboard_before_focus_switch`
- [ ] `focus_check_failure_degrades_to_copy_only`（mock WindowProvider 返回死窗口，红线 D8）
- [ ] `auto_send_flag_defaults_off`（配置默认值快照测试，红线）
- [ ] `auto_send_off_never_synththesizes_enter`（管线集成测：关开关时注入序列不含 VK_RETURN）
- [ ] `previous_foreground_window_recorded_on_panel_invoke`
- [ ] `real_sendinput_into_notepad`（`#[ignore]` 标注，本地手动跑；CI 不跑真实注入）

### M7 内存回归与闭环验收（1 周，行动项 A2/A3 收口）
tools/bench-harness：
- [ ] `synthetic_library_generator_produces_100k_metadata_rows`
- [ ] `idle_rss_under_100mb`（子进程启动 app，静置采样 WorkingSet，红线 D10）
- [ ] `browse_100k_rss_under_250mb`（驱动浏览路径后采样）
- [ ] `closed_loop_doubleclick_to_input_box_under_500ms`（端到端计时，行动项 A2 的自动化部分）
- [ ] CI 新增 `mem-regression` job，预算超标即红；趋势产物存 artifact

## 五、性能与内存回归方案（D10 落地）

1. **合成库生成器**：确定性生成 N 条元数据 + 渐变占位缩略图（无版权、可复现）；
2. **RSS 采样**：harness 以子进程拉起 app（`--bench` 模式），Win32 `GetProcessMemoryInfo` 采样 WorkingSet，静置 10s 取中位数；
3. **判定**：超预算即 CI 红，不允许「下次再修」；
4. **帧率**：布局数学用 criterion 硬测；真实渲染帧率暂为手工验收项（诚实标注，不假装自动化）。

## 六、无法自动化、必须人工验证的边界

诚实清单，写进每个 release 的检查单：

- 真实 IM 目标（微信/QQ/千牛/Telegram）的粘贴行为兼容矩阵；
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
| 焦点校验降级 | M6 mock 死窗口测试 |
| 仅 Windows | CI 仅 windows runner + platform crate cfg 门 |

## 十、里程碑顺序与工期合计

M0(0.5) → M1(1.5) → M2(1) → M3(1.5) → M4(1) → M5(2–3) → M6(1.5) → M7(1) ≈ **10–11 周**。
关键路径在 M5（瀑布流网格）；若 M5 spike 两周内达不到帧预算，回退方案：v1 先做等宽网格（布局数学简单一个数量级），变宽高比瀑布流降级为 v1.1。
