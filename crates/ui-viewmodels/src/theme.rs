//! 主题 Provider：自绘层的语义色板（ARGB u32），与 `crates/app-ui/ui/theme.slint`
//! 的 Dark 现值同源（颜色完整镜像，几何 radius 令牌留在 theme.slint，不进本层）。
//!
//! 明暗通道分工（D37）：本层色板驱动**自绘层**（背景、瓦片、状态条、设置面板）；
//! std-widgets 的内部配色由壳层写内置 Palette 的 color-scheme 同步翻转
//! （build.rs 钉样式为 fluent 只决定常量表，运行时可翻 scheme）。多主题仅
//! 定义 Provider 形态，壳层按需装配：
//! `AppSettings::light_theme` ⇄ [`LightThemeProvider`]/[`DarkThemeProvider`]。
//!
//! 外置主题包（D55 / theme-spec §3）：`theme.toml` 经 serde 解析为 [`ThemePack`]，
//! 本模块只做**纯数据**（无 I/O、无平台调用）；目录扫描与注入分别在 T0-C4 / 壳层。
//! 未知键、缺键一律报错——半套主题比没有主题更难排查。

use serde::Deserialize;
use std::fmt;

/// 自绘层语义色板：所有字段为 ARGB（0xAARRGGBB）u32。
///
/// `id` 标识主题名（内置 "dark"/"light"，外置包为 `meta.id`），供调试与日志使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeTokens {
    pub id: String,
    /// 窗口底色（最深一级）。
    pub bg_app: u32,
    /// 面板底色。
    pub bg_panel: u32,
    /// 条状区域（工具条/状态条）。
    pub bg_bar: u32,
    /// 抬起面（浮层/按钮基面）。
    pub bg_raised: u32,
    pub bg_raised_hover: u32,
    pub bg_raised_press: u32,
    /// 输入框底色。
    pub bg_input: u32,
    /// 常规描边。
    pub line: u32,
    /// 强描边（悬停/聚焦）。
    pub line_strong: u32,
    /// 控件轨道描边（开关关闭态），对浮层底 ≥3:1。
    pub control_track_border: u32,
    /// 文本三级：主/次/弱。
    pub text: u32,
    pub text_2: u32,
    pub text_3: u32,
    /// 强调色（其上文字用 accent_ink）。
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_press: u32,
    /// 强调色上的文字/图标颜色。
    pub accent_ink: u32,
    /// 强调色弱化底（选中态底）。
    pub accent_soft: u32,
    /// 危险语义色。
    pub danger: u32,
    pub danger_soft: u32,
    pub danger_press: u32,
    /// 危险填充态（danger_press）上的文字/图标。
    pub danger_ink: u32,
    /// 警示语义色。
    pub warn: u32,
    pub warn_soft: u32,
    /// 成功语义色。
    pub ok: u32,
    /// 成功语义弱化底。
    pub ok_soft: u32,
    /// 共享中语义色（与「已送达」的 ok 区分）。
    pub shared: u32,
    pub shared_soft: u32,
    /// 浮层面板底色（比抬起面再亮一档）。
    pub bg_overlay: u32,
    /// 遮罩（模态/悬停预览）。
    pub scrim: u32,
    /// 标签栏底色（状态条内标签 chip，半透明）。
    pub label_bar_bg: u32,
    pub label_bar_bg_hover: u32,
    pub label_bar_text: u32,
    /// 瓦片徽标与占位卡外观。
    pub badge_bg: u32,
    pub badge_text: u32,
    pub badge_ring: u32,
    pub tile_veil: u32,
    pub card_icon: u32,
}

/// 主题提供者：壳层/渲染层经此 trait 读取当前主题色板。
pub trait ThemeProvider {
    fn theme(&self) -> ThemeTokens;
}

/// 深色主题：与 `theme.slint` 现值一致（bg-app #1c1c1c … scrim #00000066），
/// label_bar 系为自绘状态条新增的半透明值。
pub struct DarkThemeProvider;

impl ThemeProvider for DarkThemeProvider {
    fn theme(&self) -> ThemeTokens {
        ThemeTokens {
            id: "dark".into(),
            bg_app: 0xFF1C1C1C,
            bg_panel: 0xFF202020,
            bg_bar: 0xFF252525,
            bg_raised: 0xFF2F2F2F,
            bg_raised_hover: 0xFF383838,
            bg_raised_press: 0xFF292929,
            bg_input: 0xFF1A1A1A,
            line: 0xFF383838,
            line_strong: 0xFF4A4A4A,
            control_track_border: 0xFF8A8A8A,
            text: 0xFFF4F4F4,
            text_2: 0xFFC9C9C9,
            text_3: 0xFFA6A6A6,
            accent: 0xFF60CDFF,
            accent_hover: 0xFF7AD7FF,
            accent_press: 0xFF4BB2E0,
            accent_ink: 0xFF06131A,
            accent_soft: 0xFF1F3A47,
            danger: 0xFFE4747D,
            danger_soft: 0xFF352124,
            danger_press: 0xFF2A1C1F,
            danger_ink: 0xFFFFFFFF,
            warn: 0xFFE3B457,
            warn_soft: 0xFF3A3320,
            ok: 0xFF5AC18E,
            ok_soft: 0xFF1F3A30,
            shared: 0xFFC77AE0,
            shared_soft: 0xFF3A2740,
            bg_overlay: 0xFF2C2C2C,
            scrim: 0x66000000,
            label_bar_bg: 0x99000000,
            label_bar_bg_hover: 0xCC000000,
            label_bar_text: 0xFFFFFFFF,
            badge_bg: 0x99000000,
            badge_text: 0xFFFFFFFF,
            badge_ring: 0xCCFFFFFF,
            tile_veil: 0xA6000000,
            card_icon: 0xFFA6A6A6,
        }
    }
}

/// 浅色主题（自拟合理浅色，与 dark 同构字段齐全：浅底深字）。
///
/// 本色板覆盖自绘层；std-widgets 由壳层经 Palette.color-scheme 同步翻转，
/// 两层观感一体切换（D37）。
pub struct LightThemeProvider;

impl ThemeProvider for LightThemeProvider {
    fn theme(&self) -> ThemeTokens {
        ThemeTokens {
            id: "light".into(),
            bg_app: 0xFFF7F7F8,
            bg_panel: 0xFFFFFFFF,
            bg_bar: 0xFFECECF0,
            bg_raised: 0xFFFFFFFF,
            bg_raised_hover: 0xFFF0F0F3,
            bg_raised_press: 0xFFE4E4E9,
            bg_input: 0xFFFFFFFF,
            line: 0xFFE0E0E6,
            line_strong: 0xFFC8C8D0,
            control_track_border: 0xFF8A8A8A,
            text: 0xFF1F2328,
            text_2: 0xFF444B52,
            text_3: 0xFF62696F,
            accent: 0xFF0F6CBD,
            accent_hover: 0xFF0A6FAA,
            accent_press: 0xFF095D90,
            accent_ink: 0xFFFFFFFF,
            accent_soft: 0xFFE3F0FC,
            danger: 0xFFC0343F,
            danger_soft: 0xFFFBE8EA,
            danger_press: 0xFFA92D37,
            danger_ink: 0xFFFFFFFF,
            warn: 0xFF805A0B,
            warn_soft: 0xFFF8EFD9,
            ok: 0xFF1F7A4D,
            ok_soft: 0xFFE3F2E8,
            shared: 0xFF8730A8,
            shared_soft: 0xFFF4E8F8,
            bg_overlay: 0xFFFAFAFB,
            scrim: 0x33000000,
            label_bar_bg: 0xCCFFFFFF,
            label_bar_bg_hover: 0xFFFFFFFF,
            label_bar_text: 0xFF1F2328,
            badge_bg: 0x99000000,
            badge_text: 0xFFFFFFFF,
            badge_ring: 0xCCFFFFFF,
            tile_veil: 0xD9FFFFFF,
            card_icon: 0xFF62696F,
        }
    }
}

// ---------------------------------------------------------------------------
// 外置主题包 schema（theme-spec §3）
// ---------------------------------------------------------------------------

/// 主题包解析错误。`toml::de::Error` 自带行列与键位片段，禁止吞成「主题无效」。
#[derive(Debug)]
pub struct ThemePackError(String);

impl ThemePackError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// 人类可读的错误信息（含键名/行列）。
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ThemePackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ThemePackError {}

impl From<toml::de::Error> for ThemePackError {
    fn from(error: toml::de::Error) -> Self {
        Self(error.to_string())
    }
}

/// 主题色：ARGB（0xAARRGGBB）。接受 `#rgb` / `#rrggbb` / `#rrggbbaa`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u32);

impl Rgba {
    /// 解析 CSS 风格十六进制色值；非法输入返回带原值的错误。
    pub fn parse(text: &str) -> Result<Self, ThemePackError> {
        let body = text
            .strip_prefix('#')
            .ok_or_else(|| ThemePackError::new(format!("色值 `{text}` 缺少 # 前缀")))?;
        if !body.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(ThemePackError::new(format!(
                "色值 `{text}` 含非十六进制字符"
            )));
        }
        let value = u32::from_str_radix(body, 16)
            .map_err(|_| ThemePackError::new(format!("色值 `{text}` 不是合法十六进制")))?;
        match body.len() {
            3 => {
                let r = ((value >> 8) & 0xF) * 17;
                let g = ((value >> 4) & 0xF) * 17;
                let b = (value & 0xF) * 17;
                Ok(Self(0xFF00_0000 | (r << 16) | (g << 8) | b))
            }
            6 => Ok(Self(0xFF00_0000 | value)),
            8 => Ok(Self(value.rotate_right(8))),
            _ => Err(ThemePackError::new(format!(
                "色值 `{text}` 长度非法：只支持 #rgb/#rrggbb/#rrggbbaa"
            ))),
        }
    }

    /// ARGB 数值（壳层注入 slint 用）。
    pub const fn argb(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// 像素长度（TOML 里写作 `"10px"`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Px(pub f32);

impl Px {
    /// 解析 `"<数值>px"`。
    pub fn parse(text: &str) -> Result<Self, ThemePackError> {
        let number = text
            .strip_suffix("px")
            .ok_or_else(|| ThemePackError::new(format!("长度 `{text}` 必须以 px 结尾")))?;
        let value: f32 = number
            .trim()
            .parse()
            .map_err(|_| ThemePackError::new(format!("长度 `{text}` 不是合法数值")))?;
        if !value.is_finite() || value < 0.0 {
            return Err(ThemePackError::new(format!(
                "长度 `{text}` 必须是非负有限数"
            )));
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for Px {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// 毫秒时长（TOML 里写作 `"120ms"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Millis(pub u16);

impl Millis {
    /// 解析 `"<整数>ms"`。
    pub fn parse(text: &str) -> Result<Self, ThemePackError> {
        let number = text
            .strip_suffix("ms")
            .ok_or_else(|| ThemePackError::new(format!("时长 `{text}` 必须以 ms 结尾")))?;
        let value: u16 = number
            .trim()
            .parse()
            .map_err(|_| ThemePackError::new(format!("时长 `{text}` 必须是非负整数毫秒")))?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for Millis {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// 无量纲比值（缩放/透明度/坐标），TOML 里写作 `"1.0"`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ratio(pub f32);

impl Ratio {
    /// 解析十进制小数。
    pub fn parse(text: &str) -> Result<Self, ThemePackError> {
        let value: f32 = text
            .trim()
            .parse()
            .map_err(|_| ThemePackError::new(format!("比值 `{text}` 不是合法数值")))?;
        if !value.is_finite() {
            return Err(ThemePackError::new(format!("比值 `{text}` 必须是有限数")));
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for Ratio {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// std-widgets 跟随的明暗档（D37：fluent 只有两档，如实声明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeBase {
    Dark,
    Light,
}

/// 密度只缩放尺寸，不改布局结构（theme-spec §4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    Comfortable,
    Spacious,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineCap {
    Round,
    Butt,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconSet {
    Line,
    Filled,
    Pixel,
}

/// 契约只允许 ease-out / linear（theme-spec §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Easing {
    #[serde(rename = "ease-out")]
    EaseOut,
    Linear,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeMeta {
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub version: String,
    pub base: ThemeBase,
    pub description: Option<String>,
    /// 主题包本身的许可证（随附包由校验器强制，素材另见 THIRD_PARTY.md）。
    pub license: Option<String>,
}

/// 色板：38 个核心令牌全部必填（缺一即解析失败，绝不静默继承内置值）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemePalette {
    pub bg_app: Rgba,
    pub bg_panel: Rgba,
    pub bg_bar: Rgba,
    pub bg_raised: Rgba,
    pub bg_raised_hover: Rgba,
    pub bg_raised_press: Rgba,
    pub bg_input: Rgba,
    pub line: Rgba,
    pub line_strong: Rgba,
    pub control_track_border: Rgba,
    pub text: Rgba,
    pub text_2: Rgba,
    pub text_3: Rgba,
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_press: Rgba,
    pub accent_ink: Rgba,
    pub accent_soft: Rgba,
    pub danger: Rgba,
    pub danger_soft: Rgba,
    pub danger_press: Rgba,
    pub danger_ink: Rgba,
    pub warn: Rgba,
    pub warn_soft: Rgba,
    pub ok: Rgba,
    pub ok_soft: Rgba,
    pub shared: Rgba,
    pub shared_soft: Rgba,
    pub bg_overlay: Rgba,
    pub scrim: Rgba,
    pub label_bar_bg: Rgba,
    pub label_bar_bg_hover: Rgba,
    pub label_bar_text: Rgba,
    pub badge_bg: Rgba,
    pub badge_text: Rgba,
    pub badge_ring: Rgba,
    pub tile_veil: Rgba,
    pub card_icon: Rgba,
    /// 扩展：交替行/斑马纹底（缺省回退 bg-panel）。
    pub bg_app_alt: Option<Rgba>,
    /// 扩展：焦点环（缺省 accent）。
    pub focus_ring: Option<Rgba>,
    /// 扩展：选中底（缺省 accent-soft）。
    pub selection_bg: Option<Rgba>,
}

/// 几何：全部可选，缺省由壳层用内置基准补齐。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeGeometry {
    pub radius: Option<Px>,
    pub radius_lg: Option<Px>,
    pub border_width: Option<Px>,
    pub density: Option<Density>,
    pub control_height: Option<Px>,
    pub row_height: Option<Px>,
    pub bar_height: Option<Px>,
    pub tile_radius: Option<Px>,
    pub tile_gap: Option<Px>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeTypography {
    /// 必须是系统已装族（Slint 1.17.1 无运行时字体注册）；留空 = 系统默认。
    pub font_family: Option<String>,
    pub font_size_scale: Option<Ratio>,
    pub display: Option<Px>,
    pub letter_spacing: Option<Px>,
    pub line_height: Option<Ratio>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeIcons {
    pub stroke_weight: Option<Ratio>,
    pub line_cap: Option<LineCap>,
    pub size: Option<Px>,
    pub set: Option<IconSet>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeMotion {
    /// 界面动画总闸（theme-spec §4.5）；缺省开启。
    pub enabled: Option<bool>,
    pub fast: Option<Millis>,
    pub state: Option<Millis>,
    pub enter: Option<Millis>,
    pub easing: Option<Easing>,
}

/// 贴纸：归一化坐标（0.0–1.0，相对窗口）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeSticker {
    pub image: String,
    pub x: Ratio,
    pub y: Ratio,
    pub scale: Option<Ratio>,
    pub opacity: Option<Ratio>,
}

/// 装饰：路径相对主题包目录；存在性与体积由校验器把关（T0-C2）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeDecoration {
    pub background: Option<String>,
    pub background_tint: Option<Rgba>,
    pub watermark: Option<String>,
    pub watermark_opacity: Option<Ratio>,
    pub corner: Option<String>,
    pub corner_scale: Option<Ratio>,
    #[serde(default)]
    pub stickers: Vec<ThemeSticker>,
}

/// 声音：默认全关；播放能力待评估（不得私引音频依赖）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ThemeSound {
    #[serde(default)]
    pub enabled: bool,
    pub click: Option<String>,
    pub success: Option<String>,
}

/// 外置主题包（`themes/<id>/theme.toml`）。所有段均 `deny_unknown_fields`：
/// 拼错键名当场报错，避免「半套主题」。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemePack {
    pub meta: ThemeMeta,
    pub palette: ThemePalette,
    #[serde(default)]
    pub geometry: ThemeGeometry,
    #[serde(default)]
    pub typography: ThemeTypography,
    #[serde(default)]
    pub icons: ThemeIcons,
    #[serde(default)]
    pub motion: ThemeMotion,
    #[serde(default)]
    pub decoration: ThemeDecoration,
    #[serde(default)]
    pub sound: ThemeSound,
}

impl ThemePack {
    /// 主题包入口文件名（目录扫描用）。
    pub const FILE_NAME: &'static str = "theme.toml";

    /// 纯解析：不做 I/O，不校验业务约束（校验器见 T0-C2）。
    pub fn parse(text: &str) -> Result<Self, ThemePackError> {
        Ok(toml::from_str(text)?)
    }
}

impl ThemePack {
    pub fn tokens(&self) -> ThemeTokens {
        ThemeTokens {
            id: self.meta.id.clone(),
            bg_app: self.palette.bg_app.argb(),
            bg_panel: self.palette.bg_panel.argb(),
            bg_bar: self.palette.bg_bar.argb(),
            bg_raised: self.palette.bg_raised.argb(),
            bg_raised_hover: self.palette.bg_raised_hover.argb(),
            bg_raised_press: self.palette.bg_raised_press.argb(),
            bg_input: self.palette.bg_input.argb(),
            line: self.palette.line.argb(),
            line_strong: self.palette.line_strong.argb(),
            control_track_border: self.palette.control_track_border.argb(),
            text: self.palette.text.argb(),
            text_2: self.palette.text_2.argb(),
            text_3: self.palette.text_3.argb(),
            accent: self.palette.accent.argb(),
            accent_hover: self.palette.accent_hover.argb(),
            accent_press: self.palette.accent_press.argb(),
            accent_ink: self.palette.accent_ink.argb(),
            accent_soft: self.palette.accent_soft.argb(),
            danger: self.palette.danger.argb(),
            danger_soft: self.palette.danger_soft.argb(),
            danger_press: self.palette.danger_press.argb(),
            danger_ink: self.palette.danger_ink.argb(),
            warn: self.palette.warn.argb(),
            warn_soft: self.palette.warn_soft.argb(),
            ok: self.palette.ok.argb(),
            ok_soft: self.palette.ok_soft.argb(),
            shared: self.palette.shared.argb(),
            shared_soft: self.palette.shared_soft.argb(),
            bg_overlay: self.palette.bg_overlay.argb(),
            scrim: self.palette.scrim.argb(),
            label_bar_bg: self.palette.label_bar_bg.argb(),
            label_bar_bg_hover: self.palette.label_bar_bg_hover.argb(),
            label_bar_text: self.palette.label_bar_text.argb(),
            badge_bg: self.palette.badge_bg.argb(),
            badge_text: self.palette.badge_text.argb(),
            badge_ring: self.palette.badge_ring.argb(),
            tile_veil: self.palette.tile_veil.argb(),
            card_icon: self.palette.card_icon.argb(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 简单亮度启发式：单通道算数平均（0..=255）。
    fn luminance(argb: u32) -> f64 {
        let r = (argb >> 16) & 0xFF;
        let g = (argb >> 8) & 0xFF;
        let b = argb & 0xFF;
        (r as f64 + g as f64 + b as f64) / 3.0
    }

    #[test]
    fn token_ids_are_correct() {
        assert_eq!(DarkThemeProvider.theme().id, "dark");
        assert_eq!(LightThemeProvider.theme().id, "light");
    }

    #[test]
    fn text_readable_on_app_bg_in_both_themes() {
        for tokens in [DarkThemeProvider.theme(), LightThemeProvider.theme()] {
            let diff = (luminance(tokens.text) - luminance(tokens.bg_app)).abs();
            assert!(
                diff > 100.0,
                "{}: text 与 bg_app 亮度差 {:.0} 应 > 100",
                tokens.id,
                diff
            );
        }
    }

    #[test]
    fn light_theme_is_homomorphic_and_opposed() {
        let light = LightThemeProvider.theme();
        let dark = DarkThemeProvider.theme();
        assert!(
            luminance(light.bg_app) > luminance(dark.bg_app),
            "浅色底应更亮"
        );
        assert!(
            luminance(light.text) < luminance(dark.text),
            "浅色文字应更暗"
        );
    }

    /// WCAG 相对亮度（忽略 alpha，用于不透明色对的对比度门槛）。
    fn relative_luminance(argb: u32) -> f64 {
        let channel = |shift: u32| {
            let c = ((argb >> shift) & 0xFF) as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// 半透明前景压到底色上的合成结果（用于名称条/徽标这类叠图文字）。
    fn composite_over(fg: u32, bg: u32) -> u32 {
        let alpha = ((fg >> 24) & 0xFF) as f64 / 255.0;
        let channel = |shift: u32| {
            let front = ((fg >> shift) & 0xFF) as f64;
            let back = ((bg >> shift) & 0xFF) as f64;
            (front * alpha + back * (1.0 - alpha)).round() as u32
        };
        (0xFF << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
    }

    #[test]
    fn text_3_is_readable_on_every_static_surface() {
        for tokens in [DarkThemeProvider.theme(), LightThemeProvider.theme()] {
            for (surface, color) in [
                ("bg-app", tokens.bg_app),
                ("bg-panel", tokens.bg_panel),
                ("bg-bar", tokens.bg_bar),
                ("bg-raised", tokens.bg_raised),
                ("bg-raised-hover", tokens.bg_raised_hover),
                ("bg-input", tokens.bg_input),
                ("bg-overlay", tokens.bg_overlay),
            ] {
                let ratio = contrast(tokens.text_3, color);
                assert!(
                    ratio >= 4.5,
                    "{}: text-3 对 {surface} 只有 {ratio:.2}:1（契约 §5 要求 4.5:1）",
                    tokens.id
                );
            }
        }
    }

    /// 语义色既要用在描边/状态点（3:1），也会作为文字压在 soft 底或浮层上（4.5:1）。
    #[test]
    fn semantic_text_on_soft_and_overlay_meet_contrast_floor() {
        for tokens in [DarkThemeProvider.theme(), LightThemeProvider.theme()] {
            for (name, color, surface) in [
                ("accent/bg-overlay", tokens.accent, tokens.bg_overlay),
                ("accent/accent-soft", tokens.accent, tokens.accent_soft),
                ("warn/bg-overlay", tokens.warn, tokens.bg_overlay),
                ("warn/warn-soft", tokens.warn, tokens.warn_soft),
                ("danger/bg-overlay", tokens.danger, tokens.bg_overlay),
                ("danger/bg-raised", tokens.danger, tokens.bg_raised),
                ("danger/danger-soft", tokens.danger, tokens.danger_soft),
                ("ok/bg-overlay", tokens.ok, tokens.bg_overlay),
                ("shared/bg-overlay", tokens.shared, tokens.bg_overlay),
            ] {
                let ratio = contrast(color, surface);
                assert!(
                    ratio >= 4.5,
                    "{}: {name} 只有 {ratio:.2}:1（契约 §5 要求 4.5:1）",
                    tokens.id
                );
            }
        }
    }

    /// 名称条/类型徽标是半透明底压缩略图：按最亮（纯白）与最暗（纯黑）图片合成后验证。
    #[test]
    fn tile_overlay_text_readable_on_bright_and_dark_thumbnails() {
        for tokens in [DarkThemeProvider.theme(), LightThemeProvider.theme()] {
            for backdrop in [0xFFFFFFFF, 0xFF000000] {
                for (name, bg, text) in [
                    ("label-bar", tokens.label_bar_bg, tokens.label_bar_text),
                    (
                        "label-bar-hover",
                        tokens.label_bar_bg_hover,
                        tokens.label_bar_text,
                    ),
                    ("badge", tokens.badge_bg, tokens.badge_text),
                ] {
                    let composite = composite_over(bg, backdrop);
                    let ratio = contrast(text, composite);
                    assert!(
                        ratio >= 4.5,
                        "{}: {name} 在合成底 {composite:08x} 上只有 {ratio:.2}:1",
                        tokens.id
                    );
                }
            }
        }
    }

    /// 开关轨道边界与危险按钮各状态：契约 §5 的可读下限。
    #[test]
    fn control_outline_and_danger_fills_meet_contrast_floor() {
        for tokens in [DarkThemeProvider.theme(), LightThemeProvider.theme()] {
            let boundary = contrast(tokens.control_track_border, tokens.bg_overlay);
            assert!(
                boundary >= 3.0,
                "{}: 开关轨道描边对 bg-overlay 只有 {boundary:.2}:1（契约 §5 要求 3:1）",
                tokens.id
            );
            let idle = contrast(tokens.danger, tokens.bg_bar);
            assert!(
                idle >= 4.5,
                "{}: danger 对 bg-bar 只有 {idle:.2}:1",
                tokens.id
            );
            let hover = contrast(tokens.danger, tokens.danger_soft);
            assert!(
                hover >= 4.5,
                "{}: danger 对 danger-soft 只有 {hover:.2}:1",
                tokens.id
            );
            let pressed = contrast(tokens.danger_ink, tokens.danger_press);
            assert!(
                pressed >= 4.5,
                "{}: danger-ink 对 danger-press 只有 {pressed:.2}:1",
                tokens.id
            );
        }
    }

    /// 38 个必填色板键的样本块（值不重要，只保证键齐全）。
    fn palette_block() -> String {
        const KEYS: &str = "bg-app bg-panel bg-bar bg-raised bg-raised-hover bg-raised-press \
             bg-input line line-strong control-track-border text text-2 text-3 accent \
             accent-hover accent-press accent-ink accent-soft danger danger-soft danger-press \
             danger-ink warn warn-soft ok ok-soft shared shared-soft bg-overlay scrim \
             label-bar-bg label-bar-bg-hover label-bar-text badge-bg badge-text badge-ring \
             tile-veil card-icon";
        KEYS.split_whitespace()
            .map(|key| format!("{key} = \"#808080\"\n"))
            .collect()
    }

    fn minimal_pack_toml() -> String {
        format!(
            r#"
[meta]
id = "sample"
name = "样本"
version = "1.0.0"
base = "light"

[palette]
{}
"#,
            palette_block()
        )
    }

    #[test]
    fn hex_colors_accept_rgb_rrggbb_and_rrggbbaa() {
        assert_eq!(Rgba::parse("#fff").unwrap().argb(), 0xFFFF_FFFF);
        assert_eq!(Rgba::parse("#123456").unwrap().argb(), 0xFF12_3456);
        assert_eq!(Rgba::parse("#12345678").unwrap().argb(), 0x7812_3456);
        for bad in ["123456", "#12", "#12345", "#1234567", "#gggggg", "#12 34"] {
            assert!(Rgba::parse(bad).is_err(), "`{bad}` 应被拒绝");
        }
    }

    #[test]
    fn lengths_durations_and_ratios_require_units() {
        assert_eq!(Px::parse("10px").unwrap().0, 10.0);
        assert_eq!(Millis::parse("120ms").unwrap().0, 120);
        assert_eq!(Ratio::parse("1.45").unwrap().0, 1.45);
        for bad in ["10", "px", "10em", "-2px"] {
            assert!(Px::parse(bad).is_err(), "长度 `{bad}` 应被拒绝");
        }
        for bad in ["120", "ms", "1.5ms", "-1ms"] {
            assert!(Millis::parse(bad).is_err(), "时长 `{bad}` 应被拒绝");
        }
        for bad in ["nan", "1,5"] {
            assert!(Ratio::parse(bad).is_err(), "比值 `{bad}` 应被拒绝");
        }
    }

    #[test]
    fn minimal_pack_parses_with_optional_sections_defaulted() {
        let pack = ThemePack::parse(&minimal_pack_toml()).expect("最小包应可解析");
        assert_eq!(pack.meta.id, "sample");
        assert_eq!(pack.meta.base, ThemeBase::Light);
        assert_eq!(pack.palette.text.argb(), 0xFF80_8080);
        assert!(pack.geometry.radius.is_none());
        assert!(pack.decoration.stickers.is_empty());
        assert!(!pack.sound.enabled);
        assert!(pack.motion.enabled.is_none());
    }

    #[test]
    fn unknown_key_and_section_are_rejected_with_name() {
        let text = minimal_pack_toml().replace("bg-panel", "bg-pannel");
        let error = ThemePack::parse(&text).unwrap_err().to_string();
        assert!(error.contains("bg-pannel"), "错误应点名拼错的键：{error}");
        let text = format!("{}[pallete]\n", minimal_pack_toml());
        let error = ThemePack::parse(&text).unwrap_err().to_string();
        assert!(error.contains("pallete"), "错误应点名未知段：{error}");
    }

    #[test]
    fn missing_palette_key_is_rejected_with_key_name() {
        let text: String = minimal_pack_toml()
            .lines()
            .filter(|line| !line.starts_with("text-3"))
            .collect::<Vec<_>>()
            .join("\n");
        let error = ThemePack::parse(&text).unwrap_err().to_string();
        assert!(error.contains("text-3"), "错误应点名缺失的键：{error}");
    }

    #[test]
    fn missing_required_meta_field_is_rejected_with_field_name() {
        let text = minimal_pack_toml().replace("base = \"light\"\n", "");
        let error = ThemePack::parse(&text).unwrap_err().to_string();
        assert!(error.contains("base"), "错误应点名缺失的字段：{error}");
    }

    #[test]
    fn invalid_color_value_is_named_in_error() {
        let text = minimal_pack_toml().replace("card-icon = \"#808080\"", "card-icon = \"808080\"");
        let error = ThemePack::parse(&text).unwrap_err().to_string();
        assert!(error.contains("808080"), "错误应含非法色值：{error}");
    }

    /// 模板漂移护栏：仓库里的 `themes/_template/theme.toml` 必须始终能被解析。
    #[test]
    fn shipped_template_parses() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../themes/_template/theme.toml"
        );
        let text = std::fs::read_to_string(path).expect("模板文件应存在");
        let pack = ThemePack::parse(&text).expect("模板应可解析");
        assert_eq!(pack.meta.id, "_template");
    }
}
