//! SQLite 持久化、FTS5、迁移与 smart folder 序列化底座。

use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

pub const SUPPORTED_SCHEMA_VERSION: i32 = 1;

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

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let mut store = Self { conn };
        store.init()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut store = Self { conn };
        store.init()?;
        Ok(store)
    }

    fn init(&mut self) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        let found = self.user_version()?;
        if found > SUPPORTED_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion { found });
        }
        if found < 1 {
            self.conn.execute_batch(MIGRATION_V1)?;
            self.conn
                .pragma_update(None, "user_version", SUPPORTED_SCHEMA_VERSION)?;
        }
        Ok(())
    }

    fn user_version(&self) -> Result<i32> {
        let v: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(v as i32)
    }

    pub fn schema_version(&self) -> i32 {
        self.user_version().unwrap_or(0)
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    pub fn upsert_asset(&self, meta: &AssetMeta) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = self
            .write_asset(meta)
            .and_then(|_| self.conn.execute_batch("COMMIT").map_err(StoreError::from));
        match outcome {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn write_asset(&self, meta: &AssetMeta) -> Result<()> {
        self.conn.execute(
            "INSERT INTO assets (uuid, file_name, rel_path, category, size_bytes, created_at, imported_at, phash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(uuid) DO UPDATE SET
               file_name = excluded.file_name,
               rel_path = excluded.rel_path,
               category = excluded.category,
               size_bytes = excluded.size_bytes,
               created_at = excluded.created_at,
               imported_at = excluded.imported_at,
               phash = excluded.phash",
            params![
                meta.uuid,
                meta.file_name,
                meta.rel_path,
                meta.category,
                meta.size_bytes,
                meta.created_at,
                meta.imported_at,
                meta.phash
            ],
        )?;
        self.conn
            .execute("DELETE FROM tags WHERE asset_uuid = ?1", params![meta.uuid])?;
        for tag in &meta.tags {
            self.conn.execute(
                "INSERT OR IGNORE INTO tags (asset_uuid, tag) VALUES (?1, ?2)",
                params![meta.uuid, tag],
            )?;
        }
        Ok(())
    }

    pub fn get_asset(&self, uuid: &str) -> Result<Option<AssetMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, file_name, rel_path, category, size_bytes, created_at, imported_at, phash
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
        };
        drop(rows);
        drop(stmt);
        let mut tag_stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE asset_uuid = ?1 ORDER BY tag")?;
        let tag_rows = tag_stmt.query_map(params![asset.uuid], |row| row.get::<_, String>(0))?;
        for tag in tag_rows {
            asset.tags.push(tag?);
        }
        Ok(Some(asset))
    }

    /// FTS5 trigram 全文检索：查询须为连续子串且 ≥3 字符（tokenizer 固有限制）。
    /// 查询以引号短语形式传入，规避 FTS5 查询语法对标点/保留字的解析。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let phrase = format!("\"{}\"", query.replace('"', "\"\""));
        let mut stmt = self.conn.prepare(
            "SELECT a.uuid, a.file_name
             FROM assets_fts
             JOIN assets a ON a.uuid = assets_fts.uuid
             WHERE assets_fts MATCH ?1
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
        let n = self
            .conn
            .execute("DELETE FROM assets WHERE uuid = ?1", params![uuid])?;
        Ok(n > 0)
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
}
