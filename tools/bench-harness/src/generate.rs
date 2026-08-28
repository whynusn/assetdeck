//! 确定性合成库生成器：元数据 + 渐变占位缩略图（无版权、可复现）。
//!
//! 确定性红线（spec/bench-harness quality-guidelines）：固定基准时间戳、
//! 字段全部由索引 `i` 纯算术派生，禁用时间/随机源——否则 CI 内存趋势无对比意义。
//!
//! 写入路径（spec database-guidelines）：经 [`store::Store`] 公共 API 落库，
//! 禁止手拼 SQLite。用 [`store::Store::upsert_assets`] 批量事务（分块提交）：
//! 逐行 `upsert_asset` 在 Windows 上每行一次 fsync，10 万行实测 ~14 分钟，
//! 违反「100k 规模生成秒级」承诺；行级写入语义不变。

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use image::{ImageFormat, RgbaImage};
use store::{AssetMeta, Store};

/// 批量事务分块大小：峰值内存（约 8000 行 × ~400B ≈ 3MB）与提交次数的折中。
const CHUNK: usize = 8192;

/// created_at 基准：固定常量而非 SystemTime::now（确定性红线）。
/// ⚠ 与 crates/app-ui/src/main.rs 的 `--bench` 分支共用同一契约值，改动须双侧同步。
pub const BASE_EPOCH_SECS: i64 = 1_700_000_000;

/// 占位缩略图边长（px）。design 契约：64×64 渐变。
pub const THUMB_SIZE: u32 = 64;

#[derive(Debug)]
pub enum GenerateError {
    Store(store::StoreError),
    Io(std::io::Error),
    Image(image::ImageError),
}

impl fmt::Display for GenerateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenerateError::Store(e) => write!(f, "存储错误: {e}"),
            GenerateError::Io(e) => write!(f, "IO 错误: {e}"),
            GenerateError::Image(e) => write!(f, "图像编码错误: {e}"),
        }
    }
}

impl std::error::Error for GenerateError {}

impl From<store::StoreError> for GenerateError {
    fn from(e: store::StoreError) -> Self {
        GenerateError::Store(e)
    }
}

impl From<std::io::Error> for GenerateError {
    fn from(e: std::io::Error) -> Self {
        GenerateError::Io(e)
    }
}

impl From<image::ImageError> for GenerateError {
    fn from(e: image::ImageError) -> Self {
        GenerateError::Image(e)
    }
}

pub type Result<T> = std::result::Result<T, GenerateError>;

/// 第 i 条合成资产的 uuid（TEXT 主键，纯函数确定性）。
/// ⚠ bench-harness 与 app-ui `--bench` 的共享契约标识。
pub fn uuid_of(i: u64) -> String {
    format!("bench-{i:08}")
}

/// 生成合成库到 `root`：
/// - `rows` 条元数据（uuid/file_name/created_at/phash 均由 i 派生）；
/// - 前 `thumbs` 条落盘 64×64 渐变 PNG（路径 = [`Store::thumbnail_cache_path`]）。
///
/// 幂等：重复生成同目录走 upsert 覆盖，行集不变。
pub fn generate_library(root: &Path, rows: u64, thumbs: usize) -> Result<()> {
    fs::create_dir_all(root)?;
    let store = Store::open(&root.join("meta.db"))?;

    for chunk_start in (0..rows).step_by(CHUNK) {
        let chunk_end = (chunk_start + CHUNK as u64).min(rows);
        let batch: Vec<AssetMeta> = (chunk_start..chunk_end)
            .map(|i| {
                let created_at = BASE_EPOCH_SECS + i as i64;
                AssetMeta {
                    uuid: uuid_of(i),
                    file_name: format!("asset_{i}.png"),
                    // 镜像真实导入的行形态（library::enqueue 的 objects/<uuid>/raw.png 惯例）
                    rel_path: format!("objects/{}/raw.png", uuid_of(i)),
                    category: None,
                    tags: vec![],
                    size_bytes: 65_536 + ((i % 977) * 13) as i64,
                    created_at,
                    imported_at: created_at,
                    // 全量写入确定性 phash：真实导入的图片均带 phash（去重红线），
                    // 行形态代表性优先于最小化；字节由 i 派生，不引入随机性。
                    phash: Some(i.to_be_bytes().to_vec()),
                    // 合成库的占位缩略图是 64×64 渐变，尺寸确定性派生自 i 无意义，
                    // 直接写死方形，让 grid_vm 走真实宽高比而非 fallback 公式。
                    width: Some(64),
                    height: Some(64),
                }
            })
            .collect();
        store.upsert_assets(&batch)?;
    }

    // 缩略图仅前 thumbs 条（design 权衡：「秒级生成」；浏览路径内存守卫只物化可见窗）
    let thumb_count = (thumbs as u64).min(rows);
    for i in 0..thumb_count {
        write_placeholder_thumb(root, i)?;
    }
    Ok(())
}

/// 渐变占位图：像素颜色是 (x, y, i) 的纯算术函数，编码输出逐字节可复现。
fn write_placeholder_thumb(root: &Path, i: u64) -> Result<()> {
    let mut img = RgbaImage::new(THUMB_SIZE, THUMB_SIZE);
    let bias = (i % 256) as u32;
    for y in 0..THUMB_SIZE {
        for x in 0..THUMB_SIZE {
            let r = ((x * 4) % 256) as u8;
            let g = ((y * 4 + bias) % 256) as u8;
            let b = (((x + y) * 2 + bias * 7) % 256) as u8;
            img.put_pixel(x, y, image::Rgba([r, g, b, 255]));
        }
    }
    let path: PathBuf = root.join(Store::thumbnail_cache_path(&uuid_of(i), "png"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    img.save_with_format(&path, ImageFormat::Png)?;
    Ok(())
}
