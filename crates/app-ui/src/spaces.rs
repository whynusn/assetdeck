//! 共享空间装配（D90 P1 单机半场）：注册表持久化、作用域切换与素材进出。
//!
//! 作用域切换 = 库重载的同构复用（`load_real_library(root)` → 换 vm/resolver
//! → 侧栏分类重铺 → grid.sync()）：空间视图就是「换根的库视图」，浏览/搜索/
//! 双击上框/多选整套机械零改动地作用于活动根。写路径（`spawn_lib_cmd`）经
//! [`active_root`] 按作用域寻址；导入（拖入/粘贴）在空间视图里 = 向空间投放
//! 素材（成员贡献的自然入口）。
//!
//! P1 不碰网络：添加/移出只动本地空间库；P2 在 [`SpaceVm`] 上挂 op log
//! 同步状态，这里的作用域机制不变。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use slint::{Model, Weak};
use ui_viewmodels::uuid::Uuid;
use ui_viewmodels::{
    CategoryRows, Filter, LibraryGridVm, RealAssetResolver, SpaceAssetMeta, SpaceBookEntry,
    SpaceBookState, SpaceMessage, SpaceMeta, SpaceOp, SpaceOpKind, SpaceVm, TargetNoticeTone,
    MAX_FETCH_ITEMS,
};

use crate::thumbs::{GridCtx, ThumbCache};
use crate::{
    apply_categories, default_library_root, recent_first_sorter, spawn_import_pipeline,
    sync_counts, AppWindow, SpaceRowData, COLUMNS, CONTAINER_WIDTH, GAP,
};

/// 活动空间作用域（None = 主库视图）。
#[derive(Debug, Clone)]
pub struct SpaceScope {
    pub id: Uuid,
    pub root: PathBuf,
}

/// 作用域切换所需的壳层共享件（setup 装配一次，thread_local 取回）。
pub struct SpaceHost {
    pub ui: Weak<AppWindow>,
    pub vm: Rc<RefCell<LibraryGridVm>>,
    pub resolver: Rc<RefCell<Option<RealAssetResolver>>>,
    pub grid: Rc<GridCtx>,
    pub filter_categories: Rc<RefCell<Vec<String>>>,
    pub filter_category_ids: Rc<RefCell<CategoryRows>>,
    pub current_filter: Rc<RefCell<Filter>>,
    pub filter_label: Rc<RefCell<slint::SharedString>>,
    pub thumb_cache: Rc<RefCell<ThumbCache>>,
    /// 导入在途旗标（staged 空间投递走导入管线时复用主装配的同一把锁）。
    pub importing: Arc<AtomicBool>,
}

thread_local! {
    static SPACES: RefCell<SpaceVm> = RefCell::new(SpaceVm::new());
    static SPACES_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static SPACES_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static ACTIVE: RefCell<Option<SpaceScope>> = const { RefCell::new(None) };
    static HOST: RefCell<Option<Rc<SpaceHost>>> = const { RefCell::new(None) };
}

/// 启动装载：读 spaces.json（损坏/缺失按空册起步）+ 记住目录约定。
pub fn init(spaces_json: &Path, spaces_dir: PathBuf) {
    let registry = std::fs::read_to_string(spaces_json)
        .ok()
        .and_then(|text| ui_viewmodels::SpaceRegistry::from_json(&text))
        .unwrap_or_default();
    SPACES_PATH.with(|slot| *slot.borrow_mut() = Some(spaces_json.to_path_buf()));
    // from_json 已含校验过的行；直接注入（VM 校验在动作入口把关）。
    SPACES.with(|slot| {
        *slot.borrow_mut() = SpaceVm::from_registry(registry);
    });
    SPACES_DIR.with(|slot| {
        *slot.borrow_mut() = Some(spaces_dir);
    });
}

/// worker READY 回填设备身份（变化才落盘）并刷新侧栏行（身份就绪态可显示）。
/// 身份首落 = 服务册首推（READY 早于一切对端连接，此刻推册最及时）。
pub fn handle_worker_ready(self_id: &str, self_name: &str) {
    let changed = SPACES.with(|vm| vm.borrow_mut().set_identity(self_id, self_name));
    if changed {
        persist();
        push_space_state();
    }
}

/// dirty 则原子落盘（事件驱动，零定时器）。失败只留日志：下次动作再写，
/// spaces.json 丢一次不影响内存态正确性。
pub fn persist() {
    let Some(path) = SPACES_PATH.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let json = SPACES.with(|vm| {
        if vm.borrow_mut().take_dirty() {
            Some(vm.borrow().to_json())
        } else {
            None
        }
    });
    if let Some(json) = json {
        if let Err(error) = crate::atomic_write(&path, &json) {
            logging::warn!("spaces.json 写入失败（下次动作重试）: {error}");
        }
    }
}

/// 全部空间（侧栏/弹窗行数据源）。
pub fn spaces() -> Vec<SpaceMeta> {
    SPACES.with(|vm| vm.borrow().spaces().to_vec())
}

pub fn space_by_id(id: Uuid) -> Option<SpaceMeta> {
    SPACES.with(|vm| vm.borrow().registry().find(id).cloned())
}

pub fn active() -> Option<SpaceScope> {
    ACTIVE.with(|slot| slot.borrow().clone())
}

pub fn in_space_view() -> bool {
    ACTIVE.with(|slot| slot.borrow().is_some())
}

/// 空间库根 = `<spaces 目录>/<uuid 连字符>/`（目录约定见 init）。
pub fn space_root_of(id: Uuid) -> Option<PathBuf> {
    SPACES_DIR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|dir| dir.join(SpaceVm::space_root_name(id)))
    })
}

/// 库写子命令/导入的目标根：空间视图 = 空间根，否则主库根（含默认回退）。
/// CrudCtx 与 ImportFlow 的一切寻址都过这里，作用域切换对写路径透明。
pub fn active_root(library_root: Option<&str>) -> String {
    if let Some(scope) = active() {
        return scope.root.to_string_lossy().into_owned();
    }
    library_root
        .map(str::to_string)
        .unwrap_or_else(|| default_library_root().to_string_lossy().into_owned())
}

/// 装配 SpaceHost（setup 在 vm/grid 就绪后调用一次）。
pub fn install_host(host: Rc<SpaceHost>) {
    HOST.with(|slot| *slot.borrow_mut() = Some(host));
}

fn host() -> Option<Rc<SpaceHost>> {
    HOST.with(|slot| slot.borrow().clone())
}

/// 侧栏/弹窗空间行同步：行模型 + 选中态 + 作用域旗标一次摆齐。
pub fn sync_rows(ui: &AppWindow) {
    let list = spaces();
    let selected = active().map(|scope| {
        list.iter()
            .position(|s| s.id == scope.id)
            .map(|i| i as i32)
            .unwrap_or(-1)
    });
    let rows: Vec<SpaceRowData> = list
        .iter()
        .map(|space| SpaceRowData {
            id: space.id.to_string().into(),
            name: space.name.clone().into(),
            sub_label: SpaceVm::member_label(space).into(),
        })
        .collect();
    ui.set_space_rows(slint::ModelRc::new(slint::VecModel::from(rows.clone())));
    ui.set_space_add_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
    ui.set_selected_space(selected.unwrap_or(-1));
    ui.set_in_space_view(in_space_view());
    ui.set_space_ctl_ready(SPACES.with(|vm| vm.borrow().is_ready()));
    refresh_pending(ui);
    // D92：空间册/作用域变更汇流点——投影随手刷新（enter_space 途中
    // in_space_view 已置真，这里只清侧栏态；主视图侧整窗重排亮新角标）。
    refresh_share_projection(true);
}

/// 进入空间视图（或重进刷新）：换根装载 = 库重载同构。
pub fn enter_space(id: Uuid) {
    let Some(scope_host) = host() else { return };
    let Some(space) = space_by_id(id) else { return };
    let Some(root) = space_root_of(id) else {
        return;
    };
    let Some(ui) = scope_host.ui.upgrade() else {
        return;
    };
    match load_space_catalog(&root) {
        Ok((index, resolver)) => {
            apply_categories(
                &ui,
                &scope_host.filter_categories,
                &scope_host.filter_category_ids,
                resolver.categories(),
            );
            let mut new_vm = LibraryGridVm::new(index, recent_first_sorter(), 256);
            new_vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
            if let Ok(aspects) = resolver.aspects() {
                new_vm.set_aspects(aspects);
            }
            *scope_host.vm.borrow_mut() = new_vm;
            *scope_host.resolver.borrow_mut() = Some(resolver);
            scope_host.thumb_cache.borrow_mut().clear();
            ACTIVE.with(|slot| {
                *slot.borrow_mut() = Some(SpaceScope {
                    id,
                    root: root.clone(),
                })
            });
            // 进入即对账：库里有、op log 没记的素材补 Add op（导入半途/
            // 崩溃残留由下一次进入收敛）。
            emit_missing_add_ops(id);
            // 作用域切换清过滤器：主库的分类 id 在空间分面表里无意义，
            // 残留旧过滤器会让后续 apply_filter/库写收尾把视图钉到空集。
            *scope_host.current_filter.borrow_mut() = Filter::All;
            *scope_host.filter_label.borrow_mut() = space.name.clone().into();
            ui.set_search_text("".into());
            ui.set_filter_label(space.name.clone().into());
            ui.set_selected_category(-1);
            ui.set_content_y(0.0);
            sync_counts(&ui, scope_host.vm.borrow().total(), true);
            ui.set_trash_count(0);
            sync_rows(&ui);
            scope_host.grid.sync();
        }
        Err(error) => {
            crate::show_notice(
                &ui,
                TargetNoticeTone::Warning,
                format!("打开共享空间「{}」失败：{error}", space.name),
            );
        }
    }
}

/// 退出空间视图回到主库。
pub fn exit_space() {
    let Some(scope_host) = host() else { return };
    let Some(ui) = scope_host.ui.upgrade() else {
        return;
    };
    ACTIVE.with(|slot| *slot.borrow_mut() = None);
    let Some(root) = ui_viewmodels_root() else {
        sync_rows(&ui);
        return;
    };
    match ui_viewmodels::load_real_library(&root) {
        Ok((index, resolver)) => {
            apply_categories(
                &ui,
                &scope_host.filter_categories,
                &scope_host.filter_category_ids,
                resolver.categories(),
            );
            let mut new_vm = LibraryGridVm::new(index, recent_first_sorter(), 256);
            new_vm.set_layout_params(CONTAINER_WIDTH, COLUMNS, GAP);
            if let Ok(aspects) = resolver.aspects() {
                new_vm.set_aspects(aspects);
            }
            *scope_host.vm.borrow_mut() = new_vm;
            *scope_host.resolver.borrow_mut() = Some(resolver);
            scope_host.thumb_cache.borrow_mut().clear();
            *scope_host.current_filter.borrow_mut() = Filter::All;
            *scope_host.filter_label.borrow_mut() = "全部".into();
            ui.set_search_text("".into());
            ui.set_selected_category(-1);
            ui.set_filter_label("全部".into());
            ui.set_content_y(0.0);
            sync_counts(&ui, scope_host.vm.borrow().total(), true);
            ui.set_trash_count(scope_host.vm.borrow().trash_count() as i32);
            sync_rows(&ui);
            scope_host.grid.sync();
        }
        Err(error) => {
            crate::show_notice(
                &ui,
                TargetNoticeTone::Warning,
                format!("回到我的素材失败：{error}"),
            );
        }
    }
}

// 主库根（exit/reload 用）：SpaceHost 不持主库根（避免双真相），从壳层
// thread_local 之外的唯一来源——CrudCtx 装配时同步登记。
thread_local! {
    static LIBRARY_ROOT: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn set_library_root(root: Option<String>) {
    LIBRARY_ROOT.with(|slot| *slot.borrow_mut() = root);
}

fn ui_viewmodels_root() -> Option<PathBuf> {
    LIBRARY_ROOT.with(|slot| {
        slot.borrow()
            .clone()
            .map(PathBuf::from)
            .or_else(|| Some(default_library_root()))
    })
}

/// 空间库装载（首次打开时 meta.db 可能不存在：`ensure_library_skeleton`
/// 建骨架——无并发写者的 schema 初始化，不违「单写者」纪律的意图；此后
/// 的导入/库写仍一律走子进程）。
fn load_space_catalog(
    root: &Path,
) -> Result<(ui_viewmodels::FacetIndex, RealAssetResolver), ui_viewmodels::CatalogError> {
    if !root.join("meta.db").is_file() {
        std::fs::create_dir_all(root)?;
        ui_viewmodels::ensure_library_skeleton(root)?;
    }
    ui_viewmodels::load_real_library(root)
}

/// 创建空间：注册表行 + 目录骨架 + 直接进入（创建即到达，少一次点击）。
pub fn create_space(name: &str) {
    let Some(ui_host) = host() else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let created = SPACES.with(|vm| vm.borrow_mut().create_space(name, now));
    match created {
        Ok(id) => {
            if let Some(root) = space_root_of(id) {
                if let Err(error) = std::fs::create_dir_all(&root) {
                    if let Some(ui) = ui_host.ui.upgrade() {
                        crate::show_notice(
                            &ui,
                            TargetNoticeTone::Error,
                            format!("创建空间目录失败：{error}"),
                        );
                    }
                    SPACES.with(|vm| {
                        vm.borrow_mut().leave_space(id);
                    });
                    persist();
                    return;
                }
            }
            persist();
            if let Some(ui) = ui_host.ui.upgrade() {
                sync_rows(&ui);
                crate::show_notice(
                    &ui,
                    TargetNoticeTone::Success,
                    format!("共享空间「{name}」已创建，把素材右键添加进来，或直接拖入"),
                );
            }
            enter_space(id);
        }
        Err(error) => {
            if let Some(ui) = ui_host.ui.upgrade() {
                ui.set_space_ctl_error(error.to_string().into());
            }
        }
    }
}

/// 退出并删除空间（P1：本机单成员空间，退出 = 本地副本整目录删除）。
pub fn leave_space(id: Uuid) {
    let Some(ui_host) = host() else { return };
    let was_active = active().is_some_and(|scope| scope.id == id);
    if was_active {
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
    }
    let left = SPACES.with(|vm| vm.borrow_mut().leave_space(id));
    if left {
        if let Some(root) = space_root_of(id) {
            let _ = std::fs::remove_dir_all(root);
        }
        persist();
    }
    if let Some(ui) = ui_host.ui.upgrade() {
        if was_active {
            exit_space();
        } else {
            sync_rows(&ui);
        }
        crate::show_notice(
            &ui,
            TargetNoticeTone::Success,
            "已退出共享空间，本地副本已删除".to_string(),
        );
    }
}

/// 当前邀请码（空间主侧复制用）。
pub fn invite_code(id: Uuid) -> Option<String> {
    SPACES.with(|vm| vm.borrow().invite_code(id))
}

/// 重置邀请码（旧码即刻失效）。
pub fn rotate_invite(id: Uuid) -> Option<String> {
    let rotated = SPACES.with(|vm| vm.borrow_mut().rotate_secret(id));
    if rotated.is_some() {
        persist();
    }
    rotated
}

/// 把一组文件路径添加进空间（右键「添加到共享空间」/多选操作条）：
/// 清单子进程导入 = 既有管线换根，落空间的「待分类」，空间主归类。
pub fn add_paths_to_space(id: Uuid, paths: Vec<PathBuf>, importing: Arc<AtomicBool>) {
    let Some(space) = space_by_id(id) else { return };
    let Some(root) = space_root_of(id) else {
        return;
    };
    let Some(ui_host) = host() else { return };
    let Some(ui) = ui_host.ui.upgrade() else {
        return;
    };
    if paths.is_empty() {
        return;
    }
    if importing.load(Ordering::SeqCst) {
        crate::show_notice(
            &ui,
            TargetNoticeTone::Warning,
            "正在导入中，请等进度条结束后再操作".to_string(),
        );
        return;
    }
    let Some(list) = write_import_list(&paths) else {
        crate::show_notice(
            &ui,
            TargetNoticeTone::Error,
            "无法写入导入清单（临时目录不可用）".to_string(),
        );
        return;
    };
    let args = vec![
        "--import-paths".to_string(),
        list.to_string_lossy().into_owned(),
        "--library".to_string(),
        root.to_string_lossy().into_owned(),
        // 与下方 spawn_import_pipeline 的 fast_import_mode=false 配对：
        // 档位只能是 fast|background（sample-library 的 inbox 是清单归类指令，
        // 不是 --mode 值——首版传 "inbox" 令子进程直接 exit=1）。
        "--mode".to_string(),
        "background".to_string(),
    ];
    // 进度反馈交给导入管线的进度条（Info 级瞬时信息不弹窗，落日志）。
    logging::info!("向空间「{}」加入 {} 项素材", space.name, paths.len());
    IMPORT_TARGET.with(|slot| *slot.borrow_mut() = Some(root.clone()));
    spawn_import_pipeline(
        ui_host.ui.clone(),
        args,
        root.to_string_lossy().into_owned(),
        importing,
        format!("加入「{}」", space.name),
        false,
        None,
    );
}

/// 空间素材「保存到我的素材」：同一管线的反向（空间文件 → 主库根）。
pub fn save_paths_to_library(paths: Vec<PathBuf>, importing: Arc<AtomicBool>) {
    let Some(ui_host) = host() else { return };
    let Some(ui) = ui_host.ui.upgrade() else {
        return;
    };
    let Some(root) = ui_viewmodels_root() else {
        return;
    };
    if paths.is_empty() {
        return;
    }
    if importing.load(Ordering::SeqCst) {
        crate::show_notice(
            &ui,
            TargetNoticeTone::Warning,
            "正在导入中，请等进度条结束后再操作".to_string(),
        );
        return;
    }
    let Some(list) = write_import_list(&paths) else {
        crate::show_notice(
            &ui,
            TargetNoticeTone::Error,
            "无法写入导入清单（临时目录不可用）".to_string(),
        );
        return;
    };
    let args = vec![
        "--import-paths".to_string(),
        list.to_string_lossy().into_owned(),
        "--library".to_string(),
        root.to_string_lossy().into_owned(),
        // 同上：background 与 fast_import_mode=false 配对。
        "--mode".to_string(),
        "background".to_string(),
    ];
    // 进度反馈交给导入管线的进度条（Info 级瞬时信息不弹窗，落日志）。
    logging::info!("向我的素材保存 {} 项（来自共享空间）", paths.len());
    spawn_import_pipeline(
        ui_host.ui.clone(),
        args,
        root.to_string_lossy().into_owned(),
        importing,
        "保存到我的素材".to_string(),
        false,
        None,
    );
}

fn write_import_list(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut lines = String::new();
    for path in paths {
        lines.push_str(&format!("f\tinbox\t{}\n", path.display()));
    }
    let list = std::env::temp_dir().join(format!(
        "assetdeck_space_add_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&list, lines).ok()?;
    Some(list)
}

// —— D92 共享可见性投影层 ——
//
// 「这个素材/分类被共享了吗」是派生视图，不当状态存：空间 op log（Add −
// Remove，按内容哈希）∩ 主库活动资产流 = 每分类 (总数, 已共享数) + 共享
// uuid 集。投影缓存在本模块，瓦片角标（build_rows 每窗格现查）与分类侧栏
// 共用一份；刷新点 = 空间变更汇流（sync_rows 尾）/ 空间导入收尾 / 主库
// 装载（apply_categories 尾）。无空间时投影恒空、全链路零成本；空间视图
// 内不显示共享标识（空间里的素材按定义全员可见，标了全是噪声）。

thread_local! {
    static SHARE_PROJECTION: RefCell<ui_viewmodels::ShareProjection> =
        RefCell::new(ui_viewmodels::ShareProjection::default());
}

/// 全空间成员内容哈希集：逐空间折叠 op log——Add 记哈希，Remove 经
/// op uuid → 哈希映射抵扣（与 apply 侧同款两段解析，对跨设备 uuid 命名
/// 空间无关）。op log 只有小册子量级，现算不缓存。
fn member_hashes() -> std::collections::HashSet<Vec<u8>> {
    let mut out = std::collections::HashSet::new();
    for meta in spaces() {
        let ops = load_ops(meta.id);
        let mut hash_by_op_uuid = std::collections::HashMap::new();
        for op in &ops {
            if let SpaceOpKind::Add { asset } = &op.op {
                hash_by_op_uuid.insert(asset.uuid, asset.content_hash.clone());
                out.insert(asset.content_hash.clone());
            }
        }
        for op in &ops {
            if let SpaceOpKind::Remove { asset_uuid } = &op.op {
                if let Some(hash) = hash_by_op_uuid.get(asset_uuid) {
                    out.remove(hash);
                }
            }
        }
    }
    out
}

/// (总数, 已共享数) → 侧栏态：0 无 / 1 部分 / 2 全。空分类与零共享同 0
///（「没共享」不需要视觉存在）。
fn share_state_of(total: u32, shared: u32) -> i32 {
    if shared == 0 || total == 0 {
        0
    } else if shared >= total {
        2
    } else {
        1
    }
}

/// 分类共享态模型铺设：下标 0 =「全部」= 全库聚合，其余按 ui.categories
/// 同名对齐（投影分类名与 facets 同源，NULL → 收件箱常量）。空间视图恒空
/// 模型（slint 侧按缺省 0 渲染 = 无标识）。
fn apply_share_states(ui: &AppWindow) {
    let states: Vec<i32> = if in_space_view() {
        Vec::new()
    } else {
        SHARE_PROJECTION.with(|slot| {
            let projection = slot.borrow();
            let categories = ui.get_categories();
            let mut states = Vec::with_capacity(categories.row_count());
            for i in 0..categories.row_count() {
                let state = if i == 0 {
                    let (total, shared) = projection
                        .by_category
                        .values()
                        .fold((0u32, 0u32), |(t, s), (ct, cs)| (t + ct, s + cs));
                    share_state_of(total, shared)
                } else {
                    categories
                        .row_data(i)
                        .map(|name| {
                            projection
                                .by_category
                                .get(name.as_str())
                                .map(|(total, shared)| share_state_of(*total, *shared))
                                .unwrap_or(0)
                        })
                        .unwrap_or(0)
                };
                states.push(state);
            }
            states
        })
    };
    ui.set_category_share_states(slint::ModelRc::new(slint::VecModel::from(states)));
}

/// 投影刷新：重算缓存 → 铺分类共享态 → `sync_grid` 时（主视图）整窗重排
/// 让瓦片角标取新集。库装载路径传 false（装载流程随后自己 sync，重复整排
/// 是浪费）；空间变更/导入收尾传 true。
pub fn refresh_share_projection(sync_grid: bool) {
    let Some(scope_host) = host() else { return };
    let Some(ui) = scope_host.ui.upgrade() else {
        return;
    };
    let projection = if in_space_view() {
        // 空间视图不重算主库投影：缓存语义恒为主库侧，清空只是为了瓦片
        // 角标在空间视图内不误亮（空间 uuid 与主库 uuid 不同源，本就不相交）。
        ui_viewmodels::ShareProjection::default()
    } else {
        let members = member_hashes();
        match ui_viewmodels_root() {
            Some(root) => ui_viewmodels::share_projection(&root, &members),
            None => ui_viewmodels::ShareProjection::default(),
        }
    };
    SHARE_PROJECTION.with(|slot| *slot.borrow_mut() = projection);
    apply_share_states(&ui);
    if sync_grid && !in_space_view() {
        scope_host.grid.sync();
    }
}

/// 瓦片「共享中」现查集（D92）：D80-M1 控制面标记 ∪ 空间成员投影的主库
/// uuid 集。集合尺寸 = 实际共享条数，与库规模无关。
pub fn shared_uuids() -> std::collections::HashSet<Uuid> {
    let mut out: std::collections::HashSet<Uuid> = crate::SHARE_CONTROL.with(|control| {
        control
            .borrow()
            .shared_entries()
            .iter()
            .map(|e| e.asset_uuid)
            .collect()
    });
    SHARE_PROJECTION.with(|slot| {
        for uuid in slot.borrow().shared_uuids.iter() {
            if let Ok(uuid) = Uuid::parse_str(uuid) {
                out.insert(uuid);
            }
        }
    });
    out
}

// —— D90-P2 网格半场：op log 持久化、抗熵接线、贴码加入与字节索取 ——
//
// 同步语义：op log（每空间 ops.jsonl，(lamport, author) 全序）是空间的事实
// 源；本地库是它的物化视图。op 应用一律按 content_hash 解析本地行——各
// 成员库的 uuid 各自生成（导入产新 uuid），op 里的作者 uuid 只在 op 域内
// 寻址（Remove 的「添加者本人」判定查 op 域的 uuid→作者索引）。

thread_local! {
    /// 在线对端缓存（DEVICE 行）：fetch 选持有者 + 抗熵触发。
    static ONLINE_PEERS: RefCell<std::collections::HashMap<String, Vec<String>>> =
        RefCell::new(std::collections::HashMap::new());
    /// 本会话已发起过空间抗熵的对端（每对端一轮；worker 侧比对全量收敛，
    /// 断档自愈靠下次会话，不靠重复发起——零 Timer 轮询）。
    static SYNCED_PEERS: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());
    /// 已自动受理的空间投递批次（batch → space；STAGED 路由回空间根）。
    static SPACE_BATCHES: RefCell<std::collections::HashMap<Uuid, Uuid>> =
        RefCell::new(std::collections::HashMap::new());
    /// 大小闸（设置装载时回填）：(启用, MB 上限)。拍板②：默认 500MB 可调。
    static GATE: RefCell<(bool, u32)> = const { RefCell::new((true, 500)) };
    /// 挂起的字节索取。gated = 被大小闸挡下（等用户手动放行，对端上线
    /// 不自动重试）；否则 = 无人在线（对端上线自动续传）。
    static PENDING_FETCH: RefCell<Vec<PendingFetch>> = const { RefCell::new(Vec::new()) };
    /// 导入管线的目标根（主视图里向空间投放时活动根 ≠ 目标根，收尾回调
    /// 据此归位）。导入由 importing 旗标串行，单槽即可。
    static IMPORT_TARGET: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// 设置装载后回填大小闸。
pub fn set_gate(enabled: bool, mb: u32) {
    GATE.with(|slot| *slot.borrow_mut() = (enabled, mb));
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn notice(ui: &AppWindow, severity: ui_viewmodels::notifications::Severity, text: String) {
    crate::notifications::publish(ui, severity, text);
}

fn ops_path(id: Uuid) -> Option<PathBuf> {
    space_root_of(id).map(|root| root.join("ops.jsonl"))
}

/// 空间 op log（装载即排序去重；坏行跳过——log 是追加写，崩溃残留的半行
/// 不拖垮整册）。
pub fn load_ops(id: Uuid) -> Vec<SpaceOp> {
    let Some(path) = ops_path(id) else {
        return Vec::new();
    };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    ui_viewmodels::space_ops_from_lines(&text, id)
}

fn append_ops(id: Uuid, ops: &[SpaceOp]) {
    if ops.is_empty() {
        return;
    }
    let Some(path) = ops_path(id) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = ui_viewmodels::space_ops_to_lines(ops);
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut file) => {
            if let Err(error) = std::io::Write::write_all(&mut file, text.as_bytes()) {
                logging::warn!("空间 op log 追加失败: {error}");
            }
        }
        Err(error) => logging::warn!("空间 op log 打开失败: {error}"),
    }
}

/// 组装并整册推送空间服务册（meta + op log + 字节服务索引）。
/// 触发点：worker READY / SPACE_ADMITTED / op 应用 / 空间根导入完成。
pub fn push_space_state() {
    let mut book = SpaceBookState::default();
    for meta in spaces() {
        let Some(root) = space_root_of(meta.id) else {
            continue;
        };
        let ops = load_ops(meta.id);
        let serve = ui_viewmodels::content_paths(&root)
            .into_iter()
            .map(|(hash, _uuid, path)| ui_viewmodels::ServeItem {
                content_hash: hash,
                path: path.to_string_lossy().into_owned(),
            })
            .collect();
        book.set(meta.id, SpaceBookEntry { meta, ops, serve });
    }
    crate::share_write_line(&format!(
        "SPACE_STATE\t{}",
        ui_viewmodels::space_book_json(&book)
    ));
}

/// 空间根导入完成（导入管线收尾回调点名空间根时调用）：补记 Add op
/// （库行反推——哈希/uuid/文件名/大小都在行里）+ serve 索引重推。
pub fn on_space_import_finished(_active_root: &Path) {
    // 导入目标根以登记为准（主视图里向空间投放时，活动根 ≠ 目标根）。
    let target = IMPORT_TARGET.with(|slot| slot.borrow_mut().take());
    if let Some(root) = target {
        if let Some(id) = space_id_of_root(&root) {
            emit_missing_add_ops(id);
            push_space_state();
            // op log 长了（补 Add）：主库侧共享态与瓦片角标随之翻新。
            refresh_share_projection(true);
        }
        return;
    }
    let touches_space = SPACES_DIR.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|dir| _active_root.starts_with(dir))
    });
    if touches_space {
        if let Some(id) = space_id_of_root(_active_root) {
            emit_missing_add_ops(id);
        }
        push_space_state();
        refresh_share_projection(true);
    }
}

/// 目录名（uuid 连字符形态）反查空间 id。
fn space_id_of_root(root: &Path) -> Option<Uuid> {
    let name = root.file_name()?.to_str()?;
    let id = Uuid::parse_str(name).ok()?;
    space_by_id(id).map(|_| id)
}

/// 库里有行、op log 还没记的素材 → 补 Add op（作者 = 本机，uuid = 本地行
/// uuid——「op 域 uuid = 添加者库内 uuid」的自洽形态）。进入空间视图时也
/// 跑一遍（对账式补偿：导入失败半途、崩溃残留都能被下一次进入收敛）。
fn emit_missing_add_ops(id: Uuid) {
    let Some(root) = space_root_of(id) else {
        return;
    };
    let Some(self_id) = SPACES.with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()))
    else {
        return;
    };
    let ops = load_ops(id);
    let covered: std::collections::HashSet<Vec<u8>> = ops
        .iter()
        .filter_map(|op| match &op.op {
            SpaceOpKind::Add { asset } => Some(asset.content_hash.clone()),
            _ => None,
        })
        .collect();
    let rows = ui_viewmodels::content_paths(&root);
    let mut lamport = ops.iter().map(|op| op.lamport).max().unwrap_or(0);
    let mut fresh: Vec<SpaceOp> = Vec::new();
    let now = now_secs();
    for (hash, uuid, path) in rows {
        if covered.contains(&hash) {
            continue;
        }
        let Ok(uuid) = uuid.parse::<Uuid>() else {
            continue;
        };
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        lamport += 1;
        let op = SpaceOp {
            space_id: id,
            lamport,
            author: self_id.clone(),
            ts: now,
            op: SpaceOpKind::Add {
                asset: SpaceAssetMeta {
                    uuid,
                    file_name: name,
                    kind: space_asset_kind_of(&path),
                    size_bytes: size,
                    content_hash: hash,
                },
            },
        };
        if op.validate().is_ok() {
            fresh.push(op);
        } else {
            lamport -= 1;
        }
    }
    if !fresh.is_empty() {
        logging::info!("空间 {id} 补记 {} 条 Add op（库行反推）", fresh.len());
        append_ops(id, &fresh);
    }
}

/// 展示层扩展名分类（store 行不带类别；只影响清单展示与占位图标）。
fn space_asset_kind_of(path: &Path) -> ui_viewmodels::SpaceAssetKind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => ui_viewmodels::SpaceAssetKind::Image,
        "mp4" | "mov" | "mkv" | "avi" | "webm" => ui_viewmodels::SpaceAssetKind::Video,
        "txt" | "md" => ui_viewmodels::SpaceAssetKind::Text,
        _ => ui_viewmodels::SpaceAssetKind::Other,
    }
}

/// worker 行预处理：空间相关行在进 share_vm 之前拦截（true = 已消费）。
/// DEVICE 不拦截（设备表仍归 share_vm），只挂在线钩子。
pub fn intercept_worker_line(line: &str) -> bool {
    if let Some(rest) = line.strip_prefix("DEVICE\t") {
        on_device_seen(rest);
        return false;
    }
    if let Some(rest) = line.strip_prefix("SPACE_ADMITTED\t") {
        let mut parts = rest.splitn(3, '\t');
        if let (Some(space_id), Some(peer_id), Some(name)) =
            (parts.next(), parts.next(), parts.next())
        {
            if let (Ok(space_id), Some(ui)) = (
                space_id.parse::<Uuid>(),
                host().and_then(|h| h.ui.upgrade()),
            ) {
                handle_admitted(&ui, space_id, peer_id, name);
            }
        }
        return true;
    }
    if let Some(rest) = line.strip_prefix("INCOMING\t") {
        return try_auto_accept(rest);
    }
    if let Some(rest) = line.strip_prefix("STAGED\t") {
        return try_space_staged(rest);
    }
    false
}

/// DEVICE 行钩子：在线成员出现 → 发起一轮抗熵 + 重试挂起的字节索取。
fn on_device_seen(rest: &str) {
    let parts: Vec<&str> = rest.split('\t').collect();
    let Some(id) = parts.first() else {
        return;
    };
    let addrs: Vec<String> = parts
        .get(3)
        .map(|text| {
            text.split(';')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ONLINE_PEERS.with(|slot| {
        slot.borrow_mut().insert(id.to_string(), addrs.clone());
    });
    let is_member = spaces().iter().any(|space| space.is_member(id));
    if is_member {
        let first_time = SYNCED_PEERS.with(|slot| slot.borrow_mut().insert(id.to_string()));
        if first_time {
            crate::share_write_line(&format!("SPACE_SYNC\t{id}\t{}", addrs.join(";")));
        }
        try_pending_fetch();
    }
}

/// 空间主侧：镜像 worker 的 Join 受理（名册 + Roster op 落盘），然后整册
/// 重推对齐 worker 册副本。
fn handle_admitted(ui: &AppWindow, space_id: Uuid, peer_id: &str, joiner_name: &str) {
    let Some(mut space) = space_by_id(space_id) else {
        return;
    };
    if space.is_member(peer_id) {
        return;
    }
    space.members.push(ui_viewmodels::RosterMember {
        id: peer_id.to_string(),
        name: joiner_name.to_string(),
    });
    let Some(self_id) = SPACES.with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()))
    else {
        return;
    };
    let applied = SPACES.with(|vm| vm.borrow_mut().apply_join(space.clone(), now_secs()));
    if applied.is_ok() {
        persist();
        let lamport = load_ops(space_id)
            .iter()
            .map(|op| op.lamport)
            .max()
            .unwrap_or(0)
            + 1;
        let roster = SpaceOp {
            space_id,
            lamport,
            author: self_id,
            ts: now_secs(),
            op: SpaceOpKind::Roster { space },
        };
        append_ops(space_id, std::slice::from_ref(&roster));
        push_space_state();
        sync_rows(ui);
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Success,
            format!("「{joiner_name}」加入了共享空间"),
        );
    }
}

/// 空间报文消费面（ShareAction::SpaceMessage 的落点）。
pub fn handle_space_message(ui: &AppWindow, peer_id: &str, message: SpaceMessage) {
    match message {
        SpaceMessage::Ops { ops } => merge_and_apply_ops(ui, ops),
        SpaceMessage::JoinOk { meta, ops } => apply_join_ok(ui, meta, ops),
        SpaceMessage::JoinErr { reason } => {
            ui.set_space_ctl_error(reason.clone().into());
            notice(ui, ui_viewmodels::notifications::Severity::Warning, reason);
        }
        // Summary/Fetch/Join 由 worker 在通道内应答/处置，到达 UI 即异常形态。
        other => logging::warn!("忽略异常到达 UI 的空间报文（peer={peer_id}）: {other:?}"),
    }
}

/// Ops 差量应用：验权（permits 按 op 域作者索引）→ 追加 log → 物化本地
/// （Add 缺字节挂索取、Remove 按摘要反查本地行发 purge、Roster 落册）。
fn merge_and_apply_ops(ui: &AppWindow, incoming: Vec<SpaceOp>) {
    let mut by_space: std::collections::BTreeMap<Uuid, Vec<SpaceOp>> = Default::default();
    for op in incoming {
        if op.validate().is_ok() {
            by_space.entry(op.space_id).or_default().push(op);
        }
    }
    let mut roster_changed = false;
    for (id, batch) in by_space {
        let Some(space) = space_by_id(id) else {
            logging::info!("收到未知空间 {id} 的 op，丢弃（不泄漏存在性）");
            continue;
        };
        let mut ops = load_ops(id);
        let known: std::collections::HashSet<(u64, String)> = ops
            .iter()
            .map(|op| (op.lamport, op.author.clone()))
            .collect();
        // op 域的 uuid → 添加者索引（Remove 的「添加者本人」判定）。
        let mut added_by: std::collections::HashMap<Uuid, String> = Default::default();
        for op in &ops {
            if let SpaceOpKind::Add { asset } = &op.op {
                added_by.insert(asset.uuid, op.author.clone());
            }
        }
        let mut fresh: Vec<SpaceOp> = Vec::new();
        for op in batch {
            let key = (op.lamport, op.author.clone());
            if known.contains(&key) || !op.permits(&space, &added_by) {
                continue;
            }
            if let SpaceOpKind::Add { asset } = &op.op {
                added_by.insert(asset.uuid, op.author.clone());
            }
            fresh.push(op);
        }
        if fresh.is_empty() {
            continue;
        }
        append_ops(id, &fresh);
        ops.extend(fresh.iter().cloned());
        ui_viewmodels::sort_and_dedupe_ops(&mut ops);

        // 物化：内容索引一份扫全（持有判定 + Remove 反查）。
        let Some(root) = space_root_of(id) else {
            continue;
        };
        let rows = ui_viewmodels::content_paths(&root);
        let mut have: std::collections::HashSet<Vec<u8>> = Default::default();
        let mut uuid_by_hash: std::collections::HashMap<Vec<u8>, String> = Default::default();
        for (hash, uuid, _path) in rows {
            have.insert(hash.clone());
            uuid_by_hash.insert(hash, uuid);
        }
        let mut hash_by_op_uuid: std::collections::HashMap<Uuid, Vec<u8>> = Default::default();
        for op in &ops {
            if let SpaceOpKind::Add { asset } = &op.op {
                hash_by_op_uuid.insert(asset.uuid, asset.content_hash.clone());
            }
        }
        let mut missing: Vec<SpaceAssetMeta> = Vec::new();
        let mut purge_uuids: Vec<String> = Vec::new();
        for op in &fresh {
            match &op.op {
                SpaceOpKind::Add { asset } => {
                    if !have.contains(&asset.content_hash) {
                        missing.push(asset.clone());
                    }
                }
                SpaceOpKind::Remove { asset_uuid } => {
                    if let Some(hash) = hash_by_op_uuid.get(asset_uuid) {
                        if let Some(local) = uuid_by_hash.get(hash) {
                            purge_uuids.push(local.clone());
                        }
                    }
                }
                SpaceOpKind::Roster { space: meta } => {
                    let _ = SPACES.with(|vm| vm.borrow_mut().apply_join(meta.clone(), now_secs()));
                    roster_changed = true;
                }
                // 结构 op（分类/改名/归类）应用面留 P3：log 已收，物化待接。
                other => logging::info!("空间结构 op 待 P3 物化: {other:?}"),
            }
        }
        if !purge_uuids.is_empty() {
            spawn_purge(&root, &purge_uuids);
        }
        if !missing.is_empty() {
            fetch_missing(ui, &space, missing);
        }
        if active().is_some_and(|scope| scope.id == id) {
            enter_space(id);
        }
    }
    if roster_changed {
        persist();
    }
    push_space_state();
    if roster_changed {
        sync_rows(ui);
    }
}

/// JoinOk 应用（加入侧冷启动）：建根 + 落册 + 整写 op log + 进入 + 补字节。
fn apply_join_ok(ui: &AppWindow, meta: ui_viewmodels::SpaceMeta, ops: Vec<SpaceOp>) {
    let id = meta.id;
    let root = match space_root_of(id) {
        Some(root) => root,
        None => return,
    };
    if let Err(error) = std::fs::create_dir_all(&root) {
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Error,
            format!("初始化空间目录失败：{error}"),
        );
        return;
    }
    if let Err(error) = ui_viewmodels::ensure_library_skeleton(&root) {
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Error,
            format!("初始化空间库失败：{error}"),
        );
        return;
    }
    let mut sorted = ops;
    ui_viewmodels::sort_and_dedupe_ops(&mut sorted);
    if let Some(path) = ops_path(id) {
        let _ = crate::atomic_write(&path, &ui_viewmodels::space_ops_to_lines(&sorted));
    }
    let missing = missing_from_ops(&root, &sorted);
    let name = meta.name.clone();
    if SPACES
        .with(|vm| vm.borrow_mut().apply_join(meta, now_secs()))
        .is_ok()
    {
        persist();
        sync_rows(ui);
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Success,
            format!("已加入共享空间「{name}」，素材会自动同步到本机"),
        );
        enter_space(id);
        if !missing.is_empty() {
            if let Some(space) = space_by_id(id) {
                fetch_missing(ui, &space, missing);
            }
        }
        push_space_state();
    }
}

/// op log 里 Add 了、本地库还没有字节的素材集（join 冷启动/对账共用）。
fn missing_from_ops(root: &Path, ops: &[SpaceOp]) -> Vec<SpaceAssetMeta> {
    let have: std::collections::HashSet<Vec<u8>> = ui_viewmodels::content_paths(root)
        .into_iter()
        .map(|(hash, _uuid, _path)| hash)
        .collect();
    ops.iter()
        .filter_map(|op| match &op.op {
            SpaceOpKind::Add { asset } => Some(asset),
            _ => None,
        })
        .filter(|asset| !have.contains(&asset.content_hash))
        .cloned()
        .collect()
}

/// 挂起的字节索取条目（空间 + 素材 + 是否被大小闸挡下）。
struct PendingFetch {
    space_id: Uuid,
    items: Vec<SpaceAssetMeta>,
    gated: bool,
}

fn push_pending(space_id: Uuid, items: Vec<SpaceAssetMeta>, gated: bool) {
    PENDING_FETCH.with(|slot| {
        slot.borrow_mut().push(PendingFetch {
            space_id,
            items,
            gated,
        })
    });
}

/// 大小闸挡下待手动放行的素材总数（管理弹窗的「待下载」入口据此显隐）。
pub fn pending_sync_count() -> usize {
    PENDING_FETCH.with(|slot| {
        slot.borrow()
            .iter()
            .filter(|pending| pending.gated)
            .map(|pending| pending.items.len())
            .sum()
    })
}

/// 刷新「待下载」计数（UI 属性）。
pub fn refresh_pending(ui: &AppWindow) {
    ui.set_space_pending_sync(pending_sync_count() as i32);
}

/// 手动放行（绕过大小闸的用户显式动作）：有在线成员即发；无人在线转
/// 普通挂起（上线自动续传）。
pub fn force_pending_fetch(ui: &AppWindow) {
    let pending: Vec<PendingFetch> =
        PENDING_FETCH.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
    let mut offline = 0usize;
    for entry in pending {
        if !entry.gated {
            // 非闸门挂起项原样回挂（这里只放行闸门项）。
            PENDING_FETCH.with(|slot| slot.borrow_mut().push(entry));
            continue;
        }
        if let Some(space) = space_by_id(entry.space_id) {
            if let Some((peer, addrs)) = online_member(&space) {
                send_fetch(entry.space_id, entry.items, &peer, &addrs);
                continue;
            }
        }
        offline += entry.items.len();
        push_pending(entry.space_id, entry.items, false);
    }
    if offline > 0 {
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Info,
            format!("{offline} 项已排队，等成员上线自动下载"),
        );
    }
    refresh_pending(ui);
}

/// 字节索取：大小闸（拍板②）→ 选在线持有者 → 分批发 Fetch；全员离线挂起，
/// 对端上线重试（事件驱动，零 Timer）。闸门挡下的挂起转手动（管理弹窗
/// 「待下载」入口放行）。
fn fetch_missing(ui: &AppWindow, space: &SpaceMeta, items: Vec<SpaceAssetMeta>) {
    if items.is_empty() {
        return;
    }
    let (gate_on, mb) = GATE.with(|slot| *slot.borrow());
    let total: u64 = items.iter().map(|item| item.size_bytes).sum();
    if gate_on && total > u64::from(mb) * 1024 * 1024 {
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Warning,
            format!(
                "「{}」有 {} 项新素材（{}），超过同步上限 {}MB，已挂起——可在「共享空间」里手动下载",
                space.name,
                items.len(),
                ui_viewmodels::human_size(total),
                mb
            ),
        );
        push_pending(space.id, items, true);
        refresh_pending(ui);
        return;
    }
    match online_member(space) {
        Some((peer, addrs)) => send_fetch(space.id, items, &peer, &addrs),
        None => {
            logging::info!(
                "空间「{}」{} 项待补字节，暂无在线成员（上线自动续传）",
                space.name,
                items.len()
            );
            push_pending(space.id, items, false);
        }
    }
}

fn try_pending_fetch() {
    let pending: Vec<PendingFetch> =
        PENDING_FETCH.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
    let mut kept = Vec::new();
    for mut entry in pending {
        // 闸门项只认手动放行（force_pending_fetch），对端上线不自动重试。
        if entry.gated {
            kept.push(entry);
            continue;
        }
        let mut handled = false;
        if let Some(space) = space_by_id(entry.space_id) {
            if let Some((peer, addrs)) = online_member(&space) {
                send_fetch(
                    entry.space_id,
                    std::mem::take(&mut entry.items),
                    &peer,
                    &addrs,
                );
                handled = true;
            }
        }
        if !handled {
            kept.push(entry);
        }
    }
    PENDING_FETCH.with(|slot| *slot.borrow_mut() = kept);
}

fn send_fetch(space_id: Uuid, items: Vec<SpaceAssetMeta>, peer: &str, addrs: &[String]) {
    for chunk in items.chunks(MAX_FETCH_ITEMS) {
        let Some(line) = ui_viewmodels::space_send_line(
            peer,
            addrs,
            SpaceMessage::Fetch {
                space_id,
                items: chunk.to_vec(),
            },
        ) else {
            return;
        };
        crate::share_write_line(&line);
    }
}

/// 该空间任一在线成员（非本机）。
fn online_member(space: &SpaceMeta) -> Option<(String, Vec<String>)> {
    let self_id = SPACES.with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()));
    ONLINE_PEERS.with(|slot| {
        let peers = slot.borrow();
        space
            .members
            .iter()
            .filter(|member| Some(&member.id) != self_id.as_ref())
            .find_map(|member| {
                peers
                    .get(&member.id)
                    .cloned()
                    .map(|a| (member.id.clone(), a))
            })
    })
}

/// 贴码加入：解析 → 拨空间主（SPACE_JOIN，worker 纯 id 直连）。应答
/// JoinOk/JoinErr 经 SYNC_RECV 回来。
pub fn join_by_code(ui: &AppWindow, code: &str) {
    let code = code.trim();
    let parsed = match ui_viewmodels::parse_space_invite_code(code) {
        Ok(parsed) => parsed,
        Err(error) => {
            ui.set_space_ctl_error(error.to_string().into());
            return;
        }
    };
    let Some((self_id, self_name)) = SPACES.with(|vm| {
        vm.borrow()
            .identity()
            .map(|(id, name)| (id.to_string(), name.to_string()))
    }) else {
        ui.set_space_ctl_error("共享引擎尚未就绪，请稍后再试".into());
        return;
    };
    if space_by_id(parsed.space_id).is_some() {
        ui.set_space_ctl_error("已经在该空间里了".into());
        return;
    }
    let _ = self_id;
    crate::share_write_line(&format!(
        "SPACE_JOIN\t{}\t{}\t{self_name}",
        parsed.owner_id, code
    ));
    notice(
        ui,
        ui_viewmodels::notifications::Severity::Info,
        "正在连接空间主…对方不在线时稍后会提示失败".to_string(),
    );
}

/// INCOMING 拦截：空间投递（manifest.space_id）免首连确认（入空间 =
/// 同意接收，D90 确认移位）。未知空间 = 显式拒绝。返回 true = 行已消费。
fn try_auto_accept(payload: &str) -> bool {
    let Some(manifest) = ui_viewmodels::parse_transfer_manifest(payload) else {
        return false;
    };
    let Some(space_id) = manifest.space_id else {
        return false;
    };
    let Some(ui) = host().and_then(|h| h.ui.upgrade()) else {
        return false;
    };
    let known = space_by_id(space_id).is_some_and(|space| {
        SPACES.with(|vm| {
            vm.borrow()
                .identity()
                .is_some_and(|(id, _)| space.is_member(id))
        })
    });
    if known {
        SPACE_BATCHES.with(|slot| {
            slot.borrow_mut().insert(manifest.batch_id, space_id);
        });
        crate::share_write_line(&format!("ACCEPT\t{}", manifest.batch_id));
        notice(
            &ui,
            ui_viewmodels::notifications::Severity::Info,
            format!("正在接收空间素材…共 {} 项", manifest.items.len()),
        );
    } else {
        crate::share_write_line(&format!("REJECT\t{}\t未知空间", manifest.batch_id));
    }
    true
}

/// STAGED 拦截：空间批次落空间根导入（同一导入管线，收件箱语义），乐观
/// ACK——本地导入若失败，下次抗熵发现字节仍缺会自愈重取。
fn try_space_staged(payload: &str) -> bool {
    let Some((batch, paths)) = ui_viewmodels::parse_staged_payload(payload) else {
        return false;
    };
    let Some(space_id) = SPACE_BATCHES.with(|slot| slot.borrow_mut().remove(&batch)) else {
        return false;
    };
    let Some(scope_host) = host() else {
        return true;
    };
    let Some(ui) = scope_host.ui.upgrade() else {
        return true;
    };
    add_paths_to_space(space_id, paths, scope_host.importing.clone());
    crate::share_write_line(&format!("ACK\t{batch}"));
    notice(
        &ui,
        ui_viewmodels::notifications::Severity::Info,
        "空间素材已接收，正在导入本机副本…".to_string(),
    );
    true
}

/// 空间库 purge（Remove op 物化）：单写者纪律，子进程执行。
fn spawn_purge(root: &Path, uuids: &[String]) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new(crate::helper_exe("sample-library"));
    command
        .arg("--cmd")
        .arg("purge")
        .arg("--library")
        .arg(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for uuid in uuids {
        command.arg("--uuid").arg(uuid);
    }
    command.creation_flags(CREATE_NO_WINDOW);
    match command.spawn() {
        Ok(_) => logging::info!("空间 Remove 物化：purge {} 项", uuids.len()),
        Err(error) => logging::warn!("空间 purge 子进程启动失败: {error}"),
    }
}
