//! SQLite 持久化、FTS5、迁移与 smart folder 序列化底座。
//!
//! D37 并发化改造：连接收进 Mutex。sample-library 并发流水线里多个工作
//! 线程同时 enqueue / 终态查询，单连接必须串行化；锁粒度是「单条语句到
//! 单批事务」，跨一批的巨型事务（批量导入落库）不会长期霸占连接。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};

pub const SUPPORTED_SCHEMA_VERSION: i32 = 5;
/// 多进程写并发时的等待窗（见 Store::init 注释）。
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 未分类资产在分面视图中的归属名（与 library 层落库缺省保持一致）。
/// 定义在此避免 store 反向依赖 library（分层：library 依赖 store）。
pub const INBOX_CATEGORY: &str = "待分类";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    UnsupportedSchemaVersion { found: i32 },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "sqlite 错误: {e}"),
            StoreError::UnsupportedSchemaVersion { found } => write!(
                f,
                "库 schema 版本 {found} 高于当前支持的 {SUPPORTED_SCHEMA_VERSION}，拒绝打开以防写坏"
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

pub struct AssetMeta {
    pub uuid: String,
    pub file_name: String,
    pub rel_path: String,
    pub category: Option<String>,
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

pub struct SearchHit {
    pub uuid: String,
    pub file_name: String,
}

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
        let outcome = metas
            .iter()
            .try_for_each(|meta| write_asset(&conn, meta))
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
            "SELECT uuid, file_name, rel_path, category, size_bytes, created_at, imported_at, phash, content_hash, width, height
             FROM assets WHERE uuid = ?1",
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
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let conn = self.lock();
        let phrase = format!("\"{}\"", query.replace('"', "\"\""));
        let mut stmt = conn.prepare(
            "SELECT a.uuid, a.file_name
             FROM assets_fts
             JOIN assets a ON a.uuid = assets_fts.uuid
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

    /// 窄列改分类（D48「移动到分类」/D50「统一归入」的唯一写口）。
    ///
    /// 同 set_dimensions 的理由：派生/UI 侧只改这一个字段，不读出整行再写回
    /// （并发下旧快照会覆盖新字段）。`None` = 归待分类（category 置 NULL）。
    pub fn set_category(&self, uuid: &str, category: Option<&str>) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE assets SET category = ?2 WHERE uuid = ?1",
            params![uuid, category],
        )?;
        Ok(n > 0)
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
    /// category IS NULL 归到 `INBOX_CATEGORY`（与 library 层落库缺省一致），
    /// 结果按分类名升序，确定性可测。
    pub fn distinct_categories(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(category, ?1) AS name, COUNT(*)
             FROM assets WHERE deleted = 0 GROUP BY name ORDER BY name",
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
        const SQL_ALL: &str = "SELECT uuid, file_name, rel_path, category, size_bytes, created_at, imported_at, phash, content_hash, width, height
             FROM assets ORDER BY uuid";
        const SQL_ACTIVE: &str = "SELECT uuid, file_name, rel_path, category, size_bytes, created_at, imported_at, phash, content_hash, width, height
             FROM assets WHERE deleted = 0 ORDER BY uuid";
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

/// 单行写入的具体 SQL 序列（静态化避免 Mutex 重入）；调用方必须已持锁。
pub fn write_asset(conn: &Connection, meta: &AssetMeta) -> Result<()> {
    let n = conn.prepare_cached(
        "INSERT INTO assets (uuid, file_name, rel_path, category, size_bytes, created_at, imported_at, phash, content_hash, width, height)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(uuid) DO UPDATE SET
           file_name = excluded.file_name,
           rel_path = excluded.rel_path,
           category = excluded.category,
           size_bytes = excluded.size_bytes,
           created_at = excluded.created_at,
           imported_at = excluded.imported_at,
           phash = excluded.phash,
           content_hash = excluded.content_hash,
           width = excluded.width,
           height = excluded.height",
    )?
    .execute(params![
        meta.uuid,
        meta.file_name,
        meta.rel_path,
        meta.category,
        meta.size_bytes,
        meta.created_at,
        meta.imported_at,
        meta.phash,
        meta.content_hash,
        meta.width,
        meta.height
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
