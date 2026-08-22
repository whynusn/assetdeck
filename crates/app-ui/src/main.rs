//! 薄壳：VM 装配 + Slint 回调桥接。业务逻辑全在 ui-viewmodels，本文件不做计算密集工作。

slint::include_modules!();

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ModelRc, VecModel};
use ui_viewmodels::grid_vm::LibraryGridVm;
use ui_viewmodels::{
    Asset, AssetId, CategoryId, FacetIndex, Filter, SortDirection, SortField, SortSpec, Sorter,
    TagId,
};

/// 演示数据规模：无真实库环境时的合成资产（M7 接 library 后替换）。
const DEMO_COUNT: u32 = 500;
/// 单次请求物化的可见条数（VM 内部再叠加过扫描）。
const VIEW_COUNT: usize = 24;
/// 网格容器几何：与默认窗宽匹配的演示常量（DPI/窗口自适应属手工验收清单）。
const CONTAINER_WIDTH: f32 = 964.0;
const COLUMNS: u32 = 6;
const GAP: f32 = 12.0;

/// 占位缩略图色块调色板（ARGB）：M7 换 worker 真图纹理。
const PALETTE: [u32; 8] = [
    0xFF4C6EF5, 0xFFF76707, 0xFF20C997, 0xFFBE4BDB, 0xFF228BE6, 0xFFFFA8A8, 0xFF82C91E, 0xFFFAB005,
];

fn demo_index() -> FacetIndex {
    let mut idx = FacetIndex::new();
    for i in 0..DEMO_COUNT {
        idx.insert(&Asset {
            id: AssetId(i),
            name: format!("演示资产-{i:04}"),
            category: Some(CategoryId(i % 5)),
            tags: vec![TagId(i % 7)],
            created_at: i as i64,
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

/// content_y（视口顶部像素）→ 可见首项索引：对预计算 rect 表二分（rect.y 单调不减）。
fn first_visible_index(vm: &LibraryGridVm, content_y: f32) -> usize {
    let total = vm.total();
    if total == 0 {
        return 0;
    }
    let (mut lo, mut hi) = (0usize, total - 1);
    while lo < hi {
        let mid = lo + (hi - lo) / 2 + 1;
        if vm.rect_of(mid).y <= content_y {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// 把当前窗口切片写入 tiles 模型（哑渲染的数据源）。
fn push_tiles(tiles_model: &VecModel<TileData>, vm: &LibraryGridVm, first: usize) {
    let end = (first + VIEW_COUNT).min(vm.total());
    let rows: Vec<TileData> = (first..end)
        .map(|i| {
            let r = vm.rect_of(i);
            let id = vm.id_at(i);
            TileData {
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                asset_id: id.0 as i32,
                label: format!("#{}", id.0).into(),
                color: slint::Color::from_argb_encoded(PALETTE[(id.0 % 8) as usize]),
            }
        })
        .collect();
    tiles_model.set_vec(rows);
}

/// 读滚动位置 → ensure_window → 刷新 tiles（滚动/过滤后的唯一同步入口）。
fn sync_window(ui: &AppWindow, tiles_model: &VecModel<TileData>, vm: &mut LibraryGridVm) {
    let first = first_visible_index(vm, ui.get_content_y());
    vm.ensure_window(first, VIEW_COUNT);
    push_tiles(tiles_model, vm, first);
    // 内容总高必须回填，否则 Flickable 无溢出空间、滚动路径整体失效
    ui.set_content_height(vm.content_height());
}

fn main() {
    // --bench 内存守卫模式：不开窗、不进 Slint 事件循环（design 契约）。
    if let Some(bench) = parse_bench_args(&std::env::args().skip(1).collect::<Vec<_>>()) {
        std::process::exit(run_bench(&bench));
    }

    let app = AppWindow::new().expect("AppWindow 创建失败");

    // VM 装配：合成演示数据；缩略图 provider 待 M7 接 worker 池（现为色块占位）。
    let mut vm = LibraryGridVm::new(demo_index(), recent_first_sorter(), 256);
    vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
    let vm = Rc::new(RefCell::new(vm));

    let tiles_model: Rc<VecModel<TileData>> = Rc::new(VecModel::default());
    app.set_tiles(ModelRc::from(tiles_model.clone()));
    app.set_total_text(format!("共 {} 项", vm.borrow().total()).into());

    // 滚动：content_y → 可见窗口物化 → 刷新瓦片
    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let tiles_model = tiles_model.clone();
        app.on_scroll_changed(move |_content_y| {
            // 位置经 get_content_y 统一读取，避免双数据源
            let ui = ui.unwrap();
            sync_window(&ui, &tiles_model, &mut vm.borrow_mut());
        });
    }

    // 双击：语义止步于 OpenAsset 事件（auto-send 属 M6 粘贴管线且默认关）
    {
        let vm = vm.clone();
        app.on_double_clicked(move |id| {
            let mut vm = vm.borrow_mut();
            vm.double_click(AssetId(id.max(0) as u32));
            for event in vm.take_events() {
                println!("VM 事件: {event:?}");
            }
        });
    }

    // 过滤面板 v1：全部(-1)/分类(0..4) → set_filter 并回到顶部
    {
        let ui = app.as_weak();
        let vm = vm.clone();
        let tiles_model = tiles_model.clone();
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
                format!("分类{cat}").into()
            };
            {
                let mut guard = vm.borrow_mut();
                guard.set_filter(&filter);
            }
            ui.set_content_y(0.0);
            ui.set_filter_label(label);
            ui.set_total_text(format!("共 {} 项", vm.borrow().total()).into());
            sync_window(&ui, &tiles_model, &mut vm.borrow_mut());
        });
    }

    sync_window(&app, &tiles_model, &mut vm.borrow_mut());
    app.run().expect("Slint 事件循环异常退出");
}

// ---------------------------------------------------------------------------
// --bench 内存守卫模式（M7，design.md 契约）
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
    // 缺值不得静默回落 GUI（跨层守则：错误绝不静默切换模式）
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
///
/// 数据来源（design.md 契约）：`Store::open(root/meta.db) → 读全量 AssetMeta →
/// 建 FacetIndex(uuid→顺序 AssetId 映射)`。app-ui 白名单只有 ui-viewmodels +
/// slint，故 Store 门面访问收拢在 [`ui_viewmodels::load_library_catalog`]。
/// 不创建窗口——内存守卫测的是 VM 数字层（位图 + 全量 Rect 表），诚实标注
/// 「不含真实渲染器驻留」；真实 GUI 空闲由 harness idle 模式覆盖。
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

    // 脚本化浏览：first 从 0 步进 BENCH_BROWSE_STEP 跳一窗，至尾再回首
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

    // hold 静置：父进程在此窗口内采 WorkingSet
    std::thread::sleep(Duration::from_millis(args.hold_ms));

    // 单行 JSON 结果（harness 只看退出码，不解析内部状态——error-handling spec）
    let line = format!(
        "{{\"browse_done\":true,\"total\":{total},\"browse_ms\":{browse_ms},\
         \"hold_ms\":{},\"resident_thumbs\":{}}}",
        args.hold_ms,
        vm.visible_cache_ids().len()
    );
    println!("{line}");
    let _ = std::io::stdout().flush();
    0
}
