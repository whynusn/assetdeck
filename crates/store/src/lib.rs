//! SQLite 持久化、FTS5、迁移与 smart folder 序列化底座。
//!
//! D37 并发化改造：连接收进 Mutex。sample-library 并发流水线里多个工作
//! 线程同时 enqueue / 终态查询，单连接必须串行化；锁粒度是「单条语句到
//! 单批事务」，跨一批的巨型事务（批量导入落库）不会长期霸占连接。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};

pub const SUPPORTED_SCHEMA_VERSION: i32 = 10;
/// 多进程写并发时的等待窗（见 Store::init 注释）。
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 系统收件箱的显示名。**不是**可命名的分类：收件箱身份 = `category IS NULL`，
/// 任何写入层都不得把该名落成字面量（v6 起存量字面量已迁为 NULL）。
/// 定义在此避免 store 反向依赖 library（分层：library 依赖 store）。
pub const INBOX_CATEGORY: &str = "待分类";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    UnsupportedSchemaVersion {
        found: i32,
    },
    CategoryExists(String),
    /// 待分类是系统收件箱（NULL 哨兵），不可重命名/删除，也不许分类改名为它。
    ReservedInbox,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "sqlite 错误: {e}"),
            StoreError::UnsupportedSchemaVersion { found } => write!(
                f,
                "库 schema 版本 {found} 高于当前支持的 {SUPPORTED_SCHEMA_VERSION}，拒绝打开以防写坏"
            ),
            StoreError::CategoryExists(name) => write!(f, "分类已存在: {name}"),
            StoreError::ReservedInbox => write!(
                f,
                "「{INBOX_CATEGORY}」是系统收件箱，不是可重命名/删除的普通分类"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// D61 迁移分类保留：只读旧库 `meta.db`，取 `uuid → category` 映射。
///
/// 旧库是用户数据，**不得走 [`Store::open`]**（那会跑 schema 迁移改写 user_version
/// 与列）；这里用裸 `Connection` 只发一条 SELECT。旧库形态 = v0.1.0 起的
/// `assets(uuid, category, deleted)` 三列（schema v4/v5 通用）；缺文件、非
/// SQLite 文件、表缺列一律报 `io::Error`，调用方按「无分类可读」降级为
/// `auto` 指令（迁移照常进行，只是分类落待分类）。
///
/// 只读纪律由 `read_category_by_uuid_does_not_migrate_user_version` 守卫。
pub fn read_category_by_uuid(
    db_path: &Path,
) -> std::io::Result<std::collections::HashMap<String, Option<String>>> {
    // SQLITE_OPEN_READ_WRITE（不带 CREATE）：缺文件即失败、绝不新建空库；
    // 只发 SELECT 不发写语句，DB 内容与 user_version 不变。WAL 库可能需要
    // 创建 -shm，旧库所在目录可写即可（被事故污染过的旧库目录都可写）。
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io_error)?;
    let mut stmt = conn
        .prepare("SELECT uuid, category FROM assets WHERE deleted = 0")
        .map_err(io_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(io_error)?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (uuid, category) = row.map_err(io_error)?;
        map.insert(uuid, category);
    }
    Ok(map)
}

/// rusqlite 错误 → io::Error（保留 Display，分类读取对调用方只暴露 io 接口）。
fn io_error(e: rusqlite::Error) -> std::io::Error {
    std::io::Error::other(e)
}

/// 分类名 → 稳定 id（不存在则创建）。名称解析的唯一落点；调用方须持连接锁。
/// `ON CONFLICT(name) DO NOTHING` 让并发/重复调用保持幂等，随后按唯一索引取 id。
fn ensure_category_on(conn: &Connection, name: &str) -> Result<u32> {
    conn.execute(
        "INSERT INTO categories(name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
        params![name],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM categories WHERE name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(id as u32)
}

pub struct AssetMeta {
    pub uuid: String,
    pub file_name: String,
    pub rel_path: String,
    /// 分类显示名（读路径由 categories 表 JOIN 回填；写入侧的寻址字段）。
    pub category: Option<String>,
    /// 分类外键（D87）：NULL = 系统收件箱。读路径回填；写入时 Some 直接使用，
    /// 为 None 则按 [`Self::category`] 名称 ensure（兼容只按名寻址的调用方）。
    pub category_id: Option<u32>,
    pub tags: Vec<String>,
    pub size_bytes: i64,
    pub created_at: i64,
    pub imported_at: i64,
    pub phash: Option<Vec<u8>>,
    /// 非图片素材的 SHA-256 内容摘要（D61 全类目去重）。图片恒 None——图片走
    /// pHash 感知去重（AGENTS.md 硬约束），其余类型靠逐字节等值判定。
    pub content_hash: Option<Vec<u8>>,
    /// 媒体像素宽（None = 尚未探测）。缩略图派生工序回写，UI 据此算真实宽高比。
    pub width: Option<u32>,
    /// 媒体像素高（None = 尚未探测）。
    pub height: Option<u32>,
    /// 正本文件最后修改时间（unix 秒，截断到秒）。None = 尚未基线（存量行，
    /// 启动对账首扫回填）。对账以此与文件系统 mtime/size 比对判定外部修改
    /// （D91）；导入/重索引完成时写入，其余路径不得改。
    pub mtime: Option<i64>,
    /// 正本文件缺失（外部删除/库盘漂移）。只由对账工序写：文件重新出现时
    /// 对账清除，应用内软删/恢复不触本字段（回收站语义在 `deleted`）。
    pub missing: bool,
}

impl AssetMeta {
    /// 真实宽高比（w/h）。任一维缺失或为 0 时返回 None，交由调用方回落占位值。
    pub fn aspect(&self) -> Option<f32> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(w as f32 / h as f32),
            _ => None,
        }
    }
}

/// 重索引写回行（D91）：一个被外部修改的素材重算完的派生字段全集。
/// `phash`/`content_hash` 的 None 语义与导入一致——图片只有 pHash（content_hash
/// 恒 None），非图片只有内容摘要（phash 恒 None）。
#[derive(Debug, Clone)]
pub struct ReindexedFields {
    pub uuid: String,
    pub size_bytes: i64,
    pub mtime: i64,
    pub phash: Option<Vec<u8>>,
    pub content_hash: Option<Vec<u8>>,
}

pub struct SearchHit {
    pub uuid: String,
    pub file_name: String,
}

/// FTS snippet 命中标记（Unicode 私有区，正文不可能出现）：`content_snippets`
/// 返回的片段里每个命中词前后各插一枚，UI 侧据此切段做高亮（D88）。
pub const SNIPPET_MARK_START: char = '\u{E000}';
pub const SNIPPET_MARK_END: char = '\u{E001}';

const MIGRATION_V1: &str = "
CREATE TABLE IF NOT EXISTS assets (
  uuid        TEXT PRIMARY KEY NOT NULL,
  file_name   TEXT NOT NULL,
  rel_path    TEXT NOT NULL DEFAULT '',
  category    TEXT,
  size_bytes  INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL DEFAULT 0,
  imported_at INTEGER NOT NULL DEFAULT 0,
  phash       BLOB
);
CREATE TABLE IF NOT EXISTS tags (
  asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
  tag        TEXT NOT NULL,
  PRIMARY KEY (asset_uuid, tag)
);
CREATE VIRTUAL TABLE IF NOT EXISTS assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS assets_fts_ai AFTER INSERT ON assets BEGIN
  INSERT INTO assets_fts(uuid, name) VALUES (new.uuid, new.file_name);
END;
CREATE TRIGGER IF NOT EXISTS assets_fts_au AFTER UPDATE OF file_name ON assets BEGIN
  DELETE FROM assets_fts WHERE uuid = old.uuid;
  INSERT INTO assets_fts(uuid, name) VALUES (new.uuid, new.file_name);
END;
CREATE TRIGGER IF NOT EXISTS assets_fts_ad AFTER DELETE ON assets BEGIN
  DELETE FROM assets_fts WHERE uuid = old.uuid;
END;
";

/// v2：媒体像素尺寸。瀑布流要按真实宽高比排版，否则瓦片形状与缩略图不符
/// （旧版靠 id 取模的占位公式，图片一贴上去就变形）。
/// 尺寸由缩略图派生工序（worker 子进程解码）回写，UI 只读不解码（D11）。
/// 用 ALTER TABLE 增列而非重建表：FTS 触发器与既有数据原样保留。
const MIGRATION_V2: &str = "
ALTER TABLE assets ADD COLUMN width INTEGER;
ALTER TABLE assets ADD COLUMN height INTEGER;
";

/// v3：phash 等值索引（D37 导入提速配套）。
///
/// 导入去重改为「内存 pHash 索引 + 命中后按字节等值查 uuid」两级结构：
/// 汉明扫描全程在内存 Vec<u64> 上进行（零 SQL），只有判定为疑似重复时
/// 才用这条索引 O(log N) 反查 uuid。没有索引时每次命中都要全表扫描，
/// 万级重复包（用户重复导入同一 .emo 是常见操作）会退化成平方级。
const MIGRATION_V3: &str = "
CREATE INDEX IF NOT EXISTS idx_assets_phash ON assets(phash);
";

/// v4：回收站（D46）。`deleted` 为 tombstone 标志（0=正常，1=回收站）。
///
/// 软删**不触 file_name**，FTS 行原样保留（AFTER UPDATE OF file_name 触发器
/// 不发射）——因此一切面向用户的读取路径必须显式 `deleted = 0` 过滤，
/// FTS 侧靠与 assets 的 JOIN 过滤。这条纪律写在这里，勿靠调用方自觉。
const MIGRATION_V4: &str = "
ALTER TABLE assets ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
";

/// v5：内容等值去重（D61）。pHash 只覆盖图片（解码才有感知哈希）；视频/文本/
/// 未知类型此前重复导入即双份占盘（导入去重红线只写了图片那一半）。给非图片
/// 素材记 SHA-256 内容摘要 + 等值索引。NULL = 图片类或历史行——感知去重与
/// 等值去重各管一类，语义互不侵犯。
const MIGRATION_V5: &str = "
ALTER TABLE assets ADD COLUMN content_hash BLOB;
CREATE INDEX IF NOT EXISTS idx_assets_content_hash ON assets(content_hash);
";

/// v7：分类索引（D86）。分类不是独立表（D3），改名/删除/存在性检查都是
/// `WHERE category = ?` 的批量改写。没有索引时 UPDATE 与 EXISTS 全表扫描——
/// 10 万行库改一次分类名要扫整表。索引把定位降为 O(log n + k)，重命名代价
/// 收敛到「实际引用该分类的行数」。单列、不带 deleted 谓词：回收站行也参与
/// 改名（见 `rename_category`），不能按 deleted 过滤。
const MIGRATION_V7: &str = "
CREATE INDEX IF NOT EXISTS idx_assets_category ON assets(category);
";

/// v8：分类一等化（D87）。名字从「身份」降为「可改属性」，引用方持有稳定
/// 自增 id：重命名只改 categories 一行，O(1)；删除仍需把引用行置 NULL（外键
/// 语义），但走索引且只动该分类的行。
///
/// 收件箱不进表：身份仍是 `category_id IS NULL`（D85），AUTOINCREMENT 从 1 起，
/// 0 保留给视图层 `CategoryId(0)`。先建表 + 加列，回填后再删旧列（旧索引必须
/// 先 DROP，SQLite 不允许删除被索引的列）。
const MIGRATION_V8_CREATE: &str = "
CREATE TABLE IF NOT EXISTS categories (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
ALTER TABLE assets ADD COLUMN category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL;
";

const MIGRATION_V8_FINISH: &str = "
DROP INDEX IF EXISTS idx_assets_category;
ALTER TABLE assets DROP COLUMN category;
CREATE INDEX IF NOT EXISTS idx_assets_category_id ON assets(category_id);
";

/// v9：FTS 正文检索（D88）。文本素材按正文内容检索，命中片段回填卡片预览。
///
/// 结构上两件事：
/// 1. `assets.text_content` 列 = 正文事实源（归一化 UTF-8 的前 64KB，由
///    library 层写入——store 不知道文件路径，分层禁越）。FTS 表增加 content
///    列，触发器直接读该列：导入 / 改名 / 正文回写都是「单行 UPDATE + 触发器」，
///    无需调用方关心 FTS 行。
/// 2. FTS 行号锚定 `assets.rowid`：旧触发器按 uuid 定位 FTS 行（虚拟表上
///    `WHERE uuid = ?` 是全表扫描），正文回填这种逐行写法会退化成平方级；
///    rowid 锚定后 DELETE/INSERT 全是点操作，JOIN 也从 uuid 等值降为 rowid。
///
/// 存量库正文由 library 层一次性回填（`store_meta` 旗标防重入）；迁移本身只
/// 重建 name 索引，语义与旧库完全一致。整段套事务：中途失败整体回滚下次重试。
const MIGRATION_V9: &str = "
ALTER TABLE assets ADD COLUMN text_content TEXT;
DROP TRIGGER IF EXISTS assets_fts_ai;
DROP TRIGGER IF EXISTS assets_fts_au;
DROP TRIGGER IF EXISTS assets_fts_ad;
DROP TABLE IF EXISTS assets_fts;
CREATE VIRTUAL TABLE IF NOT EXISTS assets_fts USING fts5(uuid UNINDEXED, name, content, tokenize='trigram');
CREATE TRIGGER assets_fts_ai AFTER INSERT ON assets BEGIN
  INSERT INTO assets_fts(rowid, uuid, name, content)
  VALUES (new.rowid, new.uuid, new.file_name, COALESCE(new.text_content, ''));
END;
CREATE TRIGGER assets_fts_au AFTER UPDATE OF file_name, text_content ON assets BEGIN
  DELETE FROM assets_fts WHERE rowid = old.rowid;
  INSERT INTO assets_fts(rowid, uuid, name, content)
  VALUES (old.rowid, new.uuid, new.file_name, COALESCE(new.text_content, ''));
END;
CREATE TRIGGER assets_fts_ad AFTER DELETE ON assets BEGIN
  DELETE FROM assets_fts WHERE rowid = old.rowid;
END;
CREATE TABLE IF NOT EXISTS store_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO assets_fts(rowid, uuid, name, content)
SELECT rowid, uuid, file_name, COALESCE(text_content, '') FROM assets;
";

/// v10：内容一致性对账（D91）。分裂存储（DB 索引 + 文件系统正本）的健康度
/// 取决于两边对不上时能否自愈：`mtime` 是对账判据（与文件系统 mtime/size
/// 比对，判定「正本被外部修改」），`missing` 是「正本被外部删除」的持久
/// 状态。两者都只由对账/重索引工序（tools/reindex）与导入写入——浏览/搜索/
/// 回收站路径零感知。存量行 mtime 为 NULL，对账首扫只回填基线、不判脏。
const MIGRATION_V10: &str = "
ALTER TABLE assets ADD COLUMN mtime INTEGER;
ALTER TABLE assets ADD COLUMN missing INTEGER NOT NULL DEFAULT 0;
";

#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let mut store = Self {
            conn: Mutex::new(conn),
        };
        store.init()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut store = Self {
            conn: Mutex::new(conn),
        };
        store.init()?;
        Ok(store)
    }

    fn init(&mut self) -> Result<()> {
        let conn = self.lock();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // WAL 日志模式（D37）：导入是数万行级别的写风暴，DELETE 模式逐事务
        // fsync + journal 重命名是逐行落库慢的根因之一。WAL 下提交只追加
        // -wal 文件，写入吞吐高一个量级；读侧（UI 刷新库）不再被写事务阻塞。
        // journal_mode 是数据库文件级持久属性，设一次永久生效。个别环境
        // （网络盘/只读目录）设不上 WAL 时静默回落默认 DELETE 模式——性能
        // 退化可接受，功能不可因此失败。
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        // NORMAL：WAL 模式下只有 checkpoint 时才真正 sync，与 FULL 的崩溃
        // 安全差异由 WAL 自身保证（app 崩溃不丢已提交事务，掉电最多丢最后
        // 未 checkpoint 的事务且库文件不损坏）。对「素材元数据库」这是正确的折中。
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        // 多进程写并发：app 刷新库时 derive-thumbs/sample-library 可能正在写回尺寸/元数据，
        // 默认 busy_timeout=0 时直接 SQLITE_BUSY 会让刷新失败（导入后界面不更新、无缩略图）。
        // 设 5s 等待窗，让短暂的事务重叠变成“等一下”而不是“失败”。
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let found = {
            let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            v as i32
        };
        if found > SUPPORTED_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion { found });
        }
        if found < 1 {
            conn.execute_batch(MIGRATION_V1)?;
        }
        if found < 2 {
            conn.execute_batch(MIGRATION_V2)?;
        }
        if found < 3 {
            conn.execute_batch(MIGRATION_V3)?;
        }
        if found < 4 {
            conn.execute_batch(MIGRATION_V4)?;
        }
        if found < 5 {
            conn.execute_batch(MIGRATION_V5)?;
        }
        if found < 6 {
            // v6（D85）：收件箱身份收敛为 `category IS NULL`。v5 及以前有两套
            // 表示——导入缺省写字面量「待分类」、删除分类/移动回归写 NULL——
            // 而分面装载只注册 Some(名) 的行，NULL 行从侧栏消失。这里把字面量
            // 一次性折成 NULL（含回收站行），此后它只是 NULL 的显示名。
            // 参数绑定 + 单条语句，避免迁移 SQL 再抄一遍 INBOX_CATEGORY。
            conn.execute(
                "UPDATE assets SET category = NULL WHERE category = ?1",
                params![INBOX_CATEGORY],
            )?;
        }
        if found < 7 {
            conn.execute_batch(MIGRATION_V7)?;
        }
        if found < 8 {
            // D87：建表 → 按名回填 → 删旧列，三段落在一个事务里。中途失败
            // 整体回滚（旧列还在，下次打开重试），不会留下「半迁移」库。
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let outcome = (|| -> Result<()> {
                conn.execute_batch(MIGRATION_V8_CREATE)?;
                // 历史字面量收件箱（v6 已折成 NULL）与空分类不建行；名字升序
                // 落 id，让迁移后的侧栏顺序确定可测。
                conn.execute(
                    "INSERT INTO categories(name) SELECT DISTINCT category FROM assets
                     WHERE category IS NOT NULL AND category <> ?1 ORDER BY category",
                    params![INBOX_CATEGORY],
                )?;
                conn.execute(
                    "UPDATE assets SET category_id =
                        (SELECT id FROM categories WHERE name = assets.category)
                     WHERE category IS NOT NULL AND category <> ?1",
                    params![INBOX_CATEGORY],
                )?;
                conn.execute_batch(MIGRATION_V8_FINISH)?;
                Ok(())
            })();
            match outcome {
                Ok(()) => conn.execute_batch("COMMIT")?,
                Err(error) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
        }
        if found < 9 {
            // v9（D88）：FTS 正文检索。事务包裹与 v8 同款——中途失败整体
            // 回滚（旧表还在，下次打开重试），不会留下「半迁移」库。
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let outcome = (|| -> Result<()> {
                conn.execute_batch(MIGRATION_V9)?;
                Ok(())
            })();
            match outcome {
                Ok(()) => conn.execute_batch("COMMIT")?,
                Err(error) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
        }
        if found < 10 {
            // v10（D91）：内容一致性对账列。纯增列，旧行取缺省（mtime NULL /
            // missing 0），语义即「尚未基线、未标记缺失」，无需回填。
            conn.execute_batch(MIGRATION_V10)?;
        }
        if found < SUPPORTED_SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SUPPORTED_SCHEMA_VERSION)?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> i32 {
        self.user_version().unwrap_or(0)
    }

    fn user_version(&self) -> Result<i32> {
        let conn = self.lock();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(v as i32)
    }

    pub fn has_table(&self, name: &str) -> bool {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn upsert_asset(&self, meta: &AssetMeta) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = write_asset(&conn, meta)
            .and_then(|_| conn.execute_batch("COMMIT").map_err(StoreError::from));
        match outcome {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 批量 upsert：N 行共享一次事务提交。
    ///
    /// 行级语义与 `Store::upsert_asset` 完全一致（upsert + tags 全量重写，
    /// FTS 触发器照常生效）；唯一差异是事务边界——逐行调用在 Windows 上
    /// 每行付出一次 fsync（实测 ~4ms/行），10 万行合成库生成因此从分钟级
    /// 回到秒级。首个消费方：我们的批量导入写线程。空切片为合法 no-op。
    pub fn upsert_assets(&self, metas: &[AssetMeta]) -> Result<()> {
        if metas.is_empty() {
            return Ok(());
        }
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        // 同一批内同一分类只 ensure 一次；事务级缓存不会跨批陈旧。
        let mut category_cache: HashMap<String, u32> = HashMap::new();
        let outcome = metas
            .iter()
            .try_for_each(|meta| write_asset_cached(&conn, meta, &mut category_cache))
            .and_then(|_| conn.execute_batch("COMMIT").map_err(StoreError::from));
        match outcome {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 写库单行实现（静态函数）：由调用方持锁（upsert_asset / upsert_assets），
    /// 避免 Mutex 重入死锁。prepare_cached：批量路径逐行调用时每行重新
    /// parse SQL 占总耗时大头（实测见 bench-harness 探针），缓存按 SQL 文本键控。
    pub fn write_asset_on(conn: &Connection, meta: &AssetMeta) -> Result<()> {
        write_asset(conn, meta)
    }

    pub fn get_asset(&self, uuid: &str) -> Result<Option<AssetMeta>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT a.uuid, a.file_name, a.rel_path, c.name, a.size_bytes, a.created_at, a.imported_at, a.phash, a.content_hash, a.width, a.height, a.category_id, a.mtime, a.missing
             FROM assets a
             LEFT JOIN categories c ON c.id = a.category_id
             WHERE a.uuid = ?1",
        )?;
        let mut rows = stmt.query(params![uuid])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let mut asset = AssetMeta {
            uuid: row.get(0)?,
            file_name: row.get(1)?,
            rel_path: row.get(2)?,
            category: row.get(3)?,
            tags: vec![],
            size_bytes: row.get(4)?,
            created_at: row.get(5)?,
            imported_at: row.get(6)?,
            phash: row.get(7)?,
            content_hash: row.get(8)?,
            width: row.get(9)?,
            height: row.get(10)?,
            category_id: row.get(11)?,
            mtime: row.get(12)?,
            missing: row.get::<_, i64>(13)? != 0,
        };
        drop(rows);
        drop(stmt);
        let mut tag_stmt =
            conn.prepare("SELECT tag FROM tags WHERE asset_uuid = ?1 ORDER BY tag")?;
        let tag_rows = tag_stmt.query_map(params![asset.uuid], |row| row.get::<_, String>(0))?;
        for tag in tag_rows {
            asset.tags.push(tag?);
        }
        Ok(Some(asset))
    }

    /// FTS5 trigram 全文检索：查询须为连续子串且 ≥3 字符（tokenizer 固有限制）。
    /// 查询以引号短语形式传入，规避 FTS5 查询语法对标点/保留字的解析。
    /// D46 纪律：软删不触 file_name、FTS 行保留，所以这里必须 JOIN assets
    /// 过滤 `deleted = 0`——回收站素材永不出现在搜索结果。
    /// v9 起JOIN 走 rowid 锚定（FTS 行号 = assets 行号，见 MIGRATION_V9）。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let conn = self.lock();
        let phrase = fts_phrase(query);
        let mut stmt = conn.prepare(
            "SELECT a.uuid, a.file_name
             FROM assets_fts
             JOIN assets a ON a.rowid = assets_fts.rowid
             WHERE assets_fts MATCH ?1 AND a.deleted = 0
             LIMIT ?2",
        )?;
        let hits = stmt.query_map(params![phrase, limit as i64], |row| {
            Ok(SearchHit {
                uuid: row.get(0)?,
                file_name: row.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for hit in hits {
            out.push(hit?);
        }
        Ok(out)
    }

    /// 正文命中片段（D88）：返回 (uuid, 带命中标记的片段文本)。
    ///
    /// 只回「正文列有命中」的行——纯文件名命中的行 snippet 无标记，这里
    /// 过滤掉，调用方据此回落头部预览。独立于 [`Self::search`] 限流：UI 只
    /// 渲染可视窗口的文本卡，片段供前 N 条命中足够，深翻页的命中回落预览。
    /// 标记常量见 [`SNIPPET_MARK_START`] / [`SNIPPET_MARK_END`]。
    pub fn content_snippets(&self, query: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let phrase = fts_phrase(query);
        let mut stmt = conn.prepare(
            "SELECT assets_fts.uuid,
                    snippet(assets_fts, 2, char(57344), char(57345), '…', 12) AS snip
             FROM assets_fts
             JOIN assets a ON a.rowid = assets_fts.rowid
             WHERE assets_fts MATCH ?1 AND a.deleted = 0
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![phrase, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (uuid, snippet) = row?;
            if snippet.contains(SNIPPET_MARK_START) {
                out.push((uuid, snippet));
            }
        }
        Ok(out)
    }

    /// 写入文本正文（D88）：`assets.text_content` 是事实源，FTS 行由
    /// v9 触发器同步（UPDATE OF text_content → rowid 点操作重写）。
    /// 资产不存在返回 false（窗口期竞态，调用方可忽略）。
    pub fn set_text_content(&self, uuid: &str, content: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE assets SET text_content = ?1 WHERE uuid = ?2",
            params![content, uuid],
        )?;
        Ok(n > 0)
    }

    /// KV 旗标（`store_meta`，v9）：存量库回填防重入等一次性标记。
    pub fn set_meta_flag(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO store_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta_flag(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
    }

    pub fn delete_asset(&self, uuid: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute("DELETE FROM assets WHERE uuid = ?1", params![uuid])?;
        Ok(n > 0)
    }

    // -------------------------------------------------------------------------
    // D46 回收站（tombstone）。软删只翻标志位，行、tags、FTS 全部保留；
    // 彻底删除仍走 delete_asset（FTS 删除触发 + tags 级联）。
    // -------------------------------------------------------------------------

    /// 批量移入回收站（UPDATE deleted=1）。返回命中的行数（不含已删除/不存在）；
    /// 单次事务提交，批量语义与 upsert_assets 的 fsync 教训一致。
    pub fn soft_delete_assets(&self, uuids: &[&str]) -> Result<usize> {
        if uuids.is_empty() {
            return Ok(0);
        }
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome: Result<usize> = (|| {
            let mut stmt = conn
                .prepare_cached("UPDATE assets SET deleted = 1 WHERE uuid = ?1 AND deleted = 0")?;
            let mut total = 0usize;
            for uuid in uuids {
                total += stmt.execute(params![uuid])?;
            }
            Ok(total)
        })();
        match outcome {
            Ok(total) => {
                conn.execute_batch("COMMIT")?;
                Ok(total)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 批量恢复（UPDATE deleted=0）。语义同 soft_delete_assets，返回命中行数。
    pub fn restore_assets(&self, uuids: &[&str]) -> Result<usize> {
        if uuids.is_empty() {
            return Ok(0);
        }
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome: Result<usize> = (|| {
            let mut stmt = conn
                .prepare_cached("UPDATE assets SET deleted = 0 WHERE uuid = ?1 AND deleted = 1")?;
            let mut total = 0usize;
            for uuid in uuids {
                total += stmt.execute(params![uuid])?;
            }
            Ok(total)
        })();
        match outcome {
            Ok(total) => {
                conn.execute_batch("COMMIT")?;
                Ok(total)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 单行 tombstone 查询（属性面板/恢复路径的容错用）。行不存在 = false。
    pub fn is_deleted(&self, uuid: &str) -> Result<bool> {
        let conn = self.lock();
        let n: Option<i64> = conn
            .query_row(
                "SELECT deleted FROM assets WHERE uuid = ?1",
                params![uuid],
                |row| row.get(0),
            )
            .optional()?;
        Ok(n == Some(1))
    }

    /// 回收站 uuid 清单，升序（启动回填 FacetIndex 用，一次读；量级受回收站大小约束）。
    pub fn deleted_uuids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT uuid FROM assets WHERE deleted = 1 ORDER BY uuid")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 批量标缺失（D91 对账）：正本文件在外部被删/库盘漂移的行置 `missing = 1`。
    /// 单事务；不存在的 uuid 静默跳过（对账与删除并发的窗口期，语义无害）。
    /// 只允许对账/重索引工序调用——浏览/回收站路径不得触碰本字段。
    pub fn mark_missing(&self, uuids: &[&str]) -> Result<usize> {
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<usize> {
            let mut n = 0;
            for uuid in uuids {
                n += conn.execute(
                    "UPDATE assets SET missing = 1 WHERE uuid = ?1 AND deleted = 0",
                    params![uuid],
                )?;
            }
            Ok(n)
        })();
        match outcome {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// 批量清缺失标记（D91 对账）：标记过的文件重新出现时调用。
    pub fn clear_missing(&self, uuids: &[&str]) -> Result<usize> {
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<usize> {
            let mut n = 0;
            for uuid in uuids {
                n += conn.execute(
                    "UPDATE assets SET missing = 0 WHERE uuid = ?1",
                    params![uuid],
                )?;
            }
            Ok(n)
        })();
        match outcome {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// 批量回填 mtime 基线（D91 对账首扫）：存量行 mtime 为 NULL，首扫只记录
    /// 文件系统现状、不判脏。此后对账才以「mtime/size 变了没有」判定外部修改。
    pub fn backfill_mtimes(&self, rows: &[(String, i64)]) -> Result<usize> {
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<usize> {
            let mut n = 0;
            for (uuid, mtime) in rows {
                n += conn.execute(
                    "UPDATE assets SET mtime = ?2 WHERE uuid = ?1 AND mtime IS NULL",
                    params![uuid, mtime],
                )?;
            }
            Ok(n)
        })();
        match outcome {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// 重索引写回批次（D91）：一行 = 一个被外部修改后重算完的素材。
    /// 单事务整批提交；派生字段（size/mtime/phash/content_hash）一次写齐，
    /// 同时清 missing（文件在重索引时必然存在）。行不存在静默跳过。
    pub fn set_reindexed_batch(&self, rows: &[ReindexedFields]) -> Result<usize> {
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<usize> {
            let mut n = 0;
            for row in rows {
                n += conn.execute(
                    "UPDATE assets SET size_bytes = ?2, mtime = ?3, phash = ?4,
                            content_hash = ?5, missing = 0 WHERE uuid = ?1",
                    params![
                        row.uuid,
                        row.size_bytes,
                        row.mtime,
                        row.phash,
                        row.content_hash
                    ],
                )?;
            }
            Ok(n)
        })();
        match outcome {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// 当前缺缺失标记的活行 uuid 集（D91）。回收站行不参与（正本在 trash/
    /// 是软删的预期布局，不是「缺失」）。
    pub fn missing_uuids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT uuid FROM assets WHERE missing = 1 AND deleted = 0")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 全库 uuid 集（含回收站行，D91 孤儿判定基线）：`objects/` 下没有行引用
    /// 的目录才是孤儿——回收站行的正本在 trash/，其 uuid 也必须算「被引用」，
    /// 否则软删素材的对象目录残留会被误收。
    pub fn all_uuids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT uuid FROM assets")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 窄列改分类（D48「移动到分类」/D50「统一归入」的唯一写口）。
    ///
    /// 同 set_dimensions 的理由：派生/UI 侧只改这一个字段，不读出整行再写回
    /// （并发下旧快照会覆盖新字段）。`None` = 归待分类（category_id 置 NULL）；
    /// 传入字面量 `INBOX_CATEGORY` 同样归 NULL（D85：收件箱没有字面量形态）。
    pub fn set_category(&self, uuid: &str, category: Option<&str>) -> Result<bool> {
        let name = category.filter(|name| *name != INBOX_CATEGORY);
        let conn = self.lock();
        // D87：改写外键 id，而非名字。uuid 不存在时不创建 orphan 分类。
        if name.is_some() {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM assets WHERE uuid = ?1)",
                params![uuid],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(false);
            }
        }
        let category_id = name
            .map(|name| ensure_category_on(&conn, name))
            .transpose()?;
        let n = conn.execute(
            "UPDATE assets SET category_id = ?2 WHERE uuid = ?1",
            params![uuid, category_id],
        )?;
        Ok(n > 0)
    }

    /// 分类重命名：改写 categories 表的一行，O(1)（D87）。
    ///
    /// 旧实现（D86 前）是 `UPDATE assets SET category=? WHERE category=?` 改写全行，
    /// 语义 O(k)。分类成独立表后，「改名」只改目录行；引用方拿 id，不动行。
    /// 返回被引用的行数（含回收站行）：保持原「改写了多少素材」的进度口径
    /// （调用方据此回填进度条终点，不影响改名是否完成）。
    /// **收件箱两侧都拒绝**（D85）：它由 `category_id IS NULL` 定义，既不可改名
    /// 也不可被别的分类改名成这个名字。
    pub fn rename_category(&self, old: &str, new: &str) -> Result<usize> {
        if old == INBOX_CATEGORY || new == INBOX_CATEGORY {
            return Err(StoreError::ReservedInbox);
        }
        if old == new {
            return Ok(0);
        }
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<usize> {
            let old_id: Option<u32> = conn
                .query_row(
                    "SELECT id FROM categories WHERE name = ?1",
                    params![old],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(old_id) = old_id else {
                return Ok(0);
            };
            let taken: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM categories WHERE name = ?1)",
                params![new],
                |r| r.get(0),
            )?;
            if taken {
                return Err(StoreError::CategoryExists(new.to_string()));
            }
            let refs: i64 = conn.query_row(
                "SELECT COUNT(*) FROM assets WHERE category_id = ?1",
                params![old_id],
                |r| r.get(0),
            )?;
            conn.execute(
                "UPDATE categories SET name = ?1 WHERE id = ?2",
                params![new, old_id],
            )?;
            Ok(refs as usize)
        })();
        match outcome {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 删除分类：引用该名的资产行 category_id 置 NULL（读取侧归待分类），
    /// 素材本身与对象文件不动。**含回收站行**，返回改写行数。
    ///
    /// 行内 `ON DELETE SET NULL` 也能完成一致性，但显式事务能统计影响行数
    /// （用于进度回填）；删除分类行本身。
    /// **收件箱不可删**（D85）：它不是一条分类，`clear_category(INBOX)` 直接拒绝。
    pub fn clear_category(&self, name: &str) -> Result<usize> {
        if name == INBOX_CATEGORY {
            return Err(StoreError::ReservedInbox);
        }
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<usize> {
            let id: Option<u32> = conn
                .query_row(
                    "SELECT id FROM categories WHERE name = ?1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = id else {
                return Ok(0);
            };
            let refs: i64 = conn.query_row(
                "SELECT COUNT(*) FROM assets WHERE category_id = ?1",
                params![id],
                |r| r.get(0),
            )?;
            conn.execute(
                "UPDATE assets SET category_id = NULL WHERE category_id = ?1",
                params![id],
            )?;
            conn.execute("DELETE FROM categories WHERE id = ?1", params![id])?;
            Ok(refs as usize)
        })();
        match outcome {
            Ok(n) => {
                conn.execute_batch("COMMIT")?;
                Ok(n)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 分类名 → 稳定 id（不存在则创建）。导入/移动只按名寻址，id 在 store
    /// 边界解析；`INBOX_CATEGORY` 是系统项而非真分类，拒绝创建。
    pub fn ensure_category(&self, name: &str) -> Result<u32> {
        if name == INBOX_CATEGORY {
            return Err(StoreError::ReservedInbox);
        }
        let conn = self.lock();
        ensure_category_on(&conn, name)
    }

    /// 全部命名分类（含暂时没有素材的空分类），`(id, name)` 按名升序。
    ///
    /// 装载侧据此建侧栏条目：分类是一等实体（D87），空分类仍应可见可归入，
    /// 不能像 D86 前那样只从资产行的去重名集合里反推。
    pub fn list_categories(&self) -> Result<Vec<(u32, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id, name FROM categories ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)? as u32, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 窄列改名（D48 重命名）。UPDATE file_name 自动触发 FTS 重排（v1 触发器），
    /// 不动 `objects/<uuid>/raw.<ext>` 物理路径——rel_path 与 file_name 解耦是 D14 起的既有语义。
    pub fn rename_asset(&self, uuid: &str, new_name: &str) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE assets SET file_name = ?2 WHERE uuid = ?1",
            params![uuid, new_name],
        )?;
        Ok(n > 0)
    }

    /// 全库资产计数（导入不变量与测试用）。**含回收站行**——这是低层原语，
    /// 语义如实命名；面向用户视图的计数走 distinct_categories / for_each_asset_active。
    pub fn all_assets_count(&self) -> Result<i64> {
        let conn = self.lock();
        conn.query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))
            .map_err(StoreError::from)
    }

    /// 列出去重后的分类及其计数（分面视图入口），**排除回收站**（D46：回收站
    /// 素材不占分类计数）。
    ///
    /// `category_id IS NULL` 显示的 `INBOX_CATEGORY` 是**只读映射**（D85）：
    /// 库内不再存该名字面量，收件箱身份恒为 NULL。名字来自 categories JOIN；
    /// 结果按分类名升序，确定性可测。
    pub fn distinct_categories(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(c.name, ?1) AS name, COUNT(*)
             FROM assets a
             LEFT JOIN categories c ON c.id = a.category_id
             WHERE a.deleted = 0 GROUP BY name ORDER BY name",
        )?;
        let rows = stmt.query_map(params![INBOX_CATEGORY], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 列出去重后的标签及其计数（分面视图入口），按标签名升序，**排除回收站**
    /// （D46：回收站素材不占标签计数；purge 级联删 tags 行，软删行 tags 保留但不可见）。
    pub fn distinct_tags(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tag, COUNT(*) FROM tags
             WHERE asset_uuid IN (SELECT uuid FROM assets WHERE deleted = 0)
             GROUP BY tag ORDER BY tag",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 回写媒体像素尺寸（缩略图派生工序的唯一写入口）。
    ///
    /// 为什么单独开一个窄写口而不复用 upsert_asset：派生工序只知道 w/h，
    /// 不该把整行元数据读出再写回（并发导入时会用旧快照覆盖新字段）。
    /// 返回是否命中了某一行；uuid 不存在时为 false，不视为错误。
    pub fn set_dimensions(&self, uuid: &str, width: u32, height: u32) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE assets SET width = ?2, height = ?3 WHERE uuid = ?1",
            params![uuid, width, height],
        )?;
        Ok(n > 0)
    }

    /// 批量尺寸回写（D37）：derive-thumbs 每张图单独 autocommit UPDATE 时，
    /// Windows 上每行付出一次 fsync（同 upsert_asset 的实测 ~4ms/行）。批量
    /// 版 N 行共享一次事务提交。返回命中的行数；未命中行不报错（并发下资产
    /// 行可能已被删除，与 set_dimensions 单行语义一致）。
    pub fn set_dimensions_batch(&self, dims: &[(&str, u32, u32)]) -> Result<usize> {
        if dims.is_empty() {
            return Ok(0);
        }
        let conn = self.lock();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome: Result<usize> = (|| {
            let mut stmt =
                conn.prepare_cached("UPDATE assets SET width = ?2, height = ?3 WHERE uuid = ?1")?;
            let mut total = 0usize;
            for (uuid, width, height) in dims {
                total += stmt.execute(params![uuid, width, height])?;
            }
            Ok(total)
        })();
        match outcome {
            Ok(total) => {
                conn.execute_batch("COMMIT").map_err(StoreError::from)?;
                Ok(total)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// pHash 字节等值反查 uuid（D37 导入去重的收尾查询）。
    ///
    /// 前置条件：调用方已在内存里完成汉明距离扫描并拿到与候选完全相等的
    /// hash 字节（v3 的 idx_assets_phash 等值索引才用得上）。同一字节串
    /// 可能对应多行（不同图撞出相同 64 位 hash），全部返回由调用方二次过滤。
    pub fn uuids_for_phash_exact(&self, bytes: &[u8]) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare_cached("SELECT uuid FROM assets WHERE phash = ?1 AND deleted = 0 LIMIT 16")?;
        let rows = stmt.query_map(params![bytes], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 全库 pHash 清单（uuid, hash 大端字节），**排除回收站**（D46：回收站素材
    /// 不得挡新导入的去重判定——恢复与导入的去重语义由 library 层组合）。
    /// 导入去重在 library 层比对。
    pub fn all_phashes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT uuid, phash FROM assets WHERE phash IS NOT NULL AND deleted = 0")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 内容摘要等值反查 uuid（D61 非图片去重的收尾查询）。**排除回收站**——
    /// 与 all_phashes 同一去重语义：回收站素材不挡新导入。命中即唯一
    /// （SHA-256 等值，不存在 pHash 那种多候选二次过滤），LIMIT 兜底防御。
    pub fn uuid_by_content_hash(&self, hash: &[u8]) -> Result<Option<String>> {
        let conn = self.lock();
        let uuid: Option<String> = conn
            .query_row(
                "SELECT uuid FROM assets WHERE content_hash = ?1 AND deleted = 0 LIMIT 1",
                params![hash],
                |row| row.get(0),
            )
            .optional()?;
        Ok(uuid)
    }

    /// 按 uuid 升序遍历全库资产元数据，逐行回调（含 tags）。**含回收站行**——
    /// 审计/对账路径专用；面向用户视图（浏览/派生/导出）必须用
    /// [`for_each_asset_active`](Self::for_each_asset_active)（D46 纪律）。
    ///
    /// 不物化全量 Vector——调用方边遍历边装配下游结构，峰值驻留只多一行，
    /// 契合 D3/D4「禁止全量载入内存」红线。行序 = uuid 字典序（索引扫描，
    /// 确定性）；tags 排序语义与 get_asset 一致。
    /// 注意：遍历期间持有连接锁（回调禁止再查同一 store，会自锁；跨进程
    /// 写不受影响——WAL 下读写各自独立）。
    pub fn for_each_asset<F>(&self, visit: F) -> Result<()>
    where
        F: FnMut(AssetMeta),
    {
        self.for_each_asset_impl(false, visit)
    }

    /// 同 [`for_each_asset`](Self::for_each_asset_active)，但跳过回收站行。
    /// 两条路径共用 `ORDER BY uuid`——uuid 升序不变量是 FTS 结果二分映射
    /// （D52）与行号定位的地基，任何改动不得破坏（store_spec 有守卫测试）。
    pub fn for_each_asset_active<F>(&self, visit: F) -> Result<()>
    where
        F: FnMut(AssetMeta),
    {
        self.for_each_asset_impl(true, visit)
    }

    fn for_each_asset_impl<F>(&self, only_active: bool, mut visit: F) -> Result<()>
    where
        F: FnMut(AssetMeta),
    {
        const SQL_ALL: &str = "SELECT a.uuid, a.file_name, a.rel_path, c.name, a.size_bytes, a.created_at, a.imported_at, a.phash, a.content_hash, a.width, a.height, a.category_id, a.mtime, a.missing
             FROM assets a LEFT JOIN categories c ON c.id = a.category_id ORDER BY a.uuid";
        const SQL_ACTIVE: &str = "SELECT a.uuid, a.file_name, a.rel_path, c.name, a.size_bytes, a.created_at, a.imported_at, a.phash, a.content_hash, a.width, a.height, a.category_id, a.mtime, a.missing
             FROM assets a LEFT JOIN categories c ON c.id = a.category_id WHERE a.deleted = 0 ORDER BY a.uuid";
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(if only_active { SQL_ACTIVE } else { SQL_ALL })?;
        let mut tag_stmt =
            conn.prepare_cached("SELECT tag FROM tags WHERE asset_uuid = ?1 ORDER BY tag")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut meta = AssetMeta {
                uuid: row.get(0)?,
                file_name: row.get(1)?,
                rel_path: row.get(2)?,
                category: row.get(3)?,
                tags: vec![],
                size_bytes: row.get(4)?,
                created_at: row.get(5)?,
                imported_at: row.get(6)?,
                phash: row.get(7)?,
                content_hash: row.get(8)?,
                width: row.get(9)?,
                height: row.get(10)?,
                category_id: row.get(11)?,
                mtime: row.get(12)?,
                missing: row.get::<_, i64>(13)? != 0,
            };
            let tag_rows = tag_stmt.query_map(params![meta.uuid], |row| row.get::<_, String>(0))?;
            for tag in tag_rows {
                meta.tags.push(tag?);
            }
            visit(meta);
        }
        Ok(())
    }

    /// 缩略图缓存路径：两级分片，纯函数确定性生成。
    pub fn thumbnail_cache_path(uuid: &str, ext: &str) -> PathBuf {
        let lower = uuid.to_lowercase();
        let shard1 = &lower[0..1];
        let shard2 = &lower[0..2];
        PathBuf::from("thumbs")
            .join(shard1)
            .join(shard2)
            .join(format!("{lower}.{ext}"))
    }

    /// 派生 PNG（上框用）相对路径：objects/<uuid>/paste.png，纯函数确定性生成。
    ///
    /// 为什么需要它：部分 IM（千牛）把 CF_HDROP 粘贴当成「直接发送文件」，
    /// 只有 CF_PNG 才落进输入框。图片（含 PNG 原图）因此旁挂一份等价 PNG
    /// 在对象目录里，由离线派生工序（worker 子进程解码）以 4096 cap 产出，
    /// UI 只读不解码；与 raw.<ext> 同目录，删除资产目录即连带回收，无额外 GC 语义。
    pub fn paste_png_path(uuid: &str) -> PathBuf {
        PathBuf::from("objects")
            .join(uuid.to_lowercase())
            .join("paste.png")
    }
}

/// FTS5 查询短语构造：引号包裹规避查询语法解析（search 与 content_snippets
/// 共用，防止两处查询口径漂移）。
fn fts_phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

/// 单行写入的具体 SQL 序列（静态化避免 Mutex 重入）；调用方必须已持锁。
pub fn write_asset(conn: &Connection, meta: &AssetMeta) -> Result<()> {
    let mut cache = HashMap::new();
    write_asset_cached(conn, meta, &mut cache)
}

/// 带「分类名 → id」缓存的写入实现：批量 upsert 共用一份缓存，同一分类
/// 只在首批付出一次 ensure；缓存作用域 = 一次调用/一个事务，不存在陈旧 id。
fn write_asset_cached(
    conn: &Connection,
    meta: &AssetMeta,
    category_cache: &mut HashMap<String, u32>,
) -> Result<()> {
    let category_id = match meta.category_id {
        Some(id) => Some(id),
        None => match meta.category.as_deref() {
            // D85：收件箱身份是 NULL，字面量入库会被折成 NULL（防御旧调用方）。
            Some(name) if name != INBOX_CATEGORY => {
                let id = match category_cache.get(name) {
                    Some(id) => *id,
                    None => {
                        let id = ensure_category_on(conn, name)?;
                        category_cache.insert(name.to_string(), id);
                        id
                    }
                };
                Some(id)
            }
            _ => None,
        },
    };
    let n = conn.prepare_cached(
        "INSERT INTO assets (uuid, file_name, rel_path, category_id, size_bytes, created_at, imported_at, phash, content_hash, width, height, mtime, missing)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(uuid) DO UPDATE SET
           file_name = excluded.file_name,
           rel_path = excluded.rel_path,
           category_id = excluded.category_id,
           size_bytes = excluded.size_bytes,
           created_at = excluded.created_at,
           imported_at = excluded.imported_at,
           phash = excluded.phash,
           content_hash = excluded.content_hash,
           width = excluded.width,
           height = excluded.height,
           mtime = excluded.mtime,
           missing = excluded.missing",
    )?
    .execute(params![
        meta.uuid,
        meta.file_name,
        meta.rel_path,
        category_id,
        meta.size_bytes,
        meta.created_at,
        meta.imported_at,
        meta.phash,
        meta.content_hash,
        meta.width,
        meta.height,
        meta.mtime,
        meta.missing as i64
    ])?;
    debug_assert_eq!(n, 1);
    conn.prepare_cached("DELETE FROM tags WHERE asset_uuid = ?1")?
        .execute(params![meta.uuid])?;
    for tag in &meta.tags {
        conn.prepare_cached("INSERT OR IGNORE INTO tags (asset_uuid, tag) VALUES (?1, ?2)")?
            .execute(params![meta.uuid, tag])?;
    }
    Ok(())
}
