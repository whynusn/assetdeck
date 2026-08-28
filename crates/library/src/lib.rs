//! .library 管理、异步拷贝队列与导入编排。
//!
//! 素材类别判定收敛到 media 注册表；分类推断收敛到 [`rules`]（CategoryRule）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use domain::AssetKind;
use store::Store;

pub mod rules;
pub mod trash;

pub use rules::{CategoryContext, CategoryRule, GroupNameRule, ParentDirectoryRule, RuleChain};
pub use trash::TRASH_DIR;

pub const INBOX_CATEGORY: &str = "待分类";
/// 去重判定：汉明距离 ≤ 该值视为同一资产（phash 测试安全边际 ≥16 的两倍关系）。
pub const DEDUP_THRESHOLD: u32 = 8;
const COPY_CHUNK: usize = 64 * 1024;
/// pHash 采样网格边长（phash 内部按 32×32 取像素；不足则先放大）。
const PHASH_MIN_EDGE: u32 = 32;

#[derive(Debug)]
pub enum LibraryError {
    Store(store::StoreError),
    Io(std::io::Error),
    Image(image::ImageError),
    /// D37：enqueue 后等待元数据落库确认超时（写线程故障时所有票都会顶到这里）。
    MetaTimeout(String),
    /// D46：回收站目录迁移失败（rename 拒绝 / objects 与 trash 并存待对账）。
    Trash {
        uuid: String,
        reason: String,
    },
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LibraryError::Store(e) => write!(f, "存储错误: {e}"),
            LibraryError::Io(e) => write!(f, "IO 错误: {e}"),
            LibraryError::Image(e) => write!(f, "图像解码错误: {e}"),
            LibraryError::MetaTimeout(uuid) => write!(f, "元数据落库等待超时（票 {uuid}）"),
            LibraryError::Trash { uuid, reason } => {
                write!(f, "回收站操作失败（{uuid}）: {reason}")
            }
        }
    }
}

impl std::error::Error for LibraryError {}

impl From<store::StoreError> for LibraryError {
    fn from(e: store::StoreError) -> Self {
        LibraryError::Store(e)
    }
}

impl From<std::io::Error> for LibraryError {
    fn from(e: std::io::Error) -> Self {
        LibraryError::Io(e)
    }
}

impl From<image::ImageError> for LibraryError {
    fn from(e: image::ImageError) -> Self {
        LibraryError::Image(e)
    }
}

pub type Result<T> = std::result::Result<T, LibraryError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Text,
}

#[derive(Debug, Clone)]
pub struct MediaJob {
    pub uuid: String,
    pub source: PathBuf,
    pub kind: MediaKind,
}

pub trait MediaDispatcher: Send + Sync {
    fn dispatch(&self, job: MediaJob);
}

#[derive(Default, Clone)]
pub struct RecordingDispatcher {
    jobs: std::sync::Arc<Mutex<Vec<MediaJob>>>,
}

impl RecordingDispatcher {
    pub fn new() -> Self {
        Self {
            jobs: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn jobs(&self) -> Vec<MediaJob> {
        self.jobs.lock().unwrap().clone()
    }
}

/// domain::AssetKind → 派发语义的 MediaKind；Other 无解码工序，返回 None 不派发。
fn to_media_kind(kind: AssetKind) -> Option<MediaKind> {
    match kind {
        AssetKind::Image => Some(MediaKind::Image),
        AssetKind::Video => Some(MediaKind::Video),
        AssetKind::Text => Some(MediaKind::Text),
        AssetKind::Other => None,
    }
}

impl MediaDispatcher for RecordingDispatcher {
    fn dispatch(&self, job: MediaJob) {
        self.jobs.lock().unwrap().push(job);
    }
}

struct NullDispatcher;

impl MediaDispatcher for NullDispatcher {
    fn dispatch(&self, _job: MediaJob) {}
}

pub struct ImportRequest {
    pub source: PathBuf,
    pub category: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportTicket {
    pub id: u64,
    pub uuid: String,
}

#[derive(Debug, Clone)]
pub enum EnqueueOutcome {
    Ticket(ImportTicket),
    Duplicate {
        existing_uuid: String,
    },
    /// 声明为图片却无法解码（扩展名伪装 / 文件损坏 / 格式不支持）：
    /// 不产生任何入库数据，调用方按「单文件失败」上报后继续导入后续素材。
    /// 历史行为是整批导入直接失败——一个坏图拖垮上万条素材，不可接受。
    Unsupported {
        reason: String,
    },
    Backpressure,
}

#[derive(Debug, Clone)]
pub enum CopyState {
    Pending,
    Copying { copied: u64, total: u64 },
    Done,
    Failed(String),
}

struct CopyJob {
    ticket_id: u64,
    uuid: String,
    source: PathBuf,
    dest: PathBuf,
    total: u64,
    /// 会话内登记的 phash u64 值（无 phash 的素材为 None）；失败回滚精确清除用。
    session_hash: Option<u64>,
}

/// 元数据写线程的批量操作。导入数万条时逐行 autocommit 在 Windows 上每行
/// 一次 fsync（实测 ~4ms/行），攒批共享一次事务提交是恢复秒级入库的关键。
enum DbOp {
    Upsert {
        ticket: u64,
        meta: store::AssetMeta,
    },
    /// 删除残留行（拷贝失败回滚）。排进同一队列保证与潜在 Upsert 的先后序，
    /// 避免「先删后插」复活幽灵行。
    Tombstone {
        uuid: String,
    },
}

struct Shared {
    queue: VecDeque<CopyJob>,
    states: HashMap<u64, CopyState>,
    active: usize,
    paused: bool,
    /// 元数据已提交（或该票已确认无需提交）的 ticket 集合：拷贝线程把 Done
    /// 的确认权押在这里——文件落盘不算完成，元数据行也必须已在库里（D7「无半成品」）。
    meta_ready: HashSet<u64>,
}

static TICKET_SEQ: AtomicU64 = AtomicU64::new(1);

/// 导入速度档位（D37）：前台高速 vs 后台安静。
///
/// Fast：多线程拷贝 + 大批量事务 + 全并发解码；用户显式点了导入按钮并盯着
/// 进度条，此时吞吐优先。Background：小并发低调跑，少抢磁盘/CPU，供将来
/// 「闲时补导/同步」类场景使用。CLI 缺省为 Fast。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Fast,
    Background,
}

/// 库的运行参数。预设构造分别对应 ImportMode 两档；字段显式可调，测试可压小验证行为。
#[derive(Debug, Clone)]
pub struct LibraryConfig {
    /// 在途上限（背压）。调用方按 capacity 收敛未完成票数。
    pub capacity: usize,
    /// 并发拷贝线程数。1 恢复旧的串行语义。
    pub copy_workers: usize,
    /// 元数据攒批阈值：写线程最多积攒这么多行再合并成一次事务提交。
    /// usize::MAX 表示不经队列直接同步写（旧语义，测试对照用）。
    pub meta_batch: usize,
    /// 是否启用内存 pHash 索引：去重从每次 enqueue 全表 SQL 拉取（O(M×N)）
    /// 变为纯内存 xor+popcount 扫描。关闭时保留旧行为（对照测试用）。
    pub memory_phash_index: bool,
}

impl LibraryConfig {
    /// 前台高速导入默认值：面向一次性数万条的目标场景。
    pub fn fast() -> Self {
        let cores = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        LibraryConfig {
            capacity: 64,
            copy_workers: cores.clamp(2, 8),
            meta_batch: 256,
            memory_phash_index: true,
        }
    }

    /// 后台慢速导入默认值：低并发、小批次，优先不惊扰前台使用。
    pub fn background() -> Self {
        LibraryConfig {
            capacity: 16,
            copy_workers: 1,
            meta_batch: 64,
            memory_phash_index: true,
        }
    }

    pub fn for_mode(mode: ImportMode) -> Self {
        match mode {
            ImportMode::Fast => Self::fast(),
            ImportMode::Background => Self::background(),
        }
    }
}

pub struct Library {
    root: PathBuf,
    store: Store,
    dispatcher: Box<dyn MediaDispatcher>,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    /// 元数据写队列锁（与 shared 分离：进度查询不应与写线程互相搅动）。
    db_queue: DbQueueLock,
    /// 内存 pHash 去重索引。None 表示关闭（旧行为）。Arc 共享给拷贝线程做失败清除。
    phash_index: std::sync::Arc<Option<Mutex<PHashIndex>>>,
    config: LibraryConfig,
}

/// 内存 pHash 去重索引（D37）：全部 hash 常驻一个 Vec<u64>（百万条约 8MB；
/// D4 本就认可「pHash 每图 8 字节可全量驻留」）。入库去重不再逐素材全表
/// SQL 物化字符串，而是纯内存汉明扫描；仅判定疑似重复时才用等值索引反查 uuid。
struct PHashIndex {
    hashes: Vec<u64>,
    /// 本次会话新增 hash → uuid：本地即可解析，不必等写线程提交、也不查库。
    session_uuids: HashMap<u64, String>,
}

impl PHashIndex {
    fn find_within(&self, incoming: u64, threshold: u32) -> Option<u64> {
        self.hashes
            .iter()
            .copied()
            .find(|stored| phash::hamming_distance(incoming, *stored) <= threshold)
    }

    fn remove_hash(&mut self, hash: u64) {
        if let Some(pos) = self.hashes.iter().position(|h| *h == hash) {
            self.hashes.swap_remove(pos);
        }
        self.session_uuids.remove(&hash);
    }
}

type SharedLock = Arc<(Mutex<Shared>, Condvar)>;
type DbQueueLock = Arc<(Mutex<VecDeque<DbOp>>, Condvar)>;

impl Library {
    pub fn open(root: &Path) -> Result<Self> {
        // 与历史版本完全一致的默认形态：容量 16、单拷贝线程；内存索引开启。
        Self::build(
            root,
            LibraryConfig {
                capacity: 16,
                copy_workers: 1,
                meta_batch: 128,
                memory_phash_index: true,
            },
            Box::new(NullDispatcher),
        )
    }

    pub fn open_with_capacity(root: &Path, capacity: usize) -> Result<Self> {
        Self::open_full(
            root,
            LibraryConfig {
                capacity,
                copy_workers: 1,
                meta_batch: 128,
                memory_phash_index: true,
            },
            Box::new(NullDispatcher),
        )
    }

    pub fn open_with_dispatcher(
        root: &Path,
        capacity: usize,
        dispatcher: Box<dyn MediaDispatcher>,
    ) -> Result<Self> {
        Self::open_full(
            root,
            LibraryConfig {
                capacity,
                copy_workers: 1,
                meta_batch: 128,
                memory_phash_index: true,
            },
            dispatcher,
        )
    }

    /// 双速导入装配口（D37）：CLI --mode fast|background 走这里。
    pub fn open_with_mode(mode: ImportMode, root: &Path) -> Result<Self> {
        Self::open_full(
            root,
            LibraryConfig::for_mode(mode),
            Box::new(NullDispatcher),
        )
    }

    pub fn open_full(
        root: &Path,
        config: LibraryConfig,
        dispatcher: Box<dyn MediaDispatcher>,
    ) -> Result<Self> {
        Self::build(root, config, dispatcher)
    }

    fn build(
        root: &Path,
        config: LibraryConfig,
        dispatcher: Box<dyn MediaDispatcher>,
    ) -> Result<Self> {
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("thumbs"))?;
        let store = Store::open(&root.join("meta.db"))?;

        // D46 启动对账（仅当有 tombstone 行或 trash 目录存在才付出成本）：
        // 修复 move/restore 中途崩溃造成的目录漂移。常态库 = 一次 COUNT。
        if !store.deleted_uuids()?.is_empty() || root.join(TRASH_DIR).is_dir() {
            trash::reconcile_trash_at(root, &store)?;
        }

        // 启动即装载全库 pHash 到内存（百万条约 8MB，进程生命周期内一次成本）。
        let phash_index: std::sync::Arc<Option<Mutex<PHashIndex>>> =
            std::sync::Arc::new(if config.memory_phash_index {
                let mut index = PHashIndex {
                    hashes: Vec::new(),
                    session_uuids: HashMap::new(),
                };
                for (_uuid, bytes) in store.all_phashes()? {
                    if bytes.len() == 8 {
                        let v = u64::from_be_bytes(bytes.as_slice().try_into().expect("8 字节"));
                        index.hashes.push(v);
                    }
                }
                Some(Mutex::new(index))
            } else {
                None
            });

        let shared: SharedLock = Arc::new((
            Mutex::new(Shared {
                queue: VecDeque::new(),
                states: HashMap::new(),
                active: 0,
                paused: false,
                meta_ready: HashSet::new(),
            }),
            Condvar::new(),
        ));

        // 并发拷贝线程池：数量按档位来（前台 = 核数钳 2..=8，后台 = 1）。
        let workers = config.copy_workers.max(1);
        let worker_root = root.to_path_buf();

        // 元数据写队列与写线程先建好：worker 需要 tombstone 回滚通道。
        let db_queue: DbQueueLock = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));

        for _ in 0..workers {
            let worker_root = worker_root.clone();
            let worker_shared = Arc::clone(&shared);
            let worker_db_queue = Arc::clone(&db_queue);
            let worker_phash_index = std::sync::Arc::clone(&phash_index);
            thread::spawn(move || {
                worker_loop(
                    worker_root,
                    worker_shared,
                    worker_db_queue,
                    worker_phash_index,
                )
            });
        }

        // 元数据写线程：消费 DbOp，连续 Upsert 攒到阈值共享一次事务提交。
        {
            let db_queue = Arc::clone(&db_queue);
            let writer_shared = Arc::clone(&shared);
            let batch = if config.meta_batch == 0 {
                usize::MAX
            } else {
                config.meta_batch
            };
            let db_root = root.to_path_buf();
            std::thread::spawn(move || meta_writer_loop(db_root, db_queue, writer_shared, batch));
        }

        Ok(Self {
            root: root.to_path_buf(),
            store,
            dispatcher,
            shared,
            db_queue,
            phash_index,
            config,
        })
    }

    fn phash_index(&self) -> Option<&Mutex<PHashIndex>> {
        (*self.phash_index).as_ref()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// 测试钩子：暂停/恢复拷贝工作线程，保证队列语义可确定性验证。
    pub fn set_paused(&self, paused: bool) {
        let (lock, cv) = &*self.shared;
        let mut g = lock.lock().unwrap();
        g.paused = paused;
        drop(g);
        cv.notify_all();
    }

    pub fn state_of(&self, ticket: &ImportTicket) -> Option<CopyState> {
        let (lock, _) = &*self.shared;
        let g = lock.lock().unwrap();
        g.states.get(&ticket.id).cloned()
    }

    /// 在途背压容量。
    pub fn capacity(&self) -> usize {
        self.config.capacity
    }

    /// 等待某个票到达终态（Done/Failed）。条件变量等待替代调用方的
    /// 20ms 轮询；超时返回 None。Duplicate/Unsupported 无状态条目，返回 None。
    pub fn wait_terminal(
        &self,
        ticket: &ImportTicket,
        timeout: std::time::Duration,
    ) -> Option<CopyState> {
        use std::time::{Duration, Instant};
        let (lock, cv) = &*self.shared;
        let deadline = Instant::now() + timeout;
        let mut g = lock.lock().unwrap();
        loop {
            if let Some(state @ (CopyState::Done | CopyState::Failed(_))) = g.states.get(&ticket.id)
            {
                return Some(state.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let slice = (deadline - now).min(Duration::from_millis(100));
            let (g2, _) = cv.wait_timeout(g, slice).unwrap();
            g = g2;
        }
    }

    /// 导入入口：同步完成 解码→pHash→内存去重判定；元数据进写队列攒批提交，
    /// 字节拷贝交给工作线程池异步执行（D37）。语义不变式：
    /// - 单文件失败不拖垮整批：单条 Unsupported 只上报不中止；
    /// - 完成票的元数据必然已落库：Done 由元数据提交确认后才置位；
    /// - 视频跳过解码，仅派发 media job。
    pub fn enqueue(&self, req: ImportRequest) -> Result<EnqueueOutcome> {
        {
            let (lock, _) = &*self.shared;
            let g = lock.lock().unwrap();
            if g.active >= self.config.capacity {
                return Ok(EnqueueOutcome::Backpressure);
            }
        }

        // 素材类别判定统一走 media 注册表（综合分析报告「扩展性缺口 #2」）。
        let kind = media::kind_of(&req.source);
        let file_name = req
            .source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_string());

        // 解码只发生在 Image 类目：Video/Text 天然不解码，Other（未知扩展名）
        // 也不该拿去 image::open——这与派发语义「Other 不在 v1 派生范围」对齐，
        // 旧实现之外的扩展一律不试解码。
        let phash_bytes: Option<Vec<u8>> = match kind {
            AssetKind::Image => match decode_for_phash(&req.source) {
                Ok(img) => Some(phash_of(&img)),
                Err(e) => {
                    return Ok(EnqueueOutcome::Unsupported {
                        reason: format!("图片解码失败：{e}"),
                    });
                }
            },
            AssetKind::Video | AssetKind::Text | AssetKind::Other => None,
        };

        // —— 去重判定 ——
        // 内存索引路径（D37）：O(N) 纯内存汉明扫描，无 SQL、零字符串分配；
        // 命中疑似重复才做一次等值索引反查（idx_assets_phash）定 uuid。
        // 会话内新增条目直接从本地 map 解析，不等写线程提交。
        if let Some(bytes) = &phash_bytes {
            let incoming =
                u64::from_be_bytes(bytes.as_slice().try_into().expect("phash 固定 8 字节"));
            match self.phash_index() {
                Some(index_mutex) => {
                    let matched_stored = {
                        let index = index_mutex.lock().unwrap();
                        index.find_within(incoming, DEDUP_THRESHOLD)
                    };
                    if let Some(stored) = matched_stored {
                        if let Some(dup) = self.resolve_duplicate_uuid(stored, incoming) {
                            return Ok(EnqueueOutcome::Duplicate { existing_uuid: dup });
                        }
                        // 幽灵 hash（索引有、库无此行——并发删除等）：放弃命中
                        // 继续按新素材导入，宁多一份不可丢素材。
                    }
                }
                None => {
                    // 对照旧行为（memory_phash_index 关闭）。
                    for (uuid, existing) in self.store.all_phashes()? {
                        let stored = u64::from_be_bytes(existing.as_slice().try_into().unwrap());
                        if phash::hamming_distance(incoming, stored) <= DEDUP_THRESHOLD {
                            return Ok(EnqueueOutcome::Duplicate {
                                existing_uuid: uuid,
                            });
                        }
                    }
                }
            }
        }

        let uuid = uuid::Uuid::new_v4().hyphenated().to_string();
        let rel_dir = format!("objects/{uuid}");
        fs::create_dir_all(self.root.join(&rel_dir))?;
        let ext = req
            .source
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "bin".to_string());
        let rel_path = format!("{rel_dir}/raw.{ext}");
        let size_bytes = fs::metadata(&req.source)?.len() as i64;
        let created_at = fs::metadata(&req.source)?
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let meta = store::AssetMeta {
            uuid: uuid.clone(),
            file_name,
            rel_path: rel_path.clone(),
            category: Some(req.category.unwrap_or_else(|| INBOX_CATEGORY.to_string())),
            tags: req.tags,
            size_bytes,
            created_at,
            imported_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            phash: phash_bytes.clone(),
            // 导入阶段不解码媒体，像素尺寸留给派生工序（derive-thumbs）回写。
            width: None,
            height: None,
        };

        // 票号先行：写线程的提交确认要挂到这个票上（meta_ready）。
        let id = TICKET_SEQ.fetch_add(1, Ordering::Relaxed);

        // 元数据进写队列攒批提交；拷贝线程在提交确认后才置 Done。
        self.queue_db_op(DbOp::Upsert { ticket: id, meta })?;

        // D7 语义守护：enqueue 返回前元数据必须已可见（体感瞬时入库）。
        // 写线程攒批 flush 后一次 notify_all——并发导入下所有调用方一起醒，
        // 只付出一次条件变量等待；单条导入也只需写线程一个循环周期。
        self.wait_meta_ready(id, std::time::Duration::from_secs(10))?;

        // 内存索引登记：hash 与会话 uuid 都已确定，后续同批重复直接本地解析。
        let session_hash = phash_bytes
            .as_ref()
            .map(|b| u64::from_be_bytes(b.as_slice().try_into().expect("8 字节")));
        if let (Some(index_mutex), Some(hv)) = (self.phash_index(), session_hash) {
            let mut index = index_mutex.lock().unwrap();
            index.hashes.push(hv);
            index.session_uuids.insert(hv, uuid.clone());
        }

        // 派发语义：Image/Video/Text 有解码工序，Other 不在 v1 派生范围。
        if let Some(kind) = to_media_kind(kind) {
            self.dispatcher.dispatch(MediaJob {
                uuid: uuid.clone(),
                source: req.source.clone(),
                kind,
            });
        }

        let job = CopyJob {
            ticket_id: id,
            uuid: uuid.clone(),
            source: req.source,
            dest: self.root.join(rel_path),
            total: size_bytes as u64,
            // 失败回滚时精确清掉本条登记（与上面的 push 同值）。
            session_hash,
        };
        let (lock, cv) = &*self.shared;
        let mut g = lock.lock().unwrap();
        g.states.insert(id, CopyState::Pending);
        g.queue.push_back(job);
        g.active += 1;
        drop(g);
        cv.notify_all();

        Ok(EnqueueOutcome::Ticket(ImportTicket { id, uuid }))
    }

    /// 命中的已存 hash 解析为现存 uuid：优先会话内登记（免 SQL），否则走
    /// 等值索引查询。查不到活行返回 None（放弃该次命中继续导入）。
    fn resolve_duplicate_uuid(&self, stored: u64, incoming: u64) -> Option<String> {
        if phash::hamming_distance(incoming, stored) > DEDUP_THRESHOLD {
            return None;
        }
        if let Some(index_mutex) = self.phash_index() {
            let index = index_mutex.lock().unwrap();
            if let Some(uuid) = index.session_uuids.get(&stored) {
                return Some(uuid.clone());
            }
        }
        let uuids = self
            .store
            .uuids_for_phash_exact(&stored.to_be_bytes())
            .ok()?;
        uuids.into_iter().next()
    }

    fn queue_db_op(&self, op: DbOp) -> Result<()> {
        let (lock, cv) = &*self.db_queue;
        let mut q = lock.lock().unwrap();
        q.push_back(op);
        drop(q);
        cv.notify_all();
        Ok(())
    }

    /// 等待某票的元数据提交确认（meta_ready）。条件变量等待替代轮询：
    /// 批量 flush 一次通知所有等待者。超时视为写线程故障，返回 MetaTimeout。
    fn wait_meta_ready(&self, ticket: u64, timeout: std::time::Duration) -> Result<()> {
        use std::time::{Duration, Instant};
        let (lock, cv) = &*self.shared;
        let deadline = Instant::now() + timeout;
        let mut g = lock.lock().unwrap();
        loop {
            if g.meta_ready.contains(&ticket) {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(LibraryError::MetaTimeout(ticket.to_string()));
            }
            let slice = (deadline - now).min(Duration::from_millis(100));
            let (g2, _) = cv.wait_timeout(g, slice).unwrap();
            g = g2;
        }
    }
}

/// 按文件**内容**嗅探格式解码图片。
///
/// 不能用 `image::open`：它按扩展名选解码器（「The image's format is
/// determined from the path's file extension」），PNG 内容挂 .jpg 名时会被
/// 塞进 JPEG 解码器，报出「Format error decoding Jpeg: Illegal start bytes:
/// 89504e47…」这类费解错误（实测用户素材触发）。内容嗅探对伪装扩展名免疫。
fn decode_for_phash(path: &Path) -> image::ImageResult<image::DynamicImage> {
    image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
}

/// pHash 按 32×32 网格采样：小于该尺寸的原图直接采样会越界 panic
/// （真实 1×1 图标实测崩溃，整段导入进程 101 退出）。先放大到 32×32
/// 再哈希，极小图也能正常去重，且保证导入永不因尺寸崩溃。
fn phash_of(img: &image::DynamicImage) -> Vec<u8> {
    let gray = img.to_luma8();
    let gray = if gray.width() < PHASH_MIN_EDGE || gray.height() < PHASH_MIN_EDGE {
        image::imageops::resize(
            &gray,
            gray.width().max(PHASH_MIN_EDGE),
            gray.height().max(PHASH_MIN_EDGE),
            image::imageops::FilterType::Nearest,
        )
    } else {
        gray
    };
    phash::perceptual_hash_gray(&gray).to_be_bytes().to_vec()
}

fn worker_loop(
    root: PathBuf,
    shared: SharedLock,
    db_queue: DbQueueLock,
    phash_index: std::sync::Arc<Option<Mutex<PHashIndex>>>,
) {
    loop {
        let job = {
            let (lock, cv) = &*shared;
            let mut g = lock.lock().unwrap();
            loop {
                if g.paused || g.queue.is_empty() {
                    g = cv.wait(g).unwrap();
                } else {
                    break;
                }
            }
            match g.queue.pop_front() {
                Some(job) => {
                    g.states.insert(
                        job.ticket_id,
                        CopyState::Copying {
                            copied: 0,
                            total: job.total,
                        },
                    );
                    job
                }
                None => continue,
            }
        };

        let outcome = copy_with_progress(&job.source, &job.dest, job.total, |copied| {
            let (lock, _) = &*shared;
            let mut g = lock.lock().unwrap();
            g.states.insert(
                job.ticket_id,
                CopyState::Copying {
                    copied,
                    total: job.total,
                },
            );
        });

        match outcome {
            Ok(()) => {
                // 文件已就位；但 D7「无半成品」要求元数据行也在库里才算完成。
                // 等写线程对这张票的提交确认（meta_ready），或看到写线程把票
                // 判成 Failed（如落库毒行）——两种情况都由本线程收尾并减 active。
                // Arc 克隆仅引用计数，跨循环迭代不产生所有权问题。
                finish_after_copy_ok(
                    root.as_path(),
                    shared.clone(),
                    db_queue.clone(),
                    std::sync::Arc::clone(&phash_index),
                    job,
                );
            }
            Err(e) => {
                // 拷贝失败：文件/目录清理在本线程，删除行走 tombstone 队列，
                // 与写线程可能尚未提交的同 uuid Upsert 保持先后序。
                let _ = std::fs::remove_file(&job.dest);
                let _ = std::fs::remove_dir_all(root.join("objects").join(&job.uuid));
                purge_session_hash(&phash_index, job.session_hash);
                let (q_lock, q_cv) = &*db_queue;
                {
                    let mut q = q_lock.lock().unwrap();
                    q.push_back(DbOp::Tombstone {
                        uuid: job.uuid.clone(),
                    });
                    drop(q);
                    q_cv.notify_all();
                }
                let (lock, cv) = &*shared;
                let mut g = lock.lock().unwrap();
                g.states
                    .insert(job.ticket_id, CopyState::Failed(format!("{e}")));
                g.active -= 1;
                drop(g);
                cv.notify_all();
            }
        }
    }
}

/// 失败回滚时从内存索引精确摘除该素材的 hash（防御重复导入撞上幽灵条目）。
fn purge_session_hash(phash_index: &std::sync::Arc<Option<Mutex<PHashIndex>>>, hv: Option<u64>) {
    if let Some(hv) = hv {
        let opt: Option<&Mutex<PHashIndex>> = (**phash_index).as_ref();
        if let Some(mutex) = opt {
            mutex.lock().unwrap().remove_hash(hv);
        }
    }
}

/// 拷贝成功后的收尾：等元数据提交确认 → Done；见 Failed（落库失败）→ 清盘。
/// 死锁面分析：本函数只在 states 锁上做 wait_timeout（锁随等待释放），
/// 写线程只短暂持锁标记 meta_ready / 失败态，不存在交叉等待环。
fn finish_after_copy_ok(
    root: &Path,
    shared: SharedLock,
    db_queue: DbQueueLock,
    phash_index: std::sync::Arc<Option<Mutex<PHashIndex>>>,
    job: CopyJob,
) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (lock, cv) = &*shared;
        let mut g = lock.lock().unwrap();
        // 写线程判负的票（落库反复失败）：清掉刚拷好的文件，保持无半成品。
        if let Some(CopyState::Failed(_msg)) = g.states.get(&job.ticket_id) {
            drop(g);
            let _ = std::fs::remove_file(&job.dest);
            let _ = std::fs::remove_dir_all(root.join("objects").join(&job.uuid));
            purge_session_hash(&phash_index, job.session_hash);
            let (q_lock, q_cv) = &*db_queue;
            {
                let mut q = q_lock.lock().unwrap();
                q.push_back(DbOp::Tombstone {
                    uuid: job.uuid.clone(),
                });
                drop(q);
                q_cv.notify_all();
            }
            let (lock, cv) = &*shared;
            let mut g = lock.lock().unwrap();
            g.active -= 1;
            drop(g);
            cv.notify_all();
            return; // Failed 态已由写线程登记，保留原因给调用方
        }
        if g.meta_ready.remove(&job.ticket_id) {
            g.states.insert(job.ticket_id, CopyState::Done);
            g.active -= 1;
            drop(g);
            cv.notify_all();
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            // 写线程迟迟未确认（磁盘卡死等）：按失败收尾避免占用背压名额，
            // tombstone 由本路径补发以清理潜在半提交行。
            drop(g);
            let _ = std::fs::remove_file(&job.dest);
            let _ = std::fs::remove_dir_all(root.join("objects").join(&job.uuid));
            purge_session_hash(&phash_index, job.session_hash);
            let (q_lock, q_cv) = &*db_queue;
            {
                let mut q = q_lock.lock().unwrap();
                q.push_back(DbOp::Tombstone {
                    uuid: job.uuid.clone(),
                });
                drop(q);
                q_cv.notify_all();
            }
            let (lock, cv) = &*shared;
            let mut g = lock.lock().unwrap();
            g.states.insert(
                job.ticket_id,
                CopyState::Failed("等待元数据提交确认超时".into()),
            );
            g.active -= 1;
            drop(g);
            cv.notify_all();
            return;
        }
        let slice = (deadline - now).min(Duration::from_millis(200));
        let (g2, _) = cv.wait_timeout(g, slice).unwrap();
        drop(g2);
    }
}

/// 元数据写线程（D37）：从队列批量取 DbOp；连续 Upsert 合并为一次事务
/// （阈值 max_batch），Tombstone 前先冲刷未提交的 Upsert 批，保证顺序。
/// 单批整体失败时退化为逐行提交隔离毒行；单行也失败才把票直接置为
/// Failed——随后拷贝线程负责文件侧清理，本线程不再碰磁盘。
fn meta_writer_loop(_root: PathBuf, dq: DbQueueLock, shared: SharedLock, max_batch: usize) {
    let store = match Store::open(&_root.join("meta.db")) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("meta writer 打不开库，无法继续提交元数据: {e}");
            return;
        }
    };

    loop {
        // 取一批操作（阻塞等待第一件到达；随后的取满一整窗）。
        let ops: Vec<DbOp> = {
            let (q_lock, q_cv) = &*dq;
            let mut q = q_lock.lock().unwrap();
            while q.is_empty() {
                q = q_cv.wait(q).unwrap();
            }
            let take = q.len().min(max_batch.max(1));
            q.drain(..take).collect()
        };

        // 冲刷器：攒连续 Upsert，遇 Tombstone 或结尾统一提交。
        let mut ups_meta: Vec<store::AssetMeta> = Vec::with_capacity(ops.len());
        let mut ups_tickets: Vec<u64> = Vec::with_capacity(ops.len());
        macro_rules! flush_ups {
            () => {
                if !ups_meta.is_empty() {
                    flush_upsert_batch(&store, &shared, &ups_tickets, &ups_meta);
                    ups_meta.clear();
                    ups_tickets.clear();
                }
            };
        }
        for op in ops {
            match op {
                DbOp::Upsert { ticket, meta } => {
                    ups_meta.push(meta);
                    ups_tickets.push(ticket);
                    if ups_meta.len() >= max_batch.max(1) {
                        flush_ups!();
                    }
                }
                DbOp::Tombstone { uuid } => {
                    flush_ups!();
                    // 幽灵防复活序：delete 在其前所有 upsert 提交之后执行。
                    let _ = store.delete_asset(&uuid);
                }
            }
        }
        flush_ups!();
    }
}

/// 提交一批 Upsert 并把对应票标 ready。整批失败 → 逐行重试隔离毒行；
/// 毒行票置 Failed（附带原因），成功的照常 ready。
fn flush_upsert_batch(
    store: &Store,
    shared: &SharedLock,
    tickets: &[u64],
    metas: &[store::AssetMeta],
) {
    debug_assert_eq!(tickets.len(), metas.len());
    match store.upsert_assets(metas) {
        Ok(()) => {
            let (lock, cv) = &**shared;
            let mut g = lock.lock().unwrap();
            for t in tickets {
                g.meta_ready.insert(*t);
            }
            drop(g);
            cv.notify_all();
        }
        Err(_) => {
            // 整批退化逐行：定位唯一坏行，不让一个毒素材拖死整批。
            for (t, m) in tickets.iter().zip(metas.iter()) {
                match store.upsert_asset(m) {
                    Ok(()) => {
                        let (lock, cv) = &**shared;
                        let mut g = lock.lock().unwrap();
                        g.meta_ready.insert(*t);
                        drop(g);
                        cv.notify_all();
                    }
                    Err(e) => {
                        let (lock, cv) = &**shared;
                        let mut g = lock.lock().unwrap();
                        g.states
                            .insert(*t, CopyState::Failed(format!("元数据落库失败：{e}")));
                        drop(g);
                        cv.notify_all();
                    }
                }
            }
        }
    }
}

fn copy_with_progress(
    src: &Path,
    dst: &Path,
    total: u64,
    mut on_progress: impl FnMut(u64),
) -> std::io::Result<()> {
    let mut reader = fs::File::open(src)?;
    let mut writer = fs::File::create(dst)?;
    let mut buf = vec![0u8; COPY_CHUNK];
    let mut copied = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        copied += n as u64;
        on_progress(copied.min(total));
    }
    writer.flush()?;
    Ok(())
}

/// 拷贝失败的文件侧清理（D37 重构后数据库行删除改经 tombstone 队列，
/// 与写线程串行化，杜绝独立连接与批量事务互相踩踏）。此函数仅为兼容
/// 直连库删除而保留的纯 fs 形态；worker 失败路径已内联同等逻辑。
#[allow(dead_code)]
fn rollback_failed_import(root: &Path, uuid: &str, dest: &Path) {
    let _ = fs::remove_file(dest);
    let _ = fs::remove_dir_all(root.join("objects").join(uuid));
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 极小图：导入不得 panic（历史回归：phash 32×32 网格采样越界，整段导入进程 101 退出）。
    #[test]
    fn tiny_image_import_does_not_panic_and_stores_asset() {
        let root = std::env::temp_dir().join(format!(
            "lib_tiny_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // 库根 = 独立临时目录；source 是库目录外的 1×1 图片文件。
        let source = root.join("tiny.png");
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255u8, 0, 0, 255]));
        img.save(&source).unwrap();
        let library = Library::open(&root).unwrap();
        let outcome = library
            .enqueue(ImportRequest {
                source: source.clone(),
                category: Some("测试".to_string()),
                tags: vec![],
            })
            .expect("极小图导入不得 panic/失败");
        match outcome {
            EnqueueOutcome::Ticket(ticket) => {
                // 元数据落库走异步写队列（D37）——Done 终态保证 meta 必然已提交
                //（finish_after_copy_ok 在 meta_ready 确认后才置 Done）。
                let state = library
                    .wait_terminal(&ticket, std::time::Duration::from_secs(30))
                    .expect("极小图导入应在 30s 内到达终态");
                assert!(
                    matches!(state, CopyState::Done),
                    "极小图应成功完成：{state:?}"
                );
                let meta = library.store().get_asset(&ticket.uuid).unwrap();
                assert!(meta.is_some(), "极小图也应入库");
            }
            EnqueueOutcome::Duplicate { .. } => {}
            EnqueueOutcome::Unsupported { reason } => {
                panic!("极小图解码不应失败：{reason}")
            }
            EnqueueOutcome::Backpressure => panic!("不应被背压"),
        }
        let _ = fs::remove_dir_all(&root);
    }
}
