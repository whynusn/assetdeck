//! Validated, bounded theme registry. Invalid packs never enter the UI selector.
use crate::theme::*;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

pub const MAX_PACKS: usize = 32;
pub const MAX_IMAGE_BYTES: u64 = 512 * 1024;
pub const MAX_IMAGE_EDGE: u32 = 1024;

#[derive(Debug, Clone)]
pub struct LoadedTheme {
    pub pack: ThemePack,
    pub directory: PathBuf,
}

#[derive(Debug)]
pub struct ThemeCatalog {
    pub themes: Vec<LoadedTheme>,
    pub errors: Vec<String>,
}

fn fail(key: &str, message: &str) -> Result<(), String> {
    Err(format!("{key}: {message}"))
}

fn range(key: &str, value: Option<f32>, min: f32, max: f32) -> Result<(), String> {
    if value.is_some_and(|v| !v.is_finite() || v < min || v > max) {
        return fail(key, &format!("必须在 {min}–{max} 之间"));
    }
    Ok(())
}

fn luminance(c: u32) -> f64 {
    let channel = |s: u32| {
        let v = ((c >> s) & 255) as f64 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}
fn composite(c: u32, bg: u32) -> u32 {
    let a = (c >> 24) as f64 / 255.0;
    let ch = |s: u32| {
        ((((c >> s) & 255) as f64 * a + ((bg >> s) & 255) as f64 * (1.0 - a)).round() as u32) << s
    };
    0xff00_0000 | ch(16) | ch(8) | ch(0)
}
fn contrast(key: &str, ink: u32, bg: u32, minimum: f64) -> Result<(), String> {
    let a = luminance(composite(ink, bg));
    let b = luminance(bg);
    let value = (a.max(b) + 0.05) / (a.min(b) + 0.05);
    if value + 0.001 < minimum {
        return fail(key, &format!("对比度 {value:.2}:1，至少需要 {minimum}:1"));
    }
    Ok(())
}

impl ThemePack {
    /// Pure validation; no partially supported option is silently accepted.
    pub fn validate(&self) -> Result<(), String> {
        let id = &self.meta.id;
        if !(2..=32).contains(&id.len())
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || id.starts_with('-')
        {
            return fail("meta.id", "仅支持 2–32 位小写字母、数字、连字符");
        }
        if self.meta.name.trim().is_empty() || self.meta.name.chars().count() > 40 {
            return fail("meta.name", "名称应为 1–40 字");
        }
        if self.meta.version.split('.').count() != 3
            || !self
                .meta
                .version
                .split('.')
                .all(|p| p.parse::<u32>().is_ok())
        {
            return fail("meta.version", "需要 x.y.z 版本号");
        }
        let g = &self.geometry;
        for (key, v) in [
            ("radius", g.radius),
            ("radius-lg", g.radius_lg),
            ("tile-radius", g.tile_radius),
        ] {
            range(&format!("geometry.{key}"), v.map(|v| v.0), 0.0, 24.0)?;
        }
        for (key, v, min, max) in [
            ("control-height", g.control_height, 28., 44.),
            ("row-height", g.row_height, 28., 44.),
            ("bar-height", g.bar_height, 48., 64.),
            ("tile-gap", g.tile_gap, 4., 24.),
            ("border-width", g.border_width, 0., 2.),
        ] {
            range(&format!("geometry.{key}"), v.map(|v| v.0), min, max)?;
        }
        range(
            "typography.font-size-scale",
            self.typography.font_size_scale.map(|v| v.0),
            0.9,
            1.2,
        )?;
        range(
            "typography.display",
            self.typography.display.map(|v| v.0),
            14.,
            24.,
        )?;
        range(
            "typography.letter-spacing",
            self.typography.letter_spacing.map(|v| v.0),
            0.,
            1.,
        )?;
        // Slint's Text has no configurable line-height. Reject rather than pretend.
        if self.typography.line_height.is_some() {
            return fail(
                "typography.line-height",
                "当前渲染器不支持；请使用 geometry.row-height",
            );
        }
        range(
            "icons.stroke-weight",
            self.icons.stroke_weight.map(|v| v.0),
            1.,
            2.5,
        )?;
        range("icons.size", self.icons.size.map(|v| v.0), 14., 20.)?;
        if self.icons.set.is_some_and(|v| v != IconSet::Line) {
            return fail("icons.set", "当前支持 line 图标集");
        }
        for (key, v) in [
            ("fast", self.motion.fast),
            ("state", self.motion.state),
            ("enter", self.motion.enter),
        ] {
            range(&format!("motion.{key}"), v.map(|v| v.0 as f32), 0., 200.)?;
        }
        if self.sound.enabled || self.sound.click.is_some() || self.sound.success.is_some() {
            return fail("sound", "当前版本不支持声音主题");
        }
        let d = &self.decoration;
        if self.palette.bg_app_alt.is_some() {
            return fail("palette.bg-app-alt", "当前界面没有交替背景，暂不支持此选项");
        }
        range(
            "decoration.watermark-opacity",
            d.watermark_opacity.map(|v| v.0),
            0.,
            0.3,
        )?;
        range(
            "decoration.corner-scale",
            d.corner_scale.map(|v| v.0),
            0.5,
            1.5,
        )?;
        if self.asset_paths().len() > 2 {
            return fail("decoration", "最多两张装饰图（含贴纸）");
        }
        for sticker in &d.stickers {
            range("decoration.stickers.x", Some(sticker.x.0), 0., 1.)?;
            range("decoration.stickers.y", Some(sticker.y.0), 0., 1.)?;
            range(
                "decoration.stickers.scale",
                sticker.scale.map(|v| v.0),
                0.5,
                1.5,
            )?;
            range(
                "decoration.stickers.opacity",
                sticker.opacity.map(|v| v.0),
                0.,
                1.,
            )?;
        }
        let t = self.tokens();
        let light_surface = luminance(t.bg_app) >= 0.5;
        if light_surface != (self.meta.base == ThemeBase::Light) {
            return fail("meta.base", "必须与 palette.bg-app 的明暗一致");
        }
        for (name, bg) in [
            ("bg-app", t.bg_app),
            ("bg-panel", t.bg_panel),
            ("bg-bar", t.bg_bar),
            ("bg-raised", t.bg_raised),
            ("bg-input", t.bg_input),
            ("bg-overlay", t.bg_overlay),
        ] {
            if bg >> 24 != 255 {
                return fail(&format!("palette.{name}"), "基础表面必须不透明");
            }
            for (ink, c) in [("text", t.text), ("text-2", t.text_2), ("text-3", t.text_3)] {
                contrast(&format!("palette.{ink}/{name}"), c, bg, 4.5)?;
            }
        }
        // Pressed controls promote secondary details to text-2; text-3 is only
        // used on the static/disabled surfaces checked above.
        for (name, bg) in [
            ("bg-raised-hover", t.bg_raised_hover),
            ("bg-raised-press", t.bg_raised_press),
        ] {
            if bg >> 24 != 255 {
                return fail(&format!("palette.{name}"), "控件表面必须不透明");
            }
            for (ink, c) in [("text", t.text), ("text-2", t.text_2)] {
                contrast(&format!("palette.{ink}/{name}"), c, bg, 4.5)?;
            }
        }
        for (key, c, bg) in [
            ("accent", t.accent, t.accent_soft),
            ("danger", t.danger, t.danger_soft),
            ("warn", t.warn, t.warn_soft),
            ("ok", t.ok, t.ok_soft),
            ("shared", t.shared, t.shared_soft),
        ] {
            contrast(&format!("palette.{key}/soft"), c, bg, 4.5)?;
            contrast(&format!("palette.{key}/bg-overlay"), c, t.bg_overlay, 4.5)?;
        }
        contrast(
            "palette.control-track-border",
            t.control_track_border,
            t.bg_overlay,
            3.,
        )?;
        contrast("palette.danger-ink", t.danger_ink, t.danger_press, 4.5)?;
        contrast("palette.accent-ink", t.accent_ink, t.accent, 4.5)?;
        contrast(
            "palette.accent-ink/hover",
            t.accent_ink,
            t.accent_hover,
            4.5,
        )?;
        contrast(
            "palette.accent-ink/press",
            t.accent_ink,
            t.accent_press,
            4.5,
        )?;
        if let Some(selection) = self.palette.selection_bg {
            contrast(
                "palette.selection-bg",
                t.text_2,
                composite(selection.0, t.bg_raised),
                4.5,
            )?;
        }
        for bg in [0xff00_0000, 0xffff_ffff] {
            contrast(
                "palette.badge-text",
                t.badge_text,
                composite(t.badge_bg, bg),
                4.5,
            )?;
            contrast(
                "palette.label-bar-text",
                t.label_bar_text,
                composite(t.label_bar_bg, bg),
                4.5,
            )?;
        }
        Ok(())
    }
    pub fn asset_paths(&self) -> Vec<&str> {
        let d = &self.decoration;
        d.background
            .iter()
            .chain(d.watermark.iter())
            .chain(d.corner.iter())
            .map(String::as_str)
            .chain(d.stickers.iter().map(|s| s.image.as_str()))
            .collect()
    }
    pub fn density_scale(&self) -> f32 {
        match self.geometry.density {
            Some(Density::Compact) => 0.95,
            Some(Density::Spacious) => 1.15,
            _ => 1.0,
        }
    }
    pub fn row_height(&self) -> f32 {
        self.geometry
            .row_height
            .map_or(28. * self.density_scale().max(1.), |p| p.0)
            .max(28. * self.typography.font_size_scale.map_or(1., |v| v.0))
    }
}

/// Paths must stay inside their pack, including symlinks/junctions on Windows.
pub fn checked_asset(directory: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.contains(':')
        || relative.contains('\\')
        || Path::new(relative)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("decoration.{relative}: 路径必须在主题包内"));
    }
    let base = directory.canonicalize().map_err(|e| e.to_string())?;
    let path = directory
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("decoration.{relative}: {e}"))?;
    if !path.starts_with(base) {
        return Err(format!("decoration.{relative}: 路径越界"));
    }
    if path.metadata().map_err(|e| e.to_string())?.len() > MAX_IMAGE_BYTES {
        return Err(format!("decoration.{relative}: 超过 512KB"));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if bytes.len() < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(format!("decoration.{relative}: v1 装饰只接受完整 PNG"));
    }
    if &bytes[8..12] != b"\0\0\0\r" || &bytes[12..16] != b"IHDR" {
        return Err(format!("decoration.{relative}: 缺少合法 PNG IHDR"));
    }
    for off in [16, 20] {
        let edge = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap());
        if edge == 0 || edge > MAX_IMAGE_EDGE {
            return Err(format!("decoration.{relative}: 尺寸必须为 1–1024px"));
        }
    }
    let mut offset = 8usize;
    let mut has_idat = false;
    let mut has_iend = false;
    while offset + 12 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let Some(end) = offset.checked_add(size).and_then(|v| v.checked_add(12)) else {
            return Err(format!("decoration.{relative}: PNG 区块长度无效"));
        };
        if end > bytes.len() {
            return Err(format!("decoration.{relative}: PNG 区块不完整"));
        }
        let kind = &bytes[offset + 4..offset + 8];
        if kind == b"IDAT" {
            has_idat = true;
        }
        if kind == b"IEND" {
            if size != 0 || end != bytes.len() {
                return Err(format!("decoration.{relative}: PNG 结束区块无效"));
            }
            has_iend = true;
            break;
        }
        offset = end;
    }
    if !has_idat || !has_iend {
        return Err(format!("decoration.{relative}: PNG 缺少图像数据或结束区块"));
    }
    Ok(path)
}

impl ThemeCatalog {
    pub fn load(directory: &Path) -> Self {
        let mut result = Self {
            themes: Vec::new(),
            errors: Vec::new(),
        };
        for source in [
            include_str!("../../../themes/builtin-dark/theme.toml"),
            include_str!("../../../themes/builtin-light/theme.toml"),
        ] {
            result.themes.push(LoadedTheme {
                pack: ThemePack::parse(source).expect("built-in theme"),
                directory: PathBuf::new(),
            });
        }
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return result,
            Err(e) => {
                result.errors.push(format!(
                    "themes: 无法读取主题目录 {}: {e}",
                    directory.display()
                ));
                return result;
            }
        };
        // 先排除无关条目，再保留名字最小的 32 项；不依赖文件系统枚举顺序，
        // 同时避免目录内有大量杂项时将全部路径载入内存。
        let mut selected: Vec<(OsString, std::fs::DirEntry)> = Vec::with_capacity(MAX_PACKS);
        let mut candidates = 0usize;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    result.errors.push(format!("themes: 遍历主题目录失败: {e}"));
                    continue;
                }
            };
            let name = entry.file_name();
            let label = name.to_string_lossy();
            if label.starts_with('_') || label.starts_with("builtin-") {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {}
                Ok(_) => continue,
                Err(e) => {
                    result
                        .errors
                        .push(format!("{}: 无法读取目录类型: {e}", label));
                    continue;
                }
            }
            candidates += 1;
            let at = selected.partition_point(|(known, _)| known < &name);
            if at < MAX_PACKS {
                selected.insert(at, (name, entry));
                selected.truncate(MAX_PACKS);
            }
        }
        if candidates > MAX_PACKS {
            result.errors.push(format!(
                "themes: 找到 {candidates} 个主题目录，只加载名称排序前 {MAX_PACKS} 个"
            ));
        }
        for (_, entry) in selected {
            let path = entry.path();
            let loaded = (|| -> Result<LoadedTheme, String> {
                let file = path.join("theme.toml");
                if file.metadata().map_err(|e| e.to_string())?.len() > 64 * 1024 {
                    return Err("theme.toml 超过 64KB".into());
                }
                let pack =
                    ThemePack::parse(&std::fs::read_to_string(file).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                pack.validate()?;
                if pack.meta.id != entry.file_name().to_string_lossy() {
                    return Err("meta.id 必须与目录名一致".into());
                }
                if result.themes.iter().any(|t| t.pack.meta.id == pack.meta.id) {
                    return Err("meta.id 与内置主题重复".into());
                }
                for asset in pack.asset_paths() {
                    checked_asset(&path, asset)?;
                }
                Ok(LoadedTheme {
                    pack,
                    directory: path,
                })
            })();
            match loaded {
                Ok(t) => result.themes.push(t),
                Err(e) => result
                    .errors
                    .push(format!("{}: {e}", entry.file_name().to_string_lossy())),
            }
        }
        result
    }
    pub fn index(&self, id: &str, legacy_light: bool) -> usize {
        if id.is_empty() {
            return usize::from(legacy_light);
        }
        self.themes
            .iter()
            .position(|t| t.pack.meta.id == id)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    struct TempThemes(PathBuf);

    impl TempThemes {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "assetdeck-theme-catalog-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn pack(&self, id: &str, source: &str) {
            let directory = self.0.join(id);
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("theme.toml"), source).unwrap();
        }
    }
    impl Drop for TempThemes {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn shipped_themes_validate_and_load() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes");
        let c = ThemeCatalog::load(&root);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert!(c.themes.iter().any(|t| t.pack.meta.id == "peach-cat"));
        for t in c.themes {
            t.pack.validate().unwrap();
        }
    }
    #[test]
    fn invalid_options_and_transparent_surfaces_rejected() {
        let mut p = ThemePack::parse(include_str!("../../../themes/peach-cat/theme.toml")).unwrap();
        p.motion.fast = Some(Millis(201));
        assert!(p.validate().unwrap_err().contains("motion.fast"));
        p.motion.fast = None;
        p.palette.bg_app = Rgba(0);
        assert!(p.validate().unwrap_err().contains("bg-app"));
        p.palette.bg_app = Rgba(0xffff_ffff);
        p.palette.bg_app_alt = Some(Rgba(0xffff_ffff));
        assert!(p.validate().unwrap_err().contains("bg-app-alt"));
    }
    #[test]
    fn rejects_escaping_assets() {
        for p in [
            "../secret.png",
            "C:/secret.png",
            "/secret.png",
            "assets\\secret.png",
        ] {
            assert!(checked_asset(Path::new("."), p).is_err());
        }
    }
    #[test]
    fn legacy_setting_and_missing_pack_fallback() {
        let c = ThemeCatalog::load(Path::new("absent-theme-directory"));
        assert_eq!(c.index("", true), 1);
        assert_eq!(c.index("removed-pack", true), 0);
        assert_eq!(c.themes[0].pack.tokens(), DarkThemeProvider.theme());
        assert_eq!(c.themes[1].pack.tokens(), LightThemeProvider.theme());
    }

    #[test]
    fn copied_template_has_only_supported_options() {
        let template = include_str!("../../../themes/_template/theme.toml")
            .replace("id = \"_template\"", "id = \"my-theme\"");
        ThemePack::parse(&template).unwrap().validate().unwrap();
        let root = TempThemes::new();
        root.pack("my-theme", &template);
        let catalog = ThemeCatalog::load(&root.0);
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        assert_eq!(catalog.themes[2].pack.meta.id, "my-theme");
    }

    #[test]
    fn scans_only_theme_directories_and_selects_first_32_by_name() {
        let root = TempThemes::new();
        let source = include_str!("../../../themes/builtin-light/theme.toml");
        for i in (0..33).rev() {
            let id = format!("pack-{i:02}");
            root.pack(
                &id,
                &source.replace("id = \"light\"", &format!("id = \"{id}\"")),
            );
        }
        for i in 0..40 {
            std::fs::create_dir(root.0.join(format!("_ignored-{i:02}"))).unwrap();
        }
        std::fs::create_dir(root.0.join("builtin-shadow")).unwrap();
        std::fs::write(root.0.join("plain-file"), "not a theme").unwrap();
        let catalog = ThemeCatalog::load(&root.0);
        assert_eq!(catalog.themes.len(), 2 + MAX_PACKS);
        let ids: Vec<_> = catalog
            .themes
            .iter()
            .skip(2)
            .map(|t| t.pack.meta.id.as_str())
            .collect();
        assert_eq!(ids.first().copied(), Some("pack-00"));
        assert_eq!(ids.last().copied(), Some("pack-31"));
        assert!(
            catalog.errors.iter().any(|e| e.contains("33 个主题目录")),
            "{:?}",
            catalog.errors
        );
    }

    #[test]
    fn unreadable_directory_is_reported_but_absent_directory_is_allowed() {
        let root = TempThemes::new();
        let file = root.0.join("not-a-directory");
        std::fs::write(&file, "content").unwrap();
        let failed = ThemeCatalog::load(&file);
        assert_eq!(failed.themes.len(), 2);
        assert!(
            failed.errors.iter().any(|e| e.contains("无法读取主题目录")),
            "{:?}",
            failed.errors
        );
        let absent = ThemeCatalog::load(&root.0.join("does-not-exist"));
        assert_eq!(absent.themes.len(), 2);
        assert!(absent.errors.is_empty());
    }

    #[test]
    fn png_header_without_complete_chunks_is_rejected() {
        let root = TempThemes::new();
        let image = root.0.join("fake.png");
        let mut bytes = vec![0u8; 33];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[8..12].copy_from_slice(&13u32.to_be_bytes());
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[19] = 1;
        bytes[23] = 1;
        std::fs::write(&image, bytes).unwrap();
        assert!(checked_asset(&root.0, "fake.png")
            .unwrap_err()
            .contains("PNG"));
    }
}
