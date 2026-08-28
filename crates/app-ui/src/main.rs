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

use slint::{ModelRc, Timer, TimerMode, VecModel};
use ui_viewmodels::grid_vm::LibraryGridVm;
use ui_viewmodels::{
    AppSettings, Asset, AssetId, AssetKind, AssetPayload, CategoryId, DarkThemeProvider,
    FacetIndex, FacetSearchProvider, Filter, LightThemeProvider, RealAssetResolver, SearchProvider,
    SortDirection, SortField, SortSpec, Sorter, TagId, TargetBarMode, TargetBarSnapshot,
    TargetHealth, TargetNoticeTone, TargetRoutingRuntime, TargetRuntimeDeps, ThemeProvider,
    ThemeTokens,
};

use task_runner::ChildTask;
use thumbs::{GridCtx, ThumbCache, ThumbSource, THUMB_CACHE_CAPACITY};

thread_local! {
    /// 成功提示的自动消隐计时器。Slint 的 Timer 非 Send，只能在 UI 线程持有；
    /// 放线程局部里，供 `show_notice` 每次成功时重启，警告/错误时停掉。
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
    ui.set_target_mode(ui_enums::target_bar_mode(snapshot.mode));
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

    // 渲染后端选择（细节注释见旧实现）：SLINT_BACKEND > gpu_rendering > 软件渲染。
    if std::env::var_os("SLINT_BACKEND").is_none() {
        let backend = if settings.borrow().gpu_rendering {
            "winit-femtovg"
        } else {
            "winit-software"
        };
        let _ = slint::BackendSelector::new()
            .backend_name(backend.into())
            .with_winit_window_attributes_hook(|attrs| attrs.with_transparent(false))
            .select();
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
    {
        let ui = app.as_weak();
        let paste_asset = paste_asset.clone();
        let single_click = single_click.clone();
        app.on_tile_clicked(move |id| {
            if !single_click.get() {
                return;
            }
            let ui = ui.unwrap();
            paste_asset(&ui, id);
        });
    }

    // 过滤面板 v1：全部(-1)/分类(0..) → set_filter 并回到顶。记住当前分类过滤，
    // 供清空检索时回落。选择分类会清掉检索框。
    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let thumbs = real_resolver.clone();
        let filter_categories = filter_categories.clone();
        let current_filter = current_filter.clone();
        let filter_label = filter_label.clone();
        let grid = grid.clone();
        app.on_filter_selected(move |cat| {
            let ui = ui.unwrap();
            let filter = if cat < 0 {
                Filter::All
            } else {
                Filter::InCategory(CategoryId(cat as u32))
            };
            let label: slint::SharedString = if cat < 0 {
                "全部".into()
            } else {
                let categories = filter_categories.borrow();
                categories
                    .get((cat as usize) + 1)
                    .cloned()
                    .unwrap_or_else(|| format!("分类{cat}"))
                    .into()
            };
            *current_filter.borrow_mut() = filter.clone();
            *filter_label.borrow_mut() = label.clone();
            ui.set_search_text("".into());
            ui.set_selected_category(cat);
            {
                let mut guard = vm.borrow_mut();
                guard.set_filter(&filter);
            }
            ui.set_content_y(0.0);
            ui.set_filter_label(label);
            sync_counts(&ui, vm.borrow().total(), thumbs.borrow().is_some());
            grid.sync();
        });
    }

    // 检索：统一走 SearchProvider 门面（分类/标签名 ∪ 文件名；空查询回落当前过滤）。
    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let resolver = real_resolver.clone();
        let current_filter = current_filter.clone();
        let filter_label = filter_label.clone();
        let grid = grid.clone();
        app.on_search_changed(move |query| {
            let ui = ui.unwrap();
            let query = query.to_string();
            let filter = if query.trim().is_empty() {
                current_filter.borrow().clone()
            } else {
                let base = current_filter.borrow().clone();
                match resolver.borrow().as_ref() {
                    Some(r) => FacetSearchProvider { facets: r.facets() }
                        .search(&query, &base)
                        .unwrap_or(base),
                    None => base,
                }
            };
            // 检索态清掉侧栏分类高亮（-2），避免与「全部/某分类」的选中状态冲突；
            if query.trim().is_empty() {
                ui.set_selected_category(match &filter {
                    Filter::InCategory(id) => id.0 as i32,
                    _ => -1,
                });
                ui.set_filter_label(filter_label.borrow().clone());
            } else {
                ui.set_selected_category(-2);
                ui.set_filter_label(format!("搜索「{}」", query.trim()).into());
            }
            {
                let mut guard = vm.borrow_mut();
                guard.set_filter(&filter);
            }
            ui.set_content_y(0.0);
            sync_counts(&ui, vm.borrow().total(), resolver.borrow().is_some());
            grid.sync();
        });
    }

    // 设置面板开合。
    {
        let ui = app.as_weak();
        app.on_settings_toggled(move || {
            let ui = ui.unwrap();
            ui.set_settings_open(!ui.get_settings_open());
        });
    }

    // 导入菜单（左下角单入口弹层）开合。
    {
        let ui = app.as_weak();
        app.on_import_menu_toggled(move || {
            let ui = ui.unwrap();
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
        app.on_overlay_dismissed(move || {
            let ui = ui.unwrap();
            ui.set_settings_open(false);
            ui.set_import_menu_open(false);
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

    // 导入/派生在途标记：清空库在有任务时拒绝执行，避免删到正在写入的文件。
    let importing = Arc::new(AtomicBool::new(false));

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
        let library_root = library_root.clone();
        let routing = routing.clone();
        let importing = importing.clone();
        let import_settings = settings.clone();
        app.on_import_emo_requested(move || {
            let ui = ui.unwrap();
            // 菜单项已选中：先收起弹层，再弹原生文件对话框。
            ui.set_import_menu_open(false);
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

            let root = library_root
                .clone()
                .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());

            spawn_import_pipeline(
                ui.as_weak(),
                package.to_string_lossy().into_owned(),
                root,
                importing.clone(),
                import_settings.borrow().fast_import_mode,
            );
        });
    }

    // 导入素材：原生文件夹选择器（Win32 IFileOpenDialog，消除 3 秒 PowerShell 冷启动）
    // → sample-library 后台导入 → derive-thumbs 后台派生缩略图（ChildTaskRunner 编排）。
    {
        let ui = app.as_weak();
        let library_root = library_root.clone();
        let routing = routing.clone();
        let importing = importing.clone();
        let import_settings = settings.clone();
        app.on_import_requested(move || {
            let ui = ui.unwrap();
            // 菜单项已选中：先收起弹层，再弹原生文件夹对话框。
            ui.set_import_menu_open(false);
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
                Ok(Some(path)) => path.to_string_lossy().to_string(),
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

            // 库根目录落定后交给共享管线；目录创建/权限问题在管线内前置校验。
            let root = library_root
                .clone()
                .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned());

            let fast_mode = import_settings.borrow().fast_import_mode;
            spawn_import_pipeline(ui.as_weak(), dir, root, importing.clone(), fast_mode);
        });
    }

    grid.sync();
    // GL 驱动缺失的自愈（实测：CI runner/远程桌面等无 GL 环境，femtovg 在事件循环
    // 启动时报 "Failed to initialize OpenGL driver: Could not locate glCreateShader
    // symbol" 直接退出）。winit 一进程只允许一个事件循环，进程内换后端不可行，
    // 唯一出路是换档重启：SLINT_BACKEND=winit-software + 哨兵防无限循环。这是
    // 上文渲染后端选择注释「gpu_rendering > 软件渲染」回退链的最后一级。
    if let Err(e) = app.run() {
        let can_fallback = settings.borrow().gpu_rendering
            && std::env::var_os("SLINT_BACKEND").is_none()
            && std::env::var_os("ASSETDECK_RENDER_FALLBACK").is_none();
        if !can_fallback {
            panic!("Slint 事件循环异常退出: {e}");
        }
        logging::warn!("GL 渲染初始化失败({e})，以软件渲染档重启自身");
        let exe = std::env::current_exe().expect("取当前 exe 失败");
        let respawn = std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env("SLINT_BACKEND", "winit-software")
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
    source: String,
    root: String,
    importing: Arc<AtomicBool>,
    fast_mode: bool,
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
        format!("开始导入：{}", source),
    );
    logging::info!("开始导入 source={source} root={root} fast_mode={fast_mode}");

    let weak = ui.clone();
    let root_thread = root;
    let derive = helper_exe("derive-thumbs.exe");
    let worker = helper_exe("decode-worker.exe");
    let weak_progress = weak.clone();
    importing.store(true, Ordering::SeqCst);
    let importing_phase1 = Arc::clone(&importing);
    let importing_phase2 = Arc::clone(&importing);

    // D37/D38：把档位与日志约定传给子进程（sample-library / derive-thumbs /
    // decode-worker 各自 init_from_env 读取）。
    let mode_arg: &'static str = if fast_mode { "fast" } else { "background" };
    let logs_dir_arg = logging::logs_dir();
    let log_level_arg = logging::current_level().as_str();

    // 阶段一：sample-library 导入（解码/pHash/拷贝/落库全在子进程，D11）。
    // NOTICE 行 = 整体成功但个别素材失败（伪装扩展名/损坏图），弹警示避免
    // 「部分失败被当成全成」的静默丢素材。
    let weak_notice = weak.clone();
    let mut phase1_task = ChildTask::new(
        helper,
        vec![
            source,
            root_thread.clone(),
            "--mode".into(),
            mode_arg.into(),
        ],
    );
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
                    mode_arg.into(),
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
