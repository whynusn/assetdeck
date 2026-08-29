//! 薄壳：VM 装配 + Slint 回调桥接。业务逻辑全在 ui-viewmodels，本文件不做计算密集工作。
#![windows_subsystem = "windows"]

slint::include_modules!();

mod cards;
mod task_runner;
mod thumbs;
mod ui_enums;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use slint::{Model, ModelRc, Timer, TimerMode, VecModel};
use ui_viewmodels::classify::{self, EntryKind, GroupKind, GroupMode, ImportEntry, SourceGroup};
use ui_viewmodels::grid_vm::LibraryGridVm;
use ui_viewmodels::selection::{self, MenuAction};
use ui_viewmodels::{
    AppSettings, Asset, AssetId, AssetKind, AssetPayload, CategoryId, DarkThemeProvider,
    FacetIndex, Filter, LightThemeProvider, RealAssetResolver, SearchProvider, SortDirection,
    SortField, SortSpec, Sorter, TagId, TargetBarMode, TargetBarSnapshot, TargetHealth,
    TargetNoticeTone, TargetRoutingRuntime, TargetRuntimeDeps, ThemeProvider, ThemeTokens,
};

use task_runner::ChildTask;
use thumbs::{GridCtx, ThumbCache, ThumbSource, THUMB_CACHE_CAPACITY};

thread_local! {
    /// 成功提示的自动消隐计时器。Slint 的 Timer 非 Send，只能在 UI 线程持有；
    /// 放线程局部里，供 `show_notice` 每次成功时重启，警告/错误时停掉。
    // D50 弹窗两段式动效的单发定时器：入场下一帧翻转 / 出场播完再卸载。
    static CLASSIFY_ANIM_TIMER: RefCell<Timer> = RefCell::new(Timer::default());
    static NOTICE_TIMER: RefCell<Timer> = RefCell::new(Timer::default());
}

/// 演示数据规模：无真实库环境时的合成资产。
const DEMO_COUNT: u32 = 500;
/// 网格容器几何：首帧回退值，真实值由 `viewport-width` 变更回调重算。
const CONTAINER_WIDTH: f32 = 964.0;
const COLUMNS: u32 = 6;
const GAP: f32 = 12.0;
/// 目标列宽：容器宽变化时按此推列数，保证瓦片不会被拉成横条或压成细缝。
const TARGET_COLUMN_WIDTH: f32 = 150.0;
const MIN_COLUMNS: u32 = 2;
const MAX_COLUMNS: u32 = 12;

const BUILTIN_PROFILES: &str = include_str!("../../../profiles/profiles.builtin.toml");

fn demo_index() -> FacetIndex {
    let mut idx = FacetIndex::new();
    for i in 0..DEMO_COUNT {
        idx.insert(&Asset {
            id: AssetId(i),
            name: format!("演示资产-{i:04}"),
            category: Some(CategoryId(i % 5)),
            tags: vec![TagId(i % 7)],
            created_at: i as i64,
            size_bytes: None,
            kind: AssetKind::Image,
        });
    }
    idx
}

fn recent_first_sorter() -> Sorter {
    Sorter {
        keys: vec![SortSpec {
            field: SortField::CreatedAt,
            direction: SortDirection::Desc,
        }],
    }
}

/// 容器宽度 → 列数：贴在 [`TARGET_COLUMN_WIDTH`] 并夹在合理区间内。
fn columns_for(container_width: f32) -> u32 {
    ((container_width / TARGET_COLUMN_WIDTH).round() as i64)
        .clamp(MIN_COLUMNS as i64, MAX_COLUMNS as i64) as u32
}

fn health_color(health: TargetHealth) -> slint::Color {
    let argb = match health {
        TargetHealth::Green => 0xFF5AC18E,
        TargetHealth::Yellow => 0xFFE0B84F,
        TargetHealth::Red => 0xFFE06C75,
        TargetHealth::Unknown => 0xFF777B80,
    };
    slint::Color::from_argb_encoded(argb)
}

fn sync_target_bar(
    ui: &AppWindow,
    choices_model: &VecModel<TargetChoiceData>,
    snapshot: TargetBarSnapshot,
) {
    ui.set_target_label(snapshot.label.into());
    ui.set_target_health_color(health_color(snapshot.health));
    ui.set_target_pinned(snapshot.pinned);
    let mode = ui_enums::target_bar_mode(snapshot.mode);
    if mode != ui_enums::target_bar_mode(TargetBarMode::ChooseTarget) {
        // D53：目标下拉卸载即重置 shown，防重挂载时残留 true 跳过入场。
        ui.set_target_picker_shown(false);
    }
    ui.set_target_mode(mode);
    choices_model.set_vec(
        snapshot
            .choices
            .into_iter()
            .map(|choice| {
                let available = choice.binding.hwnd.is_some();
                TargetChoiceData {
                    selection_key: choice.selection_key().into(),
                    label: choice.binding.label.into(),
                    status: if available {
                        if choice.binding.visible {
                            "运行中 · 可选择".into()
                        } else {
                            "隐藏中 · 可选择".into()
                        }
                    } else {
                        "未运行 · 选择后仅复制".into()
                    },
                    health_color: health_color(choice.health),
                    available,
                }
            })
            .collect::<Vec<_>>(),
    );
}

fn show_notice(ui: &AppWindow, tone: TargetNoticeTone, text: String) {
    ui.set_notice_tone(ui_enums::notice_tone(tone));
    ui.set_notice_text(text.into());
    // 成功类提示是「事情办成了」的一次性反馈，无需常驻挤占纵向空间：几秒后自动消隐。
    // 警告/错误需要用户读到并处理，保持常驻，由用户手动关闭或被下一条提示覆盖。
    if matches!(tone, TargetNoticeTone::Success) {
        let ui_weak = ui.as_weak();
        NOTICE_TIMER.with(|slot| {
            slot.borrow().start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(3200),
                move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_notice_text("".into());
                    }
                },
            );
        });
    } else {
        NOTICE_TIMER.with(|slot| slot.borrow().stop());
    }
}

/// 前台变化唤醒闭包工厂：只做「敲醒 UI 事件循环去 poll」一件事，
/// 真正的窗口枚举与热目标决策留在 UI 线程的 `on_poll_targets` 里。
/// 工厂化是因为退路 Timer 每轮重试接管事件驱动时都要重新 `Box` 一份。
fn poll_targets_wakeup(handle: slint::Weak<AppWindow>) -> impl Fn() + Send + Sync {
    move || {
        let handle = handle.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = handle.upgrade() {
                ui.invoke_poll_targets();
            }
        });
    }
}

// ---------------------------------------------------------------------------
// D46/D47/D48 CRUD 壳层协作件：选区同步 + 库写子命令派发 + 回收站视图。
// ---------------------------------------------------------------------------

/// CRUD 动作的共享上下文（与 handlers 同生命周期，全部 UI 线程访问）。
/// 集中一处，避免每个 handler 抄一遍五元组 clone。
#[derive(Clone)]
struct CrudCtx {
    ui: slint::Weak<AppWindow>,
    vm: Rc<RefCell<LibraryGridVm>>,
    resolver: ThumbSource,
    grid: Rc<GridCtx>,
    filter_categories: Rc<RefCell<Vec<String>>>,
    current_filter: Rc<RefCell<Filter>>,
    filter_label: Rc<RefCell<slint::SharedString>>,
    library_root: Option<String>,
    importing: Arc<AtomicBool>,
    thumb_cache: Rc<RefCell<ThumbCache>>,
}

/// 回收站视图的哨兵分类号（与 appwindow.slint 侧栏条目 -3 对应）。
const TRASH_CATEGORY: i32 = -3;

impl CrudCtx {
    fn is_trash_view(&self) -> bool {
        matches!(*self.current_filter.borrow(), Filter::Trash)
    }

    /// 当前过滤器（复制出来，避免长持借用）。
    fn filter(&self) -> Filter {
        self.current_filter.borrow().clone()
    }

    /// 菜单/操作条动作的目标 id 集：选区优先，空则回退右键命中的那张。
    fn action_targets(&self, hit: Option<AssetId>) -> Vec<AssetId> {
        let vm = self.vm.borrow();
        let ids = vm.selection_ids();
        if !ids.is_empty() {
            return ids;
        }
        hit.into_iter().collect()
    }

    /// 把选区中的 AssetId 翻成 uuid（库写子命令的入参）。解析不出来的跳过。
    fn uuids_of(&self, ids: &[AssetId]) -> Vec<String> {
        let binding = self.resolver.borrow();
        let Some(resolver) = binding.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(uuid) = resolver.uuid_of(*id) {
                out.push(uuid.to_string());
            }
        }
        out
    }

    /// 按当前过滤器刷新视图：列表重算、计数、回收站角标、滚动回顶、瓦片重建，
    /// 外加选区栏形态。过滤器切换（含回收站进出）的唯一出口。
    fn sync_view_after_filter(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let (total, trash_count) = {
            let vm = self.vm.borrow();
            (vm.total(), vm.trash_count())
        };
        ui.set_content_y(0.0);
        sync_counts(&ui, total, self.resolver.borrow().is_some());
        ui.set_trash_count(trash_count as i32);
        // 回收站空视图专属教学句（操作条不再承载长提示，见 sync_selection）。
        if self.is_trash_view() && total == 0 {
            ui.set_empty_title("回收站是空的".into());
            ui.set_empty_hint("删除的素材会出现在这里，可恢复或彻底删除".into());
        }
        self.grid.sync();
        self.sync_selection();
    }

    /// 选区/模式变化统一出口：同步顶栏高亮、操作条形态与文案，并刷瓦片勾选态。
    fn sync_selection(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let vm = self.vm.borrow();
        let multi = vm.multi_mode();
        let trash = self.is_trash_view();
        let count = vm.selected_count();
        ui.set_select_mode_active(multi);
        ui.set_selection_bar(if trash {
            ui_enums::BAR_TRASH
        } else if multi {
            ui_enums::BAR_MULTI
        } else {
            ui_enums::BAR_HIDDEN
        });
        // 操作条标签只报状态（教学句移交空态视图：长句曾把按钮挤出条外）。
        // 张/项统一为「项」：回收站与多选都可能含视频/文本。
        ui.set_selection_text(
            if trash {
                if count > 0 {
                    format!("回收站 · 已选 {count} 项")
                } else {
                    "回收站".to_string()
                }
            } else {
                format!("已选 {count} 项")
            }
            .into(),
        );
        drop(vm);
        self.grid.sync();
    }

    /// 切换过滤器（侧栏分类 / 回收站入口共用）：写 current_filter + VM 重算 +
    /// 清检索框 + 重钉侧栏高亮与顶栏后缀。与 on_filter_selected 原路径同构。
    fn apply_filter(&self, f: Filter, label: slint::SharedString) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        *self.current_filter.borrow_mut() = f.clone();
        *self.filter_label.borrow_mut() = label.clone();
        {
            let mut vm = self.vm.borrow_mut();
            vm.set_filter(&f);
        }
        // 切视图清掉检索框：搜索与分类/回收站视图不叠加，避免「回收站里搜出
        // 正常素材」的口径混乱（R2：回收站不占搜索结果）。
        ui.set_search_text("".into());
        // 高亮哨兵：-1=全部，-3=回收站，分类=其 0 基下标（与侧栏条目一致）。
        ui.set_selected_category(match &f {
            Filter::All => -1,
            Filter::Trash => TRASH_CATEGORY,
            Filter::InCategory(cat) => cat.0 as i32,
            _ => -2,
        });
        ui.set_filter_label(label);
        self.sync_view_after_filter();
    }

    /// 派发库写子命令（单写者纪律：app-ui 不直开 meta.db，见 deps_guard）。
    /// 完成后经 `libcmd-finished` 回调弹回 UI 线程收尾（与导入管线同模式：
    /// 回调闭包只许 Fn+Send，故 Rc 型上下文一律经 ui 句柄转接，不能直捕）。
    fn spawn_lib_cmd(&self, action: &str, uuids: &[String], value: Option<&str>, label: &str) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if self.importing.load(Ordering::SeqCst) {
            show_notice(
                &ui,
                TargetNoticeTone::Warning,
                "正在导入/生成缩略图，请等进度条结束后再操作".to_string(),
            );
            return;
        }
        if uuids.is_empty() && action != "empty-trash" {
            return;
        }
        let root = self
            .library_root
            .clone()
            .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
        let mut args: Vec<String> = vec!["--cmd".into(), action.into(), "--library".into(), root];
        for uuid in uuids {
            args.push("--uuid".into());
            args.push(uuid.clone());
        }
        if let Some(value) = value {
            args.push("--value".into());
            args.push(value.to_string());
        }
        ui.set_progress_visible(true);
        ui.set_progress_percent(0.0);
        ui.set_progress_text(label.into());
        logging::info!(
            "库写子命令 action={action} items={} label={label}",
            uuids.len()
        );

        let weak = ui.as_weak();
        let weak_progress = weak.clone();
        let label_owned = label.to_string();
        let weak_done = weak.clone();
        let label_done = label_owned.clone();
        let mut task = ChildTask::new(helper_exe("sample-library.exe"), args);
        if let Some(dir) = logging::logs_dir() {
            task = task
                .with_env("DSH_LOG_DIR", &dir.to_string_lossy())
                .with_env("DSH_LOG_LEVEL", logging::current_level().as_str());
        }
        let _ = task
            .with_progress(move |done, total| {
                let weak = weak_progress.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_progress_visible(true);
                        ui.set_progress_percent(if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.0
                        });
                    }
                });
            })
            .with_finished(move |success, message| {
                let weak = weak_done.clone();
                let label = label_done.clone();
                let message = message.trim().to_string();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_libcmd_finished(success, message.into(), label.into());
                    }
                });
            })
            .run_in_background();
    }

    /// 子命令收尾（libcmd-finished 的落点）：整库重载与库状态对齐，随后重钉
    /// 当前视图高亮、刷新选区栏。失败时错误原文上通知条——本地即时反馈与库
    /// 已分叉，重载后差异自然收敛。
    fn reload_after_cmd(&self, success: bool, message: &str, label: &str) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_progress_visible(false);
        let root = self
            .library_root
            .clone()
            .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
        match ui_viewmodels::load_real_library(std::path::Path::new(&root)) {
            Ok((index, resolver)) => {
                let names = resolver.category_names();
                let counts = category_counts_for(&resolver, &names);
                apply_categories(&ui, &self.filter_categories, &names, &counts);
                let mut new_vm = LibraryGridVm::new(index, recent_first_sorter(), 256);
                new_vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
                if let Ok(aspects) = resolver.aspects() {
                    new_vm.set_aspects(aspects);
                }
                *self.vm.borrow_mut() = new_vm;
                *self.resolver.borrow_mut() = Some(resolver);
                self.thumb_cache.borrow_mut().clear();
                // apply_categories 把高亮拍回「全部」，按当前过滤器重钉。
                // label 必须先落本地：`.borrow().clone()` 内联进实参时 Ref 卫队
                // 活到整条语句结束，apply_filter 内部的 borrow_mut 立即
                // BorrowMutError（D48 卡退根因，守卫测试锁定，勿回退）。
                let label = self.filter_label.borrow().clone();
                self.apply_filter(self.filter(), label);
            }
            Err(error) => {
                show_notice(&ui, TargetNoticeTone::Error, format!("库刷新失败: {error}"));
                return;
            }
        }
        if success {
            show_notice(&ui, TargetNoticeTone::Success, format!("{label}完成"));
        } else {
            let detail = if message.is_empty() {
                String::new()
            } else {
                format!("：{message}")
            };
            show_notice(&ui, TargetNoticeTone::Error, format!("{label}失败{detail}"));
        }
    }
}

// ---------------------------------------------------------------------------
// D49/D50 通用导入流：三入口汇流 → 分类数预扫描 → 归类弹窗 → 清单子进程。
// ---------------------------------------------------------------------------

/// 弹窗行模型在壳层的持有物 + 决策装配。全部 UI 线程访问。
struct ImportFlow {
    ui: slint::Weak<AppWindow>,
    rows: Rc<VecModel<ClassifyRowData>>,
    groups: Rc<RefCell<Vec<SourceGroup>>>,
    entries: Rc<RefCell<Vec<ImportEntry>>>,
    /// 待预扫描条目数（probe 回来一个减一个，归零即 finalize）。
    pending: Rc<Cell<u32>>,
    settings: Rc<RefCell<AppSettings>>,
    settings_path: Rc<std::path::PathBuf>,
    categories: Rc<RefCell<Vec<String>>>,
    importing: Arc<AtomicBool>,
    library_root: Option<String>,
}

fn mode_code(mode: GroupMode) -> i32 {
    match mode {
        GroupMode::PerSource => 0,
        GroupMode::Unified => 1,
        GroupMode::Inbox => 2,
    }
}

fn mode_of(code: i32) -> GroupMode {
    match code {
        1 => GroupMode::Unified,
        2 => GroupMode::Inbox,
        _ => GroupMode::PerSource,
    }
}

/// 档位 → --mode 值（D37）。
fn import_mode_arg(settings: &AppSettings) -> &'static str {
    if settings.fast_import_mode {
        "fast"
    } else {
        "background"
    }
}

impl ImportFlow {
    /// 入口：三入口汇流（混选多选 / 文件夹 / .emo）。先归纳条目，需要 N
    /// 标注的（目录与 .emo）逐个起 `--probe-categories`，全部回来再 finalize。
    fn open(self: &Rc<Self>, paths: Vec<std::path::PathBuf>) {
        if self.importing.load(Ordering::SeqCst) {
            if let Some(ui) = self.ui.upgrade() {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "已经在导入素材，请等进度条结束后再操作".to_string(),
                );
            }
            return;
        }
        let entries: Vec<ImportEntry> = paths
            .into_iter()
            .map(|path| {
                let kind = if path.is_dir() {
                    EntryKind::Directory
                } else if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("emo"))
                {
                    EntryKind::EmoPackage
                } else {
                    EntryKind::LooseFile
                };
                ImportEntry {
                    path,
                    kind,
                    category_count: None,
                }
            })
            .collect();
        let pending = entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Directory | EntryKind::EmoPackage))
            .count() as u32;
        *self.entries.borrow_mut() = entries;
        self.pending.set(pending);
        if pending == 0 {
            self.finalize();
            return;
        }
        let paths: Vec<std::path::PathBuf> = self
            .entries
            .borrow()
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Directory | EntryKind::EmoPackage))
            .map(|e| e.path.clone())
            .collect();
        for path in paths {
            self.spawn_probe(path);
        }
    }

    /// 单条来源的 N 预扫描：stdout 一行 `PROBE<HT>categories=<n|none>`；
    /// 子进程线程只许 Weak<AppWindow>（Send 纪律），结果经回调回 UI 线程。
    fn spawn_probe(self: &Rc<Self>, path: std::path::PathBuf) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let weak_ui = ui.as_weak();
        let weak_done = ui.as_weak();
        let mut task = ChildTask::new(
            helper_exe("sample-library.exe"),
            vec![
                "--probe-categories".into(),
                path.to_string_lossy().into_owned(),
            ],
        );
        if let Some(dir) = logging::logs_dir() {
            task = task
                .with_env("DSH_LOG_DIR", &dir.to_string_lossy())
                .with_env("DSH_LOG_LEVEL", logging::current_level().as_str());
        }
        let probe_path = path.clone();
        let _ = task
            .with_line(move |line| {
                // 行形如 `PROBE<HT>categories=3`；解析失败视作 none。
                let count = line
                    .strip_prefix("PROBE	categories=")
                    .and_then(|rest| rest.trim().parse::<usize>().ok())
                    .map(|n| n as i32)
                    .unwrap_or(-1);
                let weak = weak_ui.clone();
                let path = probe_path.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_classify_probe_result(
                            path.to_string_lossy().into_owned().into(),
                            count,
                        );
                    }
                });
            })
            .with_finished(move |success, message| {
                let weak = weak_done.clone();
                let path_text = path.to_string_lossy().into_owned();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        if !success {
                            logging::warn!("分类数预扫描失败 path={path_text}：{message}");
                        }
                        ui.invoke_classify_probe_finished();
                    }
                });
            })
            .run_in_background();
    }

    /// probe 结果落账（UI 线程）。
    fn apply_probe(&self, path: &std::path::Path, count: Option<usize>) {
        if let Some(entry) = self
            .entries
            .borrow_mut()
            .iter_mut()
            .find(|e| e.path == path)
        {
            entry.category_count = count;
        }
    }

    /// probe 终态（成败都算）：归零即装配弹窗。
    fn probe_done(self: &Rc<Self>) {
        let remaining = self.pending.get().saturating_sub(1);
        self.pending.set(remaining);
        if remaining == 0 {
            self.finalize();
        }
    }

    /// 全部 probe 就绪：D50 表分组 → 记忆预填 → 全记忆直通 / 弹窗。
    fn finalize(self: &Rc<Self>) {
        let groups = classify::plan_groups(&self.entries.borrow());
        if groups.is_empty() {
            return;
        }
        let (ask, memories) = {
            let settings = self.settings.borrow();
            (
                settings.ask_classify_on_import,
                classify::memory_defaults(&groups, &settings),
            )
        };
        if !ask && memories.iter().all(Option::is_some) {
            // R8：全部组有记忆 → 不弹窗直接套用。
            let decisions: Vec<(GroupMode, Option<String>)> = memories
                .iter()
                .map(|m| m.clone().expect("all Some 已判"))
                .collect();
            self.do_import(&groups, &decisions);
            return;
        }

        let rows: Vec<ClassifyRowData> = groups
            .iter()
            .zip(&memories)
            .map(|(group, memory)| {
                let (summary, kind_code) = match group.kind {
                    GroupKind::Package => (
                        format!("素材包（.emo / 千牛目录）· {} 个", group.paths.len()),
                        0,
                    ),
                    GroupKind::Folder => (format!("文件夹 · {} 个", group.paths.len()), 1),
                    GroupKind::Loose => (format!("散文件 · {} 个", group.paths.len()), 2),
                };
                let (chosen, unified) = memory
                    .clone()
                    .map(|(mode, category)| (mode_code(mode), category.unwrap_or_default()))
                    .unwrap_or((mode_code(group.default_mode), String::new()));
                ClassifyRowData {
                    kind: kind_code,
                    summary: summary.into(),
                    category_count: group.category_count.map_or(-1, |n| n as i32),
                    default_mode: mode_code(group.default_mode),
                    chosen_mode: chosen,
                    unified_name: unified.into(),
                }
            })
            .collect();
        *self.groups.borrow_mut() = groups;
        self.rows.set_vec(rows);

        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let candidates: Vec<slint::SharedString> = self
            .categories
            .borrow()
            .iter()
            .skip(1) // 下标 0 = 「全部」，不是分类
            .cloned()
            .map(slint::SharedString::from)
            .collect();
        ui.set_classify_categories(slint::ModelRc::from(Rc::new(VecModel::from(candidates))));
        ui.set_classify_open(true);
        // 入场动效下一帧翻转（D53 结论：init 技巧无效，首帧前置位不触发过渡）。
        let weak = self.ui.clone();
        CLASSIFY_ANIM_TIMER.with(|slot| {
            slot.borrow().start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(16),
                move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_classify_shown(true);
                    }
                },
            );
        });
    }

    /// 出场两段式：先收 shown 播反向过渡，播完再收 open 卸载。
    fn close(&self) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_classify_shown(false);
        let animated = self.settings.borrow().ui_animations;
        let weak = self.ui.clone();
        CLASSIFY_ANIM_TIMER.with(|slot| {
            slot.borrow().start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(if animated { 170 } else { 0 }),
                move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_classify_open(false);
                    }
                },
            );
        });
    }

    fn set_row_mode(&self, index: usize, code: i32) {
        if let Some(mut row) = self.rows.row_data(index) {
            row.chosen_mode = code;
            self.rows.set_row_data(index, row);
        }
    }

    fn set_row_name(&self, index: usize, name: slint::SharedString) {
        if let Some(mut row) = self.rows.row_data(index) {
            row.unified_name = name;
            self.rows.set_row_data(index, row);
        }
    }

    /// 「导入」确认：勾了记住就先落设置，再拼清单文件起子进程；「取消」路径
    /// 只 close，不产生任何库副作用。
    fn confirm(self: &Rc<Self>, remember: bool) {
        let groups = self.groups.borrow().clone();
        let decisions: Vec<(GroupMode, Option<String>)> = (0..self.rows.row_count())
            .filter_map(|i| self.rows.row_data(i))
            .map(|row| {
                let mode = mode_of(row.chosen_mode);
                let category = if mode == GroupMode::Unified {
                    let name = row.unified_name.trim().to_string();
                    if name.is_empty() {
                        None
                    } else {
                        Some(name)
                    }
                } else {
                    None
                };
                (mode, category)
            })
            .collect();
        if remember {
            self.remember_choices(&groups, &decisions);
        }
        self.do_import(&groups, &decisions);
        self.close();
    }

    /// R8：把每行决策写进设置（方式 + 统一归入的分类名）并关掉询问。
    fn remember_choices(&self, groups: &[SourceGroup], decisions: &[(GroupMode, Option<String>)]) {
        {
            let mut settings = self.settings.borrow_mut();
            settings.ask_classify_on_import = false;
            for (group, (mode, category)) in groups.iter().zip(decisions) {
                let mode_str = match mode {
                    GroupMode::PerSource => "per_source",
                    GroupMode::Unified => "unified",
                    GroupMode::Inbox => "inbox",
                };
                let category_value = category.clone().unwrap_or_default();
                match group.kind {
                    GroupKind::Package => {
                        settings.remember_package_mode = mode_str.to_string();
                        settings.remember_package_category = category_value;
                    }
                    GroupKind::Folder => {
                        settings.remember_folder_mode = mode_str.to_string();
                        settings.remember_folder_category = category_value;
                    }
                    GroupKind::Loose => {
                        settings.remember_loose_mode = mode_str.to_string();
                        settings.remember_loose_category = category_value;
                    }
                }
            }
        }
        if let Err(error) = self.settings.borrow().save(&self.settings_path) {
            logging::warn!("记忆归类选择写入设置失败: {error}");
        }
    }

    /// 决策 → 清单文件 → `--import-paths` 子进程管线（进度条/失败提示/缩略图
    /// 派生与旧两入口全同路）。
    fn do_import(&self, groups: &[SourceGroup], decisions: &[(GroupMode, Option<String>)]) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let mut lines = String::new();
        let mut total = 0usize;
        for (group, (mode, category)) in groups.iter().zip(decisions) {
            let mode_field = classify::decision_to_mode_field(*mode, category.as_deref());
            for path in &group.paths {
                let kind_char = if path.is_dir() {
                    "d"
                } else if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("emo"))
                {
                    "p"
                } else {
                    "f"
                };
                lines.push_str(&format!(
                    "{kind_char}	{mode_field}	{}
",
                    path.display()
                ));
                total += 1;
            }
        }
        if total == 0 {
            return;
        }
        let list = std::env::temp_dir().join(format!(
            "assetdeck_import_paths_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        if let Err(error) = std::fs::write(&list, lines) {
            show_notice(
                &ui,
                TargetNoticeTone::Error,
                format!("无法写入导入清单 {}: {error}", list.display()),
            );
            return;
        }
        let root = self
            .library_root
            .clone()
            .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
        let mode_arg = import_mode_arg(&self.settings.borrow());
        let args = vec![
            "--import-paths".to_string(),
            list.to_string_lossy().into_owned(),
            "--library".to_string(),
            root.clone(),
            "--mode".to_string(),
            mode_arg.to_string(),
        ];
        spawn_import_pipeline(
            self.ui.clone(),
            args,
            root,
            self.importing.clone(),
            format!("{total} 项素材"),
            self.settings.borrow().fast_import_mode,
        );
    }
}

/// 属性弹窗的字段组装（R13：尺寸/大小/导入时间/绝对路径）。
fn fill_properties(ui: &AppWindow, resolver: &RealAssetResolver, id: AssetId) -> bool {
    let Ok(Some(meta)) = resolver.meta_of(id) else {
        return false;
    };
    ui.set_props_name(meta.file_name.clone().into());
    ui.set_props_size(
        match (meta.width, meta.height) {
            (Some(w), Some(h)) => format!("{w} × {h} px"),
            _ => "—（尚未探测）".to_string(),
        }
        .into(),
    );
    ui.set_props_bytes(human_size(meta.size_bytes).into());
    ui.set_props_created(format_timestamp(meta.imported_at).into());
    ui.set_props_path(
        resolver
            .absolute_path(id)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| meta.rel_path.clone())
            .into(),
    );
    true
}

/// 字节数 → 人话（属性面板用；不进热路径）。
fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// unix 秒 → `2026-08-28 17:03` （本地时区近似：epoch + 8h 东八区约定，
/// 与素材库写入侧 created_at 的产出方式一致，见 store 导入路径）。
fn format_timestamp(secs: i64) -> String {
    if secs <= 0 {
        return "—".to_string();
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // 民用历法换算（Howard Hinnant days_from_civil 逆运算）。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    // 东八区：库写入用本地秒（见 tools 导入侧），展示不再换算。
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// D49 拖入接收端：OS Drop 回调（UI 线程消息泵派发）只把路径转交事件循环，
/// 不做任何导入决策——决策全在 classify VM 与 ImportFlow。
struct SlintFileDropSink {
    ui: slint::Weak<AppWindow>,
}

impl platform::FileDropSink for SlintFileDropSink {
    fn files_dropped(&self, paths: Vec<std::path::PathBuf>) {
        let weak = self.ui.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                let model = slint::VecModel::from(
                    paths
                        .iter()
                        .map(|p| slint::SharedString::from(p.to_string_lossy().into_owned()))
                        .collect::<Vec<_>>(),
                );
                ui.invoke_files_dropped(slint::ModelRc::new(model));
            }
        });
    }
}

/// HWND 就绪后注册拖入目标（winit 窗口在事件循环首轮才创建；未就绪则退避
/// 重试，上限 5 秒后放弃——拖拽导入是增强项，失败不阻断主流程）。
fn register_file_drop_when_ready(
    app: slint::Weak<AppWindow>,
    sink: std::sync::Arc<SlintFileDropSink>,
    attempt: u32,
) {
    use raw_window_handle::HasWindowHandle;
    const MAX_ATTEMPTS: u32 = 50;
    let Some(ui) = app.upgrade() else {
        return;
    };
    let hwnd = match ui.window().window_handle().window_handle() {
        Ok(handle) => match handle.as_raw() {
            raw_window_handle::RawWindowHandle::Win32(win32) => win32.hwnd.get(),
            _ => 0,
        },
        Err(_) => 0,
    };
    if hwnd == 0 && attempt < MAX_ATTEMPTS {
        // Timer 回调是 FnMut（不可移动捕获）：闭包只跑一次，用 Option.take 取出。
        let mut next = Some((app, sink));
        slint::Timer::default().start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(100),
            move || {
                if let Some((app, sink)) = next.take() {
                    register_file_drop_when_ready(app, sink, attempt + 1);
                }
            },
        );
        return;
    }
    match platform::win32::dragdrop::register_file_drop(hwnd, sink) {
        Ok(()) => logging::info!("拖拽导入已注册（hwnd={hwnd:#x}）"),
        Err(error) => logging::warn!("拖拽导入注册失败：{error}（可继续用导入入口）"),
    }
}

/// 总数文案 + 空态文案的唯一出口。
fn sync_counts(ui: &AppWindow, total: usize, has_library: bool) {
    ui.set_total_text(format!("共 {total} 项").into());
    if total > 0 {
        ui.set_empty_title("".into());
        ui.set_empty_hint("".into());
        return;
    }
    let (title, hint) = if !ui.get_search_text().trim().is_empty() {
        ("没有匹配的素材", "换个关键词，或清空搜索框回到全部")
    } else if !has_library {
        ("素材库还是空的", "点左下角「导入素材…」把图片/视频加进来")
    } else {
        ("这个分类下还没有素材", "换个分类，或继续导入素材")
    };
    ui.set_empty_title(title.into());
    ui.set_empty_hint(hint.into());
}

fn default_categories() -> Vec<String> {
    vec![
        "全部".to_string(),
        "分类0".to_string(),
        "分类1".to_string(),
        "分类2".to_string(),
        "分类3".to_string(),
        "分类4".to_string(),
    ]
}

fn apply_categories(
    ui: &AppWindow,
    filter_categories: &Rc<RefCell<Vec<String>>>,
    category_names: &[String],
    category_counts: &[i32],
) {
    let mut all = vec!["全部".to_string()];
    all.extend(category_names.iter().cloned());
    *filter_categories.borrow_mut() = all.clone();
    let model = Rc::new(VecModel::from(
        all.iter()
            .map(|name| slint::SharedString::from(name.as_str()))
            .collect::<Vec<_>>(),
    ));
    ui.set_categories(ModelRc::from(model));

    // 计数模型：下标 0 = 「全部」= 各分类计数之和；下标 i 对应 category_names[i-1]。
    let total: i32 = category_counts.iter().sum();
    let mut counts = vec![total];
    counts.extend_from_slice(category_counts);
    ui.set_category_counts(ModelRc::from(Rc::new(VecModel::from(counts))));
    // 重载分类后回落到「全部」高亮，顶栏后缀同步回落，避免残留旧分类名。
    ui.set_selected_category(-1);
    ui.set_filter_label("全部".into());
}

/// 按分类名对齐取计数：`category_names()` 按 CategoryId 下标排序，而
/// `facets().categories()` 按名升序，两者顺序未必一致，故按名查找。
fn category_counts_for(resolver: &RealAssetResolver, names: &[String]) -> Vec<i32> {
    names
        .iter()
        .map(|name| {
            resolver
                .facets()
                .categories()
                .iter()
                .find(|entry| &entry.name == name)
                .map(|entry| entry.count as i32)
                .unwrap_or(0)
        })
        .collect()
}

/// 运行时主题注入：把 [`ThemeTokens`] 铺到 slint 的 Theme 全局（暗/浅两套）。
fn apply_theme(ui: &AppWindow, tokens: &ThemeTokens) {
    let theme = ui.global::<Theme>();
    let c = |v: u32| slint::Color::from_argb_encoded(v);
    theme.set_bg_app(c(tokens.bg_app));
    theme.set_bg_panel(c(tokens.bg_panel));
    theme.set_bg_bar(c(tokens.bg_bar));
    theme.set_bg_raised(c(tokens.bg_raised));
    theme.set_bg_raised_hover(c(tokens.bg_raised_hover));
    theme.set_bg_raised_press(c(tokens.bg_raised_press));
    theme.set_bg_input(c(tokens.bg_input));
    theme.set_line(c(tokens.line));
    theme.set_line_strong(c(tokens.line_strong));
    theme.set_text(c(tokens.text));
    theme.set_text_2(c(tokens.text_2));
    theme.set_text_3(c(tokens.text_3));
    theme.set_accent(c(tokens.accent));
    theme.set_accent_hover(c(tokens.accent_hover));
    theme.set_accent_press(c(tokens.accent_press));
    theme.set_accent_ink(c(tokens.accent_ink));
    theme.set_accent_soft(c(tokens.accent_soft));
    theme.set_danger(c(tokens.danger));
    theme.set_danger_soft(c(tokens.danger_soft));
    theme.set_danger_press(c(tokens.danger_press));
    theme.set_warn(c(tokens.warn));
    theme.set_warn_soft(c(tokens.warn_soft));
    theme.set_ok_soft(c(tokens.ok_soft));
    theme.set_bg_overlay(c(tokens.bg_overlay));
    theme.set_scrim(c(tokens.scrim));
    theme.set_label_bar_bg(c(tokens.label_bar_bg));
    theme.set_label_bar_bg_hover(c(tokens.label_bar_bg_hover));
    theme.set_label_bar_text(c(tokens.label_bar_text));
}

/// std-widgets 明暗翻转：写内置 Palette 全局的 color-scheme 属性。
///
/// 为什么需要独立于 apply_theme：build.rs 把样式钉在 fluent-dark，Button/
/// LineEdit/ProgressIndicator/ScrollView 的内部颜色是编译期烘焙的活绑定
/// （每色都是「scheme==Dark ? 暗常量 : 亮常量」），不读 Theme 令牌。运行时把
/// color-scheme 翻到 Light 即可整体切换它们的配色（D37）。必须在 AppWindow
/// 创建后、进入事件循环前调用一次；设置实时切换时随 light_theme 再调。
fn apply_color_scheme(ui: &AppWindow, light_theme: bool) {
    // ColorScheme 枚举定义在 i-slint-core::items（宏展开），slint 面上只在
    // private_unstable_api::re_exports 可达——类型仅为传值使用，无不稳定 API 面。
    use slint::private_unstable_api::re_exports::ColorScheme;
    ui.global::<Palette>().set_color_scheme(if light_theme {
        ColorScheme::Light
    } else {
        ColorScheme::Dark
    });
}

/// 设置面板模型：从 [`AppSettings::describe`] 铺成 slint 的 SettingRowData 列表。
fn sync_settings(ui: &AppWindow, settings: &AppSettings) {
    let rows: Vec<SettingRowData> = settings
        .describe()
        .into_iter()
        .map(|view| SettingRowData {
            key: view.key.into(),
            title: view.title.into(),
            detail: view.detail.into(),
            checked: view.checked,
            enabled: view.enabled,
        })
        .collect();
    ui.set_settings(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn main() {
    // D38：日志初始化。目录 = exe 同目录 logs/（便携约定：跟 settings.toml 同一
    // 推理——无库根时贴 exe 走）。初始化失败只降级不阻断启动。
    let logs_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("logs")))
        .unwrap_or_else(|| std::path::PathBuf::from("logs"));
    logging::init(logging::InitOptions {
        dir: logs_dir,
        name: "app".to_string(),
        level: logging::Level::Info,
        mirror_stderr: false,
    });
    logging::info!("app 启动 args={:?}", std::env::args().collect::<Vec<_>>());

    // panic 进日志（D39 精神）：默认 panic 只打 stderr，真机 GUI 无控制台时
    // 就是黑洞——挂钩后 panic 现场连同 backtrace 进文件日志，低配机/CI 崩溃
    // 才可回溯。同时保留 stderr 输出，测试与控制台场景仍直接可见。
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        logging::error!("panic: {info}\n{backtrace}");
        eprintln!("panic: {info}");
    }));

    let mut library_root = parse_library_root(&std::env::args().skip(1).collect::<Vec<_>>());
    // 便携约定：未显式指定 --library-root 时，用 exe 同目录的 library/meta.db 则自动打开。
    if library_root.is_none() {
        let default = default_library_root();
        if default.join("meta.db").is_file() {
            library_root = Some(default.to_string_lossy().into_owned());
        }
    }

    // --bench 内存守卫模式：不开窗、不跑 Slint 事件循环（design 契约）。
    if let Some(bench) = parse_bench_args(&std::env::args().skip(1).collect::<Vec<_>>()) {
        std::process::exit(run_bench(&bench));
    }

    // 设置：优先随库目录，否则 exe 旁 settings.toml。必须在建窗前载入，因为渲染
    // 后端只能在创建 AppWindow 之前选定。
    let settings_path =
        ui_viewmodels::settings_path(library_root.as_deref().map(std::path::Path::new));
    let settings = Rc::new(RefCell::new(AppSettings::load(&settings_path)));
    let settings_path = Rc::new(settings_path);

    // 渲染后端选择：ASSETDECK_FORCE_SOFTWARE > SLINT_BACKEND > gpu_rendering > 软件渲染。
    // 关键：BackendSelector 的 backend 与 renderer 必须分开传——"winit-femtovg" 这种
    // 连写名字不会按连字符拆分（select() 只做整名匹配），实测会静默回落默认渲染器，
    // 导致「GPU 关闭档」从来就没真正切到软件渲染。必须 backend_name("winit") +
    // renderer_name("software"/"femtovg") 才真正生效。
    let force_software = std::env::var_os("ASSETDECK_FORCE_SOFTWARE").is_some();
    let gpu = settings.borrow().gpu_rendering;
    if force_software || std::env::var_os("SLINT_BACKEND").is_none() {
        let renderer = if force_software || !gpu {
            "software"
        } else {
            "femtovg"
        };
        logging::info!(
            "渲染档: winit/{renderer}（SLINT_BACKEND={:?} gpu_rendering={gpu} 强制软件={force_software}）",
            std::env::var_os("SLINT_BACKEND").map(|v| v.to_string_lossy().into_owned()),
        );
        match slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name(renderer.into())
            .with_winit_window_attributes_hook(|attrs| attrs.with_transparent(false))
            .select()
        {
            Ok(()) => logging::info!("渲染档选定: winit/{renderer}"),
            Err(e) => logging::warn!("渲染档选择失败({e})，回落 Slint 默认渲染器"),
        }
    }

    let app = AppWindow::new().expect("AppWindow 创建失败");
    let filter_categories = Rc::new(RefCell::new(default_categories()));

    // 主题：启动时按设置注入（浅色主题设置切换时也会实时重铺）。
    {
        let tokens = if settings.borrow().light_theme {
            LightThemeProvider.theme()
        } else {
            DarkThemeProvider.theme()
        };
        apply_theme(&app, &tokens);
        // std-widgets 同步走 Palette 通道：首帧前生效（D37）。
        apply_color_scheme(&app, settings.borrow().light_theme);
    }

    // 载入后回填 UI 属性。
    app.set_single_click_activate(settings.borrow().activate_on_single_click);
    app.set_send_after_paste(settings.borrow().send_after_paste);
    app.set_gpu_rendering(settings.borrow().gpu_rendering);
    app.set_animations_enabled(settings.borrow().ui_animations);
    app.set_sidebar_width(settings.borrow().sidebar_width);
    sync_settings(&app, &settings.borrow());
    // 单击触发模式的热读取，供瓦片单击回调判断是否上框。
    let single_click = Rc::new(Cell::new(settings.borrow().activate_on_single_click));
    // 当前分类过滤，清空检索时回落到它。
    let current_filter = Rc::new(RefCell::new(Filter::All));
    // 与 current_filter 配套的顶栏后缀文案，清空检索时一并回落。
    let filter_label = Rc::new(RefCell::new(slint::SharedString::from("全部")));

    // VM 装配：优先加载真实库；未指定 --library-root 时保留演示数据。
    let mut real_vm: Option<Rc<RefCell<LibraryGridVm>>> = None;
    let real_resolver: ThumbSource = match library_root.as_deref() {
        Some(root) => match ui_viewmodels::load_real_library(std::path::Path::new(root)) {
            Ok((index, resolver)) => {
                let names = resolver.category_names();
                let counts = category_counts_for(&resolver, &names);
                apply_categories(&app, &filter_categories, &names, &counts);

                let mut vm = LibraryGridVm::new(index, recent_first_sorter(), 256);
                vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
                match resolver.aspects() {
                    Ok(aspects) => vm.set_aspects(aspects),
                    Err(error) => eprintln!("读取素材宽高比失败，版式回落占位比例: {error}"),
                }
                real_vm = Some(Rc::new(RefCell::new(vm)));
                Rc::new(RefCell::new(Some(resolver)))
            }
            Err(error) => {
                eprintln!("真实库装载失败，回退演示数据: {error}");
                Rc::new(RefCell::new(None))
            }
        },
        None => Rc::new(RefCell::new(None)),
    };
    let vm = match real_vm.take() {
        Some(vm) => vm,
        None => {
            let mut vm = LibraryGridVm::new(demo_index(), recent_first_sorter(), 256);
            vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
            Rc::new(RefCell::new(vm))
        }
    };

    let tiles_model: Rc<VecModel<TileData>> = Rc::new(VecModel::default());
    app.set_tiles(ModelRc::from(tiles_model.clone()));
    sync_counts(&app, vm.borrow().total(), real_resolver.borrow().is_some());
    // 缩略图缓存（D43）：窗口显式驱逐 + LRU 容量兜底，替代旧无界 HashMap。
    let thumb_cache = Rc::new(RefCell::new(ThumbCache::new(THUMB_CACHE_CAPACITY)));

    // 导入/派生/库写子命令在途标记：清空库与 CRUD 动作在有任务时拒绝执行，
    // 避免删到正在写入的文件。提前到这里声明是因为 CrudCtx 要捕获它。
    let importing = Arc::new(AtomicBool::new(false));

    // 网格同步上下文：滚动/过滤/库重载共用的唯一刷新入口。
    let grid = Rc::new(GridCtx::new(
        app.as_weak(),
        vm.clone(),
        tiles_model.clone(),
        real_resolver.clone(),
        thumb_cache.clone(),
    ));

    let routing = Rc::new(RefCell::new(
        TargetRoutingRuntime::new(BUILTIN_PROFILES, None, win32_runtime_deps())
            .expect("目标画像加载失败"),
    ));
    let target_choices: Rc<VecModel<TargetChoiceData>> = Rc::new(VecModel::default());
    app.set_target_choices(ModelRc::from(target_choices.clone()));
    sync_target_bar(&app, &target_choices, routing.borrow().snapshot());

    // CRUD 协作上下文（D46–D48）：选区同步/过滤切换/库写子命令的统一出口。
    let crud = CrudCtx {
        ui: app.as_weak(),
        vm: vm.clone(),
        resolver: real_resolver.clone(),
        grid: grid.clone(),
        filter_categories: filter_categories.clone(),
        current_filter: current_filter.clone(),
        filter_label: filter_label.clone(),
        library_root: library_root.clone(),
        importing: importing.clone(),
        thumb_cache: thumb_cache.clone(),
    };
    // 启动即摆正回收站角标（库里可能已有墓碑行）。
    app.set_trash_count(crud.vm.borrow().trash_count() as i32);

    // D49/D50 通用导入流（三入口汇流 → 预扫描 → 归类弹窗 → 清单子进程）。
    let import_flow = Rc::new(ImportFlow {
        ui: app.as_weak(),
        rows: Rc::new(VecModel::default()),
        groups: Rc::new(RefCell::new(Vec::new())),
        entries: Rc::new(RefCell::new(Vec::new())),
        pending: Rc::new(Cell::new(0)),
        settings: settings.clone(),
        settings_path: settings_path.clone(),
        categories: filter_categories.clone(),
        importing: importing.clone(),
        library_root: library_root.clone(),
    });
    app.set_classify_rows(ModelRc::from(import_flow.rows.clone()));
    // 最近一次动作目标（右键命中的那张 / 重命名与属性打开的那张）。菜单收起
    // 不清它——下一次菜单或操作条动作会重新设定，读侧只在弹窗里回显。
    let menu_target: Rc<RefCell<Option<AssetId>>> = Rc::new(RefCell::new(None));
    // 「清空回收站」两步确认的武装态（第一次点=确认文案，再点才执行）。
    let empty_armed = Rc::new(Cell::new(false));

    // 库写子命令收尾：经 libcmd-finished 弹回 UI 线程（子进程线程零捕获 Rc）。
    {
        let crud = crud.clone();
        app.on_libcmd_finished(move |success, message, label| {
            crud.reload_after_cmd(success, message.as_str(), label.as_str());
        });
    }

    // D47 修饰键点击：Ctrl 增删、Shift 范围（替换/并集随 Ctrl 与否）。
    {
        let crud = crud.clone();
        app.on_tile_modified_click(move |id, ctrl, shift| {
            crud.vm.borrow_mut().single_click(
                AssetId(id.max(0) as u32),
                selection::Modifiers { ctrl, shift },
            );
            crud.sync_selection();
        });
    }

    // D48 右键：命中后置菜单目标（选区∪命中）与菜单内容/位置。
    {
        let crud = crud.clone();
        let menu_target = menu_target.clone();
        app.on_tile_right_click(move |id, x, y| {
            let id = AssetId(id.max(0) as u32);
            let hit = {
                let vm = crud.vm.borrow();
                vm.context_menu(id)
            };
            if hit.targets.is_empty() {
                return;
            }
            *menu_target.borrow_mut() = Some(id);
            let Some(ui) = crud.ui.upgrade() else { return };
            let title = match hit.targets.len() {
                1 => {
                    let binding = crud.resolver.borrow();
                    match binding
                        .as_ref()
                        .and_then(|r| r.meta_of(hit.targets[0]).ok().flatten())
                    {
                        Some(meta) => meta.file_name,
                        None => "1 项".to_string(),
                    }
                }
                n => format!("{n} 项"),
            };
            ui.set_context_menu_title(title.into());
            ui.set_context_menu_x(x);
            ui.set_context_menu_y(y);
            ui.set_context_menu_open(true);
        });
    }

    {
        let crud = crud.clone();
        app.on_context_menu_dismissed(move || {
            if let Some(ui) = crud.ui.upgrade() {
                ui.set_context_menu_open(false);
            }
        });
    }

    // D53 旧三弹层入场：init 在首帧前跑（这正是旧「init 置 shown」无效的
    // 根因），只让它报数，翻转交给 16ms 单发 Timer（必然落在首帧后）；
    // 关动画时直接置 true（时长本就钳 0ms，无需 Timer）。
    {
        let ui = app.as_weak();
        let settings = settings.clone();
        app.on_overlay_mounted(move |which| {
            let Some(ui) = ui.upgrade() else { return };
            let animated = settings.borrow().ui_animations;
            let weak = ui.as_weak();
            let flip = move || {
                if let Some(ui) = weak.upgrade() {
                    match which {
                        0 => ui.set_target_picker_shown(true),
                        1 => ui.set_import_menu_shown(true),
                        _ => ui.set_settings_shown(true),
                    }
                }
            };
            if animated {
                CLASSIFY_ANIM_TIMER.with(|slot| {
                    slot.borrow().start(
                        slint::TimerMode::SingleShot,
                        std::time::Duration::from_millis(16),
                        flip,
                    );
                });
            } else {
                flip();
            }
        });
    }

    // D50 归类弹窗回调：行内决策 / 取消 / 确认导入。
    {
        let flow = import_flow.clone();
        app.on_classify_row_mode_changed(move |index, code| {
            flow.set_row_mode(index.max(0) as usize, code);
        });
    }
    {
        let flow = import_flow.clone();
        app.on_classify_row_name_changed(move |index, name| {
            flow.set_row_name(index.max(0) as usize, name);
        });
    }
    {
        let flow = import_flow.clone();
        app.on_classify_canceled(move || {
            flow.close();
        });
    }
    {
        let flow = import_flow.clone();
        app.on_classify_confirmed(move |remember| {
            flow.confirm(remember);
        });
    }

    // probe 结果回调（子进程线程经 Weak<AppWindow> 转接的落点）。
    {
        let flow = import_flow.clone();
        app.on_classify_probe_result(move |path, count| {
            let path = std::path::PathBuf::from(path.as_str());
            flow.apply_probe(
                &path,
                if count < 0 {
                    None
                } else {
                    Some(count as usize)
                },
            );
        });
    }
    {
        let flow = import_flow.clone();
        app.on_classify_probe_finished(move || {
            flow.probe_done();
        });
    }

    // D49 拖入：路径进同一归类弹窗（importing 守卫在 open 内）。
    {
        let flow = import_flow.clone();
        app.on_files_dropped(move |paths| {
            let paths: Vec<std::path::PathBuf> = paths
                .iter()
                .map(|s| std::path::PathBuf::from(s.as_str()))
                .collect();
            if paths.is_empty() {
                return;
            }
            flow.open(paths);
        });
    }

    // 顶栏「选择」按钮：切换多选模式；退出清选区（R8/R9）。
    {
        let crud = crud.clone();
        app.on_select_mode_toggled(move || {
            {
                let mut vm = crud.vm.borrow_mut();
                if vm.multi_mode() {
                    vm.exit_multi();
                } else {
                    vm.enter_multi();
                }
            }
            crud.sync_selection();
        });
    }

    {
        let crud = crud.clone();
        app.on_select_all_requested(move || {
            crud.vm.borrow_mut().select_all_visible();
            crud.sync_selection();
        });
    }

    // Ctrl+A（key-root 捕获，检索框聚焦时已让位）：等价「全选」。
    {
        let crud = crud.clone();
        app.on_key_a_pressed(move || {
            crud.vm.borrow_mut().select_all_visible();
            crud.sync_selection();
        });
    }

    // Esc：关闭链（归类弹窗→菜单→移动→重命名→属性→清空选区退多选）。
    {
        let crud = crud.clone();
        let import_flow = import_flow.clone();
        app.on_escape_pressed(move || {
            let Some(ui) = crud.ui.upgrade() else { return };
            // 归类弹窗是模态最上层，Esc = 取消本次导入。
            if ui.get_classify_open() {
                import_flow.close();
                return;
            }
            if ui.get_context_menu_open() {
                ui.set_context_menu_open(false);
            } else if ui.get_move_menu_open() {
                ui.set_move_menu_open(false);
            } else if ui.get_rename_open() {
                ui.set_rename_open(false);
                ui.set_rename_error("".into());
            } else if ui.get_properties_open() {
                ui.set_properties_open(false);
            } else if crud.vm.borrow().multi_mode() {
                crud.vm.borrow_mut().exit_multi();
                crud.sync_selection();
            }
        });
    }

    // D48 菜单五项 → 动作派发（R10/R11：目标=选区∪命中，右键时已记
    // menu_target）。Context：手搓浮层（ContextMenuArea 在 1.17.1 Windows
    // 上被瓦片 TouchArea 的 GrabMouse 阻断，回退记 design/archives）。
    {
        let crud = crud.clone();
        let menu_target = menu_target.clone();
        let routing = routing.clone();
        app.on_menu_action(move |id| {
            let Some(action) = ui_enums::menu_action(id) else {
                return;
            };
            if let Some(ui) = crud.ui.upgrade() {
                ui.set_context_menu_open(false);
            }
            let targets = crud.action_targets(*menu_target.borrow());
            if targets.is_empty() {
                return;
            }
            match action {
                MenuAction::Copy => {
                    let Some(ui) = crud.ui.upgrade() else { return };
                    let binding = crud.resolver.borrow();
                    let Some(resolver) = binding.as_ref() else {
                        show_notice(
                            &ui,
                            TargetNoticeTone::Warning,
                            "演示库不支持复制".to_string(),
                        );
                        return;
                    };
                    // 多选时复制第一张（文件级复制=进剪贴板，与上框共用 negotiate
                    // 降级链，但绝不注入：copy_to_clipboard 只写剪贴板）。
                    match resolver.materialize(targets[0]) {
                        Ok(Some(materialized)) => {
                            let payload = materialized.as_payload();
                            match routing.borrow_mut().copy_to_clipboard(&payload) {
                                Ok(()) => show_notice(
                                    &ui,
                                    TargetNoticeTone::Success,
                                    "已复制到剪贴板".to_string(),
                                ),
                                Err(error) => show_notice(&ui, TargetNoticeTone::Warning, error),
                            }
                        }
                        _ => {
                            show_notice(&ui, TargetNoticeTone::Warning, "素材读取失败".to_string())
                        }
                    }
                }
                MenuAction::MoveToCategory => {
                    if let Some(ui) = crud.ui.upgrade() {
                        ui.set_move_menu_open(true);
                        ui.set_move_error("".into());
                    }
                }
                MenuAction::Rename => {
                    *menu_target.borrow_mut() = Some(targets[0]);
                    let Some(ui) = crud.ui.upgrade() else { return };
                    let current = {
                        let binding = crud.resolver.borrow();
                        binding
                            .as_ref()
                            .and_then(|r| r.meta_of(targets[0]).ok().flatten())
                            .map(|m| m.file_name)
                            .unwrap_or_default()
                    };
                    ui.set_rename_current(current.into());
                    ui.set_rename_error("".into());
                    ui.set_rename_open(true);
                }
                MenuAction::Properties => {
                    *menu_target.borrow_mut() = Some(targets[0]);
                    let Some(ui) = crud.ui.upgrade() else { return };
                    let filled = crud
                        .resolver
                        .borrow()
                        .as_ref()
                        .is_some_and(|r| fill_properties(&ui, r, targets[0]));
                    if filled {
                        ui.set_properties_open(true);
                    } else {
                        show_notice(
                            &ui,
                            TargetNoticeTone::Warning,
                            "该素材尚未入库完成，暂无属性可读".to_string(),
                        );
                    }
                }
                MenuAction::Delete => {
                    // UI 先行：本地隐藏即时见效，子命令落库后整库重载对齐（失败会显形回来）。
                    crud.vm.borrow_mut().hide_locally(&targets);
                    {
                        // label 先落本地再传（RefCell 卫队禁内联进实参，见守卫测试）。
                        let label = crud.filter_label.borrow().clone();
                        crud.apply_filter(crud.filter(), label);
                    }
                    let uuids = crud.uuids_of(&targets);
                    crud.spawn_lib_cmd("trash", &uuids, None, "删除");
                }
            }
        });
    }

    // 移动到分类：既服务多选操作条，也服务右键菜单（目标集取法一致）。
    {
        let crud = crud.clone();
        let menu_target = menu_target.clone();
        app.on_move_selection_requested(move || {
            let targets = crud.action_targets(*menu_target.borrow());
            if targets.is_empty() {
                return;
            }
            let Some(ui) = crud.ui.upgrade() else { return };
            ui.set_move_menu_open(true);
            ui.set_move_error("".into());
        });
    }

    {
        let crud = crud.clone();
        let menu_target = menu_target.clone();
        app.on_move_to_category(move |category| {
            if let Some(ui) = crud.ui.upgrade() {
                ui.set_move_menu_open(false);
            }
            let targets = crud.action_targets(*menu_target.borrow());
            if targets.is_empty() {
                return;
            }
            let uuids = crud.uuids_of(&targets);
            let name = category.as_str().trim().to_string();
            crud.spawn_lib_cmd("move-category", &uuids, Some(&name), "移动到分类");
        });
    }

    // 删除（操作条「删除」按钮，目标=整份选区）。
    {
        let crud = crud.clone();
        app.on_delete_selection_requested(move || {
            let targets = crud.action_targets(None);
            if targets.is_empty() {
                return;
            }
            crud.vm.borrow_mut().hide_locally(&targets);
            {
                // label 先落本地再传（RefCell 卫队禁内联进实参，见守卫测试）。
                let label = crud.filter_label.borrow().clone();
                crud.apply_filter(crud.filter(), label);
            }
            let uuids = crud.uuids_of(&targets);
            crud.spawn_lib_cmd("trash", &uuids, None, "删除");
        });
    }

    // 回收站视图动作：恢复 / 彻底删除 / 清空（两步确认）。
    {
        let crud = crud.clone();
        app.on_restore_selection_requested(move || {
            let targets = crud.action_targets(None);
            if targets.is_empty() {
                return;
            }
            let uuids = crud.uuids_of(&targets);
            crud.spawn_lib_cmd("restore", &uuids, None, "恢复");
        });
    }

    {
        let crud = crud.clone();
        app.on_purge_selection_requested(move || {
            let targets = crud.action_targets(None);
            if targets.is_empty() {
                return;
            }
            let uuids = crud.uuids_of(&targets);
            crud.spawn_lib_cmd("purge", &uuids, None, "彻底删除");
        });
    }

    {
        let crud = crud.clone();
        let empty_armed = empty_armed.clone();
        app.on_empty_trash_requested(move || {
            let Some(ui) = crud.ui.upgrade() else { return };
            if empty_armed.get() {
                empty_armed.set(false);
                ui.set_empty_trash_armed(false);
                ui.set_empty_trash_text("清空回收站".into());
                crud.spawn_lib_cmd("empty-trash", &[], None, "清空回收站");
            } else {
                empty_armed.set(true);
                ui.set_empty_trash_armed(true);
                ui.set_empty_trash_text("再点一次确认清空".into());
            }
        });
    }

    // 重命名：校验在壳层（Slint 无 trim 内建）；合法才起子命令。
    {
        let crud = crud.clone();
        let rename_target = menu_target.clone();
        app.on_rename_confirmed(move |new_name| {
            let name = new_name.as_str().trim().to_string();
            let invalid = name.is_empty() || name.contains('/') || name.contains('\\');
            if invalid {
                if let Some(ui) = crud.ui.upgrade() {
                    ui.set_rename_error("名称不能为空或含路径分隔符".into());
                }
                return;
            }
            let Some(id) = *rename_target.borrow() else {
                if let Some(ui) = crud.ui.upgrade() {
                    ui.set_rename_open(false);
                }
                return;
            };
            let Some(ui) = crud.ui.upgrade() else { return };
            ui.set_rename_open(false);
            let uuids = crud.uuids_of(&[id]);
            crud.spawn_lib_cmd("rename", &uuids, Some(&name), "重命名");
        });
    }

    {
        let crud = crud.clone();
        app.on_rename_canceled(move || {
            if let Some(ui) = crud.ui.upgrade() {
                ui.set_rename_open(false);
                ui.set_rename_error("".into());
            }
        });
    }

    // 属性「打开所在文件夹」：explorer /select 定位（与日志目录入口同法）。
    {
        let crud = crud.clone();
        let props_target = menu_target.clone();
        app.on_properties_folder_requested(move || {
            let Some(ui) = crud.ui.upgrade() else { return };
            let Some(id) = *props_target.borrow() else {
                return;
            };
            let path = crud
                .resolver
                .borrow()
                .as_ref()
                .and_then(|r| r.absolute_path(id));
            match path {
                Some(p) => {
                    let _ = std::process::Command::new("explorer.exe")
                        .arg(format!("/select,{}", p.to_string_lossy()))
                        .spawn();
                }
                None => show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "素材文件路径解析失败".to_string(),
                ),
            }
        });
    }

    {
        let crud = crud.clone();
        app.on_properties_closed(move || {
            if let Some(ui) = crud.ui.upgrade() {
                ui.set_properties_open(false);
            }
        });
    }

    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let target_choices = target_choices.clone();
        app.on_target_chip_clicked(move || {
            let ui = ui.unwrap();
            let mut routing = routing.borrow_mut();
            if let Err(error) = routing.poll() {
                show_notice(&ui, TargetNoticeTone::Warning, error);
            }
            if !routing.toggle_picker() && routing.snapshot().choices.is_empty() {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "没有检测到运行中的 IM 窗口，请先打开微信/千牛等目标应用".to_string(),
                );
            }
            sync_target_bar(&ui, &target_choices, routing.snapshot());
        });
    }

    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let target_choices = target_choices.clone();
        app.on_target_choice_selected(move |selection_key| {
            let ui = ui.unwrap();
            let mut routing = routing.borrow_mut();
            routing.choose(selection_key.as_str());
            sync_target_bar(&ui, &target_choices, routing.snapshot());
        });
    }

    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let target_choices = target_choices.clone();
        app.on_target_pin_toggled(move || {
            let ui = ui.unwrap();
            let mut routing = routing.borrow_mut();
            routing.toggle_pin();
            sync_target_bar(&ui, &target_choices, routing.snapshot());
        });
    }

    // 前台跟随：优先事件驱动。观察器接管唤醒后，前台一变就立刻 poll+刷新目标条。
    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let target_choices = target_choices.clone();
        app.on_poll_targets(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let mut routing = routing.borrow_mut();
            let before = routing.snapshot().health;
            if let Err(error) = routing.poll() {
                logging::warn!("目标轮询失败: {error}");
                show_notice(&ui, TargetNoticeTone::Warning, error);
            }
            let after = routing.snapshot().health;
            if before != after {
                logging::info!("目标焦点状态变化: {before:?} -> {after:?}");
            }
            sync_target_bar(&ui, &target_choices, routing.snapshot());
        });
    }
    // 低配机冷启动时 WinEvent 泵可能尚未装好钩子（D40）——退路 Timer 的每一轮
    // 都先重试接管，接管成功即停表，随后事件驱动接管，形成自愈闭环。
    let routing_timer = Rc::new(Timer::default());
    {
        let handle = app.as_weak();
        if routing
            .borrow_mut()
            .install_wakeup(Box::new(poll_targets_wakeup(handle)))
        {
            // 事件驱动已接管，无需退路。
        } else {
            let routing = routing.clone();
            // 回调里要 stop 自己：与 `start` 的借用必须是两个独立的 Rc 克隆。
            let timer = routing_timer.clone();
            let timer_in_callback = routing_timer.clone();
            let handle = app.as_weak();
            timer.start(
                TimerMode::Repeated,
                std::time::Duration::from_millis(2000),
                move || {
                    if routing
                        .borrow_mut()
                        .install_wakeup(Box::new(poll_targets_wakeup(handle.clone())))
                    {
                        logging::info!("WinEvent 观察器已接管前台跟随，停用退路 Timer");
                        timer_in_callback.stop();
                        return;
                    }
                    if let Some(ui) = handle.upgrade() {
                        ui.invoke_poll_targets();
                    }
                },
            );
        }
    }

    // 滚动：content_y 变化 → 可见窗口物化 → 刷新瓦片。
    {
        let grid = grid.clone();
        app.on_scroll_changed(move |_content_y| {
            grid.sync();
        });
    }

    // 视口几何变化：重算列数并刷新可见区间（窗口拉伸后版式跟随）。
    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let grid = grid.clone();
        app.on_viewport_resized(move || {
            let ui = ui.unwrap();
            let width = ui.get_viewport_width();
            if width > 1.0 {
                vm.borrow_mut()
                    .set_layout_params(width, columns_for(width), GAP);
            }
            grid.sync();
        });
    }

    // 上框闭包：语义止步于 OpenAsset 事件 → 素材落地 → routing.paste（绝不合成回车）。
    let paste_asset = {
        let vm = vm.clone();
        let routing = routing.clone();
        let resolver = real_resolver.clone();
        Rc::new(move |ui: &AppWindow, id: i32| {
            let mut vm = vm.borrow_mut();
            // 红线 A（D47）：多选模式期间上框链路完全无操作——连 active 标记
            // 都不留，模式内点击只是改选区。
            if vm.multi_mode() {
                return;
            }
            vm.double_click(AssetId(id.max(0) as u32));
            // 标出最近上框的那张，用户连续操作时能看清刚才点的是哪个。
            ui.set_active_asset_id(id);
            for event in vm.take_events() {
                if let ui_viewmodels::VmEvent::OpenAsset(asset_id) = event {
                    logging::info!("上框请求 asset_id={}", asset_id.0);
                    if let Some(resolver) = resolver.borrow().as_ref() {
                        if let Ok(Some(materialized)) = resolver.materialize(asset_id) {
                            let payload = materialized.as_payload();
                            let notice = routing.borrow_mut().paste(&payload);
                            logging::info!(
                                "上框完成 asset_id={} tone={:?} text={}",
                                asset_id.0,
                                notice.tone,
                                notice.text
                            );
                            show_notice(ui, notice.tone, notice.text);
                        } else {
                            show_notice(
                                ui,
                                TargetNoticeTone::Warning,
                                "真实素材读取失败，请检查库文件".to_string(),
                            );
                        }
                    } else {
                        let payload = AssetPayload {
                            kind: AssetKind::Text,
                            png_bytes: &[],
                            source_path: std::path::PathBuf::new(),
                            text: format!("演示素材 #{}", asset_id.0),
                        };
                        let notice = routing.borrow_mut().paste(&payload);
                        show_notice(ui, notice.tone, notice.text);
                    }
                }
            }
        })
    };

    // 双击上框：双击模式生效；单击模式下双击不重复触发（避免二次粘贴）。
    {
        let ui = app.as_weak();
        let paste_asset = paste_asset.clone();
        let single_click = single_click.clone();
        app.on_double_clicked(move |id| {
            if single_click.get() {
                return;
            }
            let ui = ui.unwrap();
            paste_asset(&ui, id);
        });
    }

    // 单击上框：仅单击模式生效；双击模式下单击不触发。
    // D47：无修饰单击同样进选区状态机——常态只更新锚点（为 Shift 范围备料），
    // 多选模式内改选区；上框链路在多选期间被 paste_asset 的红线 A 挡住。
    {
        let ui = app.as_weak();
        let paste_asset = paste_asset.clone();
        let single_click = single_click.clone();
        let crud = crud.clone();
        app.on_tile_clicked(move |id| {
            let ui = ui.unwrap();
            crud.vm
                .borrow_mut()
                .single_click(AssetId(id.max(0) as u32), selection::Modifiers::default());
            crud.sync_selection();
            if !single_click.get() {
                return;
            }
            paste_asset(&ui, id);
        });
    }

    // 过滤面板 v1：全部(-1)/分类(0..) → set_filter 并回到顶。记住当前分类过滤，
    // 供清空检索时回落。选择分类会清掉检索框。
    {
        let ui = app.as_weak();
        let crud = crud.clone();
        app.on_filter_selected(move |cat| {
            let Some(ui) = ui.upgrade() else { return };
            // 侧栏条目：-1=全部，-3=回收站（D46），0..=分类；-2（检索态）不可点。
            let (filter, label) = if cat == TRASH_CATEGORY {
                (Filter::Trash, slint::SharedString::from("回收站"))
            } else if cat < 0 {
                (Filter::All, slint::SharedString::from("全部"))
            } else {
                let categories = crud.filter_categories.borrow();
                let name: slint::SharedString = categories
                    .get((cat as usize) + 1)
                    .cloned()
                    .unwrap_or_else(|| format!("分类{cat}"))
                    .into();
                (Filter::InCategory(CategoryId(cat as u32)), name)
            };
            ui.set_selected_category(cat);
            crud.apply_filter(filter, label);
        });
    }

    // 检索（D51/D52）：统一走 HybridSearchProvider；查询与范围档在壳层缓存，
    // 切档用缓存查询立即重跑（不要求重输）。
    let apply_search = {
        let ui = app.as_weak();
        let vm = vm.clone();
        let resolver = real_resolver.clone();
        let current_filter = current_filter.clone();
        let filter_label = filter_label.clone();
        let grid = grid.clone();
        Rc::new(move |query_text: &str, scope_code: i32| {
            let Some(ui) = ui.upgrade() else { return };
            let scope = ui_enums::search_scope(scope_code);
            let query = query_text.to_string();
            // 真实库走混合路由（≥3 字符 FTS→NameIn，短查询内存路）；
            // 演示库无 FTS 源 = 纯内存路。
            let filter = if query.trim().is_empty() {
                current_filter.borrow().clone()
            } else {
                let base = current_filter.borrow().clone();
                match resolver.borrow().as_ref() {
                    Some(r) => ui_viewmodels::HybridSearchProvider {
                        facets: r.facets(),
                        fts: Some(r),
                    }
                    .search(&query, scope, &base)
                    .unwrap_or(base),
                    None => base,
                }
            };
            // 检索态清掉侧栏分类高亮（-2），避免与「全部/某分类」的选中状态冲突。
            if query.trim().is_empty() {
                ui.set_selected_category(match &filter {
                    Filter::InCategory(id) => id.0 as i32,
                    Filter::Trash => TRASH_CATEGORY,
                    _ => -1,
                });
                ui.set_filter_label(filter_label.borrow().clone());
            } else {
                ui.set_selected_category(-2);
                let suffix = match scope {
                    ui_viewmodels::SearchScope::FileName => " · 仅文件名",
                    ui_viewmodels::SearchScope::Category => " · 仅分类",
                    ui_viewmodels::SearchScope::Tag => " · 仅标签",
                    _ => "",
                };
                ui.set_filter_label(format!("搜索「{}」{suffix}", query.trim()).into());
            }
            {
                let mut guard = vm.borrow_mut();
                guard.set_filter(&filter);
            }
            ui.set_content_y(0.0);
            sync_counts(&ui, vm.borrow().total(), resolver.borrow().is_some());
            grid.sync();
        })
    };
    let current_query = Rc::new(RefCell::new(String::new()));
    let current_scope = Rc::new(Cell::new(ui_enums::SCOPE_ALL));
    {
        let apply_search = apply_search.clone();
        let current_query = current_query.clone();
        let current_scope = current_scope.clone();
        app.on_search_changed(move |query| {
            *current_query.borrow_mut() = query.to_string();
            apply_search(query.as_str(), current_scope.get());
        });
    }
    {
        let apply_search = apply_search.clone();
        let current_query = current_query.clone();
        let current_scope = current_scope.clone();
        let ui = app.as_weak();
        app.on_scope_selected(move |scope| {
            current_scope.set(scope);
            if let Some(ui) = ui.upgrade() {
                ui.set_search_scope(scope);
                ui.set_scope_menu_open(false);
            }
            apply_search(current_query.borrow().as_str(), scope);
        });
    }
    {
        let ui = app.as_weak();
        let settings = settings.clone();
        app.on_scope_menu_toggled(move || {
            let ui = ui.unwrap();
            let opening = !ui.get_scope_menu_open();
            ui.set_scope_menu_open(opening);
            if opening {
                // 入场动效下一帧翻转（D53 结论；与归类弹窗同一模式）。
                let weak = ui.as_weak();
                CLASSIFY_ANIM_TIMER.with(|slot| {
                    slot.borrow().start(
                        slint::TimerMode::SingleShot,
                        std::time::Duration::from_millis(16),
                        move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_scope_menu_shown(true);
                            }
                        },
                    );
                });
            } else {
                // 出场两段式：先收 shown，播完再卸载。
                ui.set_scope_menu_shown(false);
                let animated = settings.borrow().ui_animations;
                let weak = ui.as_weak();
                CLASSIFY_ANIM_TIMER.with(|slot| {
                    slot.borrow().start(
                        slint::TimerMode::SingleShot,
                        std::time::Duration::from_millis(if animated { 170 } else { 0 }),
                        move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_scope_menu_open(false);
                            }
                        },
                    );
                });
            }
            let _ = current_scope;
        });
    }

    // 设置面板开合。
    {
        let ui = app.as_weak();
        app.on_settings_toggled(move || {
            let ui = ui.unwrap();
            let closing = ui.get_settings_open();
            if closing {
                ui.set_settings_shown(false);
            }
            ui.set_settings_open(!ui.get_settings_open());
        });
    }

    // D49 主导入：文件对话框多选（素材 + .emo 混选）→ 归类弹窗。
    {
        let import_flow = import_flow.clone();
        let routing = routing.clone();
        let importing_flag = importing.clone();
        app.on_import_files_requested(move || {
            if importing_flag.load(Ordering::SeqCst) {
                if let Some(ui) = import_flow.ui.upgrade() {
                    show_notice(
                        &ui,
                        TargetNoticeTone::Warning,
                        "已经在导入素材，请等进度条结束后再操作".to_string(),
                    );
                }
                return;
            }
            let filter = "素材与素材包 (*.png;*.jpg;*.jpeg;*.gif;*.webp;*.bmp;*.mp4;*.mov;*.mkv;*.avi;*.webm;*.txt;*.md;*.emo)|*.png;*.jpg;*.jpeg;*.gif;*.webp;*.bmp;*.mp4;*.mov;*.mkv;*.avi;*.webm;*.txt;*.md;*.emo|所有文件 (*.*)|*.*";
            match routing.borrow_mut().dialogs().pick_open_files("选择要导入的素材", filter) {
                Ok(Some(paths)) if !paths.is_empty() => import_flow.open(paths),
                Ok(_) => {} // 取消 / 空选 = 零副作用
                Err(error) => {
                    if let Some(ui) = import_flow.ui.upgrade() {
                        show_notice(
                            &ui,
                            TargetNoticeTone::Error,
                            format!("无法打开文件选择器: {error}"),
                        );
                    }
                }
            }
        });
    }

    // 导入菜单（左下角单入口弹层）开合。
    {
        let ui = app.as_weak();
        app.on_import_menu_toggled(move || {
            let ui = ui.unwrap();
            let closing = ui.get_import_menu_open();
            if closing {
                ui.set_import_menu_shown(false);
            }
            ui.set_import_menu_open(!ui.get_import_menu_open());
        });
    }

    // 分类侧栏拖宽结束：松手才写盘（拖动过程只动内存属性，避免每帧 IO）。
    {
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        app.on_sidebar_resize_ended(move |width| {
            let mut s = settings.borrow_mut();
            let clamped = width.clamp(
                ui_viewmodels::SIDEBAR_MIN_WIDTH,
                ui_viewmodels::SIDEBAR_MAX_WIDTH,
            );
            if (s.sidebar_width - clamped).abs() < f32::EPSILON {
                return;
            }
            s.sidebar_width = clamped;
            if let Err(error) = s.save(&settings_path) {
                eprintln!("保存设置失败: {error}");
            }
        });
    }

    // 点击浮层外部：收起设置面板、导入菜单与目标下拉。
    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let target_choices = target_choices.clone();
        let import_flow = import_flow.clone();
        app.on_overlay_dismissed(move || {
            let ui = ui.unwrap();
            ui.set_settings_open(false);
            ui.set_settings_shown(false);
            ui.set_import_menu_open(false);
            ui.set_import_menu_shown(false);
            ui.set_import_menu_shown(false);
            // D51 范围菜单两段式收起（点外部 = 取消开合）。
            ui.set_scope_menu_shown(false);
            ui.set_scope_menu_open(false);
            // D46–D48 浮层（点击外部=关闭，链式同 Esc）。
            ui.set_context_menu_open(false);
            import_flow.close();
            ui.set_move_menu_open(false);
            ui.set_rename_open(false);
            ui.set_properties_open(false);
            if ui.get_target_mode() == ui_enums::target_bar_mode(TargetBarMode::ChooseTarget) {
                let mut routing = routing.borrow_mut();
                routing.toggle_picker();
                sync_target_bar(&ui, &target_choices, routing.snapshot());
            }
        });
    }

    // 手动收起提示条：用户点关闭按钮时清空 notice-text，同时停掉自动消隐计时器。
    {
        let ui = app.as_weak();
        app.on_notice_dismissed(move || {
            let ui = ui.unwrap();
            ui.set_notice_text("".into());
            NOTICE_TIMER.with(|slot| slot.borrow().stop());
        });
    }

    // D38 一键打开日志目录：资源管理器直接定位，导出=整目录拷贝/压缩。
    {
        let ui = app.as_weak();
        app.on_open_logs_requested(move || {
            let Some(ui) = ui.upgrade() else { return };
            match logging::logs_dir() {
                Some(dir) => {
                    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                    logging::info!("打开日志目录 {}", dir.display());
                }
                None => show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "日志尚未初始化（本次运行未生成日志文件）".to_string(),
                ),
            }
        });
    }

    // 通用设置开关：描述化面板（SettingSpec）的单一入口。翻转 → 持久化 → 回填。
    {
        let ui = app.as_weak();
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        let single_click = single_click.clone();
        app.on_setting_toggled(move |key| {
            let ui = ui.unwrap();
            let key = key.to_string();
            let mut s = settings.borrow_mut();
            if !s.toggle(&key) {
                return; // 未知 key：不修改、不持久化
            }
            if let Err(error) = s.save(&settings_path) {
                eprintln!("保存设置失败: {error}");
            }
            // 热字段回填：单击触发模式在瓦片回调里经 Cell 热读。
            single_click.set(s.activate_on_single_click);
            ui.set_single_click_activate(s.activate_on_single_click);
            ui.set_send_after_paste(s.send_after_paste);
            ui.set_gpu_rendering(s.gpu_rendering);
            // 浅色主题：立即实时重铺自绘层令牌（std-widgets 仍 fluent-dark，v1 边界）。
            if key == "light_theme" {
                let tokens = if s.light_theme {
                    LightThemeProvider.theme()
                } else {
                    DarkThemeProvider.theme()
                };
                apply_theme(&ui, &tokens);
                // std-widgets 同步翻 scheme，与自绘层保持同明暗（D37）。
                apply_color_scheme(&ui, s.light_theme);
            }
            // D38 细粒度诊断日志：实时切换等级（默认 Info 与 Debug/Trace）。
            if key == "verbose_diagnostics" {
                logging::set_level(if s.verbose_diagnostics {
                    logging::Level::Trace
                } else {
                    logging::Level::Info
                });
                logging::info!(
                    "诊断日志等级已切换 -> {}",
                    if s.verbose_diagnostics {
                        "trace"
                    } else {
                        "info"
                    }
                );
            }
            sync_settings(&ui, &s);
        });
    }

    // 缩略图后台派生：进度条更新 + 完成后重新装载真实库。
    {
        let ui = app.as_weak();
        app.on_thumbnail_progress(move |done, total| {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let percent = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            ui.set_progress_visible(true);
            ui.set_progress_percent(percent);
            ui.set_progress_text(format!("正在生成缩略图 {done}/{total}").into());
        });
    }

    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let thumbs = real_resolver.clone();
        let library_root = library_root.clone();
        let filter_categories = filter_categories.clone();
        let cache = thumb_cache.clone();
        let grid = grid.clone();

        app.on_thumbnails_generated(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            ui.set_progress_visible(false);
            let root = library_root
                .clone()
                .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
            match ui_viewmodels::load_real_library(std::path::Path::new(&root)) {
                Ok((index, resolver)) => {
                    let names = resolver.category_names();
                    let counts = category_counts_for(&resolver, &names);
                    apply_categories(&ui, &filter_categories, &names, &counts);

                    let mut new_vm = LibraryGridVm::new(index, recent_first_sorter(), 256);
                    new_vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
                    if let Ok(aspects) = resolver.aspects() {
                        new_vm.set_aspects(aspects);
                    }
                    *vm.borrow_mut() = new_vm;
                    *thumbs.borrow_mut() = Some(resolver);
                    cache.borrow_mut().clear();
                    ui.set_content_y(0.0);
                    sync_counts(&ui, vm.borrow().total(), true);
                    grid.sync();
                    show_notice(
                        &ui,
                        TargetNoticeTone::Success,
                        "缩略图生成完成，已刷新预览".to_string(),
                    );
                }
                Err(error) => {
                    show_notice(
                        &ui,
                        TargetNoticeTone::Error,
                        format!("缩略图生成后刷新库失败: {error}"),
                    );
                }
            }
        });
    }

    {
        let ui = app.as_weak();
        app.on_import_progress(move |done, total| {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let percent = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            ui.set_progress_visible(true);
            ui.set_progress_percent(percent);
            ui.set_progress_text(format!("正在导入素材 {done}/{total}").into());
        });
    }

    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let thumbs = real_resolver.clone();
        let library_root = library_root.clone();
        let filter_categories = filter_categories.clone();
        let cache = thumb_cache.clone();
        let grid = grid.clone();

        app.on_import_finished(move |success, message| {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            if !success {
                ui.set_progress_visible(false);
                show_notice(&ui, TargetNoticeTone::Error, message.to_string());
                return;
            }

            let root = library_root
                .clone()
                .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
            match ui_viewmodels::load_real_library(std::path::Path::new(&root)) {
                Ok((index, resolver)) => {
                    let names = resolver.category_names();
                    let counts = category_counts_for(&resolver, &names);
                    apply_categories(&ui, &filter_categories, &names, &counts);

                    let mut new_vm = LibraryGridVm::new(index, recent_first_sorter(), 256);
                    new_vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
                    if let Ok(aspects) = resolver.aspects() {
                        new_vm.set_aspects(aspects);
                    }
                    *vm.borrow_mut() = new_vm;
                    *thumbs.borrow_mut() = Some(resolver);
                    cache.borrow_mut().clear();
                    ui.set_content_y(0.0);
                    sync_counts(&ui, vm.borrow().total(), true);
                    grid.sync();
                    show_notice(&ui, TargetNoticeTone::Success, message.to_string());
                }
                Err(error) => {
                    show_notice(
                        &ui,
                        TargetNoticeTone::Error,
                        format!("导入后刷新库失败: {error}"),
                    );
                }
            }
        });
    }

    // 清空库：两次点击确认，删除当前真实库的 objects/thumbs/meta.db，并回到空库。
    // 任何情况都要把界面重置回空库：删除失败绝不早退（早退会让旧瓦片/旧 resolver 残留，
    // 出现「点击素材指向示例素材、缩略图还是删除前」的错乱状态）。
    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let thumbs = real_resolver.clone();
        let library_root = library_root.clone();
        let filter_categories = filter_categories.clone();
        let cache = thumb_cache.clone();
        let grid = grid.clone();
        let importing = importing.clone();

        let clear_armed = Rc::new(Cell::new(false));
        app.on_clear_library_requested(move || {
            let ui = ui.unwrap();
            if importing.load(Ordering::SeqCst) {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "正在导入素材/生成缩略图，请等进度条结束后再清空".to_string(),
                );
                return;
            }
            if !clear_armed.get() {
                clear_armed.set(true);
                ui.set_clear_button_armed(true);
                ui.set_clear_button_text("再次点击确认删除全部素材".into());
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "再次点击确认删除：所有素材、缩略图和索引都会被删除".to_string(),
                );
                return;
            }

            clear_armed.set(false);
            ui.set_clear_button_armed(false);
            ui.set_clear_button_text("清空素材库".into());

            let root = library_root
                .clone()
                .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
            let root_path = std::path::PathBuf::from(&root);
            let objects = root_path.join("objects");
            let thumbs_dir = root_path.join("thumbs");
            let db = root_path.join("meta.db");

            if !db.is_file() && !objects.is_dir() && !thumbs_dir.is_dir() {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "当前没有真实库可清理".to_string(),
                );
                return;
            }

            // 先释放 Store 连接，否则 Windows 会锁住 meta.db，导致删除失败。
            *thumbs.borrow_mut() = None;

            // 逐项删除并记录失败项；无论成败，界面统一重置回空库。
            let mut failed_items: Vec<String> = Vec::new();
            for path in [&objects, &thumbs_dir, &db] {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let outcome = if path.is_dir() {
                    std::fs::remove_dir_all(path)
                } else if path.is_file() {
                    std::fs::remove_file(path)
                } else {
                    continue;
                };
                if let Err(error) = outcome {
                    eprintln!("清理失败 {}: {error}", path.display());
                    failed_items.push(format!("{name}（{error}）"));
                }
            }

            // 显示层重置（空库）：tiles 立刻清空，避免旧缩略图残留。
            let empty = FacetIndex::new();
            let mut new_vm = LibraryGridVm::new(empty, recent_first_sorter(), 256);
            new_vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
            *vm.borrow_mut() = new_vm;
            ui.set_content_y(0.0);
            sync_counts(&ui, 0, false);
            apply_categories(&ui, &filter_categories, &[], &[]);
            cache.borrow_mut().clear();
            grid.sync();

            if failed_items.is_empty() {
                show_notice(
                    &ui,
                    TargetNoticeTone::Success,
                    format!("素材库已清空（{}），点「导入素材…」重新开始", root_path.display()),
                );
            } else if failed_items.iter().any(|item| item.starts_with("meta.db")) {
                show_notice(
                    &ui,
                    TargetNoticeTone::Error,
                    format!(
                        "素材文件已清除，但索引 meta.db 被占用未能删除（{}）。
 建议退出程序后删除 {} 下的 meta.db，下次导入才不会重新出现旧素材",
                        failed_items.join("；"),
                        root_path.display(),
                    ),
                );
            } else {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    format!(
                        "大部分内容已清除，以下项目暂被占用（通常是系统索引或杀毒软件扫描），稍后可重试：{}。素材库已重置为空，可直接重新导入",
                        failed_items.join("；"),
                    ),
                );
            }
        });
    }

    // 导出 .emo：进度条回调 + 完成回调。
    {
        let ui = app.as_weak();
        app.on_export_progress(move |done, total| {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let percent = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            ui.set_progress_visible(true);
            ui.set_progress_percent(percent);
            ui.set_progress_text(format!("正在导出素材 {done}/{total}").into());
        });
    }

    {
        let ui = app.as_weak();
        app.on_export_finished(move |success, message| {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            ui.set_progress_visible(false);
            if success {
                show_notice(&ui, TargetNoticeTone::Success, message.to_string());
            } else {
                show_notice(&ui, TargetNoticeTone::Error, message.to_string());
            }
        });
    }

    // 导出 .emo：原生保存对话框（Win32 IFileSaveDialog，免 PowerShell 冷启动）→
    // 后台子进程打包（ChildTaskRunner 统一 PROGRESS/stderr 编排）。
    {
        let ui = app.as_weak();
        let library_root = library_root.clone();
        let routing = routing.clone();
        app.on_export_emo_requested(move || {
            let ui = ui.unwrap();
            let root = library_root
                .clone()
                .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
            if !std::path::Path::new(&root).join("meta.db").is_file() {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "当前没有真实库可导出".to_string(),
                );
                return;
            }

            // 原生保存对话框：点「导出…」直接弹，不再经由 PowerShell。
            let save_path = match routing.borrow_mut().dialogs().pick_save_path(
                "导出素材包",
                "素材管理器导出.emo",
                "*.emo",
            ) {
                Ok(Some(path)) => path.to_string_lossy().to_string(),
                Ok(None) => return, // 用户取消
                Err(error) => {
                    show_notice(
                        &ui,
                        TargetNoticeTone::Error,
                        format!("无法打开保存对话框: {error}"),
                    );
                    return;
                }
            };

            let helper = helper_exe("sample-library.exe");
            if !helper.is_file() {
                show_notice(
                    &ui,
                    TargetNoticeTone::Error,
                    format!("未找到导出工具 {}，请确认与主程序同目录", helper.display()),
                );
                return;
            }

            ui.set_progress_visible(true);
            ui.set_progress_percent(0.0);
            ui.set_progress_text("正在准备导出...".into());
            show_notice(
                &ui,
                TargetNoticeTone::Success,
                format!("开始导出 .emo：{}", save_path),
            );

            let weak = ui.as_weak();
            let save_thread = save_path.clone();
            let weak_progress = weak.clone();
            let _ = ChildTask::new(helper, vec!["export".into(), root, save_thread.clone()])
                .with_progress(move |done, total| {
                    let weak = weak_progress.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.invoke_export_progress(done as i32, total as i32);
                        }
                    });
                })
                .with_finished(move |success, message| {
                    let weak = weak.clone();
                    let save_path = save_thread.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            let msg = if success {
                                format!("导出完成：{}", save_path)
                            } else if message.trim().is_empty() {
                                "导出失败".to_string()
                            } else {
                                format!("导出失败：{}", message.trim())
                            };
                            ui.invoke_export_finished(success, msg.into());
                        }
                    });
                })
                .run_in_background();
        });
    }

    // 导入 .emo 素材包：文件模式选择器（*.emo 过滤；D34 的「选文件」侧入口）→
    // 与文件夹导入共用的子进程管线（D24 包注册表按扩展名分发到 EmoReader）。
    // 为什么单独一个按钮：文件夹选择器（FOS_PICKFOLDERS）只列目录，.emo 这类
    // 文件根本不可见——实测用户反馈「弹出的文件选择器无法看见 .emo 文件」。
    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let importing = importing.clone();
        let import_flow = import_flow.clone();
        app.on_import_emo_requested(move || {
            let ui = ui.unwrap();
            // 菜单项已选中：先收起弹层，再弹原生文件对话框。
            ui.set_import_menu_open(false);
            ui.set_import_menu_shown(false);
            if importing.load(Ordering::SeqCst) {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "已经在导入素材，请等进度条结束后再操作".to_string(),
                );
                return;
            }

            let package = match routing.borrow_mut().dialogs().pick_open_file(
                "选择要导入的 .emo 素材包",
                "千牛素材包 (*.emo)|*.emo|所有文件 (*.*)|*.*",
            ) {
                Ok(Some(path)) => path,
                Ok(None) => return, // 用户取消
                Err(error) => {
                    show_notice(
                        &ui,
                        TargetNoticeTone::Error,
                        format!("无法打开文件选择器: {error}"),
                    );
                    return;
                }
            };

            // 「所有文件」放开了扩展名限制；非 .emo 在这里挡回并指路正确入口，
            // 免得子进程报出费解的「不支持导入的来源」。
            let is_emo = package
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("emo"));
            if !is_emo {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    format!(
                        "不是 .emo 素材包：{}。普通素材请用「导入素材… → 导入文件夹」",
                        package.display()
                    ),
                );
                return;
            }

            // R2：.emo 入口同样汇流归类弹窗（默认「按包内分类」）。
            import_flow.open(vec![package]);
        });
    }

    // 导入素材：原生文件夹选择器（Win32 IFileOpenDialog，消除 3 秒 PowerShell 冷启动）
    // → sample-library 后台导入 → derive-thumbs 后台派生缩略图（ChildTaskRunner 编排）。
    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let importing = importing.clone();
        let import_flow = import_flow.clone();
        app.on_import_requested(move || {
            let ui = ui.unwrap();
            // 菜单项已选中：先收起弹层，再弹原生文件夹对话框。
            ui.set_import_menu_open(false);
            ui.set_import_menu_shown(false);
            if importing.load(Ordering::SeqCst) {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "已经在导入素材，请等进度条结束后再操作".to_string(),
                );
                return;
            }

            // 1) 原生文件夹选择：菜单里选「导入文件夹」后直接弹，不要求手输路径。
            let dir = match routing
                .borrow_mut()
                .dialogs()
                .pick_folder("选择要导入的素材文件夹")
            {
                Ok(Some(path)) => path,
                Ok(None) => return, // 用户取消
                Err(error) => {
                    show_notice(
                        &ui,
                        TargetNoticeTone::Error,
                        format!("无法打开文件夹选择器: {error}"),
                    );
                    return;
                }
            };

            // R2：文件夹入口汇流归类弹窗（每批一次、确认才进管线）。
            import_flow.open(vec![dir]);
        });
    }

    grid.sync();
    // D49 拖拽导入：等 HWND 就绪后注册（run 前排队，循环首轮起跑重试退避）。
    {
        let sink = std::sync::Arc::new(SlintFileDropSink { ui: app.as_weak() });
        register_file_drop_when_ready(app.as_weak(), sink, 0);
    }
    // 键盘捕获根（Esc / Ctrl+A）在启动时拿到焦点：capture 模式不抢输入焦点，
    // 检索框等 LineEdit 仍可正常打字（其聚焦态在 key-root 的让位判断里处理）。
    app.invoke_key_focus_requested();
    // GL 驱动缺失的自愈（实测：CI runner/远程桌面等无 GL 环境，femtovg 在事件循环
    // 启动时报 "Failed to initialize OpenGL driver: Could not locate glCreateShader
    // symbol" 直接退出）。winit 一进程只允许一个事件循环，进程内换后端不可行，
    // 唯一出路是换档重启：SLINT_BACKEND=winit-software + 哨兵防无限循环。这是
    // 上文渲染后端选择注释「gpu_rendering > 软件渲染」回退链的最后一级。
    if let Err(e) = app.run() {
        let can_fallback = settings.borrow().gpu_rendering
            && std::env::var_os("SLINT_BACKEND").is_none()
            && std::env::var_os("ASSETDECK_RENDER_FALLBACK").is_none()
            && std::env::var_os("ASSETDECK_FORCE_SOFTWARE").is_none();
        if !can_fallback {
            panic!("Slint 事件循环异常退出: {e}");
        }
        logging::warn!("GL 渲染初始化失败({e})，以软件渲染档重启自身");
        let exe = std::env::current_exe().expect("取当前 exe 失败");
        let respawn = std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env("SLINT_BACKEND", "winit-software")
            .env("ASSETDECK_FORCE_SOFTWARE", "1")
            .env("ASSETDECK_RENDER_FALLBACK", "1")
            .spawn();
        if let Err(spawn_err) = respawn {
            panic!("Slint 事件循环异常退出: {e}（软件渲染档重启也失败: {spawn_err}）");
        }
    }
}

/// 导入管线共享编排（文件夹 / .emo 包两条入口共用）：
/// 库目录前置校验 → sample-library 子进程导入 → 成功才派生缩略图。
///
/// 两处实测修正：
/// 1. 先 `create_dir_all(库根)` 再起子进程——目录建不出/权限不足在这里给出
///    明确报错，而不是拖到子进程写库失败后，由库重载以「下无 meta.db」的
///    面目出现；
/// 2. 阶段一导入失败时**不再触发** thumbnails_generated 刷新。旧代码在
///    失败分支也弹一次库重载，库尚未建立时就抛出「缩略图生成后刷新库失败:
///    <root> 下无 meta.db」，把真正的导入错误掩盖掉了。
fn spawn_import_pipeline(
    ui: slint::Weak<AppWindow>,
    import_args: Vec<String>,
    root: String,
    importing: Arc<AtomicBool>,
    label: String,
    fast_import_mode: bool,
) {
    let ui_ready = ui.upgrade().expect("导入时 UI 已不可用");

    if let Err(error) = std::fs::create_dir_all(&root) {
        show_notice(
            &ui_ready,
            TargetNoticeTone::Error,
            format!(
                "无法创建素材库目录 {}: {error}（请检查 --library-root 参数与磁盘权限）",
                root
            ),
        );
        return;
    }

    let helper = helper_exe("sample-library.exe");
    if !helper.is_file() {
        show_notice(
            &ui_ready,
            TargetNoticeTone::Error,
            format!("未找到导入工具 {}，请确认与主程序同目录", helper.display()),
        );
        return;
    }

    ui_ready.set_progress_visible(true);
    ui_ready.set_progress_percent(0.0);
    ui_ready.set_progress_text("正在准备导入...".into());
    show_notice(
        &ui_ready,
        TargetNoticeTone::Success,
        format!("开始导入：{label}"),
    );
    logging::info!("开始导入 label={label} root={root} args={import_args:?}");

    let weak = ui.clone();
    let root_thread = root;
    let derive = helper_exe("derive-thumbs.exe");
    let worker = helper_exe("decode-worker.exe");
    let weak_progress = weak.clone();
    importing.store(true, Ordering::SeqCst);
    let importing_phase1 = Arc::clone(&importing);
    let importing_phase2 = Arc::clone(&importing);

    // D37/D38：日志约定传给子进程（derive-thumbs / decode-worker 读 env）；
    // 导入档位（--mode）已由调用方拼进 import_args。
    let logs_dir_arg = logging::logs_dir();
    let log_level_arg = logging::current_level().as_str();

    // 阶段一：sample-library 导入（解码/pHash/拷贝/落库全在子进程，D11）。
    // NOTICE 行 = 整体成功但个别素材失败（伪装扩展名/损坏图），弹警示避免
    // 「部分失败被当成全成」的静默丢素材。
    let weak_notice = weak.clone();
    let mut phase1_task = ChildTask::new(helper, import_args);
    if let Some(dir) = logs_dir_arg.clone() {
        phase1_task = phase1_task
            .with_env("DSH_LOG_DIR", &dir.to_string_lossy())
            .with_env("DSH_LOG_LEVEL", log_level_arg);
    }
    let _ = phase1_task
        .with_progress(move |done, total| {
            let weak = weak_progress.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_import_progress(done as i32, total as i32);
                }
            });
        })
        .with_notice(move |text| {
            let weak = weak_notice.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    show_notice(&ui, TargetNoticeTone::Warning, text);
                }
            });
        })
        .with_finished(move |success, message| {
            let weak = weak.clone();
            let derive = derive.clone();
            let worker = worker.clone();
            let root_thread = root_thread.clone();
            let weak_invoke = weak.clone();
            let importing_phase1 = Arc::clone(&importing_phase1);
            let importing_phase2 = Arc::clone(&importing_phase2);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak_invoke.upgrade() {
                    if !success {
                        importing_phase1.store(false, Ordering::SeqCst);
                        let msg = if message.trim().is_empty() {
                            "导入失败".to_string()
                        } else {
                            format!("导入失败：{}", message.trim())
                        };
                        ui.invoke_import_finished(false, msg.into());
                        return;
                    }
                    // 导入完成，先让 UI 刷新出素材列表。
                    ui.invoke_import_finished(true, "导入完成，正在后台生成缩略图...".into());
                }
            });

            // 阶段二：缩略图派生（同一编排器；仅当阶段一成功；缺工具时 UI 直接收尾）。
            // 失败分支绝不刷新库：那是误导性「下无 meta.db」报错的旧来源。
            if !success {
                return;
            }
            if !derive.is_file() || !worker.is_file() {
                importing.store(false, Ordering::SeqCst);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_thumbnails_generated();
                        show_notice(
                            &ui,
                            TargetNoticeTone::Warning,
                            "素材已导入，但未找到缩略图工具（derive-thumbs/decode-worker），
 缩略图暂缺；请使用完整安装包运行"
                                .to_string(),
                        );
                    }
                });
                return;
            }
            // D37 档位沿用：派生速度与导入档一致（设置在启动后可改，按当前值）。
            let mode_arg: &'static str = if fast_import_mode {
                "fast"
            } else {
                "background"
            };
            let weak_derived_progress = weak.clone();
            let weak_derived_finished = weak.clone();
            let mut phase2_task = ChildTask::new(
                derive,
                vec![
                    "--library".into(),
                    root_thread,
                    "--worker-exe".into(),
                    worker.to_string_lossy().into_owned(),
                    "--mode".into(),
                    mode_arg.to_string(),
                ],
            );
            if let Some(dir) = logs_dir_arg.clone() {
                phase2_task = phase2_task
                    .with_env("DSH_LOG_DIR", &dir.to_string_lossy())
                    .with_env("DSH_LOG_LEVEL", log_level_arg);
            }
            let _ = phase2_task
                .with_progress(move |done, total| {
                    let weak = weak_derived_progress.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.invoke_thumbnail_progress(done as i32, total as i32);
                        }
                    });
                })
                .with_finished(move |success, message| {
                    // 缩略图成败都不改变「导入已完成」的事实：都触发一次库重载；
                    // 但失败原因要浮出来——旧实现 `_success, _message` 直接吞掉，
                    // 「N 个素材缩略图派生失败」对用户完全不可见。
                    importing_phase2.store(false, Ordering::SeqCst);
                    let weak = weak_derived_finished.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.invoke_thumbnails_generated();
                            if !success && !message.trim().is_empty() {
                                show_notice(
                                    &ui,
                                    TargetNoticeTone::Warning,
                                    format!("缩略图派生未全部成功：{}", message.trim()),
                                );
                            }
                        }
                    });
                })
                .run_in_background();
        })
        .run_in_background();
}

/// 解析 `--library-root <path>` / `--library-root=<path>` 启动参数。
///
/// 含空格的路径是重灾区：壳/快捷方式忘加引号时，argv 会把一个路径按空白切成
/// 多段。旧实现只取下一段，得到 `C:\Users\Administrator\Documents\Default`
/// 这种截断根目录——后续所有导入都写向错误位置并报「下无 meta.db」（实测用户
/// 反馈）。这里把连续非旗标段重新拼回去，同时兼容 `=` 形式与成对引号残留。
fn parse_library_root(args: &[String]) -> Option<String> {
    for (index, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--library-root=") {
            return normalize_library_root_value(value);
        }
        if arg == "--library-root" {
            let rest = args.get(index + 1..).unwrap_or(&[]);
            let end = rest
                .iter()
                .position(|token| token.starts_with("--"))
                .unwrap_or(rest.len());
            if end == 0 {
                return None; // 只有旗标没有值：回落默认库路径逻辑
            }
            return normalize_library_root_value(&rest[..end].join(" "));
        }
    }
    None
}

/// 清洗解析出的值：去首尾空白与手工补引号留下的成对引号；空值视为未提供。
fn normalize_library_root_value(value: &str) -> Option<String> {
    let cleaned = value.trim().trim_matches('"').trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

fn default_library_root() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("library")))
        .unwrap_or_else(|| std::path::PathBuf::from("library"))
}

fn helper_exe(name: &str) -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from(name))
}

fn win32_runtime_deps() -> TargetRuntimeDeps {
    use platform::win32::{
        Win32Clipboard, Win32FileDialogs, Win32Focus, Win32ForegroundObserver, Win32Injector,
        Win32InputFocuser, Win32Readiness, Win32WindowActivator, Win32WindowEnumerator,
    };

    TargetRuntimeDeps {
        observer: Win32ForegroundObserver::new()
            .ok()
            .map(|observer| Box::new(observer) as Box<dyn platform::ForegroundObserver>),
        enumerator: Box::new(Win32WindowEnumerator),
        clipboard: Box::new(Win32Clipboard),
        focus: Box::new(Win32Focus),
        injector: Box::new(Win32Injector),
        activator: Box::new(Win32WindowActivator),
        readiness: Box::new(Win32Readiness),
        focuser: Box::new(Win32InputFocuser),
        dialogs: Box::new(Win32FileDialogs),
    }
}

// ---------------------------------------------------------------------------
// --bench 内存守卫模式（M7，design.md 契约）。
// ---------------------------------------------------------------------------

/// --bench 默认静置时长（供父进程采样）。
const BENCH_DEFAULT_HOLD_MS: u64 = 8_000;
/// 脚本化浏览步距与窗口条数（dispatch 契约：每 5000 项跳一窗 ensure_window(first,40)）。
const BENCH_BROWSE_STEP: usize = 5_000;
const BENCH_WINDOW_COUNT: usize = 40;

struct BenchArgs {
    root: String,
    hold_ms: u64,
}

fn parse_bench_args(args: &[String]) -> Option<BenchArgs> {
    let pos = args.iter().position(|a| a == "--bench")?;
    // 缺值不得静默回落 GUI（跨层守则：错误绝不静默切换模式）。
    let root = match args.get(pos + 1) {
        Some(v) if !v.is_empty() && !v.starts_with("--") => v.clone(),
        _ => {
            eprintln!("--bench 需要 <library-root> 参数");
            std::process::exit(64);
        }
    };
    let hold_ms = args
        .iter()
        .position(|a| a == "--bench-hold-ms")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(BENCH_DEFAULT_HOLD_MS);
    Some(BenchArgs { root, hold_ms })
}

/// 浏览脚本 + 静置采样窗口。返回进程退出码。
fn run_bench(args: &BenchArgs) -> i32 {
    use std::io::Write;
    use std::path::Path;
    use std::time::{Duration, Instant};

    let root = Path::new(&args.root);
    let index = match ui_viewmodels::load_library_catalog(root) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("--bench: 库目录装载失败: {e}");
            return 3;
        }
    };

    let mut vm = LibraryGridVm::new(index, Sorter::default(), 256);
    vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);

    let total = vm.total();
    let started = Instant::now();
    if total > 0 {
        let mut first = 0usize;
        while first < total {
            vm.ensure_window(first, BENCH_WINDOW_COUNT);
            first += BENCH_BROWSE_STEP;
        }
        vm.ensure_window(total.saturating_sub(BENCH_WINDOW_COUNT), BENCH_WINDOW_COUNT); // 至尾
        vm.ensure_window(0, BENCH_WINDOW_COUNT); // 回首
    }
    let browse_ms = started.elapsed().as_millis();

    // D43 壳层驻留守卫：逐窗装载缩略图 + 窗口显式驱逐扫过整库后，
    // 壳层缓存必须收敛于「单窗规模」——旧实现的无界 HashMap 在这里会随
    // 浏览的窗口数线性增长，本守卫把「切过的分类越多驻留越大」钉进 CI。
    let mut thumb_cache_entries = 0usize;
    if let Ok((_, resolver)) = ui_viewmodels::load_real_library(root) {
        let mut cache = ThumbCache::new(THUMB_CACHE_CAPACITY);
        let mut first = 0usize;
        while first < total {
            let end = (first + BENCH_WINDOW_COUNT).min(total);
            let window: std::collections::HashSet<u32> = (first as u32..end as u32).collect();
            for &id in &window {
                if cache.get(id).is_none() {
                    let image = resolver
                        .thumbnail_path(AssetId(id))
                        .and_then(|path| slint::Image::load_from_path(&path).ok())
                        .unwrap_or_default();
                    cache.put(id, image);
                }
            }
            cache.retain_window(&window);
            first += BENCH_BROWSE_STEP;
        }
        thumb_cache_entries = cache.len();
        if thumb_cache_entries > BENCH_WINDOW_COUNT {
            eprintln!(
                "--bench: D43 驻留守卫红：整库扫窗后缓存仍有 {thumb_cache_entries} 条（> 单窗 {BENCH_WINDOW_COUNT}）"
            );
            return 5;
        }
    }

    // hold 静置：父进程在此窗口内采 WorkingSet。
    std::thread::sleep(Duration::from_millis(args.hold_ms));

    let line = format!(
        "{{\"browse_done\":true,\"total\":{total},\"browse_ms\":{browse_ms},\"hold_ms\":{},\"resident_thumbs\":{},\"thumb_cache_entries\":{}}}",
        args.hold_ms,
        vm.visible_cache_ids().len(),
        thumb_cache_entries
    );
    println!("{line}");
    let _ = std::io::stdout().flush();
    0
}

#[cfg(test)]
mod library_root_args_spec {
    use super::parse_library_root;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    // 用正斜杠书写测试路径：本模块验证的是参数拼接/清洗逻辑本身，
    // 不涉及分隔符语义；同时避开转义歧义。

    #[test]
    fn separated_value_with_spaces_stays_intact() {
        assert_eq!(
            parse_library_root(&args(&[
                "--library-root",
                "C:/Users/Administrator/Documents/Default Project/library",
            ]))
            .as_deref(),
            Some("C:/Users/Administrator/Documents/Default Project/library"),
        );
    }

    #[test]
    fn space_split_argv_is_rejoined_until_next_flag() {
        // 壳层丢引号的真实形态：路径被按空格拆散，「Project」漂移到下一个 argv，
        // 旧实现截断成「C:/Users/Administrator/Documents/Default」这样的残根。
        assert_eq!(
            parse_library_root(&args(&[
                "--library-root",
                "C:/Users/Administrator/Documents/Default",
                "Project/library",
            ]))
            .as_deref(),
            Some("C:/Users/Administrator/Documents/Default Project/library"),
        );
    }

    #[test]
    fn equals_form_is_taken_verbatim() {
        assert_eq!(
            parse_library_root(&args(&["--library-root=D:/lib With Space"])).as_deref(),
            Some("D:/lib With Space"),
        );
    }

    #[test]
    fn stray_manual_quotes_are_trimmed() {
        assert_eq!(
            parse_library_root(&args(&["--library-root", "\"E:/my lib\""])).as_deref(),
            Some("E:/my lib"),
        );
    }

    #[test]
    fn missing_or_valueless_flags_fall_back_to_none() {
        assert_eq!(parse_library_root(&args(&[])), None);
        assert_eq!(parse_library_root(&args(&["--library-root"])), None);
        assert_eq!(parse_library_root(&args(&["--other", "x"])), None);
    }
}
