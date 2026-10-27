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
    MAX_FETCH_ITEMS, MAX_OPS_PER_MESSAGE,
};

use crate::thumbs::{GridCtx, ThumbCache};
use crate::{
    apply_categories, default_library_root, recent_first_sorter, spawn_import_pipeline,
    sync_counts, AppWindow, COLUMNS, CONTAINER_WIDTH, GAP,
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
    // READY marks a new worker session. Identity callbacks for other worker lines
    // consume this flag, so the session setup cannot accidentally rescan in a loop.
    static WORKER_SESSION_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SPACE_JOIN_PENDING: RefCell<Option<(Uuid, String)>> = const { RefCell::new(None) };
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

/// 身份变化才落盘；每个 READY 会话都须装载新 worker 的空册再发现设备。
/// main 的非 READY 事件也会回填身份，因此会话标记必须消费，防止扫描回环。
pub fn handle_worker_ready(self_id: &str, self_name: &str) {
    let changed = SPACES.with(|vm| vm.borrow_mut().set_identity(self_id, self_name));
    if changed {
        persist();
    }
    if WORKER_SESSION_PENDING.with(|pending| pending.replace(false)) {
        SYNCED_PEERS.with(|peers| peers.borrow_mut().clear());
        ONLINE_PEERS.with(|peers| peers.borrow_mut().clear());
        PUBLISHED_OPS.with(|ops| ops.borrow_mut().clear());
        PULL_CURSORS.with(|cursors| cursors.borrow_mut().clear());
        FETCHING_HASHES.with(|fetching| fetching.borrow_mut().clear());
        push_space_state();
        for peer in authorized_members() {
            SYNCED_PEERS.with(|peers| {
                peers.borrow_mut().insert(peer.clone());
            });
            start_peer_sync(&peer);
        }
        crate::share_write_line("SCAN");
    }
}

fn authorized_members() -> Vec<String> {
    let self_id = SPACES.with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()));
    let mut members = std::collections::BTreeSet::new();
    for space in spaces() {
        for member in space.members {
            if Some(member.id.clone()) != self_id {
                members.insert(member.id);
            }
        }
    }
    members.into_iter().collect()
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

/// 侧栏/弹窗团队态同步：绑定三件套（name/sub/id）+ 作用域旗标一次摆齐。
/// D93：同时最多绑定一个团队——首行即团队；空册 = 未绑定（name 空串）。
pub fn sync_rows(ui: &AppWindow) {
    let bound = spaces().into_iter().next();
    match &bound {
        Some(space) => {
            ui.set_team_name(space.name.clone().into());
            ui.set_team_sub(SpaceVm::member_label(space).into());
            ui.set_team_id(space.id.to_string().into());
        }
        None => {
            ui.set_team_name("".into());
            ui.set_team_sub("".into());
            ui.set_team_id("".into());
        }
    }
    ui.set_in_space_view(in_space_view());
    ui.set_space_ctl_ready(SPACES.with(|vm| vm.borrow().is_ready()));
    refresh_pending(ui);
    // D92：空间册/作用域变更汇流点——投影随手刷新（enter_space 途中
    // in_space_view 已置真，这里只清侧栏态；主视图侧整窗重排亮新角标）。
    refresh_share_projection(true);
}

/// D93 绑定的团队（册首行；None = 未绑定）。「共享到团队」的一切入口
/// 以它为唯一目的地——不再有空间选择弹窗。
pub fn bound_space() -> Option<SpaceMeta> {
    spaces().into_iter().next()
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
            emit_missing_add_ops(id, false);
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
                format!("打开团队「{}」失败：{error}", space.name),
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

/// 创建团队（D93：同时最多绑定一个）：注册表行 + 目录骨架 + 直接进入
/// （创建即到达，少一次点击）。已绑定 → 明说换绑路径，不弹确认框。
pub fn create_space(name: &str) {
    let Some(ui_host) = host() else { return };
    if let Some(bound) = bound_space() {
        if let Some(ui) = ui_host.ui.upgrade() {
            ui.set_space_ctl_error(
                format!(
                    "已绑定团队「{}」。先退出当前团队，再创建新团队（换绑）",
                    bound.name
                )
                .into(),
            );
        }
        return;
    }
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
                            format!("创建团队目录失败：{error}"),
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
                    format!("团队「{name}」已创建，把素材右键共享进来，或直接拖入"),
                );
            }
            enter_space(id);
            push_space_state();
        }
        Err(error) => {
            if let Some(ui) = ui_host.ui.upgrade() {
                ui.set_space_ctl_error(error.to_string().into());
            }
        }
    }
}

/// 退出并删除团队（本机副本整目录删除；D93 换绑 = 退出后再创建/贴码）。
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
        push_space_state();
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
            "已退出团队，本地副本已删除。现在可以创建或加入新团队".to_string(),
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
        push_space_state();
    }
    rotated
}

/// 把一组文件路径添加进空间（右键「共享到团队」/多选操作条）。
pub fn add_paths_to_space(id: Uuid, paths: Vec<PathBuf>, importing: Arc<AtomicBool>) {
    add_paths_to_space_for_batch(id, None, paths, importing);
}

fn add_paths_to_space_for_batch(
    id: Uuid,
    batch: Option<(Uuid, Vec<String>)>,
    paths: Vec<PathBuf>,
    importing: Arc<AtomicBool>,
) {
    let has_batch = batch.is_some();
    let callback = batch.map(|(batch, hashes)| {
        Arc::new(move |result: Result<(), String>| {
            let result = result.and_then(|()| verify_received_hashes(id, &hashes));
            let line = match result {
                Ok(()) => {
                    let delivered = |space: &Uuid, hash: &Vec<u8>| {
                        *space == id
                            && hashes.contains(
                                &hash
                                    .iter()
                                    .map(|byte| format!("{byte:02x}"))
                                    .collect::<String>(),
                            )
                    };
                    FETCHING_HASHES.with(|fetching| {
                        fetching
                            .borrow_mut()
                            .retain(|(space, hash)| !delivered(space, hash))
                    });
                    FETCH_SOURCES.with(|sources| {
                        sources
                            .borrow_mut()
                            .retain(|(space, hash), _| !delivered(space, hash))
                    });
                    FETCH_ATTEMPTS.with(|attempts| {
                        attempts
                            .borrow_mut()
                            .retain(|(space, hash), _| !delivered(space, hash))
                    });
                    format!("ACK\t{batch}")
                }
                Err(reason) => {
                    defer_failed_fetch(id);
                    format!("NACK\t{batch}\t{}", reason.replace(['\r', '\n', '\t'], " "))
                }
            };
            crate::share_write_line(&line);
        }) as crate::import_completion::Phase1Callback
    });
    let target = space_root_of(id).unwrap_or_default();
    let completion = crate::import_completion::ImportCompletion::new(target, callback);
    let reject = |reason: &str| {
        completion.complete(Err(reason.to_string()));
    };
    let Some(space) = space_by_id(id) else {
        reject("目标团队不存在");
        return;
    };
    let root = completion.target().to_path_buf();
    if root.as_os_str().is_empty() {
        reject("无法解析团队库目录");
        return;
    }
    let Some(ui_host) = host() else {
        reject("导入服务不可用");
        return;
    };
    let Some(ui) = ui_host.ui.upgrade() else {
        reject("导入界面已关闭");
        return;
    };
    if paths.is_empty() {
        reject("接收批次不含素材");
        return;
    }
    if importing.load(Ordering::SeqCst) {
        crate::show_notice(
            &ui,
            TargetNoticeTone::Warning,
            "正在导入中，请等进度条结束后再操作".to_string(),
        );
        reject("另一个导入任务仍在进行");
        return;
    }
    let Some(list) = write_import_list(&paths) else {
        crate::show_notice(
            &ui,
            TargetNoticeTone::Error,
            "无法写入导入清单（临时目录不可用）".to_string(),
        );
        reject("无法写入导入清单");
        return;
    };
    let args = vec![
        "--import-paths".to_string(),
        list.to_string_lossy().into_owned(),
        "--library".to_string(),
        root.to_string_lossy().into_owned(),
        "--mode".to_string(),
        "background".to_string(),
    ];
    logging::info!("向团队「{}」加入 {} 项素材", space.name, paths.len());
    let callback: crate::import_completion::Phase1Callback = Arc::new(move |result| {
        let _ = std::fs::remove_file(&list);
        completion.complete(result);
    });
    spawn_import_pipeline(
        ui_host.ui.clone(),
        args,
        root.to_string_lossy().into_owned(),
        importing,
        format!("加入「{}」", space.name),
        false,
        // 显式本地共享允许把已 Remove 的内容作为新贡献重新 Add；
        // 远端批次回执（batch = Some）不得自发替代 Add。
        crate::import_completion::ReconcileTail {
            callback: Some(callback),
            allow_space_readd: !has_batch,
        },
    );
}

fn verify_received_hashes(id: Uuid, expected: &[String]) -> Result<(), String> {
    space_by_id(id).ok_or("目标团队已变化")?;
    let root = space_root_of(id).ok_or("团队目录不可用")?;
    let have: std::collections::HashSet<String> = ui_viewmodels::content_paths(&root)
        .into_iter()
        .filter(|(_, _, path)| path.is_file())
        .map(|(hash, _, _)| hash.iter().map(|byte| format!("{byte:02x}")).collect())
        .collect();
    if expected.is_empty() || expected.iter().any(|hash| !have.contains(hash)) {
        return Err("接收批次尚未完整入库，未确认送达".into());
    }
    Ok(())
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
    logging::info!("向我的素材保存 {} 项（来自团队）", paths.len());
    spawn_import_pipeline(
        ui_host.ui.clone(),
        args,
        root.to_string_lossy().into_owned(),
        importing,
        "保存到我的素材".to_string(),
        false,
        crate::import_completion::ReconcileTail {
            callback: None,
            allow_space_readd: false,
        },
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

fn active_space_assets(ops: &[SpaceOp]) -> std::collections::HashMap<Vec<u8>, SpaceAssetMeta> {
    let mut ordered = ops.to_vec();
    ui_viewmodels::sort_and_dedupe_ops(&mut ordered);
    let mut by_uuid = std::collections::HashMap::<Uuid, SpaceAssetMeta>::new();
    for op in ordered {
        match op.op {
            SpaceOpKind::Add { asset } => {
                by_uuid.insert(asset.uuid, asset);
            }
            SpaceOpKind::Remove { asset_uuid } => {
                by_uuid.remove(&asset_uuid);
            }
            _ => {}
        }
    }
    by_uuid
        .into_values()
        .map(|asset| (asset.content_hash.clone(), asset))
        .collect()
}

/// 全空间成员内容哈希集：Add/Remove 按 op 全序折叠到最终活动集合。
fn member_hashes() -> std::collections::HashSet<Vec<u8>> {
    spaces()
        .into_iter()
        .flat_map(|meta| active_space_assets(&load_ops(meta.id)).into_keys())
        .collect()
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

#[derive(Clone)]
struct ReceivedBatch {
    space_id: Uuid,
    hashes: Vec<String>,
}

type StagedImport = (Uuid, ReceivedBatch, PathBuf, Vec<PathBuf>);
type FetchAttempts = std::collections::HashMap<(Uuid, Vec<u8>), std::collections::HashSet<String>>;

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
    static SPACE_BATCHES: RefCell<std::collections::HashMap<Uuid, ReceivedBatch>> =
        RefCell::new(std::collections::HashMap::new());
    /// 已向特定远端发布过的本地 op 键；只用于避免重复广播，不是事实源。
    static PUBLISHED_OPS: RefCell<std::collections::HashSet<(String, Uuid, u64, String)>> =
        RefCell::new(std::collections::HashSet::new());
    /// 大小闸（设置装载时回填）：(启用, MB 上限)。拍板②：默认 500MB 可调。
    static GATE: RefCell<(bool, u32)> = const { RefCell::new((true, 500)) };
    /// 挂起的字节索取。gated = 被大小闸挡下（等用户手动放行，对端上线
    /// 不自动重试）；否则 = 无人在线（对端上线自动续传）。
    static PENDING_FETCH: RefCell<Vec<PendingFetch>> = const { RefCell::new(Vec::new()) };
    static STAGED_IMPORTS: RefCell<std::collections::VecDeque<StagedImport>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
    static PENDING_PURGES: RefCell<std::collections::BTreeSet<Uuid>> = const { RefCell::new(std::collections::BTreeSet::new()) };
    static FETCHING_HASHES: RefCell<std::collections::HashSet<(Uuid, Vec<u8>)>> =
        RefCell::new(std::collections::HashSet::new());
    static FETCH_SOURCES: RefCell<std::collections::HashMap<(Uuid, Vec<u8>), String>> = RefCell::new(std::collections::HashMap::new());
    static FETCH_ATTEMPTS: RefCell<FetchAttempts> = RefCell::new(std::collections::HashMap::new());
    static PULL_CURSORS: RefCell<std::collections::HashMap<(String, Uuid), Option<ui_viewmodels::OpKey>>> =
        RefCell::new(std::collections::HashMap::new());
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

fn append_ops(id: Uuid, ops: &[SpaceOp]) -> bool {
    if ops.is_empty() {
        return true;
    }
    let Some(path) = ops_path(id) else {
        return false;
    };
    let text = ui_viewmodels::space_ops_to_lines(ops);
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let previous_len = file.metadata()?.len();
        if let Err(error) =
            std::io::Write::write_all(&mut file, text.as_bytes()).and_then(|()| file.sync_data())
        {
            if let Err(rollback) = file.set_len(previous_len).and_then(|()| file.sync_data()) {
                logging::warn!("空间 op log 失败回滚未完成: {rollback}");
            }
            return Err(error);
        }
        Ok(())
    })();
    if let Err(error) = result {
        logging::warn!("空间 op log 追加失败 team={id}: {error}");
        return false;
    }
    true
}

/// 组装并整册推送空间服务册（meta + op log + 字节服务索引）。
/// 触发点：worker READY / 创建、退出、换码 / SPACE_ADMITTED / op 应用 / 空间根导入完成。
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

/// 导入完成后按明确目标根补记 Add，并主动把新增操作分批发布给已授权成员。
pub fn on_space_import_finished(target_root: &Path, allow_readd: bool) -> Result<(), String> {
    let Some(id) = space_id_of_root(target_root) else {
        return Ok(());
    };
    if !emit_missing_add_ops(id, allow_readd) {
        return Err("团队操作记录写入失败，未确认送达".into());
    }
    let after = load_ops(id);
    push_space_state();
    if !after.is_empty() {
        broadcast_ops(id, &after);
        if let Some((space, ui)) = space_by_id(id).zip(host().and_then(|host| host.ui.upgrade())) {
            materialize_space_ops(&ui, &space, &after);
        }
    }
    refresh_share_projection(true);
    Ok(())
}

/// 向已授权且已认证的成员发布有限大小的 op 增量。
/// 认证会话即使没有 DEVICE 行也可用；地址为空交给 worker 走生产发现。
fn broadcast_ops(id: Uuid, ops: &[SpaceOp]) {
    if ops.is_empty() {
        return;
    }
    let members = space_by_id(id)
        .map(|space| {
            space
                .members
                .into_iter()
                .map(|member| member.id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let self_id = SPACES.with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()));
    for peer in members {
        if Some(peer.clone()) == self_id {
            continue;
        }
        broadcast_ops_to(id, &peer, ops);
    }
}

/// 向单个成员发布 `ops` 里它未见过的部分（分批 ≤ MAX_OPS_PER_MESSAGE）。
fn broadcast_ops_to(id: Uuid, peer: &str, ops: &[SpaceOp]) {
    for chunk in ops.chunks(MAX_OPS_PER_MESSAGE) {
        let unseen: Vec<SpaceOp> = chunk
            .iter()
            .filter(|op| {
                PUBLISHED_OPS.with(|seen| {
                    !seen
                        .borrow()
                        .contains(&(peer.to_string(), id, op.lamport, op.author.clone()))
                })
            })
            .cloned()
            .collect();
        if unseen.is_empty() {
            continue;
        }
        let Some(line) = ui_viewmodels::space_send_line(
            peer,
            &peer_addrs(peer),
            SpaceMessage::Ops {
                ops: unseen.clone(),
            },
        ) else {
            continue;
        };
        if crate::share_write_line(&line) {
            PUBLISHED_OPS.with(|seen| {
                let mut seen = seen.borrow_mut();
                for op in unseen {
                    seen.insert((peer.to_string(), id, op.lamport, op.author));
                }
            });
        }
    }
}

/// 向一个已认证成员补发其未见过的本地 op（一次调用一轮，不重试）。
/// 错峰启动的另一半：对端上线拉走我们的册子时，我们的增量也必须主动
/// 推给它——只靠 PullOps 应答收敛不了双方各自的离线写入。
fn broadcast_unpublished_ops(peer_id: &str) {
    for space in spaces() {
        let ops = load_ops(space.id);
        let unseen: Vec<SpaceOp> = ops
            .iter()
            .filter(|op| {
                PUBLISHED_OPS.with(|seen| {
                    !seen.borrow().contains(&(
                        peer_id.to_string(),
                        space.id,
                        op.lamport,
                        op.author.clone(),
                    ))
                })
            })
            .cloned()
            .collect();
        broadcast_ops_to(space.id, peer_id, &unseen);
    }
}

fn peer_addrs(peer: &str) -> Vec<String> {
    ONLINE_PEERS.with(|slot| slot.borrow().get(peer).cloned().unwrap_or_default())
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
fn emit_missing_add_ops(id: Uuid, allow_readd: bool) -> bool {
    let Some(root) = space_root_of(id) else {
        return false;
    };
    let Some(self_id) = SPACES.with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()))
    else {
        return false;
    };
    let ops = load_ops(id);
    let active = active_space_assets(&ops);
    let covered: std::collections::HashSet<Vec<u8>> = if allow_readd {
        active.keys().cloned().collect()
    } else {
        ops.iter()
            .filter_map(|op| match &op.op {
                SpaceOpKind::Add { asset } => Some(asset.content_hash.clone()),
                _ => None,
            })
            .collect()
    };
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
    }
    append_ops(id, &fresh)
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
    if line.starts_with("READY\t") {
        WORKER_SESSION_PENDING.with(|pending| pending.set(true));
        // READY 仍交 share_vm 采信身份；main 随后的身份回填消费会话标记。
        return false;
    }
    if line == "NETWORK_READY" {
        push_space_state();
        for peer in authorized_members() {
            start_peer_sync(&peer);
        }
        try_pending_fetch();
        return true;
    }
    if let Some(peer) = line.strip_prefix("SPACE_SYNC_FAILED\t") {
        PULL_CURSORS.with(|cursors| cursors.borrow_mut().retain(|(id, _), _| id != peer));
        PUBLISHED_OPS.with(|ops| ops.borrow_mut().retain(|(id, _, _, _)| id != peer));
        return true;
    }
    if let Some(peer) = line.strip_prefix("SPACE_PEER_ACTIVE\t") {
        // 对端发起的认证拉取会话 = 它在线且可拨。错峰启动时双向收敛的
        // 触发源：对端先拉了我们，我们的离线增量也要广播给它（一次有界，
        // 无 Timer、无回声—— SYNCED_PEERS 语义仍是不重复发起）。
        if spaces().iter().any(|space| space.is_member(peer)) {
            let first = SYNCED_PEERS.with(|slot| slot.borrow_mut().insert(peer.to_string()));
            ONLINE_PEERS.with(|slot| {
                slot.borrow_mut().entry(peer.to_string()).or_default();
            });
            if first {
                start_peer_sync(peer);
            }
            broadcast_unpublished_ops(peer);
        }
        return true;
    }
    if let Some(rest) = line.strip_prefix("SPACE_FETCH_FAILED\t") {
        if let Some((peer, id)) = rest.split_once('\t') {
            if let Ok(id) = id.parse::<Uuid>() {
                if space_by_id(id).is_some_and(|space| space.is_member(peer)) {
                    let hashes = FETCH_SOURCES.with(|sources| {
                        sources
                            .borrow()
                            .iter()
                            .filter(|((space_id, _), source)| {
                                *space_id == id && source.as_str() == peer
                            })
                            .map(|((_, hash), _)| hash.clone())
                            .collect()
                    });
                    handle_unavailable_fetch(id, peer, hashes);
                }
            }
        }
        return true;
    }
    if let Some(rest) = line.strip_prefix("done\t") {
        let mut fields = rest.split('\t');
        if let (Some(batch), Some("failed" | "rejected")) = (fields.next(), fields.next()) {
            if let Ok(batch) = batch.parse::<Uuid>() {
                if let Some(id) = SPACE_BATCHES.with(|batches| batches.borrow_mut().remove(&batch))
                {
                    defer_failed_fetch(id.space_id);
                }
            }
        }
    }
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
        .get(2)
        .map(|text| {
            text.split(';')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let addresses_changed = ONLINE_PEERS.with(|slot| {
        slot.borrow_mut()
            .insert(id.to_string(), addrs.clone())
            .as_ref()
            != Some(&addrs)
    });
    let is_member = spaces().iter().any(|space| space.is_member(id));
    if is_member {
        let first_time = SYNCED_PEERS.with(|slot| slot.borrow_mut().insert(id.to_string()));
        if first_time || addresses_changed {
            start_peer_sync(id);
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
    if self_id != space.owner_id || space.validate().is_err() {
        return;
    }
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
        op: SpaceOpKind::Roster {
            space: space.clone(),
        },
    };
    if !append_ops(space_id, std::slice::from_ref(&roster)) {
        push_space_state();
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Warning,
            "成员记录写入失败，未确认加入；请重新加入".into(),
        );
        return;
    }
    if SPACES
        .with(|vm| vm.borrow_mut().apply_join(space, now_secs()))
        .is_ok()
    {
        persist();
        broadcast_ops(space_id, std::slice::from_ref(&roster));
        push_space_state();
        sync_rows(ui);
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Success,
            format!("「{joiner_name}」加入了团队"),
        );
    }
}

/// 空间报文消费面（ShareAction::SpaceMessage 的落点）。
pub fn handle_space_message(ui: &AppWindow, peer_id: &str, message: SpaceMessage) {
    if !peer_id.is_empty() {
        ONLINE_PEERS.with(|slot| {
            slot.borrow_mut().entry(peer_id.to_string()).or_default();
        });
    }
    match message {
        SpaceMessage::Ops { ops } => merge_and_apply_ops(ui, peer_id, ops),
        SpaceMessage::OpsPage {
            space_id,
            after,
            next,
            ops,
        } => {
            let valid = SpaceMessage::OpsPage {
                space_id,
                after: after.clone(),
                next: next.clone(),
                ops: ops.clone(),
            }
            .validate()
            .is_ok();
            let expected = PULL_CURSORS.with(|cursors| {
                cursors
                    .borrow()
                    .get(&(peer_id.to_string(), space_id))
                    .cloned()
            });
            if !valid
                || expected != Some(after)
                || !space_by_id(space_id).is_some_and(|space| space.is_member(peer_id))
            {
                return;
            }
            if !merge_ops(ui, peer_id, ops, false) {
                return;
            }
            PULL_CURSORS.with(|cursors| {
                cursors
                    .borrow_mut()
                    .remove(&(peer_id.to_string(), space_id));
            });
            if let Some(next) = next {
                request_ops_page(peer_id, space_id, Some(next));
            } else if let Some(space) = space_by_id(space_id) {
                materialize_space_ops(ui, &space, &load_ops(space_id));
                push_space_state();
            }
        }
        SpaceMessage::FetchUnavailable { space_id, hashes } => {
            if space_by_id(space_id).is_some_and(|space| space.is_member(peer_id)) {
                handle_unavailable_fetch(space_id, peer_id, hashes);
            }
        }
        SpaceMessage::JoinOk { meta, ops } => apply_join_ok(ui, peer_id, meta, ops),
        SpaceMessage::JoinErr { reason } => {
            let expected = SPACE_JOIN_PENDING.with(|pending| {
                let mut pending = pending.borrow_mut();
                if pending.as_ref().is_some_and(|(_, owner)| owner == peer_id) {
                    pending.take();
                    true
                } else {
                    false
                }
            });
            if expected {
                ui.set_space_ctl_error(reason.clone().into());
                notice(ui, ui_viewmodels::notifications::Severity::Warning, reason);
            }
        }
        // 协议对象可能包含邀请码或名册 secret，异常路径也只记事件类型。
        _ => logging::warn!("忽略异常到达 UI 的空间报文 peer={peer_id}"),
    }
}

/// Ops 差量应用：验权（permits 按 op 域作者索引）→ 追加 log → 物化本地
/// （Add 缺字节挂索取、Remove 按摘要反查本地行发 purge、Roster 落册）。
fn merge_and_apply_ops(ui: &AppWindow, peer_id: &str, incoming: Vec<SpaceOp>) {
    merge_ops(ui, peer_id, incoming, true);
}

fn merge_ops(ui: &AppWindow, peer_id: &str, incoming: Vec<SpaceOp>, legacy_push: bool) -> bool {
    if incoming.len() > MAX_OPS_PER_MESSAGE {
        return false;
    }
    let mut by_space: std::collections::BTreeMap<Uuid, Vec<SpaceOp>> = Default::default();
    for op in incoming {
        if op.validate().is_ok() {
            by_space.entry(op.space_id).or_default().push(op);
        }
    }
    let mut progressed = false;
    let mut roster_changed = false;
    for (id, mut batch) in by_space {
        let Some(mut space) = space_by_id(id) else {
            continue;
        };
        if !space.is_member(peer_id) {
            logging::warn!("忽略非团队成员空间操作 peer={peer_id} space={id}");
            continue;
        }
        let mut ops = load_ops(id);
        let mut known: std::collections::HashSet<(u64, String)> = ops
            .iter()
            .map(|op| (op.lamport, op.author.clone()))
            .collect();
        let mut added_by = std::collections::HashMap::<Uuid, String>::new();
        for op in &ops {
            if let SpaceOpKind::Add { asset } = &op.op {
                added_by.insert(asset.uuid, op.author.clone());
            }
        }
        ui_viewmodels::sort_and_dedupe_ops(&mut batch);
        let previous_latest_roster = ops
            .iter()
            .filter_map(|op| match &op.op {
                SpaceOpKind::Roster { .. } => Some((op.lamport, op.author.clone())),
                _ => None,
            })
            .max();
        let mut latest_roster = previous_latest_roster.clone();
        let mut fresh = Vec::new();
        for op in batch {
            let key = (op.lamport, op.author.clone());
            if known.contains(&key) || !op.permits(&space, &added_by) {
                continue;
            }
            if let SpaceOpKind::Roster { space: next } = &op.op {
                if next.id != id || next.owner_id != space.owner_id {
                    continue;
                }
                // Roster snapshots participate in authorization in total order.
                // An older snapshot is retained in the log but cannot roll back
                // the working membership used by later operations in this page.
                if latest_roster
                    .as_ref()
                    .is_none_or(|previous| key > *previous)
                {
                    space = next.clone();
                    latest_roster = Some(key.clone());
                }
            }
            if let SpaceOpKind::Add { asset } = &op.op {
                added_by.insert(asset.uuid, op.author.clone());
            }
            known.insert(key);
            fresh.push(op);
        }
        if fresh.is_empty() {
            continue;
        }
        if !append_ops(id, &fresh) {
            return false;
        }
        progressed = true;
        ops.extend(fresh.iter().cloned());
        ui_viewmodels::sort_and_dedupe_ops(&mut ops);
        if let Some((key, next)) = ops
            .iter()
            .filter_map(|op| match &op.op {
                SpaceOpKind::Roster { space: next } => {
                    Some(((op.lamport, op.author.clone()), next))
                }
                _ => None,
            })
            .max_by(|(left, _), (right, _)| left.cmp(right))
        {
            if previous_latest_roster
                .as_ref()
                .is_none_or(|previous| key > *previous)
                && SPACES
                    .with(|vm| vm.borrow_mut().apply_join(next.clone(), now_secs()))
                    .is_ok()
            {
                space = next.clone();
                roster_changed = true;
            }
        }
        if legacy_push {
            materialize_space_ops(ui, &space, &ops);
        }
    }
    if !progressed {
        return true;
    }
    if roster_changed {
        persist();
        sync_rows(ui);
    }
    push_space_state();
    if legacy_push {
        start_peer_sync(peer_id);
    }
    true
}

fn materialize_space_ops(ui: &AppWindow, space: &SpaceMeta, ops: &[SpaceOp]) {
    let Some(root) = space_root_of(space.id) else {
        return;
    };
    let active_assets = active_space_assets(ops);
    let known_hashes: std::collections::HashSet<_> = ops
        .iter()
        .filter_map(|op| match &op.op {
            SpaceOpKind::Add { asset } => Some(asset.content_hash.clone()),
            _ => None,
        })
        .collect();
    let rows = ui_viewmodels::content_paths(&root);
    let have: std::collections::HashSet<_> = rows.iter().map(|(hash, _, _)| hash.clone()).collect();
    let purge: Vec<_> = rows
        .into_iter()
        .filter(|(hash, _, _)| known_hashes.contains(hash) && !active_assets.contains_key(hash))
        .map(|(_, uuid, _)| uuid)
        .collect();
    prune_pending_fetch(space.id, &active_assets);
    FETCH_SOURCES.with(|sources| {
        sources.borrow_mut().retain(|(id, hash), _| {
            *id != space.id || (!have.contains(hash) && active_assets.contains_key(hash))
        })
    });
    FETCH_ATTEMPTS.with(|attempts| {
        attempts.borrow_mut().retain(|(id, hash), _| {
            *id != space.id || (!have.contains(hash) && active_assets.contains_key(hash))
        })
    });
    FETCHING_HASHES.with(|fetching| {
        fetching.borrow_mut().retain(|(id, hash)| {
            *id != space.id || (!have.contains(hash) && active_assets.contains_key(hash))
        })
    });
    if !purge.is_empty() {
        PENDING_PURGES.with(|pending| pending.borrow_mut().insert(space.id));
        resume_pending_imports();
    }
    let missing = active_assets
        .into_iter()
        .filter_map(|(hash, asset)| (!have.contains(&hash)).then_some(asset))
        .collect();
    fetch_missing(ui, space, missing);
}

fn start_peer_sync(peer_id: &str) {
    for space in spaces()
        .into_iter()
        .filter(|space| space.is_member(peer_id))
    {
        PULL_CURSORS.with(|cursors| {
            cursors
                .borrow_mut()
                .insert((peer_id.to_string(), space.id), None);
        });
    }
    crate::share_write_line(&format!(
        "SPACE_SYNC\t{peer_id}\t{}",
        peer_addrs(peer_id).join(";")
    ));
}

fn request_ops_page(peer_id: &str, space_id: Uuid, after: Option<ui_viewmodels::OpKey>) {
    let message = SpaceMessage::PullOps {
        space_id,
        after: after.clone(),
    };
    if let Some(line) = ui_viewmodels::space_send_line(peer_id, &peer_addrs(peer_id), message) {
        PULL_CURSORS.with(|cursors| {
            cursors
                .borrow_mut()
                .insert((peer_id.to_string(), space_id), after);
        });
        crate::share_write_line(&line);
    }
}

/// JoinOk 应用（加入侧冷启动）：建根 + 落册 + 合并 op log + 进入 + 补字节。
fn apply_join_ok(ui: &AppWindow, peer_id: &str, meta: ui_viewmodels::SpaceMeta, ops: Vec<SpaceOp>) {
    let id = meta.id;
    let expected = SPACE_JOIN_PENDING.with(|pending| pending.borrow().clone());
    let self_id = SPACES.with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()));
    if !expected
        .as_ref()
        .is_some_and(|(expected_id, owner)| *expected_id == id && owner == peer_id)
        || meta.owner_id != peer_id
        || meta.validate().is_err()
        || !self_id.as_ref().is_some_and(|id| meta.is_member(id))
        || ops.len() > MAX_OPS_PER_MESSAGE
        || ops
            .iter()
            .any(|op| op.space_id != id || op.validate().is_err())
    {
        logging::warn!("丢弃非预期团队加入应答 peer={peer_id} team={id}");
        return;
    }
    SPACE_JOIN_PENDING.with(|pending| pending.borrow_mut().take());
    let Some(root) = space_root_of(id) else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(&root) {
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Error,
            format!("初始化团队目录失败：{error}"),
        );
        return;
    }
    if let Err(error) = ui_viewmodels::ensure_library_skeleton(&root) {
        notice(
            ui,
            ui_viewmodels::notifications::Severity::Error,
            format!("初始化团队库失败：{error}"),
        );
        return;
    }
    let mut sorted = ops;
    ui_viewmodels::sort_and_dedupe_ops(&mut sorted);
    let existing = load_ops(id);
    let known: std::collections::HashSet<(u64, String)> = existing
        .iter()
        .map(|op| (op.lamport, op.author.clone()))
        .collect();
    let fresh: Vec<SpaceOp> = sorted
        .iter()
        .filter(|op| !known.contains(&(op.lamport, op.author.clone())))
        .cloned()
        .collect();
    if !append_ops(id, &fresh) {
        ui.set_space_ctl_error("团队操作记录写入失败，请重新加入".into());
        return;
    }
    let mut merged = existing;
    merged.extend(sorted.iter().cloned());
    ui_viewmodels::sort_and_dedupe_ops(&mut merged);
    let missing = missing_from_ops(&root, &merged);
    let name = meta.name.clone();
    if SPACES
        .with(|vm| vm.borrow_mut().apply_join(meta, now_secs()))
        .is_err()
    {
        return;
    }
    ONLINE_PEERS.with(|online| {
        online.borrow_mut().entry(peer_id.to_string()).or_default();
    });
    SYNCED_PEERS.with(|synced| {
        synced.borrow_mut().insert(peer_id.to_string());
    });
    persist();
    push_space_state();
    sync_rows(ui);
    notice(
        ui,
        ui_viewmodels::notifications::Severity::Success,
        format!("已加入团队「{name}」，素材会自动同步到本机"),
    );
    enter_space(id);
    if sorted.len() >= MAX_OPS_PER_MESSAGE {
        request_ops_page(peer_id, id, None);
    } else if !missing.is_empty() {
        if let Some(space) = space_by_id(id) {
            fetch_missing(ui, &space, missing);
        }
    }
    push_space_state();
}

/// op log 里 Add 了、本地库还没有字节的素材集（join 冷启动/对账共用）。
fn missing_from_ops(root: &Path, ops: &[SpaceOp]) -> Vec<SpaceAssetMeta> {
    let have: std::collections::HashSet<Vec<u8>> = ui_viewmodels::content_paths(root)
        .into_iter()
        .map(|(hash, _uuid, _path)| hash)
        .collect();
    active_space_assets(ops)
        .into_iter()
        .filter_map(|(hash, asset)| (!have.contains(&hash)).then_some(asset))
        .collect()
}

fn prune_pending_fetch(
    id: Uuid,
    active_assets: &std::collections::HashMap<Vec<u8>, SpaceAssetMeta>,
) {
    PENDING_FETCH.with(|pending| {
        let mut pending = pending.borrow_mut();
        for entry in pending.iter_mut().filter(|entry| entry.space_id == id) {
            entry
                .items
                .retain(|asset| active_assets.contains_key(&asset.content_hash));
        }
        pending.retain(|entry| !entry.items.is_empty());
    });
}

/// 挂起的字节索取条目（空间 + 素材 + 是否被大小闸挡下）。
struct PendingFetch {
    space_id: Uuid,
    items: Vec<SpaceAssetMeta>,
    gated: bool,
}

fn push_pending(space_id: Uuid, items: Vec<SpaceAssetMeta>, gated: bool) {
    PENDING_FETCH.with(|slot| {
        let mut pending = slot.borrow_mut();
        let mut known: std::collections::HashSet<_> = pending
            .iter()
            .filter(|entry| entry.space_id == space_id)
            .flat_map(|entry| entry.items.iter().map(|asset| asset.content_hash.clone()))
            .collect();
        let items: Vec<_> = items
            .into_iter()
            .filter(|asset| known.insert(asset.content_hash.clone()))
            .collect();
        if !items.is_empty() {
            pending.push(PendingFetch {
                space_id,
                items,
                gated,
            });
        }
    });
}

fn defer_failed_fetch(space_id: Uuid) {
    FETCHING_HASHES.with(|fetching| {
        fetching.borrow_mut().retain(|(id, _)| *id != space_id);
    });
    if space_by_id(space_id).is_none() {
        return;
    }
    let Some(root) = space_root_of(space_id) else {
        return;
    };
    let items = missing_from_ops(&root, &load_ops(space_id));
    push_pending(space_id, items, false);
    if let Some(ui) = host().and_then(|host| host.ui.upgrade()) {
        refresh_pending(&ui);
    }
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
    let items: Vec<_> = FETCHING_HASHES.with(|fetching| {
        let mut fetching = fetching.borrow_mut();
        items
            .into_iter()
            .filter(|asset| fetching.insert((space.id, asset.content_hash.clone())))
            .collect()
    });
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
                "「{}」有 {} 项新素材（{}），超过同步上限 {}MB，已挂起——可在「团队」里手动下载",
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
                "团队「{}」{} 项待补字节，暂无在线成员（上线自动续传）",
                space.name,
                items.len()
            );
            push_pending(space.id, items, false);
        }
    }
}

fn try_pending_fetch() {
    FETCH_ATTEMPTS.with(|attempts| attempts.borrow_mut().clear());
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
    PENDING_FETCH.with(|slot| slot.borrow_mut().extend(kept));
}

fn handle_unavailable_fetch(space_id: Uuid, peer: &str, hashes: Vec<Vec<u8>>) {
    let Some(space) = space_by_id(space_id) else {
        return;
    };
    let Some(root) = space_root_of(space_id) else {
        return;
    };
    let missing = missing_from_ops(&root, &load_ops(space_id));
    for hash in hashes {
        let was_requested = FETCH_SOURCES.with(|sources| {
            sources
                .borrow()
                .get(&(space_id, hash.clone()))
                .is_some_and(|source| source == peer)
        });
        if !was_requested {
            continue;
        }
        FETCH_SOURCES.with(|sources| sources.borrow_mut().remove(&(space_id, hash.clone())));
        FETCHING_HASHES.with(|fetching| fetching.borrow_mut().remove(&(space_id, hash.clone())));
        let Some(asset) = missing.iter().find(|asset| asset.content_hash == hash) else {
            continue;
        };
        let next = space.members.iter().find(|member| {
            Some(member.id.as_str())
                != SPACES
                    .with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()))
                    .as_deref()
                && ONLINE_PEERS.with(|peers| peers.borrow().contains_key(&member.id))
                && FETCH_ATTEMPTS.with(|attempts| {
                    !attempts
                        .borrow()
                        .get(&(space_id, hash.clone()))
                        .is_some_and(|peers| peers.contains(&member.id))
                })
        });
        if let Some(next) = next {
            send_fetch(
                space_id,
                vec![asset.clone()],
                &next.id,
                &peer_addrs(&next.id),
            );
        } else {
            push_pending(space_id, vec![asset.clone()], false);
        }
    }
}

fn send_fetch(space_id: Uuid, items: Vec<SpaceAssetMeta>, peer: &str, addrs: &[String]) {
    FETCH_SOURCES.with(|sources| {
        let mut sources = sources.borrow_mut();
        for asset in &items {
            sources.insert((space_id, asset.content_hash.clone()), peer.to_string());
        }
    });
    FETCH_ATTEMPTS.with(|attempts| {
        let mut attempts = attempts.borrow_mut();
        for asset in &items {
            attempts
                .entry((space_id, asset.content_hash.clone()))
                .or_default()
                .insert(peer.to_string());
        }
    });
    for chunk in items.chunks(MAX_FETCH_ITEMS) {
        let Some(line) = ui_viewmodels::space_send_line(
            peer,
            addrs,
            SpaceMessage::Fetch {
                space_id,
                items: chunk.to_vec(),
            },
        ) else {
            FETCHING_HASHES.with(|fetching| {
                let mut fetching = fetching.borrow_mut();
                for asset in chunk {
                    fetching.remove(&(space_id, asset.content_hash.clone()));
                }
            });
            push_pending(space_id, chunk.to_vec(), false);
            continue;
        };
        FETCHING_HASHES.with(|fetching| {
            let mut fetching = fetching.borrow_mut();
            for asset in chunk {
                fetching.insert((space_id, asset.content_hash.clone()));
            }
        });
        if !crate::share_write_line(&line) {
            FETCHING_HASHES.with(|fetching| {
                let mut fetching = fetching.borrow_mut();
                for asset in chunk {
                    fetching.remove(&(space_id, asset.content_hash.clone()));
                }
            });
            push_pending(space_id, chunk.to_vec(), false);
        }
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
    // D93 单团队守卫：已绑定 → 只放行同团队重入提示；换绑必须先退出
    //（退出 = 本地副本删除的显式知情动作，代替换绑确认框）。
    if let Some(bound) = bound_space() {
        if bound.id != parsed.space_id {
            ui.set_space_ctl_error(
                format!("已绑定团队「{}」。先退出当前团队，再贴新码换绑", bound.name).into(),
            );
        } else {
            ui.set_space_ctl_error("已经在该团队里了".into());
        }
        return;
    }
    let _ = self_id;
    SPACE_JOIN_PENDING.with(|pending| {
        *pending.borrow_mut() = Some((parsed.space_id, parsed.owner_id.clone()));
    });
    crate::share_write_line(&format!(
        "SPACE_JOIN\t{}\t{}\t{self_name}",
        parsed.owner_id, code
    ));
    notice(
        ui,
        ui_viewmodels::notifications::Severity::Info,
        "正在连接团队主…对方不在线时稍后会提示失败".to_string(),
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
        crate::share_write_line(&format!("REJECT\t{}\t导入服务不可用", manifest.batch_id));
        return true;
    };
    let authenticated_sender = ui_viewmodels::authenticated_transfer_sender(payload);
    let known = space_by_id(space_id).is_some_and(|space| {
        authenticated_sender
            .as_ref()
            .is_some_and(|id| space.is_member(id))
            && SPACES.with(|vm| {
                vm.borrow()
                    .identity()
                    .is_some_and(|(id, _)| space.is_member(id))
            })
    });
    if known {
        SPACE_BATCHES.with(|slot| {
            slot.borrow_mut().insert(
                manifest.batch_id,
                ReceivedBatch {
                    space_id,
                    hashes: manifest
                        .items
                        .iter()
                        .map(|item| item.sha256_lower())
                        .collect(),
                },
            );
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

/// 暂存成功不代表入库成功；回执只由本批次的导入终态发出。
fn try_space_staged(payload: &str) -> bool {
    let Some((batch, paths)) = ui_viewmodels::parse_staged_payload(payload) else {
        return false;
    };
    let Some(received) = SPACE_BATCHES.with(|slot| slot.borrow_mut().remove(&batch)) else {
        return false;
    };
    let Some(root) = space_root_of(received.space_id) else {
        crate::share_write_line(&format!("NACK\t{batch}\t团队目录不可用"));
        return true;
    };
    if host().is_none() {
        crate::share_write_line(&format!("NACK\t{batch}\t导入服务不可用"));
        return true;
    }
    let queued = STAGED_IMPORTS.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.len() >= 64 {
            return false;
        }
        pending.push_back((batch, received, root, paths));
        true
    });
    if queued {
        resume_pending_imports();
    } else {
        crate::share_write_line(&format!("NACK\t{batch}\t接收队列已满"));
    }
    true
}

pub fn resume_pending_imports() {
    let Some(scope_host) = host() else { return };
    while !scope_host.importing.load(Ordering::SeqCst) {
        if let Some(id) = PENDING_PURGES.with(|pending| pending.borrow_mut().pop_first()) {
            if let Some(root) = space_root_of(id).filter(|_| space_by_id(id).is_some()) {
                let ops = load_ops(id);
                let active = active_space_assets(&ops);
                let known: std::collections::HashSet<_> = ops
                    .iter()
                    .filter_map(|op| match &op.op {
                        SpaceOpKind::Add { asset } => Some(asset.content_hash.clone()),
                        _ => None,
                    })
                    .collect();
                let uuids: Vec<_> = ui_viewmodels::content_paths(&root)
                    .into_iter()
                    .filter(|(hash, _, _)| known.contains(hash) && !active.contains_key(hash))
                    .map(|(_, uuid, _)| uuid)
                    .collect();
                if !uuids.is_empty() {
                    spawn_purge(root, uuids, scope_host.importing.clone());
                }
            }
            continue;
        }
        let next = STAGED_IMPORTS.with(|pending| pending.borrow_mut().pop_front());
        let Some((batch, received, root, paths)) = next else {
            break;
        };
        let space_id = received.space_id;
        if space_by_id(space_id).is_none() || space_root_of(space_id).as_ref() != Some(&root) {
            crate::share_write_line(&format!("NACK\t{batch}\t目标团队已变化"));
            continue;
        }
        add_paths_to_space_for_batch(
            space_id,
            Some((batch, received.hashes)),
            paths,
            scope_host.importing.clone(),
        );
    }
}

pub fn remove_assets(id: Uuid, local_uuids: &[String]) -> Result<(), String> {
    let space = space_by_id(id).ok_or("目标团队不存在")?;
    let root = space_root_of(id).ok_or("团队目录不可用")?;
    let author = SPACES
        .with(|vm| vm.borrow().identity().map(|(id, _)| id.to_string()))
        .ok_or("共享引擎尚未就绪")?;
    if !emit_missing_add_ops(id, false) {
        return Err("团队操作记录写入失败".into());
    }
    let ops = load_ops(id);
    let chosen: std::collections::HashSet<_> = local_uuids.iter().collect();
    let hashes: std::collections::HashSet<_> = ui_viewmodels::content_paths(&root)
        .into_iter()
        .filter(|(_, uuid, _)| chosen.contains(uuid))
        .map(|(hash, _, _)| hash)
        .collect();
    let mut active = std::collections::BTreeMap::new();
    let mut added_by = std::collections::HashMap::new();
    for op in &ops {
        match &op.op {
            SpaceOpKind::Add { asset } => {
                active.insert(asset.uuid, asset.clone());
                added_by.insert(asset.uuid, op.author.clone());
            }
            SpaceOpKind::Remove { asset_uuid } => {
                active.remove(asset_uuid);
            }
            _ => {}
        }
    }
    let lamport = ops.iter().map(|op| op.lamport).max().unwrap_or(0);
    let mut fresh = Vec::new();
    for (offset, asset) in active
        .into_values()
        .filter(|asset| hashes.contains(&asset.content_hash))
        .enumerate()
    {
        let lamport = lamport
            .checked_add(offset as u64 + 1)
            .ok_or("团队操作序号已达上限")?;
        let op = SpaceOp {
            space_id: id,
            lamport,
            author: author.clone(),
            ts: now_secs(),
            op: SpaceOpKind::Remove {
                asset_uuid: asset.uuid,
            },
        };
        if !op.permits(&space, &added_by) {
            return Err("只有团队主或素材添加者可以移除该素材".into());
        }
        fresh.push(op);
    }
    if !append_ops(id, &fresh) {
        return Err("团队操作记录写入失败".into());
    }
    push_space_state();
    broadcast_ops(id, &fresh);
    if let Some(ui) = host().and_then(|host| host.ui.upgrade()) {
        materialize_space_ops(&ui, &space, &load_ops(id));
    }
    refresh_share_projection(true);
    Ok(())
}

/// 空间库 purge（Remove op 物化）：单写者纪律，子进程执行。
fn spawn_purge(root: PathBuf, uuids: Vec<String>, importing: Arc<AtomicBool>) {
    if importing
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        if let Some(id) = space_id_of_root(&root) {
            PENDING_PURGES.with(|pending| pending.borrow_mut().insert(id));
        }
        return;
    }
    let mut args = vec![
        "--cmd".into(),
        "purge".into(),
        "--library".into(),
        root.to_string_lossy().into_owned(),
    ];
    for uuid in uuids {
        args.extend(["--uuid".into(), uuid]);
    }
    let _ = crate::ChildTask::new(crate::helper_exe("sample-library.exe"), args)
        .with_finished(move |success, message| {
            let root = root.clone();
            let importing_ui = importing.clone();
            let queued = slint::invoke_from_event_loop(move || {
                importing_ui.store(false, Ordering::SeqCst);
                if success {
                    push_space_state();
                    crate::refresh_import_target(&root);
                    if active().is_some_and(|scope| scope.root == root) {
                        if let Some(id) = space_id_of_root(&root) {
                            enter_space(id);
                        }
                    }
                    refresh_share_projection(true);
                } else {
                    logging::warn!("团队 Remove 物化失败: {message}");
                }
                resume_pending_imports();
            });
            if queued.is_err() {
                importing.store(false, Ordering::SeqCst);
                logging::warn!("团队 Remove 完成事件无法送达 UI");
            }
        })
        .run_in_background();
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{
        platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        ComponentHandle, VecModel,
    };
    use ui_viewmodels::{FacetIndex, ShareVm, Sorter};

    type UiEvent = Box<dyn FnOnce() + Send>;
    static TEST_EVENTS: std::sync::Mutex<Option<std::sync::mpsc::Sender<UiEvent>>> =
        std::sync::Mutex::new(None);

    thread_local! {
        static TEST_PROXY_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    struct TestProxy;

    impl slint::platform::EventLoopProxy for TestProxy {
        fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
            Ok(())
        }
        fn invoke_from_event_loop(&self, event: UiEvent) -> Result<(), slint::EventLoopError> {
            let sender = TEST_EVENTS.lock().unwrap().clone();
            sender
                .ok_or(slint::EventLoopError::EventLoopTerminated)?
                .send(event)
                .map_err(|_| slint::EventLoopError::EventLoopTerminated)
        }
    }

    struct TestPlatform;

    impl slint::platform::Platform for TestPlatform {
        fn new_event_loop_proxy(&self) -> Option<Box<dyn slint::platform::EventLoopProxy>> {
            TEST_PROXY_ENABLED.with(|enabled| {
                enabled
                    .get()
                    .then(|| Box::new(TestProxy) as Box<dyn slint::platform::EventLoopProxy>)
            })
        }
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
        }
    }

    struct Fixture {
        dir: PathBuf,
        ui: AppWindow,
        owner: String,
    }

    impl Fixture {
        fn new() -> Self {
            slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
            let dir = std::env::temp_dir().join(format!("space-worker-state-{}", Uuid::new_v4()));
            let main_root = dir.join("main");
            std::fs::create_dir_all(&main_root).unwrap();
            ui_viewmodels::ensure_library_skeleton(&main_root).unwrap();
            init(&dir.join("spaces.json"), dir.join("spaces"));
            set_library_root(Some(main_root.to_string_lossy().into_owned()));
            let owner = "a".repeat(52);
            SPACES.with(|slot| {
                let mut vm = slot.borrow_mut();
                vm.set_identity(&owner, "owner");
                vm.take_dirty();
            });
            let ui = AppWindow::new().unwrap();
            let vm = Rc::new(RefCell::new(LibraryGridVm::new(
                FacetIndex::new(),
                Sorter::default(),
                1,
            )));
            let resolver = Rc::new(RefCell::new(None));
            let thumb_cache = Rc::new(RefCell::new(ThumbCache::new(1)));
            let grid = Rc::new(GridCtx::new(
                ui.as_weak(),
                vm.clone(),
                Rc::new(VecModel::default()),
                resolver.clone(),
                thumb_cache.clone(),
                Rc::new(RefCell::new(ShareVm::new())),
            ));
            install_host(Rc::new(SpaceHost {
                ui: ui.as_weak(),
                vm,
                resolver,
                grid,
                filter_categories: Rc::new(RefCell::new(Vec::new())),
                filter_category_ids: Rc::new(RefCell::new(CategoryRows::default())),
                current_filter: Rc::new(RefCell::new(Filter::All)),
                filter_label: Rc::new(RefCell::new("".into())),
                thumb_cache,
                importing: Arc::new(AtomicBool::new(false)),
            }));
            Self { dir, ui, owner }
        }

        // Match main.rs: READY is observed without being swallowed, then identity
        // is filled back after every event. No worker or network is started.
        fn event(&self, line: &str) {
            assert!(!intercept_worker_line(line));
            handle_worker_ready(&self.owner, "owner");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            HOST.with(|slot| *slot.borrow_mut() = None);
            ACTIVE.with(|slot| *slot.borrow_mut() = None);
            SPACES.with(|slot| *slot.borrow_mut() = SpaceVm::new());
            SPACES_PATH.with(|slot| *slot.borrow_mut() = None);
            SPACES_DIR.with(|slot| *slot.borrow_mut() = None);
            LIBRARY_ROOT.with(|slot| *slot.borrow_mut() = None);
            WORKER_SESSION_PENDING.with(|pending| pending.set(false));
            ONLINE_PEERS.with(|peers| peers.borrow_mut().clear());
            SYNCED_PEERS.with(|peers| peers.borrow_mut().clear());
            PUBLISHED_OPS.with(|ops| ops.borrow_mut().clear());
            SPACE_JOIN_PENDING.with(|pending| pending.borrow_mut().take());
            SPACE_BATCHES.with(|batches| batches.borrow_mut().clear());
            PENDING_FETCH.with(|pending| pending.borrow_mut().clear());
            STAGED_IMPORTS.with(|pending| pending.borrow_mut().clear());
            PENDING_PURGES.with(|pending| pending.borrow_mut().clear());
            FETCHING_HASHES.with(|fetching| fetching.borrow_mut().clear());
            FETCH_ATTEMPTS.with(|attempts| attempts.borrow_mut().clear());
            FETCH_SOURCES.with(|sources| sources.borrow_mut().clear());
            PULL_CURSORS.with(|cursors| cursors.borrow_mut().clear());
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    // Restores the real stdin slot and reaps the fake child even on assertion
    // panic, so a regression cannot leak a pipe/process into another test.
    struct PipeCapture {
        child: Option<std::process::Child>,
        previous: Option<std::process::ChildStdin>,
    }

    impl Drop for PipeCapture {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                crate::SHARE_STDIN.with(|slot| *slot.borrow_mut() = self.previous.take());
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    // A local pass-through child supplies a real ChildStdin to the production
    // share_write_line boundary. Only synthetic state from Fixture is sent.
    fn sent_lines(action: impl FnOnce()) -> Vec<String> {
        let exe =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/findstr.exe");
        let mut child = std::process::Command::new(exe)
            .arg("^")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let previous = crate::SHARE_STDIN.with(|slot| slot.replace(child.stdin.take()));
        let mut capture = PipeCapture {
            child: Some(child),
            previous,
        };
        action();
        crate::SHARE_STDIN.with(|slot| *slot.borrow_mut() = capture.previous.take());
        let output = capture.child.take().unwrap().wait_with_output().unwrap();
        assert!(output.status.success() || output.status.code() == Some(1));
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn only_book(lines: &[String]) -> SpaceBookState {
        assert_eq!(
            lines.len(),
            1,
            "one registry mutation must send exactly one state"
        );
        let json = lines[0]
            .strip_prefix("SPACE_STATE\t")
            .expect("expected a space state command");
        serde_json::from_str(json).expect("sent state must be valid JSON")
    }

    fn member_space(fixture: &Fixture) -> (SpaceMeta, String) {
        let id = SPACES.with(|slot| slot.borrow_mut().create_space("team", 1).unwrap());
        let peer = "b".repeat(52);
        let mut meta = space_by_id(id).unwrap();
        meta.members.push(ui_viewmodels::RosterMember {
            id: peer.clone(),
            name: "member".into(),
        });
        SPACES.with(|slot| slot.borrow_mut().apply_join(meta.clone(), 1).unwrap());
        assert!(meta.is_member(&fixture.owner));
        (meta, peer)
    }

    fn sample_add(meta: &SpaceMeta, lamport: u64) -> SpaceOp {
        SpaceOp {
            space_id: meta.id,
            lamport,
            author: meta.owner_id.clone(),
            ts: 1,
            op: SpaceOpKind::Add {
                asset: SpaceAssetMeta {
                    uuid: Uuid::new_v4(),
                    file_name: "sample.txt".into(),
                    kind: ui_viewmodels::SpaceAssetKind::Text,
                    size_bytes: 4,
                    content_hash: vec![lamport as u8; 32],
                },
            },
        }
    }

    #[test]
    fn startup_syncs_authorized_members_without_device_discovery() {
        let fixture = Fixture::new();
        let (_, peer) = member_space(&fixture);
        let lines = sent_lines(|| fixture.event(&format!("READY\t{}\towner", fixture.owner)));
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("SPACE_SYNC\t{peer}\t")),
            "persisted members must be dialed without DEVICE"
        );
    }

    #[test]
    fn authenticated_ops_fetch_without_device_and_replays_stop() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let op = sample_add(&meta, 1);
        let lines = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &peer,
                SpaceMessage::Ops {
                    ops: vec![op.clone()],
                },
            )
        });
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&format!("SYNC_SEND\t{peer}\t"))
                    && line.contains("\"fetch\"")),
            "an authenticated member must be usable for missing bytes without DEVICE"
        );
        assert!(
            lines.iter().any(|line| line.starts_with("SPACE_SYNC\t")),
            "new ops must continue paginated pull"
        );
        let replay = sent_lines(|| {
            handle_space_message(&fixture.ui, &peer, SpaceMessage::Ops { ops: vec![op] })
        });
        assert!(
            replay.is_empty(),
            "replayed ops must not create a sync loop"
        );
    }

    #[test]
    fn joining_without_device_fetches_existing_assets() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        SPACES.with(|slot| {
            slot.borrow_mut().leave_space(meta.id);
        });
        let mut remote = meta;
        remote.owner_id = peer.clone();
        let op = sample_add(&remote, 1);
        SPACE_JOIN_PENDING.with(|pending| *pending.borrow_mut() = Some((remote.id, peer.clone())));
        let lines = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &peer,
                SpaceMessage::JoinOk {
                    meta: remote,
                    ops: vec![op],
                },
            )
        });
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&format!("SYNC_SEND\t{peer}\t"))
                    && line.contains("\"fetch\"")),
            "JoinOk must immediately fetch existing bytes without DEVICE"
        );
    }

    #[test]
    fn completed_team_import_publishes_new_ops_after_membership_sync() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let root = space_root_of(meta.id).unwrap();
        ui_viewmodels::ensure_library_skeleton(&root).unwrap();
        append_ops(meta.id, &[sample_add(&meta, 1)]);
        let lines = sent_lines(|| on_space_import_finished(&root, false).unwrap());
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&format!("SYNC_SEND\t{peer}\t"))
                    && line.contains("\"ops\"")),
            "completed team changes must actively publish operations, not only update a local book"
        );
    }

    #[test]
    fn removed_assets_are_not_fetched_or_retained_in_pending() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let add = sample_add(&meta, 1);
        let SpaceOpKind::Add { asset } = &add.op else {
            unreachable!()
        };
        let asset = asset.clone();
        push_pending(meta.id, vec![asset.clone()], true);
        let remove = SpaceOp {
            space_id: meta.id,
            lamport: 2,
            author: meta.owner_id.clone(),
            ts: 2,
            op: SpaceOpKind::Remove {
                asset_uuid: asset.uuid,
            },
        };
        let lines = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &peer,
                SpaceMessage::Ops {
                    ops: vec![remove.clone(), add.clone()],
                },
            )
        });
        assert!(!lines.iter().any(|line| line.contains("\"fetch\"")));
        assert_eq!(pending_sync_count(), 0);
        let root = space_root_of(meta.id).unwrap();
        assert!(missing_from_ops(&root, &[add, remove]).is_empty());
    }

    #[test]
    fn unsolicited_join_does_not_consume_expected_owner() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        SPACES.with(|slot| {
            slot.borrow_mut().leave_space(meta.id);
        });
        SPACE_JOIN_PENDING.with(|pending| *pending.borrow_mut() = Some((meta.id, peer.clone())));
        let lines = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &"c".repeat(52),
                SpaceMessage::JoinOk {
                    meta: meta.clone(),
                    ops: Vec::new(),
                },
            )
        });
        assert!(lines.is_empty());
        assert!(space_by_id(meta.id).is_none());
        assert_eq!(
            SPACE_JOIN_PENDING.with(|pending| pending.borrow().clone()),
            Some((meta.id, peer))
        );
    }

    #[test]
    fn empty_pages_and_nonmember_ops_do_not_echo() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        assert!(sent_lines(|| handle_space_message(
            &fixture.ui,
            &peer,
            SpaceMessage::Ops { ops: Vec::new() }
        ))
        .is_empty());
        assert!(sent_lines(|| handle_space_message(
            &fixture.ui,
            &"c".repeat(52),
            SpaceMessage::Ops {
                ops: vec![sample_add(&meta, 1)]
            }
        ))
        .is_empty());
        assert!(load_ops(meta.id).is_empty());
    }

    #[test]
    fn staged_busy_batch_waits_for_idle_without_early_receipt() {
        let fixture = Fixture::new();
        let (meta, _) = member_space(&fixture);
        let batch = Uuid::new_v4();
        SPACE_BATCHES.with(|batches| {
            batches.borrow_mut().insert(
                batch,
                ReceivedBatch {
                    space_id: meta.id,
                    hashes: vec!["00".repeat(32)],
                },
            )
        });
        host().unwrap().importing.store(true, Ordering::SeqCst);
        let payload = format!(
            "{batch}\t{}",
            serde_json::to_string(&vec![fixture.dir.join("sample.txt")]).unwrap()
        );
        let lines = sent_lines(|| assert!(try_space_staged(&payload)));
        assert!(
            lines.is_empty(),
            "a staged batch waits without claiming delivery"
        );
        assert_eq!(STAGED_IMPORTS.with(|pending| pending.borrow().len()), 1);
        assert!(sent_lines(|| assert!(!try_space_staged(&payload))).is_empty());
        SPACES.with(|spaces| {
            spaces.borrow_mut().leave_space(meta.id);
        });
        host().unwrap().importing.store(false, Ordering::SeqCst);
        let refused = sent_lines(resume_pending_imports);
        assert_eq!(
            refused
                .iter()
                .filter(|line| line.starts_with(&format!("NACK\t{batch}\t")))
                .count(),
            1
        );
        assert!(STAGED_IMPORTS.with(|pending| pending.borrow().is_empty()));
    }

    #[test]
    fn incoming_team_bytes_require_authenticated_member_metadata() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let batch = Uuid::new_v4();
        let mut manifest = serde_json::json!({
            "batch_id": batch,
            "sender_name": "member",
            "space_id": meta.id,
            "items": [{
                "asset_uuid": Uuid::new_v4(),
                "file_name": "sample.txt",
                "kind": ui_viewmodels::AssetKind::Text,
                "size_bytes": 4,
                "sha256_hex": "00".repeat(32),
                "category_hint": null
            }]
        });
        let missing = sent_lines(|| assert!(try_auto_accept(&manifest.to_string())));
        assert!(missing
            .iter()
            .any(|line| line.starts_with(&format!("REJECT\t{batch}\t"))));
        manifest["authenticated_sender_id"] = serde_json::json!("c".repeat(52));
        let outsider = sent_lines(|| assert!(try_auto_accept(&manifest.to_string())));
        assert!(outsider
            .iter()
            .any(|line| line.starts_with(&format!("REJECT\t{batch}\t"))));
        manifest["authenticated_sender_id"] = serde_json::json!(peer);
        let member = sent_lines(|| assert!(try_auto_accept(&manifest.to_string())));
        assert_eq!(member, vec![format!("ACCEPT\t{batch}")]);
    }

    #[test]
    fn cursor_pages_defer_materialization_and_replays_terminate() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let add = sample_add(&meta, 1);
        let SpaceOpKind::Add { asset } = &add.op else {
            unreachable!()
        };
        let remove = SpaceOp {
            space_id: meta.id,
            lamport: 2,
            author: meta.owner_id.clone(),
            ts: 2,
            op: SpaceOpKind::Remove {
                asset_uuid: asset.uuid,
            },
        };
        let _ = sent_lines(|| start_peer_sync(&peer));
        let first = SpaceMessage::OpsPage {
            space_id: meta.id,
            after: None,
            next: Some(ui_viewmodels::OpKey::of(&add)),
            ops: vec![add.clone()],
        };
        let lines = sent_lines(|| handle_space_message(&fixture.ui, &peer, first.clone()));
        assert!(lines.iter().any(|line| line.contains("\"pull_ops\"")));
        assert!(!lines.iter().any(|line| line.contains("\"fetch\"")));
        assert!(sent_lines(|| handle_space_message(&fixture.ui, &peer, first)).is_empty());
        let middle = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &peer,
                SpaceMessage::OpsPage {
                    space_id: meta.id,
                    after: Some(ui_viewmodels::OpKey::of(&add)),
                    next: Some(ui_viewmodels::OpKey::of(&remove)),
                    ops: vec![remove.clone()],
                },
            )
        });
        assert!(middle.iter().any(|line| line.contains("\"pull_ops\"")));
        assert!(!middle.iter().any(|line| line.contains("\"fetch\"")));
        let terminal = SpaceMessage::OpsPage {
            space_id: meta.id,
            after: Some(ui_viewmodels::OpKey::of(&remove)),
            next: None,
            ops: Vec::new(),
        };
        let last = sent_lines(|| handle_space_message(&fixture.ui, &peer, terminal.clone()));
        assert!(!last
            .iter()
            .any(|line| line.contains("\"pull_ops\"") || line.contains("\"fetch\"")));
        assert!(PULL_CURSORS.with(|cursors| cursors.borrow().is_empty()));
        assert!(sent_lines(|| handle_space_message(&fixture.ui, &peer, terminal)).is_empty());
        assert_eq!(load_ops(meta.id).len(), 2);
    }

    #[test]
    fn restarting_sync_resets_cursor_and_failed_log_writes_do_not_advance() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let op = sample_add(&meta, 1);
        PULL_CURSORS.with(|cursors| {
            cursors
                .borrow_mut()
                .insert((peer.clone(), meta.id), Some(ui_viewmodels::OpKey::of(&op)));
        });
        let _ = sent_lines(|| start_peer_sync(&peer));
        assert_eq!(
            PULL_CURSORS.with(|cursors| cursors.borrow().get(&(peer.clone(), meta.id)).cloned()),
            Some(None)
        );
        std::fs::create_dir_all(ops_path(meta.id).unwrap()).unwrap();
        let lines = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &peer,
                SpaceMessage::OpsPage {
                    space_id: meta.id,
                    after: None,
                    next: Some(ui_viewmodels::OpKey::of(&op)),
                    ops: vec![op],
                },
            )
        });
        assert!(lines.is_empty());
        assert!(load_ops(meta.id).is_empty());
        assert_eq!(
            PULL_CURSORS.with(|cursors| cursors.borrow().get(&(peer.clone(), meta.id)).cloned()),
            Some(None)
        );
    }

    #[test]
    fn failed_fetch_waits_for_a_new_event_and_deduplicates_pending_items() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let op = sample_add(&meta, 1);
        let _ = sent_lines(|| {
            handle_space_message(&fixture.ui, &peer, SpaceMessage::Ops { ops: vec![op] })
        });
        assert_eq!(FETCHING_HASHES.with(|hashes| hashes.borrow().len()), 1);
        let failure = format!("SPACE_FETCH_FAILED\t{peer}\t{}", meta.id);
        assert!(sent_lines(|| assert!(intercept_worker_line(&failure))).is_empty());
        assert!(sent_lines(|| assert!(intercept_worker_line(&failure))).is_empty());
        assert!(FETCHING_HASHES.with(|hashes| hashes.borrow().is_empty()));
        assert_eq!(
            PENDING_FETCH.with(|pending| pending
                .borrow()
                .iter()
                .map(|entry| entry.items.len())
                .sum::<usize>()),
            1
        );
        let retried =
            sent_lines(|| fixture.event(&format!("DEVICE\t{peer}\tmember\t127.0.0.1:9000")));
        assert_eq!(
            retried
                .iter()
                .filter(|line| line.contains("\"fetch\""))
                .count(),
            1
        );
        assert_eq!(pending_sync_count(), 0);
    }

    #[test]
    fn unavailable_holder_tries_each_member_once_then_waits() {
        let fixture = Fixture::new();
        let (mut meta, peer) = member_space(&fixture);
        let other = "c".repeat(52);
        meta.members.push(ui_viewmodels::RosterMember {
            id: other.clone(),
            name: "replica".into(),
        });
        SPACES.with(|vm| vm.borrow_mut().apply_join(meta.clone(), 1).unwrap());
        ONLINE_PEERS.with(|peers| {
            peers.borrow_mut().insert(peer.clone(), Vec::new());
            peers.borrow_mut().insert(other.clone(), Vec::new());
        });
        let op = sample_add(&meta, 1);
        let SpaceOpKind::Add { asset } = &op.op else {
            unreachable!()
        };
        let asset = asset.clone();
        assert!(append_ops(meta.id, &[op]));
        let _ = sent_lines(|| send_fetch(meta.id, vec![asset.clone()], &peer, &[]));
        let lines = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &peer,
                SpaceMessage::FetchUnavailable {
                    space_id: meta.id,
                    hashes: vec![asset.content_hash.clone()],
                },
            )
        });
        assert!(lines
            .iter()
            .any(|line| line.starts_with(&format!("SYNC_SEND\t{other}\t"))
                && line.contains("\"fetch\"")));
        let lines = sent_lines(|| {
            handle_space_message(
                &fixture.ui,
                &other,
                SpaceMessage::FetchUnavailable {
                    space_id: meta.id,
                    hashes: vec![asset.content_hash.clone()],
                },
            )
        });
        assert!(lines.is_empty());
        assert!(FETCHING_HASHES.with(|fetching| fetching.borrow().is_empty()));
        assert_eq!(
            PENDING_FETCH.with(|pending| pending
                .borrow()
                .iter()
                .map(|entry| entry.items.len())
                .sum::<usize>()),
            1
        );
    }

    #[test]
    fn staged_queue_is_bounded_and_overflow_never_claims_delivery() {
        let fixture = Fixture::new();
        let (meta, _) = member_space(&fixture);
        host().unwrap().importing.store(true, Ordering::SeqCst);
        let lines = sent_lines(|| {
            for _ in 0..65 {
                let batch = Uuid::new_v4();
                SPACE_BATCHES.with(|batches| {
                    batches.borrow_mut().insert(
                        batch,
                        ReceivedBatch {
                            space_id: meta.id,
                            hashes: vec!["00".repeat(32)],
                        },
                    )
                });
                let payload = format!(
                    "{batch}\t{}",
                    serde_json::to_string(&vec![fixture.dir.join("sample.txt")]).unwrap()
                );
                assert!(try_space_staged(&payload));
            }
        });
        assert_eq!(STAGED_IMPORTS.with(|queue| queue.borrow().len()), 64);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("NACK\t"))
                .count(),
            1
        );
        assert!(!lines.iter().any(|line| line.starts_with("ACK\t")));
    }

    #[test]
    #[ignore = "requires freshly built sample-library.exe; run alone with --test-threads=1"]
    fn staged_real_import_receipts_only_after_durable_content() {
        TEST_PROXY_ENABLED.with(|enabled| enabled.set(true));
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let root = space_root_of(meta.id).unwrap();
        ui_viewmodels::ensure_library_skeleton(&root).unwrap();
        let helper = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("sample-library.exe");
        assert!(
            helper.is_file(),
            "build sample-library before this acceptance test"
        );
        let helpers = fixture.dir.join("helpers");
        std::fs::create_dir(&helpers).unwrap();
        std::fs::copy(helper, helpers.join("sample-library.exe")).unwrap();
        crate::TEST_HELPER_DIR.with(|slot| *slot.borrow_mut() = Some(helpers));
        let (sender, receiver) = std::sync::mpsc::channel();
        *TEST_EVENTS.lock().unwrap() = Some(sender);
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                *TEST_EVENTS.lock().unwrap() = None;
                crate::TEST_HELPER_DIR.with(|slot| *slot.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let importing = host().unwrap().importing.clone();
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let source = fixture.dir.join("received sample.txt");
        std::fs::write(&source, b"test").unwrap();
        let mut add = sample_add(&meta, 1);
        if let SpaceOpKind::Add { asset } = &mut add.op {
            asset.content_hash = (0..64)
                .step_by(2)
                .map(|i| u8::from_str_radix(&hash[i..i + 2], 16).unwrap())
                .collect();
        }
        assert!(append_ops(meta.id, &[add]));
        let run = |source: &Path, expected_hash: &str| {
            let batch = Uuid::new_v4();
            let incoming = serde_json::json!({
                "batch_id": batch, "sender_name": "member", "space_id": meta.id,
                "authenticated_sender_id": peer,
                "items": [{"asset_uuid": Uuid::new_v4(), "file_name": source.file_name().unwrap().to_str().unwrap(),
                    "kind": ui_viewmodels::AssetKind::Text, "size_bytes": 4,
                    "sha256_hex": expected_hash, "category_hint": null}]
            });
            let staged = format!("{batch}\t{}", serde_json::to_string(&vec![source]).unwrap());
            let lines = sent_lines(|| {
                assert!(try_auto_accept(&incoming.to_string()));
                importing.store(true, Ordering::SeqCst);
                assert!(try_space_staged(&staged));
                assert!(
                    ui_viewmodels::content_paths(&root).is_empty()
                        || verify_received_hashes(meta.id, &[hash.into()]).is_ok()
                );
                // The user leaves the team view before the queued batch starts.
                ACTIVE.with(|slot| *slot.borrow_mut() = None);
                importing.store(false, Ordering::SeqCst);
                resume_pending_imports();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while importing.load(Ordering::SeqCst) {
                    let event = receiver
                        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                        .expect("import completion event");
                    event();
                }
                while let Ok(event) = receiver.try_recv() {
                    event();
                }
            });
            (batch, lines)
        };
        let (batch, lines) = run(&source, hash);
        assert_eq!(
            lines
                .iter()
                .filter(|line| *line == &format!("ACK\t{batch}"))
                .count(),
            1
        );
        assert!(!lines.iter().any(|line| line.starts_with("NACK\t")));
        verify_received_hashes(meta.id, &[hash.into()]).unwrap();
        let rows = ui_viewmodels::content_paths(&root);
        assert_eq!(rows.len(), 1);
        assert_eq!(std::fs::read(&rows[0].2).unwrap(), b"test");
        assert!(ui_viewmodels::content_paths(&fixture.dir.join("main")).is_empty());
        let publish = lines
            .iter()
            .position(|line| line.starts_with("SPACE_STATE\t"))
            .unwrap();
        let receipt = lines
            .iter()
            .position(|line| line.starts_with("ACK\t"))
            .unwrap();
        assert!(publish < receipt);
        let unsupported = fixture.dir.join("unsupported.bin");
        std::fs::write(&unsupported, b"data").unwrap();
        let (batch, lines) = run(&unsupported, &"01".repeat(32));
        assert!(!lines.iter().any(|line| line == &format!("ACK\t{batch}")));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with(&format!("NACK\t{batch}\t")))
                .count(),
            1
        );
        // Deletion waits for the same writer gate and removes every local replica row.
        importing.store(true, Ordering::SeqCst);
        let lines = sent_lines(|| remove_assets(meta.id, &[rows[0].1.clone()]).unwrap());
        assert!(lines.iter().any(|line| line.contains("\"remove\"")));
        assert_eq!(ui_viewmodels::content_paths(&root).len(), 1);
        assert!(active_space_assets(&load_ops(meta.id)).is_empty());
        let refreshed = Rc::new(std::cell::Cell::new(false));
        let observed = refreshed.clone();
        let expected_root = root.clone();
        crate::IMPORT_REFRESH.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(move |target| {
                assert_eq!(target, expected_root);
                observed.set(true);
            }))
        });
        let _ = sent_lines(|| {
            importing.store(false, Ordering::SeqCst);
            resume_pending_imports();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while importing.load(Ordering::SeqCst) {
                receiver
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("purge completion event")();
            }
        });
        crate::IMPORT_REFRESH.with(|slot| *slot.borrow_mut() = None);
        assert!(refreshed.get());
        assert!(ui_viewmodels::content_paths(&root).is_empty());
        assert!(missing_from_ops(&root, &load_ops(meta.id)).is_empty());

        // Explicit local reimport of the removed bytes (right-click "共享到团队"):
        // a fresh op-domain Add must be authored, published once, and the row
        // must survive materialization instead of being purged again.
        let reimport = sent_lines(|| {
            add_paths_to_space_for_batch(meta.id, None, vec![source.clone()], importing.clone());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while importing.load(Ordering::SeqCst) {
                let event = receiver
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("reimport completion event");
                event();
            }
            while let Ok(event) = receiver.try_recv() {
                event();
            }
        });
        assert!(
            !reimport
                .iter()
                .any(|line| line.starts_with("NACK\t") || line.starts_with("ACK\t")),
            "an explicit local import carries no transfer receipt: {reimport:?}"
        );
        let ops = load_ops(meta.id);
        let hash_bytes: Vec<u8> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&hash[i..i + 2], 16).unwrap())
            .collect();
        assert!(
            active_space_assets(&ops).contains_key(&hash_bytes),
            "explicit reimport after Remove must restore the content as active: {ops:?}"
        );
        assert_eq!(ui_viewmodels::content_paths(&root).len(), 1);
        assert!(
            reimport
                .iter()
                .filter(|line| line.starts_with(&format!("SYNC_SEND\t{peer}\t"))
                    && line.contains("\"add\""))
                .count()
                <= 1,
            "the fresh Add must be published once per peer: {reimport:?}"
        );
        // Reconciliation while the row awaits removal must not resurrect it.
        let rows = ui_viewmodels::content_paths(&root);
        assert_eq!(rows.len(), 1);
        importing.store(true, Ordering::SeqCst);
        let _ = sent_lines(|| remove_assets(meta.id, &[rows[0].1.clone()]).unwrap());
        let before = load_ops(meta.id);
        assert!(emit_missing_add_ops(meta.id, false));
        assert_eq!(
            load_ops(meta.id).len(),
            before.len(),
            "opening the view while a Remove purge is pending must not resurrect the asset"
        );
        importing.store(false, Ordering::SeqCst);
        resume_pending_imports();
        assert!(active_space_assets(&load_ops(meta.id)).is_empty());
    }

    #[test]
    fn network_ready_and_device_events_keep_persisted_peers_reachable() {
        let fixture = Fixture::new();
        let (_, peer) = member_space(&fixture);
        let lines = sent_lines(|| assert!(intercept_worker_line("NETWORK_READY")));
        assert!(lines
            .iter()
            .any(|line| line == &format!("SPACE_SYNC\t{peer}\t")));
        let _ = sent_lines(|| fixture.event(&format!("DEVICE\t{peer}\tmember\t127.0.0.1:9000")));
        assert_eq!(peer_addrs(&peer), vec!["127.0.0.1:9000".to_string()]);
    }

    #[test]
    fn worker_session_republishes_unchanged_identity_before_one_scan() {
        let fixture = Fixture::new();
        // Model a persisted registry reloaded before READY, not a new identity.
        let id = SPACES.with(|slot| slot.borrow_mut().create_space("team", 1).unwrap());
        persist();
        init(&fixture.dir.join("spaces.json"), fixture.dir.join("spaces"));
        let ready = format!("READY\t{}\towner", fixture.owner);
        let lines = sent_lines(|| {
            fixture.event(&ready);
            fixture.event("SCAN_DONE");
        });
        assert_eq!(
            lines.len(),
            2,
            "each READY must publish state then scan even with unchanged identity"
        );
        let book = only_book(&lines[..1]);
        assert!(book.spaces.contains_key(&id));
        assert_eq!(lines[1], "SCAN");
        assert!(fixture.ui.get_space_ctl_error().is_empty());
        assert!(sent_lines(|| fixture.event("SCAN_DONE")).is_empty());

        // A fresh READY means a fresh worker session even with the same identity.
        let restarted = sent_lines(|| fixture.event(&ready));
        assert_eq!(restarted.len(), 2);
        assert!(only_book(&restarted[..1]).spaces.contains_key(&id));
        assert_eq!(restarted[1], "SCAN");
    }

    #[test]
    fn registry_mutations_publish_current_book_without_extra_scans() {
        let fixture = Fixture::new();
        let created = sent_lines(|| create_space("team"));
        let id = bound_space().expect("team created").id;
        let created_book = only_book(&created);
        assert!(created_book.spaces.contains_key(&id));
        assert!(fixture.ui.get_space_ctl_error().is_empty());

        // Compare the real outgoing state against live state without printing
        // either synthetic invitation secret in an assertion failure.
        let before = created_book.spaces[&id].meta.invite_secret.clone();
        let rotated = sent_lines(|| assert!(rotate_invite(id).is_some()));
        let rotated_book = only_book(&rotated);
        let current = space_by_id(id).unwrap();
        assert!(rotated_book.spaces[&id].meta.invite_secret == current.invite_secret);
        assert!(rotated_book.spaces[&id].meta.invite_secret != before);
        assert_eq!(rotated_book.spaces[&id].meta.secret_gen, 2);
        assert!(only_book(&sent_lines(|| leave_space(id))).spaces.is_empty());
        assert!(bound_space().is_none());
        assert!(sent_lines(|| leave_space(id)).is_empty());
    }

    #[test]
    fn roster_and_new_member_add_in_one_page_are_authorized_in_order() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        let newcomer = "c".repeat(52);
        // Owner admits the newcomer then the newcomer adds an asset — same page.
        let mut roster_meta = meta.clone();
        roster_meta.members.push(ui_viewmodels::RosterMember {
            id: newcomer.clone(),
            name: "replica".into(),
        });
        let roster = SpaceOp {
            space_id: meta.id,
            lamport: 1,
            author: meta.owner_id.clone(),
            ts: 1,
            op: SpaceOpKind::Roster {
                space: roster_meta.clone(),
            },
        };
        let new_add = SpaceOp {
            space_id: meta.id,
            lamport: 2,
            author: newcomer.clone(),
            ts: 2,
            op: SpaceOpKind::Add {
                asset: SpaceAssetMeta {
                    uuid: Uuid::new_v4(),
                    file_name: "newcomer.txt".into(),
                    kind: ui_viewmodels::SpaceAssetKind::Text,
                    size_bytes: 4,
                    content_hash: vec![9; 32],
                },
            },
        };
        let page = SpaceMessage::OpsPage {
            space_id: meta.id,
            after: None,
            next: None,
            ops: vec![roster.clone(), new_add.clone()],
        };
        let _ = sent_lines(|| {
            PULL_CURSORS.with(|cursors| {
                cursors.borrow_mut().insert((peer.clone(), meta.id), None);
            });
            handle_space_message(&fixture.ui, &peer, page.clone())
        });
        let ops = load_ops(meta.id);
        assert!(
            ops.contains(&roster) && ops.contains(&new_add),
            "a page carrying Roster then the new member's Add must keep both: {ops:?}"
        );
        let space = space_by_id(meta.id).unwrap();
        assert!(space.is_member(&newcomer), "roster must be applied");
        // Older roster arriving later must not roll back the newer membership.
        let stale = SpaceOp {
            ts: 3,
            ..SpaceOp {
                space_id: meta.id,
                lamport: 0,
                author: meta.owner_id.clone(),
                ts: 0,
                op: SpaceOpKind::Roster {
                    space: meta.clone(),
                },
            }
            .clone()
        };
        let _ = sent_lines(|| {
            handle_space_message(&fixture.ui, &peer, SpaceMessage::Ops { ops: vec![stale] })
        });
        let space = space_by_id(meta.id).unwrap();
        assert!(
            space.is_member(&newcomer),
            "an older roster snapshot must not revoke newer membership"
        );
    }

    #[test]
    fn peer_active_session_triggers_one_bounded_reciprocal_pull() {
        let fixture = Fixture::new();
        let (meta, peer) = member_space(&fixture);
        // The startup attempt already happened (peer was offline); the peer now
        // initiates a pull session. No DEVICE line ever arrives.
        let first =
            sent_lines(|| assert!(intercept_worker_line(&format!("SPACE_PEER_ACTIVE\t{peer}"))));
        assert!(
            first
                .iter()
                .any(|line| line == &format!("SPACE_SYNC\t{peer}\t")),
            "an authenticated pull session must wake one reciprocal sync"
        );
        // Our own offline increments must be published to the now-reachable peer.
        let op = sample_add(&meta, 1);
        assert!(append_ops(meta.id, std::slice::from_ref(&op)));
        let lines =
            sent_lines(|| assert!(intercept_worker_line(&format!("SPACE_PEER_ACTIVE\t{peer}"))));
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&format!("SYNC_SEND\t{peer}\t"))
                    && line.contains("\"ops\"")),
            "offline local ops must be broadcast to the peer that just pulled from us: {lines:?}"
        );
        // Repeat sessions must not loop: SYNCED_PEERS means "attempted".
        let repeat =
            sent_lines(|| assert!(intercept_worker_line(&format!("SPACE_PEER_ACTIVE\t{peer}"))));
        assert!(
            !repeat.iter().any(|line| line.starts_with("SPACE_SYNC\t")),
            "repeated peer-active events must not re-trigger sync: {repeat:?}"
        );
        // A non-member identity gets nothing.
        let outsider = sent_lines(|| {
            assert!(intercept_worker_line(&format!(
                "SPACE_PEER_ACTIVE\t{}",
                "d".repeat(52)
            )))
        });
        assert!(
            !outsider
                .iter()
                .any(|line| line.starts_with("SPACE_SYNC\t") || line.starts_with("SYNC_SEND\t")),
            "non-members must not be dialed"
        );
    }
}
