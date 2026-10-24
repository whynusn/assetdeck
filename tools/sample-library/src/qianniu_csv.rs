//! 千牛「快捷短语」CSV 导出拆解（拖入 / 导入素材 → 批量文本素材入库）。
//!
//! 导出格式（附件实测 UTF-8 带 BOM；编码统一走 media::normalize_text_to_utf8，
//! GBK / UTF-16 同样能吃）：首行表头 `快捷短语编码,快捷短语,快捷短语分组,
//! 快捷短语图片`。列语义映射（用户定盘）：编码 → 标签（tags 表，检索「仅
//! 标签」作用域可见）、短语 → 文本素材正文（D88 text_content + FTS）、分组
//! → 分类（D87 按名 ensure）、图片列忽略。
//!
//! 实现约束：`ImportRequest` 强依赖磁盘 source，拆解产物必须落临时 txt——
//! 每条一个子目录（编码列可能重名/为空，标题互撞时不许互相覆盖）。temp 根
//! 交调用方 cleanups，run_import 完成后统一删除（与 .emo 解包同一后置时序，
//! 2026-08-30 整包丢失事故的教训）。正文去重由 library enqueue 的 SHA-256
//! 拦截天然兜住：文件里完全相同的短语不会重复入库。

use std::path::{Path, PathBuf};

use crate::packages::ImportedAsset;

/// 表头判定只认前两列（后两列不作强校验——分组/图片列在旧导出里可能缺）。
const HEADER_CODE: &str = "快捷短语编码";
const HEADER_PHRASE: &str = "快捷短语";

/// 编码为空时的标题来源：正文折叠空白后取前若干字符。
const TITLE_MAX_CHARS: usize = 32;
/// 标题字节数帽（UTF-8）：32 个汉字 = 96 字节，同时兜住全 ASCII 长串。
const TITLE_MAX_BYTES: usize = 96;
const TITLE_FALLBACK: &str = "快捷短语";

/// csv 本体不作为素材导入；这里只是「可能是快捷短语导出」的入口候选
/// （真伪由表头判定，与 media::is_phrase_csv_candidate 同一语义）。
pub fn is_phrase_csv(path: &Path) -> bool {
    media::is_phrase_csv_candidate(path)
}

/// RFC4180 子集解析：逗号分隔、双引号包裹（"" 转义、引号内可含逗号/换行）、
/// \r\n 与 \n 都算行尾。裸引号（不在字段首）按字面字符处理；文件尾未闭合
/// 引号按已闭合收尾——宁可宽容解析也不让一行坏数据丢掉整批短语。
pub fn parse_csv_records(text: &str) -> Vec<Vec<String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => in_quotes = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(c),
        }
    }
    // 尾行无换行符（或尾逗号产生空字段）不丢；纯换行结尾不产幻影空行。
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

/// 表头判定：首行前两列（去首尾空白）恰为「快捷短语编码」「快捷短语」。
pub fn looks_like_phrase_csv(records: &[Vec<String>]) -> bool {
    let Some(header) = records.first() else {
        return false;
    };
    let code = header.first().map(|s| s.trim()).unwrap_or_default();
    let phrase = header.get(1).map(|s| s.trim()).unwrap_or_default();
    code == HEADER_CODE && phrase == HEADER_PHRASE
}

/// 字节 → 归一化 UTF-8（BOM 剥壳 / UTF-16 / GBK 兜底）→ RFC4180 解析。
pub fn parse_phrase_csv_bytes(bytes: &[u8]) -> Vec<Vec<String>> {
    let normalized = media::normalize_text_to_utf8(bytes);
    let text = std::str::from_utf8(normalized.as_ref()).unwrap_or_default();
    parse_csv_records(text)
}

/// 一条短语拆解后的入库计划。
pub struct PlannedPhrase {
    /// 卡片标题（= 临时 txt 文件名去 .txt）。
    pub title: String,
    /// 短语正文（首尾空白裁掉，内部换行保留）。
    pub body: String,
    /// 分类：第三列有值用第三列，否则继承整批的归类决策。
    pub category: Option<String>,
    /// 标签：第一列编码（可空）。
    pub tags: Vec<String>,
}

/// 表头后的每一行 → 入库计划；无正文的行整行丢弃。
pub fn plan_phrase_assets(
    records: &[Vec<String>],
    fallback_category: Option<&str>,
) -> Vec<PlannedPhrase> {
    records
        .iter()
        .skip(1)
        .filter_map(|row| {
            let body = row.get(1)?.trim();
            if body.is_empty() {
                return None;
            }
            let code = row.first().map(|s| s.trim()).unwrap_or_default();
            let group = row.get(2).map(|s| s.trim()).unwrap_or_default();
            let title_source = if code.is_empty() { body } else { code };
            Some(PlannedPhrase {
                title: sanitize_title(title_source),
                body: body.to_string(),
                category: if group.is_empty() {
                    fallback_category.map(str::to_string)
                } else {
                    Some(group.to_string())
                },
                tags: if code.is_empty() {
                    Vec::new()
                } else {
                    vec![code.to_string()]
                },
            })
        })
        .collect()
}

/// 卡片标题 / 临时文件名安全化：Windows 非法字符与控制字符折成空格、空白
/// 收敛、限长（字符 + 字节双帽）、掐尾点空格、设备保留名加前缀。
fn sanitize_title(raw: &str) -> String {
    let collapsed: String = raw
        .chars()
        .map(|c| if is_illegal_filename_char(c) { ' ' } else { c })
        .collect();
    let collapsed = collapsed.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for c in collapsed.chars().take(TITLE_MAX_CHARS) {
        if out.len() + c.len_utf8() > TITLE_MAX_BYTES {
            break;
        }
        out.push(c);
    }
    let trimmed = out.trim().trim_end_matches(['.', ' ']).trim();
    if trimmed.is_empty() {
        return TITLE_FALLBACK.to_string();
    }
    if is_reserved_device_name(trimmed) {
        return format!("_{trimmed}");
    }
    trimmed.to_string()
}

fn is_illegal_filename_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        || (c as u32) < 0x20
        || c == '\u{7f}'
}

/// Windows 设备保留名对任意扩展名都成立（CON.txt 同样非法）， stem 判定即可。
fn is_reserved_device_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper.starts_with("COM") && upper[3..].parse::<u8>().is_ok_and(|n| (1..=9).contains(&n))
        || upper.starts_with("LPT") && upper[3..].parse::<u8>().is_ok_and(|n| (1..=9).contains(&n))
}

/// 拆解产物：assets 的 source 全在 temp_root 下，调用方在 run_import 完成
/// 后删除 temp_root（先导入后清理，时序不可反转）。
pub struct ExplodedPhraseCsv {
    pub temp_root: PathBuf,
    pub assets: Vec<ImportedAsset>,
    /// 无正文被整行丢弃的行数（进 NOTICE，避免「行数对不上」无人解释）。
    pub skipped_rows: usize,
}

/// 单个快捷短语 CSV → 拆解产物。文件超帽 / 表头不符 / 一条短语都没有都算
/// 失败（Err），由调用方按「不支持的文件」记账跳过，不拖垮整批。
pub fn explode_phrase_csv(
    path: &Path,
    fallback_category: Option<&str>,
) -> Result<ExplodedPhraseCsv, String> {
    const MAX_CSV_BYTES: u64 = 64 * 1024 * 1024;
    let len = std::fs::metadata(path)
        .map_err(|e| format!("读元数据失败: {e}"))?
        .len();
    if len > MAX_CSV_BYTES {
        return Err(format!(
            "文件 {}MB 超过快捷短语 CSV 上限 64MB",
            len / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let records = parse_phrase_csv_bytes(&bytes);
    if !looks_like_phrase_csv(&records) {
        return Err("首行不是千牛快捷短语表头（快捷短语编码/快捷短语/…）".into());
    }
    let plans = plan_phrase_assets(&records, fallback_category);
    if plans.is_empty() {
        return Err("没有任何带正文的短语行".into());
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_root =
        std::env::temp_dir().join(format!("qianniu_phrases_{}_{}", std::process::id(), nanos));
    let skipped_rows = records.len().saturating_sub(1) - plans.len();
    let sources = write_phrase_files(&plans, &temp_root).map_err(|e| e.to_string())?;
    let assets = plans
        .into_iter()
        .zip(sources)
        .map(|(plan, source)| ImportedAsset {
            source,
            category: plan.category,
            tags: plan.tags,
        })
        .collect();
    Ok(ExplodedPhraseCsv {
        temp_root,
        assets,
        skipped_rows,
    })
}

/// 计划 → 临时 txt 文件（UTF-8 无 BOM：library 归一化零拷贝直通）。每条一个
/// 子目录：标题可能重名，目录隔离避免互相覆盖。
fn write_phrase_files(plans: &[PlannedPhrase], root: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(root)?;
    let mut sources = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let dir = root.join(format!("row_{index:06}"));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.txt", plan.title));
        std::fs::write(&path, plan.body.as_bytes())?;
        sources.push(path);
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(fields: &[&str]) -> Vec<String> {
        fields.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn csv_parser_handles_quotes_embedded_newlines_and_trailing_rows() {
        let text = "a,\"b,c\",\"d\"\"e\"\r\nf,\"g\nh\",\r\n";
        assert_eq!(
            parse_csv_records(text),
            vec![row(&["a", "b,c", "d\"e"]), row(&["f", "g\nh", ""])]
        );
        // 尾行无换行不丢；尾逗号产生空字段；空输入零记录。
        assert_eq!(parse_csv_records("x,y"), vec![row(&["x", "y"])]);
        assert_eq!(parse_csv_records("x,"), vec![row(&["x", ""])]);
        assert!(parse_csv_records("").is_empty());
    }

    #[test]
    fn header_sniff_requires_exact_qianniu_columns() {
        let good =
            parse_csv_records("快捷短语编码,快捷短语,快捷短语分组,快捷短语图片\n,你好,床垫,");
        assert!(looks_like_phrase_csv(&good));
        let bad = parse_csv_records("编码,短语\n,你好,");
        assert!(!looks_like_phrase_csv(&bad));
        assert!(!looks_like_phrase_csv(&[]));
    }

    #[test]
    fn normalization_feeds_bom_and_gbk_bytes() {
        // UTF-8 BOM：剥壳后表头照常命中。
        let mut bom = vec![0xEFu8, 0xBB, 0xBF];
        bom.extend_from_slice("快捷短语编码,快捷短语\n,正文,分组,".as_bytes());
        let records = parse_phrase_csv_bytes(&bom);
        assert!(looks_like_phrase_csv(&records));

        // GBK 兜底（你好 = C4E3 BAC3）：解码进解析，不产生乱码字段。
        let gbk = [0xC4u8, 0xE3, 0xBA, 0xC3];
        assert_eq!(parse_phrase_csv_bytes(&gbk), vec![row(&["你好"])]);
    }

    #[test]
    fn plan_maps_code_tag_group_category_and_skips_empty_rows() {
        let records = parse_csv_records(
            "快捷短语编码,快捷短语,快捷短语分组,快捷短语图片\n\
             榫卯,榫卯结构 床头床尾,材质,\n\
             ,无编码短语,,\n\
             ,   ,床垫,\n\
             ,\"多行短语第一行\n第二行\",物流,\n\
             ,尾部行,待分类,",
        );
        let plans = plan_phrase_assets(&records, Some("弹窗决策"));
        assert_eq!(plans.len(), 4, "正文全空白的行整行丢弃");
        assert_eq!(plans[0].title, "榫卯");
        assert_eq!(plans[0].tags, vec!["榫卯".to_string()]);
        assert_eq!(plans[0].category.as_deref(), Some("材质"));
        assert_eq!(plans[1].title, "无编码短语", "无编码 → 正文摘录做标题");
        assert!(plans[1].tags.is_empty());
        assert_eq!(
            plans[1].category.as_deref(),
            Some("弹窗决策"),
            "空分组继承决策"
        );
        assert_eq!(plans[2].body, "多行短语第一行\n第二行", "正文内部换行保留");
        assert_eq!(plans[2].category.as_deref(), Some("物流"));
        // 「待分类」是保留名：计划层原样带过，library enqueue 过滤成收件箱。
        assert_eq!(plans[3].category.as_deref(), Some("待分类"));
    }

    #[test]
    fn title_sanitizes_illegal_chars_caps_length_and_handles_reserved_names() {
        assert_eq!(sanitize_title("a<b>:c\"d"), "a b c d");
        assert_eq!(sanitize_title("行1\n行2  行3"), "行1 行2 行3");
        assert_eq!(sanitize_title("结束."), "结束");
        let long = sanitize_title(&"长".repeat(50));
        assert_eq!(long.chars().count(), TITLE_MAX_CHARS);
        assert_eq!(sanitize_title("CON"), "_CON", "设备保留名任意扩展名都非法");
        assert_eq!(sanitize_title("   "), TITLE_FALLBACK);
        assert_eq!(sanitize_title(""), TITLE_FALLBACK);
    }

    #[test]
    fn explode_writes_temp_files_and_maps_assets() {
        let root = temp_root("explode");
        let csv = root.join("phrases.csv");
        let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
        bytes.extend_from_slice(
            "快捷短语编码,快捷短语,快捷短语分组,快捷短语图片\n榫卯,榫卯结构,材质,\n,第二短语,,\n,,床垫,\n"
                .as_bytes(),
        );
        std::fs::write(&csv, bytes).unwrap();

        let exploded = explode_phrase_csv(&csv, None).unwrap();
        assert_eq!(exploded.assets.len(), 2);
        assert_eq!(exploded.skipped_rows, 1, "无正文行计数进 NOTICE");
        assert_eq!(exploded.assets[0].tags, vec!["榫卯".to_string()]);
        assert_eq!(exploded.assets[0].category.as_deref(), Some("材质"));
        assert_eq!(exploded.assets[1].category, None, "空分组无决策 = 收件箱");
        assert!(exploded.assets[0].source.is_file());
        assert_eq!(
            std::fs::read_to_string(&exploded.assets[0].source).unwrap(),
            "榫卯结构"
        );

        // 表头不符 → Err（调用方按不支持文件跳过）。
        let bad = root.join("bad.csv");
        std::fs::write(&bad, "a,b\n1,2\n").unwrap();
        assert!(explode_phrase_csv(&bad, None).is_err());

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&exploded.temp_root).unwrap();
    }

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("qianniu_csv_{tag}_{}_{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
