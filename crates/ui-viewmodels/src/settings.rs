//! 应用设置模型：交互触发行为与发送开关的持久化。
//!
//! 分层依据：app-ui 依赖白名单只有本 crate + slint + platform，故设置的
//! 序列化/读写收拢在本 VM 层（serde + toml），壳层只读写属性。
//!
//! 红线：`send_after_paste` 默认 false 且为受控占位——打开只持久化，v1 不接
//! 真实发送链路，上框永远止步输入框。核心链路绝不合成回车。
//!
//! 设置项描述化：[`SETTING_SPECS`] 静态描述每条设置项（键/标题/控件种类），
//! [`AppSettings::describe`] 按此序产出动态视图，[`AppSettings::toggle`]
//! 按 key 翻转字段——壳层设置面板不需要再维护一份 key 表。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 便携设置。全字段都带 `#[serde(default)]`，缺字段回落默认而非报错。
///
/// 只派生 PartialEq 不派生 Eq：sidebar_width 是 f32（拖宽持久化），无 Eq。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// 素材上框触发行为：true = 单击即上框；false（默认）= 双击才上框。
    #[serde(default)]
    pub activate_on_single_click: bool,
    /// 上框后是否立即发送。默认 false，且 v1 为受控占位（不接真实发送）。
    #[serde(default)]
    pub send_after_paste: bool,
    /// 渲染后端：false（默认）= CPU 软件渲染；true = GPU（femtovg/OpenGL）。
    ///
    /// 默认翻转（2026-08-29 用户实测）：femtovg 在窗口 resize 时逐帧重建
    /// 渲染面，低配机/远程桌面拖拽窗口明显卡顿——而目标用户正是低配机
    /// （D40–D45 全部来自低配机实测）。软件档渲染完整（software-renderer-path
    /// 已修矢量图标缺失），window-resize 平滑。此选项只在启动时读一次
    /// （后端仅能在建窗前选定），改动需重启才生效。
    #[serde(default = "default_gpu_rendering")]
    pub gpu_rendering: bool,
    /// 浅色主题。默认 false = 深色。
    ///
    /// 实时生效（D37）：自绘层走 ThemeTokens 注入，std-widgets 走内置
    /// Palette 的 color-scheme 翻转，两路同时切换，无需重启。
    #[serde(default)]
    pub light_theme: bool,
    /// 界面动画：true（默认）= 弹层展开播过渡动画（Slint animate 实现）；
    /// false = 全部时长钳到 0，立即展开。录屏/低性能设备可关。
    #[serde(default = "default_ui_animations")]
    pub ui_animations: bool,
    /// 左侧分类侧栏宽度（逻辑像素）。拖动分隔条实时改，松手持久化；
    /// 读回时夹取到 SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH。
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// 导入速度档位（D37）：true（默认）= 前台高速导入；false = 后台慢速。
    /// 只影响后续发起的导入任务，不影响已在跑的。
    #[serde(default = "default_fast_import")]
    pub fast_import_mode: bool,
    /// 细粒度诊断日志（D38）：默认 false 只记低频重要事件（Info）；
    /// 临时开启后 Debug/Trace（上框/轮询等高频事件）全部落盘，排查完再关。
    #[serde(default)]
    pub verbose_diagnostics: bool,
    /// D50 R8「导入时询问归类」：true（默认）= 每次导入弹归类弹窗；
    /// false = 按下方三组记忆直接套用，不再询问。设置面板可重新打开。
    #[serde(default = "default_ask_classify")]
    pub ask_classify_on_import: bool,
    /// 记住的包组（.emo/千牛结构目录）方式：per_source | unified | inbox；
    /// 空串 = 未记忆。统一归入时连分类名一并记（下同）。
    #[serde(default)]
    pub remember_package_mode: String,
    #[serde(default)]
    pub remember_package_category: String,
    /// 记住的文件夹组方式。
    #[serde(default)]
    pub remember_folder_mode: String,
    #[serde(default)]
    pub remember_folder_category: String,
    /// 记住的散文件组方式。
    #[serde(default)]
    pub remember_loose_mode: String,
    #[serde(default)]
    pub remember_loose_category: String,
    /// D56 自动检查更新：true（默认）= 启动后静默检查（≥24h 节流）；
    /// false = 只能经设置面板手动检查。
    #[serde(default = "default_auto_update_check")]
    pub auto_update_check: bool,
    /// 上次发起更新检查的时刻（Unix 秒）。无论成败都刷新——失败也退避，
    /// 不让每次启动都对不可达的源白打一发。0 = 从未检查。
    #[serde(default)]
    pub last_check_unix: u64,
    /// 「跳过此版本」记录的版本号；空串 = 没跳过过。静默检查命中该版本
    /// 不再弹窗（手动检查不受影响）。
    #[serde(default)]
    pub dismissed_version: String,
}

fn default_auto_update_check() -> bool {
    true
}

fn default_ask_classify() -> bool {
    true
}

fn default_fast_import() -> bool {
    true
}

/// 侧栏宽度夹取下限（与 appwindow.slint 的 min-sidebar-width 一致）。
pub const SIDEBAR_MIN_WIDTH: f32 = 150.0;
/// 侧栏宽度夹取上限（与 appwindow.slint 的 max-sidebar-width 一致）。
pub const SIDEBAR_MAX_WIDTH: f32 = 420.0;

fn default_ui_animations() -> bool {
    true
}

fn default_sidebar_width() -> f32 {
    212.0
}

impl AppSettings {
    /// 非开关字段的读回净化：夹取拖宽范围，防手改 settings.toml 塞进荒谬值。
    pub fn sanitized(mut self) -> Self {
        self.sidebar_width = self
            .sidebar_width
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        self
    }
}

impl AppSettings {
    /// 从磁盘读取；文件缺失/解析失败一律回落默认，绝不 panic。
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str::<AppSettings>(&text)
                .unwrap_or_default()
                .sanitized(),
            Err(_) => AppSettings::default(),
        }
    }

    /// 原子写入：先写同目录 tmp 再 rename 覆盖，避免半写坏文件。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let body = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, body.as_bytes())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn default_gpu_rendering() -> bool {
    false
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            activate_on_single_click: false,
            send_after_paste: false,
            gpu_rendering: default_gpu_rendering(),
            light_theme: false,
            ui_animations: true,
            sidebar_width: default_sidebar_width(),
            fast_import_mode: default_fast_import(),
            verbose_diagnostics: false,
            ask_classify_on_import: default_ask_classify(),
            remember_package_mode: String::new(),
            remember_package_category: String::new(),
            remember_folder_mode: String::new(),
            remember_folder_category: String::new(),
            remember_loose_mode: String::new(),
            remember_loose_category: String::new(),
            auto_update_check: default_auto_update_check(),
            last_check_unix: 0,
            dismissed_version: String::new(),
        }
    }
}

impl AppSettings {
    /// 按 [`SETTING_SPECS`] 顺序产出全部设置项视图（含动态说明文案与当前值）。
    ///
    /// 分区标题按 `section` 字段变化就地插入（`header=true` 的视图，key 为空、
    /// 无开关），壳层照序渲染即可。顺序即展示顺序，无需再维护第二份 key 表。
    pub fn describe(&self) -> Vec<SettingView> {
        let mut views = Vec::with_capacity(SETTING_SPECS.len() + 4);
        let mut last_section = "";
        for spec in SETTING_SPECS {
            if spec.section != last_section {
                views.push(SettingView {
                    key: String::new(),
                    title: spec.section.to_string(),
                    detail: String::new(),
                    checked: false,
                    enabled: false,
                    header: true,
                });
                last_section = spec.section;
            }
            views.push(SettingView {
                key: spec.key.to_string(),
                title: spec.title.to_string(),
                detail: self.detail_for(spec.key).to_string(),
                checked: self.value_of(spec.key),
                enabled: self.enabled_for(spec.key),
                header: false,
            });
        }
        views
    }

    /// 按 key 翻转对应开关字段；返回是否认识该 key。
    ///
    /// 未知 key 返回 `false` 且不修改任何字段（调用方可忽略或记日志）。
    pub fn toggle(&mut self, key: &str) -> bool {
        let Some(slot) = self.slot_mut(key) else {
            return false;
        };
        *slot = !*slot;
        true
    }

    /// key → 当前字段值。未知 key 返回 false（配合 [`SETTING_SPECS`] 使用，
    /// 正常路径不会问未知 key）。
    fn value_of(&self, key: &str) -> bool {
        match key {
            "activate_on_single_click" => self.activate_on_single_click,
            "send_after_paste" => self.send_after_paste,
            "gpu_rendering" => self.gpu_rendering,
            "light_theme" => self.light_theme,
            "ui_animations" => self.ui_animations,
            "fast_import_mode" => self.fast_import_mode,
            "verbose_diagnostics" => self.verbose_diagnostics,
            "ask_classify_on_import" => self.ask_classify_on_import,
            "auto_update_check" => self.auto_update_check,
            _ => false,
        }
    }

    /// key → 可变字段引用。未知 key 返回 `None`。
    fn slot_mut(&mut self, key: &str) -> Option<&mut bool> {
        match key {
            "activate_on_single_click" => Some(&mut self.activate_on_single_click),
            "send_after_paste" => Some(&mut self.send_after_paste),
            "gpu_rendering" => Some(&mut self.gpu_rendering),
            "light_theme" => Some(&mut self.light_theme),
            "ui_animations" => Some(&mut self.ui_animations),
            "fast_import_mode" => Some(&mut self.fast_import_mode),
            "verbose_diagnostics" => Some(&mut self.verbose_diagnostics),
            "ask_classify_on_import" => Some(&mut self.ask_classify_on_import),
            "auto_update_check" => Some(&mut self.auto_update_check),
            _ => None,
        }
    }

    /// key → 控件可用性。占位功能保持可见但不可交互，避免用户误以为已生效。
    fn enabled_for(&self, key: &str) -> bool {
        !matches!(key, "send_after_paste")
    }

    /// key → 动态说明文案（设置面板次要行）。一句话讲清「开了会怎样/何时生效」，
    /// 不写「开启（默认）：……」这类把出厂状态复读给用户的长句——文字墙会
    /// 把整块面板拖成需要阅读而不是扫一眼的表单（真机截图教训）。
    fn detail_for(&self, key: &str) -> &'static str {
        match key {
            "activate_on_single_click" => "开启：单击素材即上框；关闭：双击上框。",
            "send_after_paste" => "即将支持：上框目前止步于输入框，不会自动发送。",
            "gpu_rendering" => "重启后生效。默认关闭：GPU 档拖拽窗口易卡顿。",
            "light_theme" => "深浅主题立即切换，无需重启。",
            "ui_animations" => "关闭后弹层立即展开，不播过渡动画。",
            "fast_import_mode" => "多线程高速导入；关闭后转后台慢速，不抢前台。",
            "verbose_diagnostics" => "排查问题时临时开启，记录高频调试细节。",
            "ask_classify_on_import" => "每次导入前询问归类；关闭后按记住的方式直接导入。",
            "auto_update_check" => "启动后静默检查新版本（一天最多一次）；面板下方可随时手动检查。",
            _ => "",
        }
    }
}

/// 设置项控件种类。v1 只支持开关（Toggle），后续再按需扩展下拉/输入等。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Toggle,
}

/// 设置项静态规格：键、标题、所属分区与控件种类。构造辅助是 `const fn`，
/// 可静态初始化（见 [`SETTING_SPECS`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingSpec<'a> {
    pub key: &'a str,
    pub title: &'a str,
    /// 所属分区名；相邻 spec 的 section 变化处即面板分区标题插入点。
    pub section: &'a str,
    pub kind: SettingKind,
}

impl<'a> SettingSpec<'a> {
    /// const 构造辅助：支持 `static` 上下文静态初始化。
    pub const fn new(key: &'a str, title: &'a str, section: &'a str, kind: SettingKind) -> Self {
        SettingSpec {
            key,
            title,
            section,
            kind,
        }
    }
}

/// 全部设置项（v1 全为开关）。顺序即设置面板展示顺序（按分区聚簇）；
/// [`AppSettings::describe`] 按此顺序产出视图，[`AppSettings::toggle`] 按 key 翻转。
pub static SETTING_SPECS: &[SettingSpec<'static>] = &[
    SettingSpec::new(
        "activate_on_single_click",
        "上框触发方式",
        "上框行为",
        SettingKind::Toggle,
    ),
    SettingSpec::new(
        "send_after_paste",
        "上框后自动发送",
        "上框行为",
        SettingKind::Toggle,
    ),
    SettingSpec::new(
        "ask_classify_on_import",
        "导入时询问归类",
        "导入",
        SettingKind::Toggle,
    ),
    SettingSpec::new(
        "fast_import_mode",
        "前台高速导入",
        "导入",
        SettingKind::Toggle,
    ),
    SettingSpec::new("light_theme", "浅色主题", "外观", SettingKind::Toggle),
    SettingSpec::new("ui_animations", "界面动画", "外观", SettingKind::Toggle),
    SettingSpec::new("gpu_rendering", "GPU 渲染", "高级", SettingKind::Toggle),
    SettingSpec::new(
        "verbose_diagnostics",
        "细粒度诊断日志",
        "高级",
        SettingKind::Toggle,
    ),
    SettingSpec::new(
        "auto_update_check",
        "自动检查更新",
        "高级",
        SettingKind::Toggle,
    ),
];

/// 设置项动态视图：`describe()` 为每条规格补齐说明文案与当前值；
/// 分区标题行 `header=true`（key 空、无开关）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingView {
    pub key: String,
    pub title: String,
    /// 动态说明文案（一句话讲清效果与生效时机）。
    pub detail: String,
    pub checked: bool,
    pub enabled: bool,
    pub header: bool,
}

/// 设置文件位置（便携约定）：有库根 → `<root>/settings.toml`；
/// 否则 exe 同目录 → `<exe_dir>/settings.toml`（求不到 exe 目录时退回当前目录）。
pub fn settings_path(library_root: Option<&Path>) -> PathBuf {
    if let Some(root) = library_root {
        return root.join("settings.toml");
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    exe_dir.join("settings.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_covers_all_specs_with_matching_checked() {
        let settings = AppSettings {
            activate_on_single_click: true,
            send_after_paste: false,
            gpu_rendering: true,
            light_theme: false,
            ui_animations: false,
            sidebar_width: 300.0,
            fast_import_mode: true,
            verbose_diagnostics: false,
            ask_classify_on_import: true,
            remember_package_mode: String::new(),
            remember_package_category: String::new(),
            remember_folder_mode: String::new(),
            remember_folder_category: String::new(),
            remember_loose_mode: String::new(),
            remember_loose_category: String::new(),
            auto_update_check: true,
            last_check_unix: 0,
            dismissed_version: String::new(),
        };
        let views = settings.describe();

        // 分区标题按 section 变化插入：4 组 → 恰好 4 条 header，且位置正确。
        let headers: Vec<&SettingView> = views.iter().filter(|v| v.header).collect();
        assert_eq!(
            headers
                .iter()
                .map(|h| h.title.as_str())
                .collect::<Vec<_>>(),
            ["上框行为", "导入", "外观", "高级"],
            "分区标题按顺序恰好出现一次"
        );

        // 非标题行与 SETTING_SPECS 一一同构（顺序 + 字段）。
        let rows: Vec<&SettingView> = views.iter().filter(|v| !v.header).collect();
        assert_eq!(rows.len(), SETTING_SPECS.len(), "设置行覆盖全部规格");
        for (view, spec) in rows.iter().zip(SETTING_SPECS.iter()) {
            assert_eq!(view.key, spec.key);
            assert_eq!(view.title, spec.title);
            assert!(!view.detail.is_empty(), "{} 应有动态说明文案", spec.key);
            assert!(
                !view.detail.contains('\n'),
                "{} 的说明应为一句话，不换行",
                spec.key
            );
            let expected = match spec.key {
                "activate_on_single_click" => settings.activate_on_single_click,
                "send_after_paste" => settings.send_after_paste,
                "gpu_rendering" => settings.gpu_rendering,
                "light_theme" => settings.light_theme,
                "ui_animations" => settings.ui_animations,
                "fast_import_mode" => settings.fast_import_mode,
                "verbose_diagnostics" => settings.verbose_diagnostics,
                "ask_classify_on_import" => settings.ask_classify_on_import,
                "auto_update_check" => settings.auto_update_check,
                other => unreachable!("SETTING_SPECS 出现未知 key: {other}"),
            };
            assert_eq!(
                view.checked, expected,
                "{} 的 checked 应与字段一致",
                spec.key
            );
        }
        // 标题行紧邻各自组首行：header 之后紧跟该组第一条设置。
        assert!(views[0].header, "首项之前应有分组标题");
        assert!(!views[1].header && views[1].key == "activate_on_single_click");
        assert_eq!(views[2].key, "send_after_paste", "同组连续排列");
        assert!(views[3].header, "分组切换处插入标题");
    }

    #[test]
    fn toggle_flips_and_roundtrips_via_disk() {
        let dir = PathBuf::from("target").join("tmp").join("settings-toggle");
        let path = dir.join("settings.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录失败");

        let mut settings = AppSettings::default();
        assert!(settings.toggle("activate_on_single_click"));
        assert!(settings.activate_on_single_click);
        assert!(settings.toggle("activate_on_single_click"));
        assert!(!settings.activate_on_single_click, "再翻一次应还原");

        assert!(settings.toggle("light_theme"));
        assert!(settings.light_theme);
        settings.save(&path).expect("写设置失败");

        let loaded = AppSettings::load(&path);
        assert!(loaded.light_theme, "light_theme 参与持久化 round-trip");
        assert_eq!(
            loaded.activate_on_single_click, settings.activate_on_single_click,
            "翻转结果持久化 round-trip"
        );
        assert_eq!(
            loaded.sidebar_width, settings.sidebar_width,
            "侧栏宽度参与持久化 round-trip"
        );
        assert!(loaded.ui_animations, "ui_animations 参与持久化 round-trip");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_clamps_out_of_range_sidebar_width() {
        let dir = PathBuf::from("target").join("tmp").join("settings-clamp");
        let path = dir.join("settings.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录失败");

        fs::write(&path, "sidebar_width = 9999.0").expect("写设置失败");
        let loaded = AppSettings::load(&path);
        assert_eq!(loaded.sidebar_width, SIDEBAR_MAX_WIDTH, "超上限夹回 420");

        fs::write(&path, "sidebar_width = 1.0").expect("写设置失败");
        let loaded = AppSettings::load(&path);
        assert_eq!(loaded.sidebar_width, SIDEBAR_MIN_WIDTH, "低于下限夹回 150");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_check_fields_roundtrip_via_disk() {
        let dir = PathBuf::from("target").join("tmp").join("settings-update");
        let path = dir.join("settings.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录失败");

        let settings = AppSettings {
            last_check_unix: 1_700_000_000,
            dismissed_version: "v0.2.0".to_string(),
            ..AppSettings::default()
        };
        settings.save(&path).expect("写设置失败");
        let loaded = AppSettings::load(&path);
        assert_eq!(loaded.last_check_unix, 1_700_000_000, "检查时刻 round-trip");
        assert_eq!(loaded.dismissed_version, "v0.2.0", "跳过版本 round-trip");
        assert!(loaded.auto_update_check, "默认开启自动检查");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_spec_key_is_recognized_by_toggle() {
        let mut settings = AppSettings::default();
        for spec in SETTING_SPECS {
            assert!(settings.toggle(spec.key), "{} 应被 toggle 认识", spec.key);
        }
    }

    #[test]
    fn unknown_key_returns_false_without_mutation() {
        let mut settings = AppSettings::default();
        let before = settings.clone();
        assert!(!settings.toggle("no_such_setting"));
        assert_eq!(settings, before, "未知 key 不修改任何字段");
    }
}
