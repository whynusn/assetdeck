//! UI 卫生度棘轮（ux-audit 根因护栏）：只允许下降，不允许上升。
//!
//! 统计口径（与 automation/ux-audit/design-contract.md §1 对齐）：
//! - hex_outside_theme：theme.slint 之外的颜色字面量（含注释里的色值）。
//! - font_size_literals / font_size_off_scale：`font-size: <n>px` 字面量与
//!   不在设计字阶（10/11/12/14/16）内的个数。
//! - border_radius_literals / border_radius_off_scale：`border-radius: <n>px`
//!   字面量与不在圆角阶（4/7/8/999=胶囊）内的个数。
//!
//! 基线与允许值在 automation/ux-audit/hygiene-baseline.json。修完一处后
//! 必须同步下调基线；基线数字升高 = 回归，直接失败。
//! 打印当前计数：UX_AUDIT_PRINT_COUNTS=1 cargo test -p app-ui --test ui_hygiene -- --nocapture

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Baseline {
    counts: Counts,
    allowed_font_sizes: Vec<f64>,
    allowed_radii: Vec<f64>,
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Counts {
    hex_outside_theme: usize,
    font_size_literals: usize,
    font_size_off_scale: usize,
    border_radius_literals: usize,
    border_radius_off_scale: usize,
}

fn load_baseline() -> Baseline {
    let path = repo_root().join("automation/ux-audit/hygiene-baseline.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
    let json: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("解析 {} 失败: {e}", path.display()));
    let c = &json["counts"];
    let read = |key: &str| -> usize {
        c[key]
            .as_u64()
            .unwrap_or_else(|| panic!("基线缺少数值字段 counts.{key}")) as usize
    };
    let read_list = |key: &str| -> Vec<f64> {
        json[key]
            .as_array()
            .unwrap_or_else(|| panic!("基线缺少数组字段 {key}"))
            .iter()
            .map(|v| v.as_f64().expect("允许值必须是数字"))
            .collect()
    };
    Baseline {
        counts: Counts {
            hex_outside_theme: read("hex_outside_theme"),
            font_size_literals: read("font_size_literals"),
            font_size_off_scale: read("font_size_off_scale"),
            border_radius_literals: read("border_radius_literals"),
            border_radius_off_scale: read("border_radius_off_scale"),
        },
        allowed_font_sizes: read_list("allowed_font_sizes_px"),
        allowed_radii: read_list("allowed_radii_px"),
    }
}

fn ui_sources() -> Vec<(String, String)> {
    let dir = repo_root().join("crates/app-ui/ui");
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", dir.display()))
        .map(|e| e.expect("读取 ui 目录项失败").path())
        .filter(|p| p.extension().is_some_and(|x| x == "slint"))
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
        out.push((name, text));
    }
    assert!(!out.is_empty(), "crates/app-ui/ui 下没有找到 .slint 源文件");
    out
}

/// 去掉行注释，避免注释里的色值/示例计入字面量。
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn count_hex_literals(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            let len = j - start;
            let boundary = j >= bytes.len()
                || !(bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-');
            if boundary && matches!(len, 3 | 4 | 6 | 8) {
                count += 1;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    count
}

/// 统计 `property: <number>px;` 形式的内联字面量（返回总数与不在允许阶内的个数）。
fn count_px_literals(src: &str, property: &str, allowed: &[f64]) -> (usize, usize) {
    let mut total = 0;
    let mut off_scale = 0;
    let needle = format!("{property}:");
    let mut rest = src;
    while let Some(pos) = rest.find(&needle) {
        let after = &rest[pos + needle.len()..];
        let value = after.split(';').next().unwrap_or("").trim();
        if let Some(num) = value.strip_suffix("px") {
            if let Ok(n) = num.trim().parse::<f64>() {
                total += 1;
                if !allowed.iter().any(|a| (a - n).abs() < f64::EPSILON) {
                    off_scale += 1;
                }
            }
        }
        rest = &after[after.chars().next().map(|c| c.len_utf8()).unwrap_or(0)..];
    }
    (total, off_scale)
}

fn measure(baseline: &Baseline) -> Counts {
    let mut counts = Counts::default();
    for (name, source) in ui_sources() {
        let code = strip_line_comments(&source);
        if name != "theme.slint" {
            counts.hex_outside_theme += count_hex_literals(&code);
        }
        let (font_total, font_off) =
            count_px_literals(&code, "font-size", &baseline.allowed_font_sizes);
        counts.font_size_literals += font_total;
        counts.font_size_off_scale += font_off;
        let (radius_total, radius_off) =
            count_px_literals(&code, "border-radius", &baseline.allowed_radii);
        counts.border_radius_literals += radius_total;
        counts.border_radius_off_scale += radius_off;
    }
    counts
}

#[test]
fn ui_hygiene_counts_never_exceed_baseline() {
    let baseline = load_baseline();
    let current = measure(&baseline);

    if std::env::var_os("UX_AUDIT_PRINT_COUNTS").is_some() {
        println!("UX_AUDIT_COUNTS={current:?}");
    }

    let mut regressions = Vec::new();
    let checks = [
        ("hex_outside_theme", current.hex_outside_theme, baseline.counts.hex_outside_theme),
        ("font_size_literals", current.font_size_literals, baseline.counts.font_size_literals),
        ("font_size_off_scale", current.font_size_off_scale, baseline.counts.font_size_off_scale),
        ("border_radius_literals", current.border_radius_literals, baseline.counts.border_radius_literals),
        ("border_radius_off_scale", current.border_radius_off_scale, baseline.counts.border_radius_off_scale),
    ];
    for (name, now, cap) in checks {
        if now > cap {
            regressions.push(format!("{name}: {now} > 基线 {cap}"));
        }
    }
    assert!(
        regressions.is_empty(),
        "UI 卫生度回归（棘轮只降不升）：{}\n当前 {current:?}\n\
         修复内联字面量（走 Theme/Typography 令牌）后，才允许在 \
         automation/ux-audit/hygiene-baseline.json 下调基线。",
        regressions.join("；")
    );
}

#[test]
fn baseline_scale_cannot_be_silently_widened() {
    // 允许阶是设计契约的一部分：想加字号/圆角必须先改 design-contract.md 并
    // 走 contract: 提交，不能靠改 baseline 让新字面量蒙混过关。
    let baseline = load_baseline();
    assert_eq!(
        baseline.allowed_font_sizes,
        vec![10.0, 11.0, 12.0, 14.0, 16.0],
        "字号阶被改动：请同步 design-contract.md §1.2，并用 contract: 提交说明理由"
    );
    assert_eq!(
        baseline.allowed_radii,
        vec![4.0, 7.0, 8.0, 999.0],
        "圆角阶被改动：请同步 design-contract.md §1.3，并用 contract: 提交说明理由"
    );
    assert!(
        baseline.counts.font_size_off_scale <= baseline.counts.font_size_literals
            && baseline.counts.border_radius_off_scale <= baseline.counts.border_radius_literals,
        "基线自相矛盾：off_scale 不可能大于字面量总数"
    );
}
