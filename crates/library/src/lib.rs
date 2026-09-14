//! .library 管理与导入编排（全量导入：拷贝先行、元数据后提交）。
//!
//! 素材类别判定收敛到 media 注册表；分类推断收敛到 [`rules`]（CategoryRule）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use domain::AssetKind;
use sha2::{Digest, Sha256};
use store::Store;

pub mod rules;
pub mod trash;

pub use rules::{CategoryContext, CategoryRule, GroupNameRule, ParentDirectoryRule, RuleChain};
pub use trash::TRASH_DIR;

pub const INBOX_CATEGORY: &str = "待分类";
/// 相似提示阈值（D65）：图片 pHash 汉明距离 ≤ 该值时**照常导入**并在结果里
/// 标注「与已有素材高度相似」。它不再是丢弃判据——pHash 对截图/表情包类
/// 素材的误判代价是不可逆的静默丢素材（D60 实证：同窗口连拍 5 连丢），
/// 判死权收归 SHA-256 字节等值，相似裁决权交还用户。
/// phash 测试安全边际（无关图案 ≥16）仍高于此阈值。
pub const SIMILAR_DISTANCE_THRESHOLD: u32 = 12;
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

/// 相似命中记录（D65）：素材照常入库，附带「与库内哪个已有素材相似、
/// 感知距离多少」——调用方负责把这条提醒浮出给用户，由用户裁决去留。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimilarityHit {
    pub existing_uuid: String,
    pub distance: u32,
}

#[derive(Debug, Clone)]
pub enum EnqueueOutcome {
    /// 素材已接受入库。`similarity` 非 None 表示与库内已有素材高度相似
    /// （pHash 距离 ≤ SIMILAR_DISTANCE_THRESHOLD）——这是提醒，不是拦截。
    Ticket {
        ticket: ImportTicket,
        similarity: Option<SimilarityHit>,
    },
    /// 与库内已有素材**字节级完全相同**（SHA-256 内容等值，D65 起覆盖包括
    /// 图片在内的全部类目）。这是系统唯一自动跳过的形态——零歧义、不重复
    /// 占盘；调用方必须在结果里点名，不得静默。
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

/// 元数据写线程的批量操作。导入数万条时逐行 autocommit 在 Windows 上每行
/// 一次 fsync（实测 ~4ms/行），攒批共享一次事务提交是恢复秒级入库的关键。
enum DbOp {
    Upsert { ticket: u64, meta: store::AssetMeta },
}

struct Shared {
    states: HashMap<u64, CopyState>,
    active: usize,
    /// 元数据已提交（或该票已确认无需提交）的 ticket 集合：enqueue 把 Done
    /// 的确认权押在这里——文件落盘不算完成，元数据行也必须已在库里（D7「无半成品」）。
    meta_ready: HashSet<u64>,
}

static TICKET_SEQ: AtomicU64 = AtomicU64::new(1);

/// 导入速度档位（D37）。
///
/// Fast：大批量事务 + 全并发解码 + 高背压上限；用户显式点了导入按钮并盯着
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
        LibraryConfig {
            capacity: 64,
            meta_batch: 256,
            memory_phash_index: true,
        }
    }

    /// 后台慢速导入默认值：低并发、小批次，优先不惊扰前台使用。
    pub fn background() -> Self {
        LibraryConfig {
            capacity: 16,
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
    /// 非图片内容摘要的会话登记（D61）：digest → uuid。写线程攒批提交前也能
    /// 命中同批重复，与 pHash 会话路径同语义；只存本次导入会话的条目。
    content_session: Arc<Mutex<HashMap<[u8; 32], String>>>,
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
    /// 阈值内**最近**命中（D65）：相似提示要给出可信的距离读数，取最小
    /// 距离而不是首个命中。距离 0 提前收表。
    fn nearest_within(&self, incoming: u64, threshold: u32) -> Option<(u64, u32)> {
        let mut best: Option<(u64, u32)> = None;
        for &stored in &self.hashes {
            let distance = phash::hamming_distance(incoming, stored);
            if distance <= threshold && best.as_ref().is_none_or(|(_, bd)| distance < *bd) {
                best = Some((stored, distance));
                if distance == 0 {
                    break;
                }
            }
        }
        best
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
        // 与历史版本完全一致的默认形态：容量 16；内存索引开启。
        Self::build(
            root,
            LibraryConfig {
                capacity: 16,
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

        // D61 内容去重会话登记（空表起）：同批次重复在写线程攒批提交前也能
        // 命中，与 pHash 会话路径同语义。
        let content_session: Arc<Mutex<HashMap<[u8; 32], String>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let shared: SharedLock = Arc::new((
            Mutex::new(Shared {
                states: HashMap::new(),
                active: 0,
                meta_ready: HashSet::new(),
            }),
            Condvar::new(),
        ));

        // 元数据写线程：消费 DbOp，连续 Upsert 攒到阈值共享一次事务提交。
        let db_queue: DbQueueLock = {
            let db_queue: DbQueueLock = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
            let writer_shared = Arc::clone(&shared);
            let writer_queue = Arc::clone(&db_queue);
            let batch = if config.meta_batch == 0 {
                usize::MAX
            } else {
                config.meta_batch
            };
            let db_root = root.to_path_buf();
            std::thread::spawn(move || {
                meta_writer_loop(db_root, writer_queue, writer_shared, batch)
            });
            db_queue
        };

        Ok(Self {
            root: root.to_path_buf(),
            store,
            dispatcher,
            shared,
            db_queue,
            phash_index,
            content_session,
            config,
        })
    }

    fn phash_index(&self) -> Option<&Mutex<PHashIndex>> {
        (*self.phash_index).as_ref()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn state_of(&self, ticket: &ImportTicket) -> Option<CopyState> {
        let (lock, _) = &*self.shared;
        let g = lock.lock().unwrap();
        g.states.get(&ticket.id).cloned()
    }

    pub fn capacity(&self) -> usize {
        self.config.capacity
    }

    /// 等待某个票到达终态（Done/Failed）。Done 即元数据已提交（enqueue 在
    /// 提交确认后才置 Done）。条件变量等待替代调用方的轮询；超时返回 None。
    /// Duplicate/Unsupported 无状态条目，返回 None。
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

    /// 导入入口（全量导入）：同步完成 解码→pHash→去重判定→**字节拷贝**→
    /// 元数据提交。语义不变式：
    /// - **enqueue 返回 Ok 时素材已完整复制入库**（objects/<uuid>/raw.* 已
    ///   落盘、元数据行已可见）——源文件此后删除/移动不再影响素材（真实
    ///   用户场景：导入后清理源文件，素材不再凭空消失）；
    /// - 拷贝失败不产生任何库内痕迹（无行、无半成品目录），调用方按
    ///   「单文件失败」上报后继续；
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

        // —— 文本尺寸闸与归一化（D60 库内文本不变量）——
        // 尺寸按转码后字节计，超限在入口硬拒绝——病态大文本不进拷贝队列
        // （粘贴端对文本本就是同步读盘）。归一化字节同时是文本的内容摘要源
        // （入库的正是这份字节）。
        let (size_bytes, normalized_text): (i64, Option<Vec<u8>>) = if kind == AssetKind::Text {
            if fs::metadata(&req.source)?.len() > TEXT_IMPORT_MAX_BYTES {
                return Ok(EnqueueOutcome::Unsupported {
                    reason: format!(
                        "文本素材超过 {}MB，暂不支持导入",
                        TEXT_IMPORT_MAX_BYTES / (1024 * 1024)
                    ),
                });
            }
            let raw = fs::read(&req.source)?;
            let normalized = media::normalize_text_to_utf8(&raw).into_owned();
            (normalized.len() as i64, Some(normalized))
        } else {
            (fs::metadata(&req.source)?.len() as i64, None)
        };

        // —— 内容等值去重（D65：全类目，图片不再例外）——
        // SHA-256 字节等值是唯一无歧义的重复判据，命中即跳过（不重复占盘，
        // D7 红线），调用方必须点名上报。图片此前只走 pHash（D61 的分工），
        // 有两个问题：字节相同的重导入也要白付一次解码才被拦下；pHash 距离
        // 阈值承担了它承担不了的「判死」职责（同窗口连拍截图距离轻易 ≤8，
        // D60 实证 5 连丢）。现在摘要先行：字节相同直接短路（顺带省掉解码），
        // 否则继续走解码与相似提示。不做「库内无同尺寸就跳过哈希」的预过滤
        // （D61 教训：首份素材不落摘要，之后同内容文件永远找不到比对目标）。
        // 诚实边界：同批并发导入同一文件，首份登记会话摘要前第二份已查重的
        // 极小窗口内两份都会入库——窗口语义与 D61 相同。
        let content_hash: [u8; 32] = match &normalized_text {
            Some(bytes) => sha256_bytes(bytes),
            None => sha256_file(&req.source)?,
        };
        if let Some(existing) = self.find_content_duplicate(&content_hash)? {
            return Ok(EnqueueOutcome::Duplicate {
                existing_uuid: existing,
            });
        }

        // —— 解码与相似提示（D65：pHash 从「判死」降级为「提醒」）——
        // 解码只发生在 Image 类目：Video/Text 天然不解码，Other（未知扩展名）
        // 也不该拿去 image::open——这与派发语义「Other 不在 v1 派生范围」对齐，
        // 旧实现之外的扩展一律不试解码。
        let phash_bytes: Option<Vec<u8>> = match kind {
            AssetKind::Image => match decode_for_phash(&req.source) {
                Ok(img) => phash_of(&img),
                Err(e) => {
                    return Ok(EnqueueOutcome::Unsupported {
                        reason: format!("图片解码失败：{e}"),
                    });
                }
            },
            AssetKind::Video | AssetKind::Text | AssetKind::Other => None,
        };

        // 相似查找：内存索引（D37）O(N) 汉明扫描取最近命中，命中后等值索引
        // 反查 uuid 定位已有素材；会话内新增条目直接从本地 map 解析，不等
        // 写线程提交。幽灵 hash（索引有、库无此行——并发删除等）放弃该次
        // 提醒照常导入，宁多一份不可丢素材。
        let similarity: Option<SimilarityHit> = match &phash_bytes {
            Some(bytes) => {
                let incoming =
                    u64::from_be_bytes(bytes.as_slice().try_into().expect("phash 固定 8 字节"));
                self.find_similar(incoming)?
            }
            None => None,
        };

        let uuid = uuid::Uuid::new_v4().hyphenated().to_string();
        let rel_dir = format!("objects/{uuid}");
        fs::create_dir_all(self.root.join(&rel_dir))?;
        let ext = req
            .source
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "bin".to_string());
        let rel_path = format!("{rel_dir}/raw.{ext}");
        let dest = self.root.join(&rel_path);
        // created_at 取自源文件元数据：必须在拷贝前读，拷贝后源文件可能已被清理。
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
            // D65：全部类目（含图片）都落内容摘要——字节等值查重与后续
            // 「库内有没有这份数据」类审计都靠它。
            content_hash: Some(content_hash.to_vec()),
            // 导入阶段不解码媒体，像素尺寸留给派生工序（derive-thumbs）回写。
            width: None,
            height: None,
        };

        // 票号先行：写线程的提交确认要挂到这个票上（meta_ready）。
        // 在途计数在此入账（拷贝在本线程内完成，Done/失败两处出账）。
        let id = TICKET_SEQ.fetch_add(1, Ordering::Relaxed);
        {
            let (lock, _) = &*self.shared;
            let mut g = lock.lock().unwrap();
            g.states.insert(id, CopyState::Pending);
            g.active += 1;
        }

        // —— 全量拷贝（同步，调用线程内完成）——
        // 旧实现把字节拷贝丢进独立线程池、元数据先行落库（D37）：enqueue 返回
        // 与字节落盘之间存在窗口期，用户此刻删除源文件会让拷贝失败 → 回滚删行，
        // 素材凭空消失（真实用户事故）。现在拷贝先行：enqueue 返回时字节已在
        // 库内，源文件此后删除/移动不再影响素材。进度仍上报 CopyState::Copying
        // （state_of 观察者可见）。
        {
            let shared = std::sync::Arc::clone(&self.shared);
            let total = size_bytes as u64;
            let progress = move |copied: u64| {
                let (lock, _) = &*shared;
                let mut g = lock.lock().unwrap();
                g.states.insert(id, CopyState::Copying { copied, total });
            };
            // 文本素材走归一化拷贝（D60 库内文本不变量的写入点），其余逐块复制。
            let outcome = if kind == AssetKind::Text {
                copy_text_normalized(&req.source, &dest, total, progress)
            } else {
                copy_with_progress(&req.source, &dest, total, progress)
            };
            if let Err(e) = outcome {
                // 拷贝失败 = 无行、无半成品：元数据尚未提交，无需回滚删行；
                // 清掉半成品目录后按单文件失败上报，不拖垮整批。
                let _ = fs::remove_dir_all(self.root.join(&rel_dir));
                let (lock, cv) = &*self.shared;
                let mut g = lock.lock().unwrap();
                g.states.insert(id, CopyState::Failed(format!("{e}")));
                g.active -= 1;
                drop(g);
                cv.notify_all();
                return Err(LibraryError::Io(e));
            }
        }

        // 字节已就位；元数据进写队列攒批提交（D7「无半成品」不变式保持：
        // Done 由提交确认后才置位，完成票的元数据必然已落库）。
        self.queue_db_op(DbOp::Upsert { ticket: id, meta })?;

        // D7 语义守护：enqueue 返回前元数据必须已可见（体感瞬时入库）。
        // 写线程攒批 flush 后一次 notify_all——并发导入下所有调用方一起醒，
        // 只付出一次条件变量等待；单条导入也只需写线程一个循环周期。
        if let Err(e) = self.wait_meta_ready(id, std::time::Duration::from_secs(10)) {
            // 写线程故障（10s 内无提交确认）：按失败收尾出账，避免占用背压名额。
            let (lock, cv) = &*self.shared;
            let mut g = lock.lock().unwrap();
            g.states.insert(id, CopyState::Failed(format!("{e}")));
            g.active -= 1;
            drop(g);
            cv.notify_all();
            return Err(e);
        }

        // 内存索引登记：hash 与会话 uuid 都已确定，后续同批重复直接本地解析。
        // （登记在拷贝成功之后：失败路径无需清理。）
        let session_hash = phash_bytes
            .as_ref()
            .map(|b| u64::from_be_bytes(b.as_slice().try_into().expect("8 字节")));
        if let (Some(index_mutex), Some(hv)) = (self.phash_index(), session_hash) {
            let mut index = index_mutex.lock().unwrap();
            index.hashes.push(hv);
            index.session_uuids.insert(hv, uuid.clone());
        }
        self.content_session
            .lock()
            .unwrap()
            .insert(content_hash, uuid.clone());
        // 派发语义：Image/Video/Text 有解码工序，Other 不在 v1 派生范围。
        // 派发用库内正本路径——源文件此后删除/移动不影响派发工序。
        if let Some(kind) = to_media_kind(kind) {
            self.dispatcher.dispatch(MediaJob {
                uuid: uuid.clone(),
                source: dest,
                kind,
            });
        }

        {
            let (lock, cv) = &*self.shared;
            let mut g = lock.lock().unwrap();
            g.states.insert(id, CopyState::Done);
            g.active -= 1;
            drop(g);
            cv.notify_all();
        }

        Ok(EnqueueOutcome::Ticket {
            ticket: ImportTicket { id, uuid },
            similarity,
        })
    }

    /// 相似命中（D65）→ 已存 uuid 与距离：内存索引取阈值内最近 hash，命中
    /// 后优先会话内登记（免 SQL），否则走等值索引查询；查不到活行（幽灵
    /// hash）返回 None——放弃该次提醒，宁多一份不可丢素材。
    /// 无内存索引的对照路径（memory_phash_index=false）全表线性扫描取最近。
    fn find_similar(&self, incoming: u64) -> Result<Option<SimilarityHit>> {
        match self.phash_index() {
            Some(index_mutex) => {
                let matched = {
                    let index = index_mutex.lock().unwrap();
                    index.nearest_within(incoming, SIMILAR_DISTANCE_THRESHOLD)
                };
                Ok(matched.and_then(|(stored, distance)| {
                    self.resolve_similar_uuid(stored, incoming)
                        .map(|existing_uuid| SimilarityHit {
                            existing_uuid,
                            distance,
                        })
                }))
            }
            None => {
                let mut best: Option<(String, u32)> = None;
                for (uuid, existing) in self.store.all_phashes()? {
                    let stored = u64::from_be_bytes(existing.as_slice().try_into().unwrap());
                    let distance = phash::hamming_distance(incoming, stored);
                    if distance <= SIMILAR_DISTANCE_THRESHOLD
                        && best.as_ref().is_none_or(|(_, bd)| distance < *bd)
                    {
                        best = Some((uuid, distance));
                    }
                }
                Ok(best.map(|(existing_uuid, distance)| SimilarityHit {
                    existing_uuid,
                    distance,
                }))
            }
        }
    }

    /// 命中的已存 hash 解析为现存 uuid：优先会话内登记（免 SQL），否则走
    /// 等值索引查询。查不到活行返回 None（放弃该次提醒继续导入）。
    fn resolve_similar_uuid(&self, stored: u64, incoming: u64) -> Option<String> {
        if phash::hamming_distance(incoming, stored) > SIMILAR_DISTANCE_THRESHOLD {
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

    /// 内容摘要 → 已存 uuid：先查会话登记（写线程攒批提交前也能命中，与
    /// pHash 会话路径同语义），未中走等值索引。回收站行不参与（与 all_phashes
    /// 的 D46 语义一致）——会话 map 不感知软删/清空，命中必须复核「行存在
    /// 且未删」，否则「删了/清了之后重导同一文件」会被静默吞掉（D65 回归
    /// 测试钉死）；复核不过回落 SQL 等值路径兜底。
    fn find_content_duplicate(&self, digest: &[u8; 32]) -> Result<Option<String>> {
        let session_hit = self.content_session.lock().unwrap().get(digest).cloned();
        if let Some(uuid) = session_hit {
            let alive = self.store.get_asset(&uuid)?.is_some() && !self.store.is_deleted(&uuid)?;
            if alive {
                return Ok(Some(uuid));
            }
        }
        Ok(self.store.uuid_by_content_hash(digest)?)
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

/// 非图片素材的 SHA-256 内容摘要：流式读取，峰值驻留一个 chunk（D3 纪律：
/// 数 GB 视频也不整读进内存）。仅在 size 预过滤命中后才付出这次整读。
fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().into())
}

/// 文本走归一化字节的摘要（入库的正是这份字节，跨批次可比）。
fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// pHash 按 32×32 网格采样：小于该尺寸的原图直接采样会越界 panic
/// （真实 1×1 图标实测崩溃，整段导入进程 101 退出）。先放大到 32×32
/// 再哈希；低信息图（放大后仍近纯色，如 1×1 纯色图标）返回 None——
/// 不产出不可信 hash（D65 低信息守卫，历史缺陷：此类图 hash 由取整
/// 噪声决定、与内容无关，曾互判重复静默丢素材）。
fn phash_of(img: &image::DynamicImage) -> Option<Vec<u8>> {
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
    phash::reliable_phash(&gray).map(|h| h.to_be_bytes().to_vec())
}

/// 元数据写线程（D37）：从队列批量取 DbOp；连续 Upsert 合并为一次事务
/// （阈值 max_batch）。单批整体失败时退化为逐行提交隔离毒行；单行也失败
/// 才把票直接置为 Failed（附带原因），成功的照常标 ready。
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

/// 文本素材入库字节上限（D60 库内文本不变量的资源闸）：超过即入口硬拒绝。
const TEXT_IMPORT_MAX_BYTES: u64 = 8 * 1024 * 1024;

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

/// 文本素材拷贝：读取 → 归一化 UTF-8 → 落盘。与 enqueue 的尺寸预计算共用
/// [`media::normalize_text_to_utf8`]，保证 meta.size_bytes 与实盘字节一致。
fn copy_text_normalized(
    src: &Path,
    dst: &Path,
    total: u64,
    mut on_progress: impl FnMut(u64),
) -> std::io::Result<()> {
    let bytes = fs::read(src)?;
    let normalized = media::normalize_text_to_utf8(&bytes);
    fs::write(dst, normalized.as_ref())?;
    on_progress(total.max(normalized.len() as u64));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("lib_{tag}_{}_{}", std::process::id(), nanos));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// 结构化图案（与 phash 测试同族公式）：灰度正弦+渐变，pHash 可信。
    fn structured_png(path: &Path, shift: i16) {
        let img = image::GrayImage::from_fn(64, 64, |x, y| {
            let fx = x as f64 / 64.0;
            let fy = y as f64 / 64.0;
            let v = 110.0
                + 40.0 * (std::f64::consts::TAU * 3.0 * fx).sin()
                + 25.0 * (std::f64::consts::TAU * 2.0 * fy).cos()
                + 30.0 * fx
                + shift as f64;
            image::Luma([v.clamp(0.0, 255.0) as u8])
        });
        image::DynamicImage::ImageLuma8(img).save(path).unwrap();
    }

    fn solid_png(path: &Path, rgb: [u8; 3]) {
        let img = image::RgbImage::from_fn(64, 64, |_x, _y| image::Rgb(rgb));
        image::DynamicImage::ImageRgb8(img).save(path).unwrap();
    }

    fn stripes_png(path: &Path, horizontal: bool) {
        let img = image::GrayImage::from_fn(64, 64, |x, y| {
            let coord = if horizontal { y } else { x };
            let band = (coord / 8).is_multiple_of(2);
            image::Luma([if band { 220 } else { 35 }])
        });
        image::DynamicImage::ImageLuma8(img).save(path).unwrap();
    }

    /// 导入并等终态（D7：Done 即元数据已落库），返回 (uuid, 相似提醒)。
    fn import_and_wait(library: &Library, source: &Path) -> (String, Option<SimilarityHit>) {
        match library
            .enqueue(ImportRequest {
                source: source.to_path_buf(),
                category: Some("测试".to_string()),
                tags: vec![],
            })
            .expect("enqueue 不得失败")
        {
            EnqueueOutcome::Ticket { ticket, similarity } => {
                let state = library
                    .wait_terminal(&ticket, std::time::Duration::from_secs(30))
                    .expect("等待终态超时");
                assert!(matches!(state, CopyState::Done), "导入应成功：{state:?}");
                (ticket.uuid, similarity)
            }
            other => panic!("预期入库，实际 {other:?}"),
        }
    }

    // ----- 全量导入语义：enqueue 返回即素材完整入库，源文件删除不再丢素材 -----

    /// 真实用户事故回归：导入完成后删除源文件，素材必须原样保留（库内正本
    /// 完整、元数据行在库）。旧实现的窗口期（enqueue 返回与字节落盘之间）
    /// 已由「拷贝先行」消灭，此处钉死不变式。
    #[test]
    fn source_deleted_after_import_leaves_asset_intact() {
        let root = temp_root("delete_source");
        let source = root.join("src.png");
        structured_png(&source, 0);
        let library = Library::open(&root).unwrap();

        let (uuid, _) = import_and_wait(&library, &source);
        // enqueue 返回即字节已在库内：库内正本存在且与源内容一致。
        let meta = library.store().get_asset(&uuid).unwrap().unwrap();
        let copy = root.join(&meta.rel_path);
        assert!(copy.is_file(), "库内正本必须已落盘: {}", copy.display());
        assert!(
            fs::read(&copy).unwrap() == fs::read(&source).unwrap(),
            "库内正本与源内容一致"
        );

        // 删除源文件：素材行与库内正本都不受影响。
        fs::remove_file(&source).unwrap();
        assert!(
            library.store().get_asset(&uuid).unwrap().is_some(),
            "删除源文件后素材行必须在库"
        );
        assert!(copy.is_file(), "删除源文件后库内正本必须原样保留");
        let _ = fs::remove_dir_all(&root);
    }

    /// 媒体派发用库内正本路径：源文件删除后派发工序照常可用（旧实现派发
    /// 源路径，删源后派发失败）。派发发生在 enqueue 内同步完成，返回时
    /// source 字段即库内正本。
    #[test]
    fn media_job_targets_library_copy_not_source() {
        let root = temp_root("media_job_path");
        let source = root.join("src.png");
        structured_png(&source, 0);
        let recorder = RecordingDispatcher::new();
        let jobs_probe = recorder.clone();
        let library = Library::open_full(
            &root,
            LibraryConfig {
                capacity: 16,
                meta_batch: 128,
                memory_phash_index: true,
            },
            Box::new(recorder),
        )
        .unwrap();

        let (uuid, _) = import_and_wait(&library, &source);
        let jobs = jobs_probe.jobs();
        assert_eq!(jobs.len(), 1, "Image 素材应派发一条 media job");
        assert_eq!(jobs[0].uuid, uuid);
        assert!(jobs[0].source.is_file(), "派发路径必须是已落盘的库内正本");
        assert!(
            jobs[0].source.starts_with(root.join("objects")),
            "派发路径必须在 objects/ 下（库内正本）: {:?}",
            jobs[0].source
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// 拷贝/摘要阶段的不可读源不产生库内痕迹：源替换为不可读形态（目录
    /// 占位同名文件）→ enqueue 必 Err，无行、无半成品目录。
    #[test]
    fn unreadable_source_leaves_no_row_and_no_orphan() {
        let root = temp_root("copy_fail");
        let source = root.join("src.png");
        structured_png(&source, 0);
        let library = Library::open(&root).unwrap();

        let outcome = library.enqueue(ImportRequest {
            source: source.clone(),
            category: Some("测试".to_string()),
            tags: vec![],
        });
        match outcome {
            Ok(EnqueueOutcome::Ticket { .. }) | Ok(EnqueueOutcome::Duplicate { .. }) => {}
            other => panic!("首份导入应成功: {other:?}"),
        }
        // 不可读注入：源替换为目录（读文件/摘要阶段即 Err）。
        fs::remove_file(&source).unwrap();
        fs::create_dir(&source).unwrap();
        let outcome = library.enqueue(ImportRequest {
            source: source.clone(),
            category: Some("测试".to_string()),
            tags: vec![],
        });
        assert!(outcome.is_err(), "目录占位源必须失败");
        assert_eq!(
            library.store().all_assets_count().unwrap(),
            1,
            "失败导入不得留下新行"
        );
        let _ = fs::remove_dir_all(&root);
    }

    // ----- D65 语义：判死权收归字节等值，pHash 只提醒 -----

    #[test]
    fn byte_identical_image_reimport_is_exact_duplicate() {
        let root = temp_root("exact_dup");
        let source = root.join("img.png");
        structured_png(&source, 0);
        let library = Library::open(&root).unwrap();

        let (first, similarity) = import_and_wait(&library, &source);
        assert!(similarity.is_none(), "首份入库不应有相似提醒");

        // 图片现在也落内容摘要（D65）：字节相同 → 精确重复，不再依赖解码。
        let second = library
            .enqueue(ImportRequest {
                source: source.clone(),
                category: Some("测试".to_string()),
                tags: vec![],
            })
            .unwrap();
        match second {
            EnqueueOutcome::Duplicate { existing_uuid } => {
                assert_eq!(existing_uuid, first, "重复判定应指向首份素材");
            }
            other => panic!("字节相同应判精确重复，实际 {other:?}"),
        }

        let meta = library.store().get_asset(&first).unwrap().unwrap();
        assert!(
            meta.content_hash.is_some(),
            "图片资产也应携带内容摘要（D65 全类目不变量）"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// 历史缺陷回归（D65 低信息守卫）：两张颜色完全不同的纯色图，旧实现
    /// pHash 全为 0 → 第二张被判重复静默丢弃；现在都必须入库且无相似提醒。
    #[test]
    fn distinct_flat_color_images_both_import_without_similarity() {
        let root = temp_root("flat_pair");
        let red = root.join("red.png");
        let blue = root.join("blue.png");
        solid_png(&red, [220, 40, 40]);
        solid_png(&blue, [40, 60, 220]);
        let library = Library::open(&root).unwrap();

        let (first, first_sim) = import_and_wait(&library, &red);
        let (second, second_sim) = import_and_wait(&library, &blue);
        assert_ne!(first, second, "两张不同的图都必须入库");
        assert!(
            first_sim.is_none() && second_sim.is_none(),
            "纯色图无可信 pHash，不得互报相似"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn near_duplicate_image_imports_and_reports_similarity() {
        let root = temp_root("near_dup");
        let a = root.join("a.png");
        let b = root.join("b.png");
        structured_png(&a, 0);
        structured_png(&b, 8); // 轻微亮度平移：phash 测试实测距离 ≤10
        let library = Library::open(&root).unwrap();

        let (uuid_a, _) = import_and_wait(&library, &a);
        let (uuid_b, similarity) = import_and_wait(&library, &b);
        assert_ne!(uuid_a, uuid_b, "近重复素材照常入库，绝不静默丢弃");
        let hit = similarity.expect("近重复必须带相似提醒");
        assert_eq!(hit.existing_uuid, uuid_a);
        assert!(
            hit.distance <= SIMILAR_DISTANCE_THRESHOLD,
            "距离读数 {} 应在阈值内",
            hit.distance
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unrelated_images_import_without_similarity() {
        let root = temp_root("unrelated");
        let h = root.join("h.png");
        let v = root.join("v.png");
        stripes_png(&h, true);
        stripes_png(&v, false);
        let library = Library::open(&root).unwrap();

        let (_, h_sim) = import_and_wait(&library, &h);
        let (_, v_sim) = import_and_wait(&library, &v);
        assert!(
            h_sim.is_none() && v_sim.is_none(),
            "无关图案距离 ≥16，不应报相似"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// 1×1 极小图：导入不得 panic（历史回归：phash 32×32 网格采样越界，整段导入进程 101 退出）。
    #[test]
    fn tiny_image_import_does_not_panic_and_stores_asset() {
        let root = temp_root("tiny");
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
            EnqueueOutcome::Ticket { ticket, similarity } => {
                // Done 终态保证 meta 必然已提交（enqueue 在写线程提交确认后才
                // 置 Done；全量导入语义下字节也已落库）。
                let state = library
                    .wait_terminal(&ticket, std::time::Duration::from_secs(30))
                    .expect("极小图导入应在 30s 内到达终态");
                assert!(
                    matches!(state, CopyState::Done),
                    "极小图应成功完成：{state:?}"
                );
                let meta = library.store().get_asset(&ticket.uuid).unwrap();
                assert!(meta.is_some(), "极小图也应入库");
                assert!(
                    similarity.is_none(),
                    "1×1 纯色图无可信 pHash，不得产出相似提醒"
                );
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
