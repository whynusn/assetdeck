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

use slint::Weak;
use ui_viewmodels::uuid::Uuid;
use ui_viewmodels::{
    CategoryRows, Filter, LibraryGridVm, RealAssetResolver, SpaceMeta, SpaceVm, TargetNoticeTone,
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
pub fn handle_worker_ready(self_id: &str, self_name: &str) {
    let changed = SPACES.with(|vm| vm.borrow_mut().set_identity(self_id, self_name));
    if changed {
        persist();
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
        "--mode".to_string(),
        "inbox".to_string(),
    ];
    // 进度反馈交给导入管线的进度条（Info 级瞬时信息不弹窗，落日志）。
    logging::info!("向空间「{}」加入 {} 项素材", space.name, paths.len());
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
        "--mode".to_string(),
        "inbox".to_string(),
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
