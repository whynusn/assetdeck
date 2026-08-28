//! AppSettings round-trip 与容错回落：设置持久化数据侧契约。

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
    assert!(s.gpu_rendering, "默认 GPU 渲染，避开软件脏矩形黑块");
    assert!(!s.light_theme, "默认深色主题");
    assert!(s.ui_animations, "默认开界面动画");
    assert_eq!(s.sidebar_width, 212.0, "默认侧栏宽度 212");
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
        ui_animations: false,
        sidebar_width: 300.0,
        fast_import_mode: false,
        verbose_diagnostics: true,
        ask_classify_on_import: false,
        remember_package_mode: "per_source".into(),
        remember_package_category: "表情".into(),
        remember_folder_mode: String::new(),
        remember_folder_category: String::new(),
        remember_loose_mode: "unified".into(),
        remember_loose_category: "相册".into(),
    };
    s.save(&path).expect("写设置失败");
    let loaded = AppSettings::load(&path);
    assert_eq!(loaded, s);
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
    assert!(loaded.gpu_rendering, "缺字段回落默认 GPU 渲染");
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
