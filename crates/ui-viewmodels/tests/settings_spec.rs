//! AppSettings round-trip 与容错回落：设置持久化数据侧契约。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use ui_viewmodels::{settings_path, AppSettings};

fn tmp_dir(tag: &str) -> PathBuf {
    let root = PathBuf::from("target").join("tmp").join(tag);
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("建临时目录失败");
    root
}

#[test]
fn defaults_are_conservative() {
    let s = AppSettings::default();
    assert!(!s.activate_on_single_click, "默认双击触发");
    assert!(!s.send_after_paste, "默认不发送（红线）");
    assert!(
        !s.gpu_rendering,
        "默认软件渲染（2026-08-29 翻转：femtovg resize 卡顿，目标用户为低配机）"
    );
    assert!(!s.light_theme, "默认深色主题");
    assert!(s.ui_animations, "默认开界面动画");
    assert_eq!(s.sidebar_width, 212.0, "默认侧栏宽度 212");
    assert!(s.auto_update_check, "默认开自动检查更新（D56）");
    assert_eq!(s.last_check_unix, 0, "默认从未检查过");
    assert!(s.dismissed_version.is_empty(), "默认没有跳过的版本");
    assert!(s.nudge_offscreen_window, "默认开屏外窗口自愈（D77）");
    assert!(
        !s.auto_elevate_on_launch,
        "默认不自动提权（D78）：UAC 每次启动弹窗 + 管理员下拖入导入失效，须用户显式选择"
    );
    assert!(
        s.suppress_activate_on_asset_click,
        "默认开素材点击免激活（D83，用户拍板新方案即默认）"
    );
    assert!(s.space_sync_gate, "默认开大小闸（D90 拍板②：500MB 起可调）");
    assert_eq!(s.space_sync_gate_mb, 500, "默认闸值 500MB");
    assert!(!s.direct_send_enabled, "默认隐藏直发（D90 拍板④）");
}

#[test]
fn save_then_load_roundtrips() {
    let dir = tmp_dir("settings-roundtrip");
    let path = dir.join("settings.toml");
    let s = AppSettings {
        activate_on_single_click: true,
        send_after_paste: true,
        gpu_rendering: true,
        light_theme: true,
        theme_id: "peach-cat".into(),
        ui_animations: false,
        sidebar_width: 300.0,
        fast_import_mode: false,
        verbose_diagnostics: true,
        auto_update_check: false,
        last_check_unix: 1_700_000_000,
        dismissed_version: "v0.2.0".into(),
        nudge_offscreen_window: false,
        auto_elevate_on_launch: true,
        suppress_activate_on_asset_click: false,
        space_sync_gate: false,
        space_sync_gate_mb: 2048,
        direct_send_enabled: true,
        input_point_overrides: BTreeMap::new(),
    };
    s.save(&path).expect("写设置失败");
    let loaded = AppSettings::load(&path);
    assert_eq!(loaded, s);
    assert!(
        loaded.auto_elevate_on_launch,
        "自动提权参与持久化 round-trip"
    );
    assert!(
        !loaded.suppress_activate_on_asset_click,
        "素材点击免激活参与持久化 round-trip（D83）"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn missing_file_falls_back_to_default() {
    let dir = tmp_dir("settings-missing");
    let loaded = AppSettings::load(&dir.join("nope.toml"));
    assert_eq!(loaded, AppSettings::default());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn partial_toml_fills_missing_fields() {
    let dir = tmp_dir("settings-partial");
    let path = dir.join("settings.toml");
    fs::write(&path, "activate_on_single_click = true\n").expect("写部分设置失败");
    let loaded = AppSettings::load(&path);
    assert!(loaded.activate_on_single_click);
    assert!(!loaded.send_after_paste, "缺字段回落默认 false");
    assert!(!loaded.gpu_rendering, "缺字段回落默认（软件渲染）");
    assert!(!loaded.light_theme, "缺字段回落默认 false");
    assert!(loaded.ui_animations, "缺字段回落默认开动画");
    assert_eq!(loaded.sidebar_width, 212.0, "缺字段回落默认侧栏宽度");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_content_falls_back_to_default() {
    let dir = tmp_dir("settings-corrupt");
    let path = dir.join("settings.toml");
    fs::write(&path, "this is not = valid = toml ][").expect("写损坏设置失败");
    let loaded = AppSettings::load(&path);
    assert_eq!(loaded, AppSettings::default(), "损坏内容回落默认不 panic");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn settings_path_prefers_library_root() {
    let root = PathBuf::from("some").join("lib");
    let p = settings_path(Some(&root));
    assert_eq!(p, root.join("settings.toml"));
}

#[test]
fn theme_choice_survives_first_import_settings_migration() {
    let dir = tmp_dir("theme-settings-migration");
    let legacy = dir.join("program-settings.toml");
    let destination = dir.join("new-library/settings.toml");
    let settings = AppSettings {
        theme_id: "peach-cat".into(),
        light_theme: true,
        ..AppSettings::default()
    };
    settings.save(&legacy).unwrap();
    ui_viewmodels::settings::migrate_legacy_settings(&legacy, &destination).unwrap();
    assert_eq!(AppSettings::load(&destination), settings);
    assert!(legacy.is_file(), "旧设置保留以支持旧版本");
    AppSettings::default().save(&destination).unwrap();
    ui_viewmodels::settings::migrate_legacy_settings(&legacy, &destination).unwrap();
    assert_eq!(
        AppSettings::load(&destination),
        AppSettings::default(),
        "已有库设置绝不被覆盖"
    );
    let _ = fs::remove_dir_all(dir);
}
