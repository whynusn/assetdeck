//! 素材包读写器抽象（综合分析报告「三.1」）。
//!
//! 入口（main.rs）只面向「包」编程：读 = `PackageRegistry::reader_for(path) -> read`，
//! 写 = `PackageRegistry::writer_for(path) -> write(&library, output)`。新包格式
//! （zip / eagle 等）只需在这里注册新的 `AssetPackageReader` / `AssetPackageWriter`，
//! 不再改动 main.rs。
//!
//! 当前内置两种：
//! - `DirectoryReader`：普通目录递归扫描（config.json 分类白名单 +
//!   EmotionConfig.json originalFile 过滤 + media 注册表导入判定 +
//!   library::rules 分类规则）；
//! - `EmoReader` / `EmoWriter`：千牛 .emo（zip）包，解压 / 打包走
//!   PowerShell + System.IO.Compression。

use library::rules::{default_import_chain, CategoryContext, RuleChain};
use library::Library;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 一条待导入素材。
pub struct ImportedAsset {
    pub source: PathBuf,
    pub category: Option<String>,
    pub tags: Vec<String>,
}

/// 一次「读包」的产物。
pub struct PackageRead {
    pub assets: Vec<ImportedAsset>,
    /// 解压出的临时根：main 完成导入后应删除（全部资源已拷入 .library）。
    pub cleanup: Option<PathBuf>,
}

/// 包读写错误。
#[derive(Debug)]
pub enum PackageError {
    Io(std::io::Error),
    Message(String),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageError::Io(e) => write!(f, "IO 错误: {e}"),
            PackageError::Message(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PackageError::Io(e) => Some(e),
            PackageError::Message(_) => None,
        }
    }
}

impl From<std::io::Error> for PackageError {
    fn from(e: std::io::Error) -> Self {
        PackageError::Io(e)
    }
}

impl From<String> for PackageError {
    fn from(e: String) -> Self {
        PackageError::Message(e)
    }
}

/// 素材包读取器：把任意格式的「包」读成统一的资产列表。
pub trait AssetPackageReader: Send + Sync {
    /// 格式名（诊断 / 日志用）；当前入口流程暂不调用。
    #[allow(dead_code)]
    fn name(&self) -> &str;
    fn can_read(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> Result<PackageRead, PackageError>;
}

/// 素材包写出器：把整个 .library 写成一个「包」。
pub trait AssetPackageWriter: Send + Sync {
    /// 格式名（诊断 / 日志用）；当前入口流程暂不调用。
    #[allow(dead_code)]
    fn name(&self) -> &str;
    fn can_write(&self, path: &Path) -> bool;
    fn write(&self, library: &Library, output: &Path) -> Result<(), PackageError>;
}

/// 注册表：按路径挑选能处理该路径的读写器（首个命中者）。
pub struct PackageRegistry {
    readers: Vec<Box<dyn AssetPackageReader>>,
    writers: Vec<Box<dyn AssetPackageWriter>>,
}

impl PackageRegistry {
    pub fn new() -> Self {
        Self {
            readers: Vec::new(),
            writers: Vec::new(),
        }
    }

    pub fn register_reader(&mut self, reader: Box<dyn AssetPackageReader>) -> &mut Self {
        self.readers.push(reader);
        self
    }

    pub fn register_writer(&mut self, writer: Box<dyn AssetPackageWriter>) -> &mut Self {
        self.writers.push(writer);
        self
    }

    pub fn reader_for(&self, path: &Path) -> Option<&dyn AssetPackageReader> {
        self.readers.iter().find(|reader| reader.can_read(path)).map(|r| r.as_ref())
    }

    pub fn writer_for(&self, path: &Path) -> Option<&dyn AssetPackageWriter> {
        self.writers.iter().find(|writer| writer.can_write(path)).map(|w| w.as_ref())
    }
}

// ---------- DirectoryReader ----------

/// 目录素材包：递归扫描目录，支持 config.json 分类白名单与千牛
/// EmotionConfig.json（originalFile 过滤、groupName 分类、qianniu 标签）。
pub struct DirectoryReader;

impl AssetPackageReader for DirectoryReader {
    fn name(&self) -> &str {
        "directory"
    }

    fn can_read(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn read(&self, path: &Path) -> Result<PackageRead, PackageError> {
        let assets = scan_directory(path).map_err(PackageError::Message)?;
        Ok(PackageRead {
            assets,
            cleanup: None,
        })
    }
}

fn scan_directory(root: &Path) -> Result<Vec<ImportedAsset>, String> {
    let categories = read_config_categories(root)?;
    let allowed: Option<HashSet<String>> = categories.map(|names| names.into_iter().collect());
    let chain = default_import_chain();
    let mut assets = Vec::new();
    collect_files_inner(root, root, allowed.as_ref(), &chain, &mut assets)?;
    assets.sort_by(|a, b| a.source.cmp(&b.source));
    Ok(assets)
}

/// 递归收集目录内可导入资产（沿用既有 collect_files 语义）：
/// - 子目录含 EmotionConfig.json 时按千牛语义只收 originalFile 条目；
/// - 普通目录跳过 config.json，按 media 注册表过滤扩展名；
/// - 分类统一走 library::rules（GroupNameRule 优先、ParentDirectoryRule 兜底），
///   并受 config.json 白名单约束；普通目录源码不挂 qianniu 标签。
fn collect_files_inner(
    root: &Path,
    dir: &Path,
    allowed: Option<&HashSet<String>>,
    chain: &RuleChain,
    out: &mut Vec<ImportedAsset>,
) -> Result<(), String> {
    let emotion_config = dir.join("EmotionConfig.json");
    if dir != root && emotion_config.is_file() {
        return collect_emotion_config_dir(dir, &emotion_config, allowed, chain, out);
    }

    // 单个目录读不动（被占用/权限）只跳过并留痕，不让整批导入失败——
    // 与「单文件损坏不拖垮整批」同一语义。entry 级错误仍向上抛（系统性
    // IO 故障应当硬失败，静默吞掉会变成悄悄丢素材）。
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("warn: 无法读取目录 {}：{e}（跳过）", dir.display());
            return Ok(());
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_inner(root, &path, allowed, chain, out)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("config.json") {
            continue;
        }
        if !is_supported_asset_ext(&path) {
            continue;
        }

        // 普通目录：分类 = 父目录名（ParentDirectoryRule）；根目录下直接文件无分类。
        let category = chain
            .resolve(&path, &CategoryContext { explicit: None, group: None })
            .filter(|_| path.parent().map(|parent| parent != root).unwrap_or(false))
            .filter(|name| allowed.map(|set| set.contains(name)).unwrap_or(true));
        out.push(ImportedAsset {
            source: path,
            category,
            tags: Vec::new(),
        });
    }
    Ok(())
}

/// 千牛 EmotionConfig.json 目录语义：只收集条目里声明的 originalFile
/// （fixedFile 是派生修正图，导入时跳过避免重复入库）。
fn collect_emotion_config_dir(
    dir: &Path,
    config_path: &Path,
    allowed: Option<&HashSet<String>>,
    chain: &RuleChain,
    out: &mut Vec<ImportedAsset>,
) -> Result<(), String> {
    let text = std::fs::read_to_string(config_path)
        .map_err(|e| format!("{}: {e}", config_path.display()))?;
    let text = text.trim_start_matches('\u{feff}');
    let entries: Vec<serde_json::Value> = serde_json::from_str(text)
        .map_err(|e| format!("invalid {}: {e}", config_path.display()))?;

    for entry in entries {
        let original = entry
            .get("originalFile")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if original.is_empty() {
            continue;
        }
        let path = dir.join(original);
        if !path.is_file() {
            continue;
        }
        if !is_supported_asset_ext(&path) {
            continue;
        }

        // 分类规则（default_import_chain）：GroupNameRule 优先、ParentDirectoryRule
        // 兜底。白名单先作用于 groupName：不在 config.json 白名单时退回目录名，
        // 与既有行为一致。
        let group = entry
            .get("groupName")
            .and_then(|v| v.as_str())
            .filter(|name| allowed.map(|set| set.contains(*name)).unwrap_or(true));
        let category = chain
            .resolve(&path, &CategoryContext { explicit: None, group })
            .filter(|name| allowed.map(|set| set.contains(name)).unwrap_or(true));
        out.push(ImportedAsset {
            source: path,
            category,
            // 千牛 EmotionConfig 素材包的内容整体视为 qianniu 来源。
            tags: vec!["qianniu".to_string()],
        });
    }
    Ok(())
}

/// 可导入扩展名判定：收敛到 media 注册表（综合分析报告「扩展性缺口 #2」）。
fn is_supported_asset_ext(path: &Path) -> bool {
    media::is_importable(path)
}

/// 素材包根目录 config.json 的「允许分类」白名单；无该文件时不过滤。
fn read_config_categories(root: &Path) -> Result<Option<Vec<String>>, String> {
    let config = root.join("config.json");
    if !config.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&config).map_err(|e| e.to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    let categories: Vec<String> =
        serde_json::from_str(text).map_err(|e| format!("invalid config.json: {e}"))?;
    Ok(Some(categories))
}

// ---------- EmoReader / EmoWriter ----------

/// 千牛 .emo 素材包（zip）：解压到临时目录后委托 DirectoryReader 扫描，
/// 返回的 cleanup 指向该临时根，由 main 导入完成后删除。
pub struct EmoReader {
    inner: DirectoryReader,
}

impl EmoReader {
    pub fn new() -> Self {
        Self {
            inner: DirectoryReader,
        }
    }
}

impl AssetPackageReader for EmoReader {
    fn name(&self) -> &str {
        "qianniu-emo"
    }

    fn can_read(&self, path: &Path) -> bool {
        is_emo_archive(path)
    }

    fn read(&self, path: &Path) -> Result<PackageRead, PackageError> {
        let dest = std::env::temp_dir().join(format!(
            "qianniu_emo_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dest)?;
        extract_emo(path, &dest)?;
        let mut package = self.inner.read(&dest)?;
        package.cleanup = Some(dest);
        Ok(package)
    }
}

/// .emo 且是文件（目录叫 .emo 不算归档，交给 DirectoryReader）。
fn is_emo_archive(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("emo"))
            .unwrap_or(false)
}

/// PowerShell + System.IO.Compression 解压到目标目录。
fn extract_emo(emo: &Path, dest: &Path) -> Result<(), PackageError> {
    let emo_abs = std::path::absolute(emo)?;
    let dest_abs = std::path::absolute(dest)?;
    let script = format!(
        "Add-Type -AssemblyName System.IO.Compression.FileSystem; \
         [System.IO.Compression.ZipFile]::ExtractToDirectory('{}', '{}')",
        emo_abs.display().to_string().replace('\'', "''"),
        dest_abs.display().to_string().replace('\'', "''")
    );
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("failed to run powershell for emo extraction: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "emo extraction failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

/// 千牛 .emo 素材包写出：按分类整理到临时目录 + config.json + zip 打包。
pub struct EmoWriter {
    /// 素材库根目录（导出时把 rel_path 还原为源文件绝对路径；library 未暴露根）。
    library_root: PathBuf,
}

impl EmoWriter {
    pub fn new(library_root: PathBuf) -> Self {
        Self { library_root }
    }
}

impl AssetPackageWriter for EmoWriter {
    fn name(&self) -> &str {
        "qianniu-emo"
    }

    fn can_write(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("emo"))
            .unwrap_or(false)
    }

    fn write(&self, library: &Library, output: &Path) -> Result<(), PackageError> {
        export_qianniu(library, &self.library_root, output)
    }
}

fn export_qianniu(
    library: &Library,
    library_root: &Path,
    output: &Path,
) -> Result<(), PackageError> {
    let temp = std::env::temp_dir().join(format!(
        "qianniu_export_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&temp)?;

    let result = (|| -> Result<(), PackageError> {
        let total = library
            .store()
            .all_assets_count()
            .map_err(|e| e.to_string())?
            as usize;
        let mut categories: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut done = 0usize;
        let error: RefCell<Option<String>> = RefCell::new(None);

        library
            .store()
            .for_each_asset(|meta| {
                if error.borrow().is_some() {
                    return;
                }
                let category = meta
                    .category
                    .clone()
                    .unwrap_or_else(|| "Uncategorized".to_string());
                if seen.insert(category.clone()) {
                    categories.push(category.clone());
                }
                let category_dir = temp.join(&category);
                if let Err(e) = std::fs::create_dir_all(&category_dir) {
                    *error.borrow_mut() = Some(e.to_string());
                    return;
                }
                let source = join_rel(library_root, &meta.rel_path);
                let dest_name = if meta.file_name.is_empty() {
                    format!("{}.bin", meta.uuid)
                } else {
                    meta.file_name.clone()
                };
                if let Err(e) = std::fs::copy(&source, category_dir.join(&dest_name)) {
                    *error.borrow_mut() = Some(format!("{}: {e}", source.display()));
                    return;
                }
                done += 1;
                println!("PROGRESS\t{done}\t{total}");
            })
            .map_err(|e| e.to_string())?;

        if let Some(e) = error.into_inner() {
            return Err(e.into());
        }

        let config = serde_json::to_string(&categories).map_err(|e| e.to_string())?;
        std::fs::write(temp.join("config.json"), config)?;

        let temp_abs = std::path::absolute(&temp)?;
        let out_abs = std::path::absolute(output)?;
        if let Some(parent) = out_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if out_abs.exists() {
            std::fs::remove_file(&out_abs)?;
        }
        let script = format!(
            "Add-Type -AssemblyName System.IO.Compression.FileSystem; \
             [System.IO.Compression.ZipFile]::CreateFromDirectory('{}', '{}', \
             [System.IO.Compression.CompressionLevel]::Optimal, $false)",
            temp_abs.display().to_string().replace('\'', "''"),
            out_abs.display().to_string().replace('\'', "''")
        );
        let powershell = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .output()
            .map_err(|e| format!("failed to run powershell for emo export: {e}"))?;
        if !powershell.status.success() {
            return Err(format!(
                "emo export failed: {}",
                String::from_utf8_lossy(&powershell.stderr).trim()
            )
            .into());
        }
        Ok(())
    })();

    let _ = std::fs::remove_dir_all(&temp);
    result
}

/// `rel_path` 以 '/' 分隔存储，必须逐段拼接（整串 join 会产出混合分隔路径）。
fn join_rel(root: &Path, rel_path: &str) -> PathBuf {
    let mut joined = root.to_path_buf();
    for segment in rel_path.split('/').filter(|s| !s.is_empty()) {
        joined.push(segment);
    }
    std::path::absolute(&joined).unwrap_or(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "sample_library_{tag}_{}_{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_file(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn emotion_config_imports_only_original_files() {
        let root = temp_root("emotion_config");
        write_file(&root.join("config.json"), r#"["马赛床"]"#.as_bytes());
        write_file(
            &root.join("马赛床/EmotionConfig.json"),
            r#"[{"originalFile":"a.jpg","fixedFile":"afixed.jpg","groupName":"马赛床"}]"#
                .as_bytes(),
        );
        write_file(&root.join("马赛床/a.jpg"), b"a");
        write_file(&root.join("马赛床/afixed.jpg"), b"fixed");
        write_file(&root.join("马赛床/not_listed.png"), b"not");

        let reader = DirectoryReader;
        let package = reader.read(&root).unwrap();
        let names: Vec<_> = package
            .assets
            .iter()
            .map(|a| {
                (
                    a.source.file_name().unwrap().to_str().unwrap().to_string(),
                    a.category.clone(),
                    a.tags.clone(),
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![(
                "a.jpg".to_string(),
                Some("马赛床".to_string()),
                vec!["qianniu".to_string()]
            )]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plain_directory_falls_back_to_recursive_scan() {
        let root = temp_root("plain_dir");
        write_file(&root.join("config.json"), br#"["cat"]"#);
        write_file(&root.join("cat/a.jpg"), b"a");
        write_file(&root.join("cat/b.png"), b"b");

        let reader = DirectoryReader;
        let package = reader.read(&root).unwrap();
        assert_eq!(package.assets.len(), 2);
        // 普通目录：不挂 qianniu 标签。
        assert!(package.assets.iter().all(|a| a.tags.is_empty()));
        // 分类 = 父目录名（ParentDirectoryRule），并受 config.json 白名单约束。
        assert!(package.assets.iter().all(|a| a.category.as_deref() == Some("cat")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn emo_extension_directory_is_not_treated_as_archive() {
        let dir = temp_root("emo_dir");
        let emo_dir = dir.join("素材包.emo");
        fs::create_dir_all(&emo_dir).unwrap();
        assert!(!EmoReader::new().can_read(&emo_dir));
        assert!(DirectoryReader.can_read(&emo_dir));
        fs::remove_dir_all(dir).unwrap();
    }
}
