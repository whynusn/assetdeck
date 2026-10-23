//! 主题 Provider：自绘层的语义色板（ARGB u32），与 `crates/app-ui/ui/theme.slint`
//! 的 Dark 现值同源（颜色完整镜像，几何 radius 令牌留在 theme.slint，不进本层）。
//!
//! 明暗通道分工（D37）：本层色板驱动**自绘层**（背景、瓦片、状态条、设置面板）；
//! std-widgets 的内部配色由壳层写内置 Palette 的 color-scheme 同步翻转
//! （build.rs 钉样式为 fluent 只决定常量表，运行时可翻 scheme）。多主题仅
//! 定义 Provider 形态，壳层按需装配：
//! `AppSettings::light_theme` ⇄ [`LightThemeProvider`]/[`DarkThemeProvider`]。

/// 自绘层语义色板：所有字段为 ARGB（0xAARRGGBB）u32。
///
/// `id` 标识主题名（"dark" | "light"），供调试与日志使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeTokens {
    pub id: &'static str,
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
            id: "dark",
            bg_app: 0xFF1C1C1C,
            bg_panel: 0xFF202020,
            bg_bar: 0xFF252525,
            bg_raised: 0xFF2F2F2F,
            bg_raised_hover: 0xFF383838,
            bg_raised_press: 0xFF292929,
            bg_input: 0xFF1A1A1A,
            line: 0xFF383838,
            line_strong: 0xFF4A4A4A,
            text: 0xFFF4F4F4,
            text_2: 0xFFC9C9C9,
            text_3: 0xFFA6A6A6,
            accent: 0xFF60CDFF,
            accent_hover: 0xFF7AD7FF,
            accent_press: 0xFF4BB2E0,
            accent_ink: 0xFF06131A,
            accent_soft: 0xFF1F3A47,
            danger: 0xFFE0666F,
            danger_soft: 0xFF3A2427,
            danger_press: 0xFF472A2E,
            warn: 0xFFE3B457,
            warn_soft: 0xFF3A3320,
            ok: 0xFF5AC18E,
            ok_soft: 0xFF1F3A30,
            shared: 0xFFC77AE0,
            shared_soft: 0xFF3A2740,
            bg_overlay: 0xFF2C2C2C,
            scrim: 0x66000000,
            label_bar_bg: 0x80000000,
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
            id: "light",
            bg_app: 0xFFF7F7F8,
            bg_panel: 0xFFFFFFFF,
            bg_bar: 0xFFECECF0,
            bg_raised: 0xFFFFFFFF,
            bg_raised_hover: 0xFFF0F0F3,
            bg_raised_press: 0xFFE4E4E9,
            bg_input: 0xFFFFFFFF,
            line: 0xFFE0E0E6,
            line_strong: 0xFFC8C8D0,
            text: 0xFF1F2328,
            text_2: 0xFF444B52,
            text_3: 0xFF62696F,
            accent: 0xFF0B7EC2,
            accent_hover: 0xFF0A6FAA,
            accent_press: 0xFF095D90,
            accent_ink: 0xFFFFFFFF,
            accent_soft: 0xFFE3F0FC,
            danger: 0xFFC0343F,
            danger_soft: 0xFFFBE8EA,
            danger_press: 0xFFA92D37,
            warn: 0xFF9A6B10,
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

    #[test]
    fn text_3_and_semantic_text_meet_contrast_floor() {
        for tokens in [DarkThemeProvider.theme(), LightThemeProvider.theme()] {
            for (surface, color) in [
                ("bg-raised", tokens.bg_raised),
                ("bg-raised-hover", tokens.bg_raised_hover),
                ("bg-overlay", tokens.bg_overlay),
            ] {
                let ratio = contrast(tokens.text_3, color);
                assert!(
                    ratio >= 4.5,
                    "{}: text-3 对 {surface} 只有 {ratio:.2}:1（契约 §5 要求 4.5:1）",
                    tokens.id
                );
            }
            for (semantic, color) in [("ok", tokens.ok), ("shared", tokens.shared)] {
                let ratio = contrast(color, tokens.bg_overlay);
                assert!(
                    ratio >= 4.5,
                    "{}: {semantic} 对 bg-overlay 只有 {ratio:.2}",
                    tokens.id
                );
            }
        }
    }
}
