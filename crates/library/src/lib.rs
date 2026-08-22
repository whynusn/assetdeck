//! .library 管理、异步拷贝队列与导入编排。

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use store::Store;

pub const INBOX_CATEGORY: &str = "待分类";
/// 去重判定：汉明距离 ≤ 该值视为同一资产（phash 测试安全边际 ≥16 的两倍关系）。
pub const DEDUP_THRESHOLD: u32 = 8;
const VIDEO_EXTS: [&str; 5] = ["mp4", "mov", "mkv", "webm", "avi"];
const COPY_CHUNK: usize = 64 * 1024;

#[derive(Debug)]
pub enum LibraryError {
    Store(store::StoreError),
    Io(std::io::Error),
    Image(image::ImageError),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LibraryError::Store(e) => write!(f, "存储错误: {e}"),
            LibraryError::Io(e) => write!(f, "IO 错误: {e}"),
            LibraryError::Image(e) => write!(f, "图像解码错误: {e}"),
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
    Duplicate { existing_uuid: String },
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
}

struct Shared {
    queue: VecDeque<CopyJob>,
    states: HashMap<u64, CopyState>,
    active: usize,
    paused: bool,
}

static TICKET_SEQ: AtomicU64 = AtomicU64::new(1);

pub struct Library {
    root: PathBuf,
    store: Store,
    dispatcher: Box<dyn MediaDispatcher>,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    capacity: usize,
}

type SharedLock = Arc<(Mutex<Shared>, Condvar)>;

impl Library {
    pub fn open(root: &Path) -> Result<Self> {
        Self::build(root, 16, Box::new(NullDispatcher))
    }

    pub fn open_with_capacity(root: &Path, capacity: usize) -> Result<Self> {
        Self::build(root, capacity, Box::new(NullDispatcher))
    }

    pub fn open_with_dispatcher(
        root: &Path,
        capacity: usize,
        dispatcher: Box<dyn MediaDispatcher>,
    ) -> Result<Self> {
        Self::build(root, capacity, dispatcher)
    }

    fn build(root: &Path, capacity: usize, dispatcher: Box<dyn MediaDispatcher>) -> Result<Self> {
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("thumbs"))?;
        let store = Store::open(&root.join("meta.db"))?;
        let shared: SharedLock = Arc::new((
            Mutex::new(Shared {
                queue: VecDeque::new(),
                states: HashMap::new(),
                active: 0,
                paused: false,
            }),
            Condvar::new(),
        ));
        let worker_root = root.to_path_buf();
        let worker_shared = Arc::clone(&shared);
        thread::spawn(move || worker_loop(worker_root, worker_shared));
        Ok(Self {
            root: root.to_path_buf(),
            store,
            dispatcher,
            shared,
            capacity,
        })
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

    /// 导入入口：同步完成 解码→pHash→去重→元数据落库（D7 体感瞬时入库），
    /// 字节拷贝交给工作线程异步执行。视频跳过解码，仅派发 media job。
    pub fn enqueue(&self, req: ImportRequest) -> Result<EnqueueOutcome> {
        {
            let (lock, _) = &*self.shared;
            let g = lock.lock().unwrap();
            if g.active >= self.capacity {
                return Ok(EnqueueOutcome::Backpressure);
            }
        }

        let ext = req
            .source
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let is_video = VIDEO_EXTS.contains(&ext.as_str());
        let file_name = req
            .source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_string());

        let phash_bytes: Option<Vec<u8>> = if is_video {
            None
        } else {
            let img = image::open(&req.source)?;
            let hash = phash::perceptual_hash_gray(&img.to_luma8());
            Some(hash.to_be_bytes().to_vec())
        };

        if let Some(bytes) = &phash_bytes {
            let incoming =
                u64::from_be_bytes(bytes.as_slice().try_into().expect("phash 固定 8 字节"));
            for (uuid, existing) in self.store.all_phashes()? {
                let stored = u64::from_be_bytes(existing.as_slice().try_into().unwrap());
                if phash::hamming_distance(incoming, stored) <= DEDUP_THRESHOLD {
                    return Ok(EnqueueOutcome::Duplicate {
                        existing_uuid: uuid,
                    });
                }
            }
        }

        let uuid = uuid::Uuid::new_v4().hyphenated().to_string();
        let rel_dir = format!("objects/{uuid}");
        fs::create_dir_all(self.root.join(&rel_dir))?;
        let rel_path = format!("{rel_dir}/raw.{ext}");
        let size_bytes = fs::metadata(&req.source)?.len() as i64;
        let created_at = fs::metadata(&req.source)?
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.store.upsert_asset(&store::AssetMeta {
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
            phash: phash_bytes,
        })?;

        let kind = if is_video {
            MediaKind::Video
        } else {
            MediaKind::Image
        };
        self.dispatcher.dispatch(MediaJob {
            uuid: uuid.clone(),
            source: req.source.clone(),
            kind,
        });

        let id = TICKET_SEQ.fetch_add(1, Ordering::Relaxed);
        let job = CopyJob {
            ticket_id: id,
            uuid: uuid.clone(),
            source: req.source,
            dest: self.root.join(rel_path),
            total: size_bytes as u64,
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
}

fn worker_loop(root: PathBuf, shared: SharedLock) {
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

        let final_state = match outcome {
            Ok(()) => CopyState::Done,
            Err(e) => {
                rollback_failed_import(&root, &job.uuid, &job.dest);
                CopyState::Failed(format!("{e}"))
            }
        };

        let (lock, cv) = &*shared;
        let mut g = lock.lock().unwrap();
        g.states.insert(job.ticket_id, final_state);
        g.active -= 1;
        drop(g);
        cv.notify_all();
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

/// 拷贝失败回滚：删残留文件与元数据行，保持「无半成品」不变量。
fn rollback_failed_import(root: &Path, uuid: &str, dest: &Path) {
    let _ = fs::remove_file(dest);
    let _ = fs::remove_dir_all(root.join("objects").join(uuid));
    if let Ok(store) = Store::open(&root.join("meta.db")) {
        let _ = store.delete_asset(uuid);
    }
}
