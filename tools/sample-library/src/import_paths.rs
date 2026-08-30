//! `--import-paths` 收集层（D49/D50 阶段 1）：
//!
//! UI 归类弹窗把一次混选拆成若干「来源组」，每组一个归类决策；壳层把这些
//! 决策写成临时清单文件，一次子进程调用完成整批导入（单进度条、单次重载）。
//!
//! 清单格式（设计修订：design.md §1.2 —— 混选各组分属不同决策，全局
//! `--category-override` 会把 .emo 组的包内分类一并覆盖，故改为**逐行指令**）：
//!
//! ```text
//! <kind>\t<mode>\t<path>
//! ```
//!
//! - `kind`：`f`=散文件、`d`=目录、`p`=.emo 包（UI 侧提示；实际读取仍走
//!   PackageRegistry 首命中，D24 纪律不变）
//! - `mode`：`auto`（按来源规则=现行为）| `inbox`（强制待分类）| `category:<名称>`
//!   （统一归入，explicit 胜过包内规则——RuleChain 既有语义）
//! - 路径字段取行内剩余全部字节（splitn），Windows 盘符冒号无碍。

use std::path::{Path, PathBuf};

use crate::packages::{DirectoryReader, EmoReader, ImportedAsset, PackageRegistry};
use crate::CliMode;

/// 来源种类（清单行首列；仅作 UI 决策回显，读取侧以注册表为准）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Loose,
    Folder,
    Package,
}

/// 单条来源的归类指令（D50 三方式 → 管线显式指令，不再依赖静默 RuleChain 兜底）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportDirective {
    /// 按来源规则（.emo=包内分类、目录=按文件夹名、散文件=待分类）。
    Auto,
    /// 放入待分类（category 置 None，落库侧归一为 INBOX_CATEGORY）。
    Inbox,
    /// 统一归入指定分类（explicit > 包内规则）。
    Category(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportLine {
    pub kind: SourceKind,
    pub directive: ImportDirective,
    pub path: PathBuf,
}

/// 解析清单文本：三列制表符分隔；空行/纯空白行跳过；kind/mode 非法即报错
/// （清单是壳层机器生成的，出错说明上游 bug，宁可硬失败不给静默半批导入）。
pub fn parse_import_paths(text: &str) -> Result<Vec<ImportLine>, String> {
    let mut lines = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let fail = |why: String| format!("--import-paths 第 {} 行非法：{why}", index + 1);
        let mut fields = raw.splitn(3, '\t');
        let kind = match fields.next().unwrap_or_default() {
            "f" => SourceKind::Loose,
            "d" => SourceKind::Folder,
            "p" => SourceKind::Package,
            other => return Err(fail(format!("未知来源种类 {other:?}"))),
        };
        let mode_raw = fields.next().unwrap_or_default();
        let directive = if mode_raw == "auto" {
            ImportDirective::Auto
        } else if mode_raw == "inbox" {
            ImportDirective::Inbox
        } else if let Some(name) = mode_raw.strip_prefix("category:") {
            if name.trim().is_empty() {
                return Err(fail("category: 后缺分类名".into()));
            }
            ImportDirective::Category(name.to_string())
        } else {
            return Err(fail(format!("未知归类指令 {mode_raw:?}")));
        };
        let path = fields.next().ok_or_else(|| fail("缺路径列".into()))?;
        if path.trim().is_empty() {
            return Err(fail("路径为空".into()));
        }
        lines.push(ImportLine {
            kind,
            directive,
            path: PathBuf::from(path),
        });
    }
    Ok(lines)
}

/// 指令作用到单个素材（读取器已按来源规则填好 category，指令按需覆盖）。
fn apply_directive(asset: &mut ImportedAsset, directive: &ImportDirective) {
    match directive {
        ImportDirective::Auto => {}
        ImportDirective::Inbox => asset.category = None,
        ImportDirective::Category(name) => asset.category = Some(name.clone()),
    }
}

/// 逐条来源走 PackageRegistry（首命中），汇总全部素材后**一次性** run_import
/// （单进度条、单 done 汇总）。单条来源读失败只记账跳过（与目录扫描
/// 「单目录读不动不拖垮整批」同一语义）；清单为空 = 什么都不做，库不建。
pub fn run_import_paths(lines: &[ImportLine], out: &Path, mode: CliMode) -> Result<(), String> {
    if lines.is_empty() {
        // 取消/空选择路径：不 open 库（open 会建目录与 meta.db）。
        return Ok(());
    }
    let mut registry = PackageRegistry::new();
    registry
        .register_reader(Box::new(EmoReader::new()))
        .register_reader(Box::new(DirectoryReader));

    let mut assets: Vec<ImportedAsset> = Vec::new();
    // 清理必须**后置**（2026-08-30 用户 .emo 整包丢失事故）：EmoReader 把包
    // 解到临时目录，assets 的 source 路径全指向里面——读完即删的话，
    // run_import 拿到的路径全部失效，「导入完成」实际 imported=0。镜像
    // import_package 的正确序：先导入，后清理解包目录。
    let mut cleanups: Vec<PathBuf> = Vec::new();
    for line in lines {
        // 单文件来源（D49 主导入多选的主体）：DirectoryReader 只认目录，散
        // 文件直接入列；可导入性按 media 注册表判扩展名，不支持 = 静默跳过
        // （R4 拖入语义一致）。
        if line.path.is_file() && !crate::packages::is_emo_archive(&line.path) {
            if media::is_importable(&line.path) {
                let mut asset = ImportedAsset {
                    source: line.path.clone(),
                    category: None,
                    tags: Vec::new(),
                };
                apply_directive(&mut asset, &line.directive);
                assets.push(asset);
            } else {
                eprintln!("warn: 跳过不支持的文件类型: {}", line.path.display());
            }
            continue;
        }
        let Some(reader) = registry.reader_for(&line.path) else {
            // 不支持的类型：静默跳过（R4 拖入语义），日志留痕即可。
            eprintln!("warn: 跳过不支持的导入来源: {}", line.path.display());
            continue;
        };
        match reader.read(&line.path) {
            Ok(mut package) => {
                for asset in &mut package.assets {
                    apply_directive(asset, &line.directive);
                }
                assets.extend(package.assets);
                if let Some(cleanup) = package.cleanup.take() {
                    cleanups.push(cleanup);
                }
            }
            Err(error) => {
                eprintln!(
                    "warn: 读取来源失败 {}: {error}（跳过）",
                    line.path.display()
                );
            }
        }
    }
    let result = crate::run_import(&assets, out, mode);
    for cleanup in cleanups {
        let _ = std::fs::remove_dir_all(cleanup);
    }
    result
}

/// `--import-paths <file> --library <root> [--mode fast|background]` 分支。
pub fn run_cli(args: &[String]) -> Result<(), String> {
    let mut paths_file: Option<String> = None;
    let mut library: Option<PathBuf> = None;
    let mut mode = CliMode::Fast;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--import-paths" => {
                paths_file = Some(it.next().ok_or("--import-paths 缺少值")?.clone())
            }
            "--library" => library = Some(PathBuf::from(it.next().ok_or("--library 缺少值")?)),
            "--mode" => mode = CliMode::parse(it.next().ok_or("--mode 缺少值")?)?,
            other => return Err(format!("--import-paths 分支不识别的参数: {other}")),
        }
    }
    let paths_file = paths_file.ok_or("--import-paths 分支需要 --import-paths <清单文件>")?;
    let library = library.ok_or("--import-paths 分支需要 --library <库根>")?;
    let text = std::fs::read_to_string(&paths_file)
        .map_err(|e| format!("读取清单 {paths_file} 失败: {e}"))?;
    let lines = parse_import_paths(&text)?;
    run_import_paths(&lines, &library, mode)
}

/// `--probe-categories <path>` 分支：stdout 出一行 `PROBE<HT>categories=<n|none>`
/// 一行（ChildTask 的行回调在阶段 3 扩展后由 UI 解析）；出错走非零退出码。
pub fn run_probe(args: &[String]) -> Result<(), String> {
    let mut target: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--probe-categories" => {
                target = Some(PathBuf::from(it.next().ok_or("--probe-categories 缺少值")?))
            }
            other => return Err(format!("--probe-categories 分支不识别的参数: {other}")),
        }
    }
    let target = target.ok_or("--probe-categories 需要 <path>")?;
    let count = probe_source_categories(&target)?;
    match count {
        Some(n) => println!("PROBE	categories={n}"),
        None => println!("PROBE	categories=none"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 来源分类数预扫描（D50「含 N 个分类」标注；C2：只读结构，零解码零解压）
// ---------------------------------------------------------------------------

/// 预扫描来源的可归类数：
/// - .emo 包 → zip 中央目录里文件条目的顶层目录去重数（不解压，读尾即可）；
/// - 千牛结构目录 → 递归数 EmotionConfig.json 的去重 groupName（与读取器
///   同一判定：根目录自身的 config 不算——reader 对 dir==root 不走千牛分支）；
/// - 普通目录 / 散文件 → None（弹窗不给 N 标注）。
pub fn probe_source_categories(path: &Path) -> Result<Option<usize>, String> {
    if path.is_file() {
        if crate::packages::is_emo_archive(path) {
            probe_emo(path)
        } else {
            Ok(None)
        }
    } else if path.is_dir() {
        Ok(probe_dir_categories(path))
    } else {
        Ok(None)
    }
}

fn probe_emo(path: &Path) -> Result<Option<usize>, String> {
    let mut tops = std::collections::HashSet::new();
    for name in list_zip_entry_names(path)? {
        // 目录条目（以 / 结尾）与根级散文件不构成分类；顶层目录 = 首段。
        if let Some(top) = name.split('/').next() {
            if name.contains('/') && !top.is_empty() {
                tops.insert(top.to_string());
            }
        }
    }
    Ok(Some(tops.len()))
}

fn probe_dir_categories(root: &Path) -> Option<usize> {
    let mut groups = std::collections::HashSet::new();
    walk_emotion_configs(root, root, &mut groups);
    if groups.is_empty() {
        None
    } else {
        Some(groups.len())
    }
}

/// 与 collect_files_inner 的千牛判定逐字对齐：子目录（含任意深度）挂
/// EmotionConfig.json 即收其 groupName 且不再下探；根目录自身不算。
fn walk_emotion_configs(root: &Path, dir: &Path, groups: &mut std::collections::HashSet<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let config = path.join("EmotionConfig.json");
        if path != root && config.is_file() {
            for name in groups_from_emotion_config(&config) {
                groups.insert(name);
            }
            continue;
        }
        walk_emotion_configs(root, &path, groups);
    }
}

/// EmotionConfig.json → 非空 groupName 列表（BOM 容忍与读取器一致）。
fn groups_from_emotion_config(config: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(config) else {
        return Vec::new();
    };
    let text = text.trim_start_matches('\u{feff}');
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(text) else {
        return Vec::new();
    };
    let mut groups = Vec::new();
    for entry in entries {
        if let Some(name) = entry.get("groupName").and_then(|v| v.as_str()) {
            if !name.is_empty() && !groups.contains(&name.to_string()) {
                groups.push(name.to_string());
            }
        }
    }
    groups
}

/// 极简 zip 中央目录读取：只取文件条目名，不解压不校验 CRC。
/// 实现约束：.emo 可达数十 MB，绝不整读进内存——EOCD 从文件尾窗扫描，
/// 中央目录逐条 seek 读（内存纪律 D3/D4 的子进程侧同样适用）。
fn list_zip_entry_names(path: &Path) -> Result<Vec<String>, String> {
    use std::io::{Read, Seek, SeekFrom};

    const EOCD_SIG: u32 = 0x0605_4b50;
    const CEN_SIG: u32 = 0x0201_4b50;

    let mut file =
        std::fs::File::open(path).map_err(|e| format!("打开 {} 失败: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len < 22 {
        return Err("不是有效的 zip（过短）".into());
    }
    // EOCD 最长 22 + 65535 注释；尾窗取 66KB 足够。
    let window = 66_000u64.min(len);
    file.seek(SeekFrom::End(-(window as i64)))
        .map_err(|e| e.to_string())?;
    let mut tail = vec![0u8; window as usize];
    file.read_exact(&mut tail).map_err(|e| e.to_string())?;

    let eocd = tail
        .windows(4)
        .rev()
        .find(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) == EOCD_SIG)
        .ok_or("找不到 zip 中央目录结尾记录（EOCD）")?;
    let at =
        |offset: usize| -> usize { (eocd.as_ptr() as usize - tail.as_ptr() as usize) + offset };
    let cd_size = u32::from_le_bytes(tail[at(12)..at(16)].try_into().unwrap()) as u64;
    let cd_offset = u32::from_le_bytes(tail[at(16)..at(20)].try_into().unwrap()) as u64;
    // ZIP64 溢出（0xFFFFFFFF）的包 .emo 场景不存在；直接按不支持处理。
    if cd_offset == u64::from(u32::MAX) || cd_size == u64::from(u32::MAX) {
        return Err("ZIP64 包暂不支持预扫描".into());
    }

    file.seek(SeekFrom::Start(cd_offset))
        .map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    let mut header = [0u8; 46];
    let mut remaining = cd_size;
    while remaining >= 46 {
        if file.read_exact(&mut header).is_err() {
            break;
        }
        if u32::from_le_bytes(header[0..4].try_into().unwrap()) != CEN_SIG {
            break;
        }
        let name_len = u16::from_le_bytes(header[28..30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(header[30..32].try_into().unwrap()) as usize;
        let comment_len = u16::from_le_bytes(header[32..34].try_into().unwrap()) as usize;
        let mut name_buf = vec![0u8; name_len];
        if file.read_exact(&mut name_buf).is_err() {
            break;
        }
        let skip = extra_len + comment_len;
        if skip > 0 {
            file.seek(SeekFrom::Current(skip as i64))
                .map_err(|e| e.to_string())?;
        }
        names.push(String::from_utf8_lossy(&name_buf).into_owned());
        remaining -= (46 + name_len + skip) as u64;
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("import_paths_{tag}_{}_{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_png(path: &Path) {
        let img = image::RgbImage::from_fn(8, 8, |_x, _y| image::Rgb([180, 180, 180]));
        image::DynamicImage::ImageRgb8(img)
            .save(path)
            .expect("写测试图失败");
    }

    /// 千牛结构目录：容器/组A/{EmotionConfig.json, a.jpg}——读取器对源根自身
    /// 的 config 不走千牛分支（dir != root 判定），fixture 必须包一层容器。
    fn make_qianniu_container(root: &Path) -> PathBuf {
        let group = root.join("容器").join("组A");
        fs::create_dir_all(&group).unwrap();
        fs::write(
            group.join("EmotionConfig.json"),
            r#"[{"originalFile":"a.jpg","groupName":"表情组"}]"#,
        )
        .unwrap();
        write_png(&group.join("a.jpg"));
        root.join("容器")
    }

    // ----- 红灯 1：清单解析（三分来源 + 三种指令 + 容错） -----

    #[test]
    fn parses_three_kinds_and_mode_directives() {
        let text = "f\tauto\tC:\\a.png\n\
                    \n\
                    d\tauto\tC:\\目录\n\
                    p\tauto\tC:\\x.emo\n\
                    f\tcategory:相册\tC:\\b.jpg\n\
                    f\tinbox\tC:\\c.png\n";
        let lines = parse_import_paths(text).unwrap();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].kind, SourceKind::Loose);
        assert_eq!(lines[1].kind, SourceKind::Folder);
        assert_eq!(lines[2].kind, SourceKind::Package);
        assert_eq!(lines[3].directive, ImportDirective::Category("相册".into()));
        assert_eq!(lines[4].directive, ImportDirective::Inbox);
        assert_eq!(lines[3].path, PathBuf::from("C:\\b.jpg"));

        assert_eq!(parse_import_paths("  \n\t\n").unwrap().len(), 0);
        assert!(parse_import_paths("f\tauto").is_err(), "缺路径列要报错");
        assert!(
            parse_import_paths("x\tauto\tC:\\a").is_err(),
            "未知 kind 要报错"
        );
        assert!(
            parse_import_paths("f\tbogus\tC:\\a").is_err(),
            "未知 mode 要报错"
        );
        assert!(
            parse_import_paths("f\tcategory: \tC:\\a").is_err(),
            "空分类名要报错"
        );
    }

    // ----- 红灯 6：分类数预扫描（.emo zip / 千牛目录 / 普通目录三分） -----

    /// 手工构造 stored（method 0）zip：probe 只读名字，但 fixture 做成真包
    /// （CRC 正确），以后复用不踩假 zip 的坑。
    fn write_stored_zip(path: &Path, entries: &[(&str, &[u8])]) {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xFFFF_FFFFu32;
            for &b in data {
                crc ^= b as u32;
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
                }
            }
            !crc
        }
        fn le16(v: u16) -> [u8; 2] {
            v.to_le_bytes()
        }
        fn le32(v: u32) -> [u8; 4] {
            v.to_le_bytes()
        }

        let mut body = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let offset = body.len() as u32;
            let crc = crc32(data);
            let name = name.as_bytes();
            body.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            body.extend_from_slice(&le16(20)); // version needed
            body.extend_from_slice(&le16(0x0800)); // flags: UTF-8
            body.extend_from_slice(&le16(0)); // method: stored
            body.extend_from_slice(&le16(0)); // time
            body.extend_from_slice(&le16(0)); // date
            body.extend_from_slice(&le32(crc));
            body.extend_from_slice(&le32(data.len() as u32));
            body.extend_from_slice(&le32(data.len() as u32));
            body.extend_from_slice(&le16(name.len() as u16));
            body.extend_from_slice(&le16(0)); // extra len
            body.extend_from_slice(name);
            body.extend_from_slice(data);

            central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            central.extend_from_slice(&le16(20)); // version made by
            central.extend_from_slice(&le16(20)); // version needed
            central.extend_from_slice(&le16(0x0800));
            central.extend_from_slice(&le16(0)); // method
            central.extend_from_slice(&le16(0));
            central.extend_from_slice(&le16(0));
            central.extend_from_slice(&le32(crc));
            central.extend_from_slice(&le32(data.len() as u32));
            central.extend_from_slice(&le32(data.len() as u32));
            central.extend_from_slice(&le16(name.len() as u16));
            central.extend_from_slice(&le16(0)); // extra
            central.extend_from_slice(&le16(0)); // comment
            central.extend_from_slice(&le16(0)); // disk start
            central.extend_from_slice(&le16(0)); // internal attrs
            central.extend_from_slice(&le32(0)); // external attrs
            central.extend_from_slice(&le32(offset));
            central.extend_from_slice(name);
        }
        let mut out = body;
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        let cd_size = out.len() as u32 - cd_offset;
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        out.extend_from_slice(&le16(0)); // disk
        out.extend_from_slice(&le16(0)); // cd disk
        out.extend_from_slice(&le16(entries.len() as u16));
        out.extend_from_slice(&le16(entries.len() as u16));
        out.extend_from_slice(&le32(cd_size));
        out.extend_from_slice(&le32(cd_offset));
        out.extend_from_slice(&le16(0)); // comment len
        fs::write(path, out).unwrap();
    }

    #[test]
    fn probe_counts_emo_categories_from_zip_central_directory() {
        let root = temp_root("probe_emo");
        let emo = root.join("pack.emo");
        write_stored_zip(
            &emo,
            &[
                ("表情A/a.png", b"a" as &[u8]),
                ("表情A/b.png", b"b"),
                ("表情B/c.png", b"c"),
                ("readme.txt", b"root file"),
            ],
        );
        assert_eq!(probe_source_categories(&emo).unwrap(), Some(2));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn probe_counts_qianniu_groups_and_none_for_plain_sources() {
        let root = temp_root("probe_dir");
        let qn = make_qianniu_container(&root);
        assert_eq!(probe_source_categories(&qn).unwrap(), Some(1));

        // 普通目录（无 EmotionConfig）与散文件 → None（不给 N 标注）。
        let plain = root.join("plain");
        fs::create_dir_all(&plain).unwrap();
        write_png(&plain.join("x.png"));
        assert_eq!(probe_source_categories(&plain).unwrap(), None);
        assert_eq!(probe_source_categories(&plain.join("x.png")).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }
}
