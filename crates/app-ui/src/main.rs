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
use std::sync::{Arc, Mutex};

use platform::{HttpFileDownloader, HttpTextFetcher, UrlOpener};
use slint::{Model, ModelRc, Timer, TimerMode, VecModel};
use ui_viewmodels::classify::{
    self, ClassifyTarget, EntryKind, GroupMode, ImportEntry, SourceGroup,
};
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
    /// D56 更新弹窗两段式入场（同 CLASSIFY_ANIM_TIMER 模式）。
    static UPDATE_ANIM_TIMER: RefCell<Timer> = RefCell::new(Timer::default());
    /// D56 更新检查的 UI 线程装配句柄。worker 线程经 invoke_from_event_loop
    /// 弹回 UI 线程后从这里取回 VM/设置 Rc——Rc 非 Send，不能跨线程携带；
    /// 收尾闭包只带 Weak + 纯数据（Send），Rc 在 UI 线程侧取。
    static UPDATE_WIRING: RefCell<Option<UpdateWiring>> = const { RefCell::new(None) };
}

/// D56/D70 更新链路在 UI 线程持有的共享件（见 [`UPDATE_WIRING`]）。
/// vm/apply_vm 是 UI 线程专属 Rc；cancel 用 Arc——下载线程（Send 域）要
/// 持有同一份取消旗标，UI 线程与线程双侧都能 store/load。
struct UpdateWiring {
    vm: Rc<RefCell<ui_viewmodels::UpdateCheckVm>>,
    apply_vm: Rc<RefCell<ui_viewmodels::UpdateApplyVm>>,
    cancel: Arc<AtomicBool>,
    settings: Rc<RefCell<AppSettings>>,
    settings_path: Rc<std::path::PathBuf>,
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
                    default_label: choice.base_label.into(),
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

/// 原子写入：先写同目录 tmp 再 rename 覆盖，避免半写坏文件（与 settings 保存同一模式）。
fn atomic_write(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(content.as_bytes())?;
    file.sync_all().ok();
    drop(file);
    std::fs::rename(&tmp, path)
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
        ui.set_selection_count(count as i32);
        // 操作条标签只报状态（教学句移交空态视图：长句曾把按钮挤出条外）。
        // 张/项统一为「项」：回收站与多选都可能含视频/文本。
        // 0 选时给操作指引而非「已选 0 项」的空读数——此刻该做的是去点素材。
        ui.set_selection_text(
            if trash {
                if count > 0 {
                    format!("回收站 · 已选 {count} 项")
                } else {
                    "回收站".to_string()
                }
            } else if count > 0 {
                format!("已选 {count} 项")
            } else {
                "点击素材选择，Esc 退出".to_string()
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

/// 弹窗状态在壳层的持有物 + 决策装配。全部 UI 线程访问。
struct ImportFlow {
    ui: slint::Weak<AppWindow>,
    groups: Rc<RefCell<Vec<SourceGroup>>>,
    /// D66 静默直通包（finalize 分流产物；弹窗确认后与组决策合并成清单）。
    silent: Rc<RefCell<Vec<std::path::PathBuf>>>,
    entries: Rc<RefCell<Vec<ImportEntry>>>,
    /// 待预扫描条目数（probe 回来一个减一个，归零即 finalize）。
    pending: Rc<Cell<u32>>,
    settings: Rc<RefCell<AppSettings>>,
    categories: Rc<RefCell<Vec<String>>>,
    importing: Arc<AtomicBool>,
    library_root: Option<String>,
}

/// 归入候选封顶（弹窗列表限高，超出部分靠继续输入收窄）。
const MATCH_CAP: usize = 8;

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

    /// 全部 probe 就绪：D66 分流（可解析包静默直通）→ R8 记忆直通 / 弹窗
    /// （批次级：只读清单行 + 单输入框 + 实时候选与预告行）。
    fn finalize(self: &Rc<Self>) {
        let plan = classify::plan_import(&self.entries.borrow());
        let groups_len = plan.groups.len();
        logging::info!(
            "导入 finalize：entries={} silent_packages={} groups={}",
            self.entries.borrow().len(),
            plan.silent_packages.len(),
            groups_len
        );
        if groups_len == 0 {
            // 可解析包全部静默按包内分类直通（D66）：整批 .emo 拖入零点击。
            if !plan.silent_packages.is_empty() {
                logging::info!(
                    "可解析包静默直通：{} 个包按包内分类导入",
                    plan.silent_packages.len()
                );
                self.do_import(&[], None, &plan.silent_packages);
            } else {
                logging::warn!("plan_import 两组皆空（全部条目被过滤？），不弹窗");
            }
            return;
        }
        *self.groups.borrow_mut() = plan.groups.clone();
        *self.silent.borrow_mut() = plan.silent_packages;
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_classify_summary(classify::manifest_summary(&plan.groups).into());
        ui.set_classify_argument(
            classify::dialog_prefill(&plan.groups)
                .unwrap_or_default()
                .into(),
        );
        self.refresh_target(&ui);
        ui.set_classify_open(true);
        logging::info!("归类弹窗已置位 classify_open（groups={groups_len}）");
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

    /// 输入同步：argument 落属性 → 重滤候选 + 刷新实时预告行。
    fn set_name(&self, name: slint::SharedString) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_classify_argument(name.clone());
        self.refresh_matches(&ui, &name);
        self.refresh_target(&ui);
    }

    /// 候选点选 = 用列表规范名回填输入框（与输入同路，预告行随之转「已有分类」）。
    fn set_picked(&self, name: slint::SharedString) {
        self.set_name(name);
    }

    /// 候选重滤（全类目来自壳层 categories，skip(1) 跳「全部」表头）。
    fn refresh_matches(&self, ui: &AppWindow, typed: &str) {
        let candidates: Vec<String> = self.categories.borrow().iter().skip(1).cloned().collect();
        let hits = classify::filter_category_matches(&candidates, typed, MATCH_CAP);
        ui.set_classify_matches(ModelRc::from(Rc::new(VecModel::from(
            hits.into_iter()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        ))));
    }

    /// 实时预告行：输入框内容 → 目标解析（与 confirm 共用 resolve_target，
    /// 导入前把结果说清楚）。
    fn refresh_target(&self, ui: &AppWindow) {
        let candidates: Vec<String> = self.categories.borrow().iter().skip(1).cloned().collect();
        let typed = ui.get_classify_argument();
        let target = classify::resolve_target(&candidates, typed.as_str());
        ui.set_classify_hint(target.hint().into());
        ui.set_classify_hint_kind(match &target {
            ClassifyTarget::Inbox => ui_enums::CLASSIFY_HINT_INBOX,
            ClassifyTarget::Existing(_) => ui_enums::CLASSIFY_HINT_EXISTING,
            ClassifyTarget::Fresh(_) => ui_enums::CLASSIFY_HINT_NEW,
        });
    }

    /// 「导入」确认：kind = UiEnums.classify-confirm-*（0 按输入解析，
    /// 1 直入待分类）。自动建分类时 toast 点名。
    fn confirm(self: &Rc<Self>, kind: i32) {
        let groups = self.groups.borrow().clone();
        let silent = self.silent.borrow().clone();
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let decision = match kind {
            ui_enums::CLASSIFY_CONFIRM_INBOX => (GroupMode::Inbox, None),
            ui_enums::CLASSIFY_CONFIRM_RESOLVE => {
                let candidates: Vec<String> = self
                    .categories
                    .borrow()
                    .iter()
                    .skip(1) // 下标 0 = 「全部」，不是分类
                    .cloned()
                    .collect();
                match classify::resolve_target(&candidates, ui.get_classify_argument().as_str()) {
                    ClassifyTarget::Inbox => (GroupMode::Inbox, None),
                    ClassifyTarget::Existing(name) => (GroupMode::Into, Some(name)),
                    ClassifyTarget::Fresh(name) => {
                        // 无感知红线：自动建分类必须点名（输入「待分类」= inbox
                        // 指令，不会真的建重名分类）。
                        if name != classify::INBOX_CATEGORY {
                            show_notice(
                                &ui,
                                TargetNoticeTone::Success,
                                format!("未精确匹配到已有分类，已自动创建「{name}」并导入"),
                            );
                        }
                        (GroupMode::Create, Some(name))
                    }
                }
            }
            other => {
                logging::warn!("classify-confirmed 未知 kind={other}，按待分类处理");
                (GroupMode::Inbox, None)
            }
        };
        self.do_import(&groups, Some(decision), &silent);
        self.close();
    }

    /// 决策 + 静默直通包 → 清单文件 → `--import-paths` 子进程管线（进度条/
    /// 失败提示/缩略图派生与旧两入口全同路）。批次级一个决策；decision 为
    /// None = 只发静默直通行（整批可解析包）。
    fn do_import(
        &self,
        groups: &[SourceGroup],
        decision: Option<(GroupMode, Option<String>)>,
        silent_packages: &[std::path::PathBuf],
    ) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let mut lines = String::new();
        let mut total = 0usize;
        // D66 静默直通包：p\tauto\t = 按包内分类，先于弹窗决策行。
        for path in silent_packages {
            lines.push_str(&format!("p\tauto\t{}\n", path.display()));
            total += 1;
        }
        let mode_field = decision
            .as_ref()
            .map(|(mode, category)| classify::decision_to_mode_field(*mode, category.as_deref()))
            .unwrap_or_else(|| "inbox".to_string());
        for group in groups {
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
            None,
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
        logging::info!(
            "拖拽 Drop 送达：{} 条路径（{}）",
            paths.len(),
            paths
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
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

/// 主窗口就绪（WinEvent 钩子回调，事件驱动，无轮询）后的挂载项：D49 拖拽
/// 导入注册 + 恢复重绘守卫（唤出黑屏兜底，机制详见 platform::win32::paint_guard
/// 模块注释：守卫钩 WM_PAINT 全窗失效，应用层翻转 repaint-nudge 标脏整窗，
/// 强制软件渲染器部分重绘管线画一整帧）。任一失败只告警，不阻断主流程。
fn mount_when_window_ready(
    hwnd: isize,
    sink: std::sync::Arc<SlintFileDropSink>,
    ui: slint::Weak<AppWindow>,
) {
    match platform::win32::dragdrop::register_file_drop(hwnd, sink) {
        Ok(()) => logging::info!("拖拽导入已注册（hwnd={hwnd:#x}）"),
        Err(error) => logging::warn!("拖拽导入注册失败：{error}（可继续用导入入口）"),
    }
    let paint_ui = ui;
    match platform::win32::paint_guard::install(
        hwnd,
        Box::new(move || {
            if let Some(ui) = paint_ui.upgrade() {
                logging::info!("整窗失效重绘哨兵触发（repaint-nudge 翻转）");
                ui.set_repaint_nudge(!ui.get_repaint_nudge());
            }
        }),
    ) {
        Ok(()) => logging::info!("恢复重绘守卫已安装（hwnd={hwnd:#x}）"),
        Err(error) => logging::warn!("恢复重绘守卫安装失败：{error}（不影响常规重绘）"),
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

/// 设置面板模型：从 [`AppSettings::describe`] 铺成 slint 的 SettingRowData 列表
/// （含分区标题行，header=true 的行 key 为空、面板侧只渲染组名）。
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
            header: view.header,
        })
        .collect();
    ui.set_settings(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// 发起一次更新检查（D56）：网络在后台线程（独立 WinHTTP 会话，UI 线程零
/// 阻塞），收尾经 `invoke_from_event_loop` 回 UI 线程统一改 VM 与设置——
/// 设置写盘只发生在 UI 线程，与其它写入点互不竞态。须在 UI 线程调用
/// （[`UPDATE_WIRING`] 与 Slint 状态都是线程局部的）。
/// `silent`：启动静默档（失败零打扰，只记日志）；false = 手动档（面板可见）。
fn spawn_update_check(ui: slint::Weak<AppWindow>, feeds_path: std::path::PathBuf, silent: bool) {
    let (vm, settings) = UPDATE_WIRING.with(|slot| {
        let guard = slot.borrow();
        let wiring = guard.as_ref().expect("更新装配未初始化");
        (wiring.vm.clone(), wiring.settings.clone())
    });
    // 检查态立刻可见：按钮进「检查中…」，状态行先落到「正在检查更新…」。
    vm.borrow_mut().begin_check();
    if let Some(app) = ui.upgrade() {
        app.set_update_checking(true);
        let status = vm.borrow().status_text(
            settings.borrow().last_check_unix,
            ui_viewmodels::unix_now_secs(),
        );
        app.set_update_status_text(status.into());
        app.set_update_status_danger(false);
    }
    let feeds = ui_viewmodels::load_feeds(&feeds_path);
    if !silent {
        logging::info!("手动检查更新（{} 个源）", feeds.len());
    }
    std::thread::spawn(move || {
        let started_unix = ui_viewmodels::unix_now_secs();
        let outcome = ui_viewmodels::check_update(
            &platform::win32::Win32HttpFetcher,
            &feeds,
            env!("CARGO_PKG_VERSION"),
        );
        // 闭包只带 Weak + 纯数据（Send）；Rc 装配件在 UI 线程侧经槽位取回。
        let _ = slint::invoke_from_event_loop(move || {
            apply_update_outcome(&ui, outcome, started_unix, silent);
        });
    });
}

/// 更新检查收尾（UI 线程）：VM 裁决动作 → 回填面板状态/弹窗数据/角标 →
/// 按需弹窗（两段式入场）。弹窗动作决策（静默失败不弹、跳过版本静默不弹）
/// 在 VM 里，这里只做属性回填。
fn apply_update_outcome(
    ui: &slint::Weak<AppWindow>,
    outcome: ui_viewmodels::CheckOutcome,
    started_unix: u64,
    silent: bool,
) {
    let Some(app) = ui.upgrade() else {
        return;
    };
    let (update_vm, settings, settings_path) = UPDATE_WIRING.with(|slot| {
        let guard = slot.borrow();
        let wiring = guard.as_ref().expect("更新装配未初始化");
        (
            wiring.vm.clone(),
            wiring.settings.clone(),
            wiring.settings_path.clone(),
        )
    });
    if let ui_viewmodels::CheckOutcome::Failed(message) = &outcome {
        // 静默档失败不进面板，但要留痕——否则「从没弹过窗」无从排查。
        if silent {
            logging::warn!("静默检查更新失败: {message}");
        }
    }
    let action = update_vm.borrow_mut().finish(outcome, silent);
    // 检查时刻落盘（成败皆记：静默节流按「发起过」计）。
    {
        let mut stored = settings.borrow_mut();
        stored.last_check_unix = started_unix;
        if let Err(error) = stored.save(&settings_path) {
            logging::warn!("保存设置失败: {error}");
        }
    }
    app.set_update_checking(false);
    {
        let vm = update_vm.borrow();
        app.set_update_status_text(
            vm.status_text(
                settings.borrow().last_check_unix,
                ui_viewmodels::unix_now_secs(),
            )
            .into(),
        );
        app.set_update_status_danger(vm.status_is_error());
        app.set_update_badge(vm.badge_visible());
        if let Some(release) = vm.available() {
            app.set_update_version(release.version.clone().into());
            app.set_update_notes(release.notes.clone().into());
            app.set_update_url(release.url.clone().into());
        }
    }
    if action == ui_viewmodels::UpdateUiAction::OpenDialog {
        app.set_update_open(true);
        // 两段式入场（D53）：下一帧再翻 shown，首帧前置位不触发过渡。
        let weak = app.as_weak();
        UPDATE_ANIM_TIMER.with(|slot| {
            slot.borrow().start(
                TimerMode::SingleShot,
                std::time::Duration::from_millis(16),
                move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_update_shown(true);
                    }
                },
            );
        });
    }
    logging::info!("更新检查收尾 silent={silent} action={action:?}");
}

/// 取消在途更新下载（UI 线程，D70）：置取消旗标 → VM 回 Idle → 弹窗回
/// 初始态。下载线程稍后以 Err 退出，其迟到的失败被 VM 的 Idle 态吞掉
/// （见 UpdateApplyVm::mark_failed），不会把弹窗拽回错误态。
fn cancel_update_download(ui: &AppWindow) {
    let (apply_vm, cancel) = UPDATE_WIRING.with(|slot| {
        let guard = slot.borrow();
        let wiring = guard.as_ref().expect("更新装配未初始化");
        (wiring.apply_vm.clone(), wiring.cancel.clone())
    });
    if !apply_vm.borrow().is_downloading() {
        return;
    }
    cancel.store(true, Ordering::SeqCst);
    apply_vm.borrow_mut().reset();
    ui.set_update_downloading(false);
    ui.set_update_progress(0.0);
    ui.set_update_progress_text("".into());
    logging::info!("已取消更新下载");
}

/// exe 目录是否为安装器的默认安装目录（D70 模式判定）。判定结果只决定
/// 是否给安装器传 `--no-shortcuts`——两种形态的解包目标都是 exe 所在目录，
/// 判错的差异上限是「快捷方式没刷新」，无破坏性。
fn is_installer_install(exe_dir: &std::path::Path) -> bool {
    let known = std::env::var_os("LOCALAPPDATA")
        .map(|base| {
            std::path::PathBuf::from(base)
                .join("Programs")
                .join("素材管理器")
        })
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|base| {
                std::path::PathBuf::from(base)
                    .join("AppData")
                    .join("Local")
                    .join("Programs")
                    .join("素材管理器")
            })
        });
    matches!(known, Some(path) if exe_dir == path)
}

/// 应用内更新（D70 统一安装器路径）：下载新 `assetdeck-installer-<ver>.exe`
/// （其 payload 内嵌 dist.tar.gz，一个文件即完整新版本）→ SHA-256 校验 →
/// spawn `--silent --install-dir=<exe 目录> --wait-pid=<本进程>` → 本进程
/// 退出。安装器持 PROCESS_SYNCHRONIZE 句柄等老进程退出后接管解包与重启，
/// 运行中 exe 的文件锁由「先退出、安装器后解包」的时序消解。
/// 须在 UI 线程调用；网络全在后台线程，进度经 invoke_from_event_loop 回弹。
fn spawn_update_install(ui: &slint::Weak<AppWindow>, importing: &AtomicBool) {
    let (check_vm, apply_vm, cancel, feeds_config_path) = UPDATE_WIRING.with(|slot| {
        let guard = slot.borrow();
        let wiring = guard.as_ref().expect("更新装配未初始化");
        // D71 下载镜像清单与检查源共用 update_feeds.toml（settings 同目录约定）。
        let feeds_config_path = wiring
            .settings_path
            .parent()
            .map(|dir| dir.join("update_feeds.toml"))
            .unwrap_or_else(|| std::path::PathBuf::from("update_feeds.toml"));
        (
            wiring.vm.clone(),
            wiring.apply_vm.clone(),
            wiring.cancel.clone(),
            feeds_config_path,
        )
    });
    if apply_vm.borrow().is_busy() {
        return;
    }
    if importing.load(Ordering::SeqCst) {
        if let Some(app) = ui.upgrade() {
            show_notice(
                &app,
                TargetNoticeTone::Error,
                "素材导入进行中，导入完成后再更新".to_string(),
            );
        }
        return;
    }
    let Some(release) = check_vm.borrow().available().cloned() else {
        return;
    };
    let installer_name = ui_viewmodels::installer_asset_name(&release.version);
    let Some(installer_asset) =
        ui_viewmodels::pick_asset(&release.assets, &installer_name).cloned()
    else {
        let message = "发布清单缺少安装包，无法应用内更新".to_string();
        logging::warn!("{message}");
        apply_vm.borrow_mut().begin_download();
        apply_vm.borrow_mut().mark_failed(message.clone());
        if let Some(app) = ui.upgrade() {
            app.set_update_apply_error(message.into());
        }
        return;
    };
    let Some(sums_asset) = ui_viewmodels::pick_asset(&release.assets, ui_viewmodels::SUMS_ASSET_NAME)
        .cloned()
    else {
        let message = "发布清单缺少校验和清单（SHA256SUMS.txt）".to_string();
        logging::warn!("{message}");
        apply_vm.borrow_mut().begin_download();
        apply_vm.borrow_mut().mark_failed(message.clone());
        if let Some(app) = ui.upgrade() {
            app.set_update_apply_error(message.into());
        }
        return;
    };

    let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let installer_mode = is_installer_install(&exe_dir);
    // 固定名覆盖写：临时目录里永远至多一份更新安装器，不累积历史版本。
    let dest = std::env::temp_dir().join("assetdeck-installer-update.exe");
    let pid = std::process::id();

    apply_vm.borrow_mut().begin_download();
    cancel.store(false, Ordering::SeqCst);
    if let Some(app) = ui.upgrade() {
        app.set_update_downloading(true);
        app.set_update_progress(0.0);
        app.set_update_progress_text("准备下载…".into());
        app.set_update_apply_error("".into());
    }
    logging::info!(
        "应用内更新开始 version={} installer={} mode={}",
        release.version,
        installer_asset.url,
        if installer_mode { "installer" } else { "portable" }
    );

    let ui_weak = ui.clone();
    let installer_url = installer_asset.url;
    let sums_url = sums_asset.url;
    std::thread::spawn(move || {
        // 进度节流：100ms 一跳；首块与收尾块（received==total）必发。
        // 格式化留在 VM（可测），线程只回传原始字节数。
        const PROGRESS_MIN_INTERVAL: std::time::Duration =
            std::time::Duration::from_millis(100);
        // D71 镜像测速参数：64 KiB 取样、8s 上限——超 8s 拿不到 64KiB 的源
        // 不配当「最快」，落选但不淘汰（全灭时仍有兜底资格）。
        const PROBE_TIMEOUT_MS: u64 = 8_000;
        const PROBE_SAMPLE_BYTES: u32 = 64 * 1024;
        // SUMS 锚定原始源的专用上限：清单只有几百字节，10s 足够礼貌。
        const SUMS_ANCHOR_TIMEOUT_MS: u64 = 10_000;
        let last_tick = std::cell::Cell::new(
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );

        let report_failure = |weak: slint::Weak<AppWindow>, message: String| {
            let _ = slint::invoke_from_event_loop(move || {
                let apply_vm = UPDATE_WIRING.with(|slot| {
                    slot.borrow().as_ref().expect("更新装配未初始化").apply_vm.clone()
                });
                apply_vm.borrow_mut().mark_failed(message.clone());
                if let Some(ui) = weak.upgrade() {
                    ui.set_update_downloading(false);
                    ui.set_update_progress(0.0);
                    ui.set_update_progress_text("".into());
                    if let Some(error) = apply_vm.borrow().failure() {
                        ui.set_update_apply_error(error.to_string().into());
                    }
                }
            });
        };
        // 阶段文案直写属性（进度行在下载态会被 VM 的 progress_text 接管，
        // 测速/选源阶段还没有字节数，这里借同一行展示阶段）。
        let set_phase = |text: &str| {
            let _ = slint::invoke_from_event_loop({
                let weak = ui_weak.clone();
                let text = text.to_string();
                move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_update_progress_text(text.into());
                    }
                }
            });
        };

        let run = || -> Result<(), String> {
            let fetcher = platform::win32::Win32HttpFetcher;
            let downloader = platform::win32::Win32FileDownloader;

            // ① 候选源 = 镜像前缀改写 + 原始 URL 压轴；并行测速选最快（D71）。
            // 唯一候选（镜像被配置关闭）时跳过测速，直连即可。
            let mirrors = ui_viewmodels::load_download_mirrors(&feeds_config_path);
            let candidates = ui_viewmodels::mirror_candidates(&mirrors, &installer_url);
            let mut probe_ms: Vec<Option<u64>> = Vec::new();
            let mut sums_from_origin: Option<String> = None;
            if candidates.len() > 1 {
                set_phase("正在选择最快下载源…");
                std::thread::scope(|scope| {
                    let mut probes = Vec::new();
                    for url in &candidates {
                        probes.push(scope.spawn(|| {
                            downloader
                                .probe_sample(url, PROBE_TIMEOUT_MS, PROBE_SAMPLE_BYTES, &cancel)
                                .ok()
                        }));
                    }
                    // ② SHA256SUMS 锚定原始源（信任锚不跟着镜像走）；
                    // 失败才降级为「经镜像取得」并留告警，签名（批 C）前
                    // 这是镜像内容可信度的根。
                    let origin = scope.spawn(|| {
                        fetcher.fetch_text(&sums_url, SUMS_ANCHOR_TIMEOUT_MS).ok()
                    });
                    for probe in probes {
                        probe_ms.push(probe.join().unwrap_or(None));
                    }
                    sums_from_origin = origin.join().unwrap_or(None);
                });
            } else {
                probe_ms.push(None);
            }
            if cancel.load(Ordering::SeqCst) {
                return Err("下载已取消".into());
            }
            let order = ui_viewmodels::rank_by_probe(&probe_ms);
            let winner = order[0];
            match probe_ms[winner] {
                Some(ms) => set_phase(&format!(
                    "已选择最快源 {}（{ms}ms）",
                    ui_viewmodels::mirror_label(&candidates[winner])
                )),
                None => set_phase("开始直连下载…"),
            }

            // ③ 逐候选：下载 → 校验。网络失败与哈希不符（镜像滞后/被篡改）
            // 都顺延下一候选；清单缺条目与本地摘要失败是恒定错误，换源无解。
            let mut sums_loaded: Option<String> = sums_from_origin;
            let mut expected: Option<String> = None;
            let mut attempts: Vec<String> = Vec::new();
            for &index in &order {
                let url = &candidates[index];
                let label = ui_viewmodels::mirror_label(url);
                if cancel.load(Ordering::SeqCst) {
                    return Err("下载已取消".into());
                }
                if expected.is_none() {
                    if sums_loaded.is_none() {
                        match fetcher.fetch_text(url, ui_viewmodels::DOWNLOAD_TIMEOUT_MS) {
                            Ok(text) => {
                                sums_loaded = Some(text);
                                logging::warn!(
                                    "SHA256SUMS 原始源不可达，经镜像 {label} 取得（降级）"
                                );
                            }
                            Err(error) => {
                                attempts.push(format!("{label}: 清单获取失败（{error}）"));
                                continue;
                            }
                        }
                    }
                    let text = sums_loaded.as_deref().expect("清单已装载");
                    match ui_viewmodels::parse_sha256_sums(text)
                        .into_iter()
                        .find(|(_, name)| name == &installer_name)
                    {
                        Some((hash, _)) => expected = Some(hash),
                        None => return Err("校验和清单缺少安装包条目".into()),
                    }
                }
                set_phase(&format!("正在从 {label} 下载…"));
                logging::info!("尝试下载源 {label}");
                let weak_for_progress = ui_weak.clone();
                if let Err(error) = downloader.download_to_file(
                    url,
                    &dest,
                    ui_viewmodels::DOWNLOAD_TIMEOUT_MS,
                    ui_viewmodels::MAX_DOWNLOAD_BYTES,
                    &mut |received, total| {
                        let now = std::time::Instant::now();
                        if received == total
                            || now.duration_since(last_tick.get()) >= PROGRESS_MIN_INTERVAL
                        {
                            last_tick.set(now);
                            let weak = weak_for_progress.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                let apply_vm = UPDATE_WIRING.with(|slot| {
                                    slot.borrow()
                                        .as_ref()
                                        .expect("更新装配未初始化")
                                        .apply_vm
                                        .clone()
                                });
                                apply_vm.borrow_mut().set_progress(received, total);
                                if let Some(ui) = weak.upgrade() {
                                    let vm = apply_vm.borrow();
                                    ui.set_update_progress(vm.progress_ratio());
                                    if let Some(text) = vm.progress_text() {
                                        ui.set_update_progress_text(text.into());
                                    }
                                }
                            });
                        }
                    },
                    &cancel,
                ) {
                    attempts.push(format!("{label}: {error}"));
                    continue;
                }

                let actual = platform::win32::sha256_file_hex(&dest)
                    .map_err(|error| format!("计算安装包摘要失败: {error}"))?;
                let expected_hash = expected.as_deref().expect("校验和已在下载前就绪");
                if !ui_viewmodels::hash_matches(expected_hash, &actual) {
                    attempts.push(format!(
                        "{label}: SHA-256 不符（内容疑似滞后或被篡改）"
                    ));
                    continue;
                }
                logging::info!("安装包校验通过 source={label}");
                return Ok(());
            }
            Err(format!("所有下载源均失败——{}", attempts.join("；")))
        };

        if let Err(message) = run() {
            if cancel.load(Ordering::SeqCst) {
                logging::info!("更新下载已取消，丢弃半成品");
                return;
            }
            logging::warn!("应用内更新失败: {message}");
            report_failure(ui_weak, message);
            return;
        }

        // 校验通过 → 移交安装器。--install-dir 指向 exe 所在目录：安装版即
        // 原安装目录，便携版即便携目录；安装版多刷一次快捷方式（无害且保新）。
        let mut command = std::process::Command::new(&dest);
        command
            .arg("--silent")
            .arg("--install-dir")
            .arg(&exe_dir)
            .arg(format!("--wait-pid={pid}"));
        if !installer_mode {
            command.arg("--no-shortcuts");
        }
        match command.spawn() {
            Err(error) => {
                report_failure(ui_weak, format!("无法启动安装器: {error}"));
            }
            Ok(_) => {
                logging::info!("安装器已启动，移交更新（wait-pid={pid}）");
                let _ = slint::invoke_from_event_loop(move || {
                    let apply_vm = UPDATE_WIRING.with(|slot| {
                        slot.borrow().as_ref().expect("更新装配未初始化").apply_vm.clone()
                    });
                    apply_vm.borrow_mut().mark_launching();
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_update_downloading(false);
                        ui.set_update_launching(true);
                    }
                    logging::info!("应用内更新完成移交，进程退出");
                    std::process::exit(0);
                });
            }
        }
    });
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

    // D59 目标配置双通道：用户画像 profiles.user.toml（同 id 覆盖内置）与实例
    // 别名册 targets.json。与设置同目录约定（库根优先，回退 exe 旁）。
    let target_config_dir = settings_path
        .parent()
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let profiles_user_path = target_config_dir.join("profiles.user.toml");
    let targets_json_path = target_config_dir.join("targets.json");
    // D56 更新源清单：与 settings.toml 同目录（库根优先，回退 exe 旁）。
    // 缺省用内置 GitHub 主源；国内镜像顺序回落经此文件配置。
    let feeds_path = target_config_dir.join("update_feeds.toml");

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
    // D56 更新检查：VM 初始化（带回「跳过此版本」的记忆）+ 版本读数注入。
    let update_vm = Rc::new(RefCell::new(ui_viewmodels::UpdateCheckVm::new(
        settings.borrow().dismissed_version.clone(),
    )));
    // D70 应用内更新：应用段状态机 + 取消旗标（Arc，下载线程同持）。
    let update_apply_vm = Rc::new(RefCell::new(ui_viewmodels::UpdateApplyVm::new()));
    let update_cancel = Arc::new(AtomicBool::new(false));
    app.set_app_version(format!("v{}", env!("CARGO_PKG_VERSION")).into());
    // 装配句柄进 UI 线程槽：worker 收尾闭包（只带 Send 数据）回 UI 后取用。
    UPDATE_WIRING.with(|slot| {
        *slot.borrow_mut() = Some(UpdateWiring {
            vm: update_vm.clone(),
            apply_vm: update_apply_vm.clone(),
            cancel: update_cancel.clone(),
            settings: settings.clone(),
            settings_path: settings_path.clone(),
        })
    });
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

    // 用户画像损坏不得拖垮主程序：退回内置画像并留痕（内置画像有编译期测试兜底）。
    let profiles_user = std::fs::read_to_string(&profiles_user_path);
    let profiles_user = match profiles_user {
        Ok(content) => match ui_viewmodels::TargetRoutingRuntime::profile_load_check(
            BUILTIN_PROFILES,
            Some(&content),
        ) {
            Ok(()) => Some(content),
            Err(error) => {
                logging::error!(
                    "用户画像 {} 解析失败，本次退回内置画像: {error}",
                    profiles_user_path.display()
                );
                None
            }
        },
        Err(_) => None,
    };
    let routing = Rc::new(RefCell::new(
        TargetRoutingRuntime::new(
            BUILTIN_PROFILES,
            profiles_user.as_deref(),
            win32_runtime_deps(),
        )
        .expect("目标画像加载失败"),
    ));
    // 实例别名册：坏文件按空册处理（装饰性数据不阻断目标功能）。
    if let Ok(content) = std::fs::read_to_string(&targets_json_path) {
        routing
            .borrow_mut()
            .set_aliases(ui_viewmodels::AliasMap::parse(&content));
    }
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

    // D61 旧版库迁移入口：启动即检测 exe 旁遗留库（设置面板展开时也会重检）。
    refresh_migration_entry(&app, library_root.as_deref());

    // D49/D50 通用导入流（三入口汇流 → 预扫描 → 归类弹窗 → 清单子进程）。
    let import_flow = Rc::new(ImportFlow {
        ui: app.as_weak(),
        groups: Rc::new(RefCell::new(Vec::new())),
        silent: Rc::new(RefCell::new(Vec::new())),
        entries: Rc::new(RefCell::new(Vec::new())),
        pending: Rc::new(Cell::new(0)),
        settings: settings.clone(),
        categories: filter_categories.clone(),
        importing: importing.clone(),
        library_root: library_root.clone(),
    });
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

    // D66 归类弹窗回调：输入同步 / 候选点选 / 取消 / 确认导入。
    {
        let flow = import_flow.clone();
        app.on_classify_name_changed(move |name| {
            flow.set_name(name);
        });
    }
    {
        let flow = import_flow.clone();
        app.on_classify_picked(move |name| {
            flow.set_picked(name);
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
        app.on_classify_confirmed(move |kind| {
            flow.confirm(kind);
        });
    }
    // D65 导入结果弹窗：两段式出场（同 classify——先收 shown 播反向过渡，
    // 播完再收 open 卸载）。
    {
        let ui = app.as_weak();
        app.on_import_result_closed(move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let animated = ui.get_animations_enabled();
            ui.set_import_result_shown(false);
            let weak = ui.as_weak();
            Timer::single_shot(
                std::time::Duration::from_millis(if animated { 170 } else { 0 }),
                move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_import_result_open(false);
                    }
                },
            );
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
            logging::info!("UI files_dropped 回调：{} 条", paths.len());
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

    // 目标重命名弹层的 pending 键：Esc 关闭链（下方）要 clone，故声明在链前。
    let pending_target_rename: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Esc：关闭链（归类弹窗→更新→菜单级→重命名→属性→目标重命名→设置
    // →清空选区退多选）。D67：全部浮层可 Esc 收起——点外部不再关工作流
    // 弹窗后，Esc 是它们与显式按钮并列的键盘出口。
    {
        let crud = crud.clone();
        let import_flow = import_flow.clone();
        let pending_target_rename = pending_target_rename.clone();
        app.on_escape_pressed(move || {
            let Some(ui) = crud.ui.upgrade() else { return };
            // 归类弹窗是模态最上层，Esc = 取消本次导入。
            if ui.get_classify_open() {
                import_flow.close();
                return;
            }
            // D56 新版本弹窗：Esc = 以后再说（不改「跳过」状态）；下载中
            // Esc = 取消下载再关弹窗（D70）——弹窗关了下载若继续，完成时会
            // 毫无预兆地退出应用，不可接受。
            if ui.get_update_open() {
                if ui.get_update_downloading() {
                    cancel_update_download(&ui);
                }
                ui.set_update_open(false);
                ui.set_update_shown(false);
                return;
            }
            // 菜单级浮层（D67 补：此前只有点外部一条路）。
            if ui.get_import_menu_open() {
                ui.set_import_menu_open(false);
                ui.set_import_menu_shown(false);
                return;
            }
            if ui.get_scope_menu_open() {
                ui.set_scope_menu_shown(false);
                ui.set_scope_menu_open(false);
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
            } else if ui.get_target_rename_open() {
                pending_target_rename.replace(None);
                ui.set_target_rename_open(false);
            } else if ui.get_settings_open() {
                ui.set_settings_shown(false);
                ui.set_settings_open(false);
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
                    "没有检测到运行中的 IM 窗口，请先打开微信/千牛/拼多多商家版等目标应用"
                        .to_string(),
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

    // D59 目标实例重命名：右键 picker 行 → 弹层 → 保存进 targets.json（原子写）。
    // pending 键在弹层存续期间持有 selection_key；取消/Esc 只关弹层不落数据
    // （D67 起点外部不再关它；声明在 Esc 关闭链前，链里共用同一 Rc）。
    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let pending = pending_target_rename.clone();
        app.on_target_rename_requested(move |selection_key| {
            let ui = ui.unwrap();
            let routing = routing.borrow_mut();
            // 别名键在 instance_id（exe:pid）上，休眠占位（空 instance_id）不可命名。
            let Some(choice) = routing
                .snapshot()
                .choices
                .into_iter()
                .find(|choice| choice.selection_key() == selection_key.as_str())
                .filter(|choice| !choice.binding.instance_id.is_empty())
            else {
                return;
            };
            pending.replace(Some(selection_key.to_string()));
            ui.set_target_rename_default(choice.base_label.clone().into());
            ui.set_target_rename_text(choice.binding.label.clone().into());
            ui.set_target_rename_open(true);
        });
    }
    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let target_choices = target_choices.clone();
        let pending = pending_target_rename.clone();
        let targets_json = targets_json_path.clone();
        app.on_target_rename_confirmed(move |alias| {
            let ui = ui.unwrap();
            let Some(key) = pending.replace(None) else {
                ui.set_target_rename_open(false);
                return;
            };
            let mut routing = routing.borrow_mut();
            if !routing.rename_target(key.as_str(), Some(alias.as_str())) {
                ui.set_target_rename_open(false);
                return;
            }
            let json = routing.aliases().to_json();
            if let Err(error) = atomic_write(&targets_json, &json) {
                logging::error!("别名册 {} 保存失败: {error}", targets_json.display());
                show_notice(
                    &ui,
                    TargetNoticeTone::Error,
                    format!("别名保存失败: {error}"),
                );
            }
            ui.set_target_rename_open(false);
            sync_target_bar(&ui, &target_choices, routing.snapshot());
        });
    }
    {
        let ui = app.as_weak();
        let pending = pending_target_rename.clone();
        app.on_target_rename_cancelled(move || {
            let ui = ui.unwrap();
            pending.replace(None);
            ui.set_target_rename_open(false);
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
                        match resolver.materialize(asset_id) {
                            Ok(Some(materialized)) => {
                                let payload = materialized.as_payload();
                                let notice = routing.borrow_mut().paste(&payload);
                                logging::info!(
                                    "上框完成 asset_id={} tone={:?} text={}",
                                    asset_id.0,
                                    notice.tone,
                                    notice.text
                                );
                                show_notice(ui, notice.tone, notice.text);
                            }
                            // 物化落空/出错必须留痕：此分支曾零日志，排障时
                            // 「上框请求后无下文」在日志里无迹可循（实测两类：
                            // 非入库素材被点击、文本素材编码读取失败）。
                            Ok(None) => {
                                logging::warn!(
                                    "上框物化落空 asset_id={}：素材不在当前库索引",
                                    asset_id.0
                                );
                                show_notice(
                                    ui,
                                    TargetNoticeTone::Warning,
                                    "真实素材读取失败，请检查库文件".to_string(),
                                );
                            }
                            Err(error) => {
                                logging::warn!("上框物化出错 asset_id={}：{error}", asset_id.0);
                                show_notice(
                                    ui,
                                    TargetNoticeTone::Warning,
                                    "真实素材读取失败，请检查库文件".to_string(),
                                );
                            }
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
        let library_root = library_root.clone();
        app.on_settings_toggled(move || {
            let ui = ui.unwrap();
            let closing = ui.get_settings_open();
            if closing {
                ui.set_settings_shown(false);
            } else {
                // D61：每次展开重检旧版库（迁移可能刚被文件管理器改名/删除）。
                refresh_migration_entry(&ui, library_root.as_deref());
            }
            ui.set_settings_open(!ui.get_settings_open());
        });
    }

    // D67 设置面板显式关闭：点外部不再收起（防误触），出口 = 面板头部
    // 「关闭」按钮或 Esc。
    {
        let ui = app.as_weak();
        app.on_settings_closed(move || {
            let ui = ui.unwrap();
            ui.set_settings_shown(false);
            ui.set_settings_open(false);
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

    // 点击浮层外部（挡板，appwindow.slint）：D67 分级——只收菜单级浮层；
    // 工作流弹窗（设置/归类/重命名/属性/目标重命名/更新）只挡不关，关闭
    // 走显式按钮与 Esc，误触背景不丢输入。
    {
        let ui = app.as_weak();
        let routing = routing.clone();
        let target_choices = target_choices.clone();
        app.on_overlay_dismissed(move || {
            let ui = ui.unwrap();
            ui.set_import_menu_open(false);
            ui.set_import_menu_shown(false);
            // D51 范围菜单两段式收起（点外部 = 取消开合）。
            ui.set_scope_menu_shown(false);
            ui.set_scope_menu_open(false);
            ui.set_context_menu_open(false);
            ui.set_move_menu_open(false);
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

    // D61 旧版库一键迁移：改名留档 → --import-paths 重放导入（复用导入管线
    // 的进度/去重/缩略图编排，D49 清单格式）。失败回滚改名，素材原位无损；
    // 成功写完成标记收账，入口自动消失。
    {
        let ui = app.as_weak();
        let importing = importing.clone();
        let settings = settings.clone();
        let library_root = library_root.clone();
        app.on_migrate_legacy_requested(move || {
            let Some(ui) = ui.upgrade() else { return };
            if importing.load(Ordering::SeqCst) {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "有导入任务进行中，请稍后再迁移".to_string(),
                );
                return;
            }
            let root = library_root
                .clone()
                .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());
            // 候选目录与 refresh_migration_entry 同一份（D64）：exe 旁 + 安装器
            // 默认安装目录。
            let mut candidate_dirs: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(exe) = std::env::current_exe() {
                if let Some(parent) = exe.parent() {
                    candidate_dirs.push(parent.to_path_buf());
                }
            }
            if let Some(default_install) =
                ui_viewmodels::legacy_migration::default_legacy_install_dir()
            {
                if !candidate_dirs.contains(&default_install) {
                    candidate_dirs.push(default_install);
                }
            }
            let Some(legacy) = ui_viewmodels::legacy_migration::detect_legacy_library_multi(
                &candidate_dirs,
                std::path::Path::new(&root),
            ) else {
                ui.set_migrate_legacy_available(false);
                return;
            };
            if legacy.file_count == 0 {
                ui.set_migrate_legacy_available(false);
                return;
            }
            // 改名先行，清单随后（指向备份路径）：清单若在改名前写、引用旧
            // 路径，改名后全部失效，worker 会静默跳过整批（imported=0 却
            // 「成功」收账——真机验证抓到的原始缺陷）。清单写失败则回滚
            // 改名，旧目录原位无损。
            let renamed = !legacy.is_backup;
            let original = legacy.source.clone();
            // 备份建在旧库自己所在目录（可能与 exe 旁不同——D64 跨目录候选）。
            let detected_dir = original
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .expect("取当前 exe 失败")
                        .parent()
                        .expect("exe 必有父目录")
                        .to_path_buf()
                });
            let backup = if renamed {
                match ui_viewmodels::legacy_migration::rename_to_backup(&original, &detected_dir)
                {
                    Ok(backup) => backup,
                    Err(error) => {
                        show_notice(
                            &ui,
                            TargetNoticeTone::Error,
                            format!("旧库目录无法改名（请关闭旧版素材管理器后重试）: {error}"),
                        );
                        return;
                    }
                }
            } else {
                original.clone()
            };
            let list = std::env::temp_dir().join(format!(
                "assetdeck_migrate_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            // D61 分类保留：读旧库 uuid→category（best-effort；读不到按
            // 无分类走 auto，迁移照常进行）。在改名之后读备份目录的 meta.db。
            let categories =
                match ui_viewmodels::legacy_migration::read_legacy_categories(
                    &backup.join("meta.db"),
                ) {
                    Ok(map) => map,
                    Err(error) => {
                        logging::warn!("旧库分类读取失败（按待分类迁移）: {error}");
                        std::collections::HashMap::new()
                    }
                };
            if let Err(error) = ui_viewmodels::legacy_migration::write_import_manifest(
                &backup,
                &list,
                &categories,
            ) {
                if renamed {
                    if let Err(rollback) = std::fs::rename(&backup, &original) {
                        logging::warn!("清单写失败且改名回滚失败 backup={} : {rollback}", backup.display());
                    }
                }
                let _ = std::fs::remove_file(&list);
                show_notice(
                    &ui,
                    TargetNoticeTone::Error,
                    format!("无法生成迁移清单: {error}"),
                );
                return;
            }
            logging::info!(
                "旧版库迁移开始 source={} backup={} files={}",
                original.display(),
                backup.display(),
                legacy.file_count
            );
            let label = format!("旧版素材迁移（{} 项）", legacy.file_count);
            let mode_arg = import_mode_arg(&settings.borrow());
            let args = vec![
                "--import-paths".to_string(),
                list.to_string_lossy().into_owned(),
                "--library".to_string(),
                root.clone(),
                "--mode".to_string(),
                mode_arg.to_string(),
            ];
            let weak_post = ui.as_weak();
            let library_root_post = library_root.clone();
            let backup_post = backup.clone();
            let original_post = original.clone();
            let list_post = list.clone();
            let post: std::sync::Arc<dyn Fn(bool) + Send + Sync> =
                std::sync::Arc::new(move |success: bool| {
                if success {
                    if let Err(error) =
                        ui_viewmodels::legacy_migration::mark_migrated(&backup_post)
                    {
                        logging::warn!("迁移完成标记写入失败（不影响迁移结果）: {error}");
                    }
                    if let Some(ui) = weak_post.upgrade() {
                        show_notice(
                            &ui,
                            TargetNoticeTone::Success,
                            format!(
                                "旧版素材已并入统一库；旧目录留档为「{}」，确认无误后可手动删除",
                                backup_post
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            ),
                        );
                    }
                } else if renamed {
                    // 导入失败：回滚改名，素材原位无损，下次入口照常出现。
                    match std::fs::rename(&backup_post, &original_post) {
                        Ok(()) => {
                            logging::info!("迁移导入失败，已回滚改名 {}", original_post.display())
                        }
                        Err(error) => logging::warn!(
                            "迁移导入失败且改名回滚失败 backup={} : {error}（备份保留，入口可重试）",
                            backup_post.display()
                        ),
                    }
                }
                let _ = std::fs::remove_file(&list_post);
                if let Some(ui) = weak_post.upgrade() {
                    refresh_migration_entry(&ui, library_root_post.as_deref());
                }
            });
            spawn_import_pipeline(
                ui.as_weak(),
                args,
                root,
                importing.clone(),
                label,
                settings.borrow().fast_import_mode,
                Some(post),
            );
        });
    }

    // D64 已完成态：打开迁移留档目录（资源管理器直接定位）。路径来自检测
    // 时的属性，实际存在性在这里再兜一次底。
    {
        let ui = app.as_weak();
        app.on_open_migration_backup_requested(move || {
            let Some(ui) = ui.upgrade() else { return };
            let backup = ui.get_migrate_legacy_backup_path().to_string();
            if backup.is_empty() || !std::path::Path::new(&backup).is_dir() {
                show_notice(
                    &ui,
                    TargetNoticeTone::Warning,
                    "留档目录不存在（可能已被清理）".to_string(),
                );
                return;
            }
            let _ = std::process::Command::new("explorer.exe")
                .arg(&backup)
                .spawn();
            logging::info!("打开迁移留档目录 {backup}");
        });
    }

    // D56 手动检查更新：设置面板「关于」区按钮。在途时按钮已禁用，这里兜底防抖。
    {
        let ui = app.as_weak();
        let update_vm = update_vm.clone();
        let feeds_path = feeds_path.clone();
        app.on_update_check_requested(move || {
            if update_vm.borrow().is_checking() {
                return;
            }
            spawn_update_check(ui.clone(), feeds_path.clone(), false);
        });
    }

    // D56 新版本弹窗三动作：打开发布页（系统默认浏览器）/ 以后再说 / 跳过此版本。
    {
        let ui = app.as_weak();
        app.on_update_open_release_page(move || {
            let Some(ui) = ui.upgrade() else { return };
            let url = ui.get_update_url().to_string();
            if url.is_empty() {
                return;
            }
            match platform::win32::Win32UrlOpener.open_url(&url) {
                Ok(()) => {
                    ui.set_update_open(false);
                    ui.set_update_shown(false);
                    logging::info!("已打开发布页 {url}");
                }
                Err(error) => show_notice(
                    &ui,
                    TargetNoticeTone::Error,
                    format!("无法打开发布页: {error}"),
                ),
            }
        });
    }
    {
        let ui = app.as_weak();
        let update_vm = update_vm.clone();
        let settings = settings.clone();
        let settings_path = settings_path.clone();
        app.on_update_skip_version(move || {
            let Some(ui) = ui.upgrade() else { return };
            let Some(version) = update_vm.borrow_mut().skip_version() else {
                return; // 无命中更新时跳过无意义
            };
            {
                let mut stored = settings.borrow_mut();
                stored.dismissed_version = version.clone();
                if let Err(error) = stored.save(&settings_path) {
                    logging::warn!("保存设置失败: {error}");
                }
            }
            ui.set_update_open(false);
            ui.set_update_shown(false);
            ui.set_update_badge(update_vm.borrow().badge_visible());
            ui.set_update_status_text(
                update_vm
                    .borrow()
                    .status_text(
                        settings.borrow().last_check_unix,
                        ui_viewmodels::unix_now_secs(),
                    )
                    .into(),
            );
            logging::info!("已跳过版本 {version}");
        });
    }

    // D70 应用内更新：立即更新（失败重试走同一回调）与取消下载。
    {
        let ui = app.as_weak();
        let update_vm = update_vm.clone();
        let apply_vm = update_apply_vm.clone();
        let importing_flag = importing.clone();
        app.on_update_install_requested(move || {
            if apply_vm.borrow().is_busy() {
                return; // 在途防抖（按钮禁用是壳层职责，这里兜底）
            }
            if update_vm.borrow().available().is_none() {
                return; // 无命中更新（弹窗残留）时无动作
            }
            spawn_update_install(&ui, &importing_flag);
        });
    }
    {
        let ui = app.as_weak();
        app.on_update_cancel_install(move || {
            if let Some(ui) = ui.upgrade() {
                cancel_update_download(&ui);
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

    // D56 静默检查：开关开启且距上次 ≥24h 才发起。后台线程跑网络，结果经
    // 事件循环回 UI——失败只记日志（VM 静默档），不打扰启动过程。
    {
        let stored = settings.borrow();
        let now = ui_viewmodels::unix_now_secs();
        if stored
            .last_check_unix
            .saturating_add(ui_viewmodels::CHECK_INTERVAL_SECS)
            <= now
            && stored.auto_update_check
        {
            logging::info!(
                "静默检查更新（上次检查 {} 秒前）",
                now - stored.last_check_unix
            );
            spawn_update_check(app.as_weak(), feeds_path.clone(), true);
        }
    }

    grid.sync();
    // D49 拖拽导入 + 唤出黑屏兜底（整窗标脏强制全量重绘）：事件驱动等主窗口
    // 就绪——run 前 WinEvent 钩子挂号，窗口首次可见即一次性装配（无轮询）。
    {
        let sink = std::sync::Arc::new(SlintFileDropSink { ui: app.as_weak() });
        let ui_weak = app.as_weak();
        if let Err(error) =
            platform::win32::window_ready::on_first_visible_window(Box::new(move |hwnd| {
                mount_when_window_ready(hwnd, sink, ui_weak)
            }))
        {
            logging::warn!("窗口就绪钩子挂号失败：{error}（拖拽导入与重绘守卫不可用）");
        }
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

/// D61/D64：启动与设置面板展开时检测遗留库，回填设置面板迁移入口。
/// 检测候选 = 当前 exe 旁 + 安装器默认安装目录（便携 zip 换目录装、换路径
/// 重装时旧库不在新 exe 旁——用户实测「一开始就没有迁移按钮」的根因）。
/// 三态：可迁移（按钮）/ 已收账（留档可见 + 打开留档目录）/ 无事（隐藏）。
/// 每次检测结果都落 Info 日志——「按钮为什么没出现」从此有现场可查。
/// 检测是纯目录扫描（零解码零 SQL，见 ui-viewmodels::legacy_migration），
/// UI 线程调用安全。
fn refresh_migration_entry(ui: &AppWindow, library_root: Option<&str>) {
    let root = library_root
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_library_root);
    let mut candidate_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidate_dirs.push(parent.to_path_buf());
        }
    }
    if let Some(default_install) = ui_viewmodels::legacy_migration::default_legacy_install_dir() {
        if !candidate_dirs.contains(&default_install) {
            candidate_dirs.push(default_install);
        }
    }
    let detected =
        ui_viewmodels::legacy_migration::detect_legacy_library_multi(&candidate_dirs, &root);
    match detected {
        Some(legacy) => {
            let away_from_exe = std::env::current_exe().ok().is_some_and(|exe| {
                exe.parent()
                    .map(|parent| parent != legacy.source.parent().unwrap_or(parent))
                    .unwrap_or(false)
            });
            ui.set_migrate_legacy_available(true);
            ui.set_migrate_legacy_done(false);
            ui.set_migrate_legacy_detail(
                format!(
                    "检测到旧版素材库{}：{} 个文件（{:.1} MB）。迁移自动去重合并，旧目录改名留档。",
                    if away_from_exe {
                        format!(
                            "（位于 {}）",
                            legacy.source.parent().unwrap_or(&legacy.source).display()
                        )
                    } else {
                        String::new()
                    },
                    legacy.file_count,
                    legacy.total_bytes as f64 / (1024.0 * 1024.0)
                )
                .into(),
            );
            logging::info!(
                "迁移检测：发现可迁移候选 {}（{} 个文件）",
                legacy.source.display(),
                legacy.file_count
            );
        }
        None => {
            ui.set_migrate_legacy_available(false);
            match ui_viewmodels::legacy_migration::find_completed_backup(&candidate_dirs) {
                Some(backup) => {
                    ui.set_migrate_legacy_done(true);
                    ui.set_migrate_legacy_backup_path(backup.to_string_lossy().into_owned().into());
                    ui.set_migrate_legacy_detail(
                        format!(
                            "旧素材已并入统一库。留档目录：{}（确认无误后可手动删除）。",
                            backup.display()
                        )
                        .into(),
                    );
                    logging::info!("迁移检测：全部已收账，留档于 {}", backup.display());
                }
                None => {
                    ui.set_migrate_legacy_done(false);
                    ui.set_migrate_legacy_detail("".into());
                    logging::info!("迁移检测：候选目录 {} 个，均无遗留库", candidate_dirs.len());
                }
            }
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
    // D61 迁移收尾钩子：阶段一成败各调一次，在 UI 线程、先于成败回显。
    // Arc<dyn Fn> 以配合 ChildTask with_finished 的 Fn 约束（钩子内部自幂等，
    // 重复调用无害）；None = 普通导入无后置动作。
    post_phase1: Option<std::sync::Arc<dyn Fn(bool) + Send + Sync>>,
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
    // 新导入开始时收起上一次的结果弹窗，避免两层弹窗叠放。
    ui_ready.set_import_result_open(false);
    ui_ready.set_import_result_shown(false);
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
    // worker stdout 的非协议行（imported/duplicate/similar/failed/done/timing）
    // 落日志：duplicate/failed/similar 是「素材为什么没出现/为什么被提醒」的
    // 唯一现场记录。done 行的统计另存一份，完成通知带「新增/重复/相似/失败」
    // 摘要（D64/D65）——「导入完成」配 0 新增是用户读成失败的重灾区
    // （2026-08-30 两起：全失败与全重复）。RESULTITEM 明细行（D65）另收一份，
    // 完成时喂导入结果弹窗逐项点名。
    #[derive(Debug, Clone, Copy, Default)]
    struct DoneStats {
        imported: u32,
        skipped: u32,
        similar: u32,
        failed: u32,
    }
    let done_stats: Arc<Mutex<Option<DoneStats>>> = Arc::new(Mutex::new(None));
    let done_stats_line = Arc::clone(&done_stats);
    /// RESULTITEM 明细行（kind, source, existing, distance）：
    /// kind = ui_enums::IMPORT_RESULT_*（D65）。
    type ResultItems = Vec<(i32, String, String, i32)>;
    let result_items: Arc<Mutex<ResultItems>> = Arc::new(Mutex::new(Vec::new()));
    let result_items_line = Arc::clone(&result_items);
    phase1_task = phase1_task.with_line(move |line| {
        if let Some(rest) = line.strip_prefix("done:") {
            let mut stats = DoneStats::default();
            for field in rest.split_whitespace() {
                if let Some(v) = field
                    .strip_prefix("imported=")
                    .and_then(|v| v.parse::<u32>().ok())
                {
                    stats.imported = v;
                } else if let Some(v) = field
                    .strip_prefix("skipped=")
                    .and_then(|v| v.parse::<u32>().ok())
                {
                    stats.skipped = v;
                } else if let Some(v) = field
                    .strip_prefix("similar=")
                    .and_then(|v| v.parse::<u32>().ok())
                {
                    stats.similar = v;
                } else if let Some(v) = field
                    .strip_prefix("failed=")
                    .and_then(|v| v.parse::<u32>().ok())
                {
                    stats.failed = v;
                }
            }
            *done_stats_line.lock().unwrap() = Some(stats);
        } else if let Some(rest) = line.strip_prefix("RESULTITEM\t") {
            // D65 明细行：kind\textra\tsource\texisting。existing 取行内剩余
            // 全部（失败原因即便含制表符也不丢内容）；source 为空 = 上游坏行。
            let mut fields = rest.splitn(4, '\t');
            let kind = match fields.next().unwrap_or_default() {
                "similar" => ui_enums::IMPORT_RESULT_SIMILAR,
                "failed" => ui_enums::IMPORT_RESULT_FAILED,
                _ => ui_enums::IMPORT_RESULT_EXACT,
            };
            let distance = fields
                .next()
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0);
            let source = fields.next().unwrap_or_default().to_string();
            let existing = fields.next().unwrap_or_default().to_string();
            let mut list = result_items_line.lock().unwrap();
            if !source.is_empty() && list.len() < 480 {
                list.push((kind, source, existing, distance));
            }
        }
        logging::info!("sample-library: {line}");
    });
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
                    // 子进程 NOTICE 自带色调前缀（D64）：警示=常驻黄条（素材
                    // 失败），提示=绿条自动消隐（重复素材已在库中——它不是
                    // 失败，黄条曾被用户读成「导入失败」）。未知前缀按警示兜底。
                    let tone = if text.starts_with("提示：") {
                        TargetNoticeTone::Success
                    } else {
                        TargetNoticeTone::Warning
                    };
                    show_notice(&ui, tone, text);
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
            let post_hook = post_phase1.clone();
            let done_stats_finished = Arc::clone(&done_stats);
            let result_items_finished = Arc::clone(&result_items);
            let _ = slint::invoke_from_event_loop(move || {
                // D61 迁移收尾钩子先跑：改名回滚/完成标记要发生在用户看到
                // 「导入失败/完成」回显之前。
                if let Some(post) = post_hook.as_ref() {
                    post(success);
                }
                if let Some(ui) = weak_invoke.upgrade() {
                    if !success {
                        importing_phase1.store(false, Ordering::SeqCst);
                        // 去掉子进程 eprintln 的「sample-library failed: 」前缀——
                        // UI 只说人话，工具名留给日志。
                        let trimmed = message
                            .trim()
                            .strip_prefix("sample-library failed: ")
                            .unwrap_or(message.trim());
                        let msg = if trimmed.is_empty() {
                            "导入失败".to_string()
                        } else {
                            format!("导入失败：{trimmed}")
                        };
                        ui.invoke_import_finished(false, msg.into());
                        return;
                    }
                    // 导入完成：带「新增/重复/相似/失败」摘要（D64/D65）。全重复
                    // 时明说「未新增」——裸的「导入完成」配 0 新增会被读成失败；
                    // 摘要段只拼非零项，0 值不占行。
                    let stats = *done_stats_finished.lock().unwrap();
                    let summary = match stats {
                        Some(s) if s.imported == 0 && s.failed == 0 => {
                            if s.similar > 0 {
                                format!(
                                    "导入完成：{} 个素材已在库中 · 相似素材 {} 个已导入，见明细",
                                    s.skipped, s.similar
                                )
                            } else {
                                format!("导入完成：{} 个素材已在库中，未新增", s.skipped)
                            }
                        }
                        Some(s) => {
                            let mut parts = vec![format!("新增 {}", s.imported)];
                            if s.skipped > 0 {
                                parts.push(format!("重复 {}", s.skipped));
                            }
                            if s.similar > 0 {
                                parts.push(format!("相似 {}", s.similar));
                            }
                            if s.failed > 0 {
                                parts.push(format!("失败 {}", s.failed));
                            }
                            format!("导入完成：{}", parts.join(" · "))
                        }
                        None => "导入完成，正在后台生成缩略图...".to_string(),
                    };
                    // D65 结果弹窗：有需要核对/知情的条目（跳过/相似/失败）才弹，
                    // 干净导入维持轻提示不加点击成本。明细行来自 RESULTITEM 协议。
                    let rows: Vec<ImportResultRowData> = result_items_finished
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(kind, source, existing, distance)| ImportResultRowData {
                            kind: *kind,
                            source: source.as_str().into(),
                            existing: existing.as_str().into(),
                            distance: *distance,
                        })
                        .collect();
                    if !rows.is_empty() {
                        ui.set_import_result_summary(summary.as_str().into());
                        ui.set_import_result_rows(slint::ModelRc::from(Rc::new(VecModel::from(
                            rows,
                        ))));
                        ui.set_import_result_open(true);
                        let weak_anim = weak_invoke.clone();
                        Timer::single_shot(std::time::Duration::from_millis(16), move || {
                            if let Some(ui) = weak_anim.upgrade() {
                                ui.set_import_result_shown(true);
                            }
                        });
                    }
                    ui.invoke_import_finished(true, summary.into());
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
            phase2_task =
                phase2_task.with_line(move |line| logging::info!("derive-thumbs: {line}"));
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

/// 缺省库根：`%LOCALAPPDATA%\asset-manager\library`（与 logs 的平台缺省目录
/// 同一数据根）。库根绝不跟 exe 走——exe 同目录库会让每个副本分裂出独立库
/// （互不可见、互不去重），且安装目录里的库会被重装/验证流程清空（2026-08-29
/// 实测：重装安装版清掉用户当天导入的全部素材）。开发/便携隔离用
/// `--library-root` 显式指定。
fn default_library_root() -> std::path::PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(base) if !base.is_empty() => std::path::PathBuf::from(base)
            .join("asset-manager")
            .join("library"),
        _ => {
            logging::warn!("LOCALAPPDATA 未设置，库根回落 exe 同目录（多副本将各自分裂）");
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("library")))
                .unwrap_or_else(|| std::path::PathBuf::from("library"))
        }
    }
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
