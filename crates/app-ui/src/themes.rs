//! Theme assembly. Assets are prepared before any visible state is mutated.
use crate::{thumbs::GridCtx, AppWindow, Metrics, Motion, Spacing, Theme, ThemeArt, Typography};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};
use ui_viewmodels::{
    theme::*,
    theme_catalog::{checked_asset, LoadedTheme, ThemeCatalog, MAX_PACKS},
    AppSettings,
};

fn catalog(user_dir: &Path) -> ThemeCatalog {
    let mut c = ThemeCatalog::load(user_dir);
    let packaged = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("themes")));
    let mut roots = packaged.into_iter().collect::<Vec<_>>();
    if cfg!(debug_assertions) {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../themes"));
    }
    for root in roots {
        if root == user_dir {
            continue;
        }
        let bundled = ThemeCatalog::load(&root);
        c.errors.extend(bundled.errors);
        for t in bundled.themes.into_iter().skip(2) {
            if !c.themes.iter().any(|x| x.pack.meta.id == t.pack.meta.id) {
                if c.themes.len() >= MAX_PACKS + 2 {
                    c.errors
                        .push("主题总数已达 32 个，未继续加载随附主题".into());
                    break;
                }
                c.themes.push(t);
            }
        }
    }
    c
}

pub(crate) fn apply(ui: &AppWindow, t: &LoadedTheme) -> Result<(), String> {
    let p = &t.pack;
    let load = |path: &str| -> Result<slint::Image, String> {
        let path = checked_asset(&t.directory, path)?;
        slint::Image::load_from_path(&path).map_err(|e| format!("装饰图片加载失败: {e}"))
    };
    let d = &p.decoration;
    let background = d
        .background
        .as_deref()
        .map(load)
        .transpose()?
        .unwrap_or_default();
    let mut art = Vec::new();
    if let Some(path) = &d.corner {
        art.push(ThemeArt {
            picture: load(path)?,
            x: 0.85,
            y: 0.5,
            scale: d.corner_scale.map_or(1., |v| v.0),
            alpha: 1.,
        });
    }
    if let Some(path) = &d.watermark {
        art.push(ThemeArt {
            picture: load(path)?,
            x: 0.05,
            y: 0.5,
            scale: 1.,
            alpha: d.watermark_opacity.map_or(0.18, |v| v.0),
        });
    }
    for a in &d.stickers {
        art.push(ThemeArt {
            picture: load(&a.image)?,
            x: a.x.0,
            y: a.y.0,
            scale: a.scale.map_or(1., |v| v.0),
            alpha: a.opacity.map_or(1., |v| v.0),
        });
    }
    let tokens = p.tokens();
    crate::apply_theme(ui, &tokens);
    crate::apply_color_scheme(ui, p.meta.base == ThemeBase::Light);
    let theme = ui.global::<Theme>();
    let g = &p.geometry;
    theme.set_radius(g.radius.map_or(4., |v| v.0));
    theme.set_radius_lg(g.radius_lg.map_or(8., |v| v.0));
    theme.set_tile_radius(g.tile_radius.map_or(8., |v| v.0));
    theme.set_border_width(g.border_width.map_or(1., |v| v.0));
    theme.set_icon_weight(p.icons.stroke_weight.map_or(1.7, |v| v.0));
    theme.set_icon_size(p.icons.size.map_or(16., |v| v.0));
    theme.set_icon_cap(match p.icons.line_cap {
        Some(LineCap::Butt) => 1,
        Some(LineCap::Square) => 2,
        _ => 0,
    });
    theme.set_focus_ring(slint::Color::from_argb_encoded(
        p.palette.focus_ring.map_or(tokens.accent, |c| c.0),
    ));
    theme.set_selection_bg(slint::Color::from_argb_encoded(
        p.palette.selection_bg.map_or(tokens.accent_soft, |c| c.0),
    ));
    let scale = p.typography.font_size_scale.map_or(1., |v| v.0);
    let typography = ui.global::<Typography>();
    typography.set_scale(scale);
    typography.set_display(p.typography.display.map_or(16. * scale, |v| v.0));
    typography.set_family(
        p.typography
            .font_family
            .as_deref()
            .filter(|f| !f.is_empty())
            .unwrap_or("Segoe UI")
            .into(),
    );
    typography.set_tracking(p.typography.letter_spacing.map_or(0., |v| v.0));
    let density = p.density_scale();
    ui.global::<Spacing>().set_scale(density);
    let m = ui.global::<Metrics>();
    m.set_control(
        g.control_height
            .map_or(30. * density, |v| v.0)
            .max(28. * scale),
    );
    m.set_control_primary(32. * density.max(scale));
    m.set_row_compact(p.row_height());
    m.set_bar(g.bar_height.map_or(48. * density, |v| v.0));
    m.set_icon_button(30. * density.max(scale));
    m.set_hit_min(28. * density.max(scale));
    m.set_menu_row(46. * density.max(scale));
    m.set_toggle_width(40. * density);
    m.set_toggle_height(22. * density);
    m.set_lock_area(30. * density);
    let motion = ui.global::<Motion>();
    motion.set_theme_enabled(p.motion.enabled.unwrap_or(true));
    motion.set_fast(p.motion.fast.map_or(100, |v| v.0 as i64));
    motion.set_state(p.motion.state.map_or(120, |v| v.0 as i64));
    motion.set_enter(p.motion.enter.map_or(150, |v| v.0 as i64));
    motion.set_linear(p.motion.easing == Some(Easing::Linear));
    ui.set_theme_font_scale(scale);
    ui.set_theme_density(density);
    ui.set_theme_row_height(p.row_height());
    ui.set_theme_tile_gap(g.tile_gap.map_or(12. * density, |v| v.0));
    ui.set_theme_background(background);
    ui.set_theme_background_tint(slint::Color::from_argb_encoded(d.background_tint.map_or(
        if d.background.is_some() {
            (tokens.bg_app & 0x00ff_ffff) | 0xe600_0000
        } else {
            0
        },
        |v| v.0,
    )));
    ui.set_theme_art(ModelRc::new(VecModel::from(art)));
    ui.set_theme_description(p.meta.description.as_deref().unwrap_or(&p.meta.name).into());
    Ok(())
}

fn refresh(ui: &AppWindow, c: &ThemeCatalog, settings: &AppSettings) -> i32 {
    ui.set_theme_names(ModelRc::new(VecModel::from(
        c.themes
            .iter()
            .map(|t| t.pack.meta.name.clone().into())
            .collect::<Vec<_>>(),
    )));
    let selected = c.index(&settings.theme_id, settings.light_theme);
    let mut errors = c.errors.clone();
    let mut actual = selected;
    if !settings.theme_id.is_empty() && c.themes[selected].pack.meta.id != settings.theme_id {
        errors.push(format!("主题 {} 不可用，已回退深色", settings.theme_id));
    }
    if let Err(e) = apply(ui, &c.themes[selected]) {
        errors.push(e);
        actual = 0;
        let _ = apply(ui, &c.themes[0]);
    }
    ui.set_theme_index(actual as i32);
    for e in &errors {
        logging::warn!("主题: {e}");
    }
    ui.set_theme_error(errors.join("\n").into());
    actual as i32
}

pub(crate) fn install(
    ui: &AppWindow,
    settings: Rc<RefCell<AppSettings>>,
    settings_path: Rc<PathBuf>,
    grid: Rc<GridCtx>,
) {
    let dir = settings_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("themes");
    let c = Rc::new(RefCell::new(catalog(&dir)));
    let applied = Rc::new(Cell::new(refresh(ui, &c.borrow(), &settings.borrow())));
    {
        let weak = ui.as_weak();
        let c = c.clone();
        let settings = settings.clone();
        let grid = grid.clone();
        let applied = applied.clone();
        ui.on_theme_selected(move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let c = c.borrow();
            let Some(t) = usize::try_from(index).ok().and_then(|i| c.themes.get(i)) else {
                ui.set_theme_index(applied.get());
                return;
            };
            if let Err(e) = apply(&ui, t) {
                ui.set_theme_index(applied.get());
                ui.set_theme_error(e.into());
                return;
            }
            let mut s = settings.borrow_mut();
            s.theme_id = t.pack.meta.id.clone();
            s.light_theme = t.pack.meta.base == ThemeBase::Light;
            ui.set_theme_index(index);
            applied.set(index);
            ui.set_theme_error(c.errors.join("\n").into());
            if let Err(e) = s.save(&settings_path) {
                ui.set_theme_error(format!("主题已切换，但设置保存失败: {e}").into());
            }
            grid.sync();
        });
    }
    let weak = ui.as_weak();
    ui.on_themes_reload(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        *c.borrow_mut() = catalog(&dir);
        applied.set(refresh(&ui, &c.borrow(), &settings.borrow()));
        grid.sync();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thumbs::ThumbCache;
    use slint::{
        platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        Model,
    };
    use ui_viewmodels::{FacetIndex, LibraryGridVm, ShareVm, Sorter};

    struct TestPlatform;
    impl slint::platform::Platform for TestPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
        }
    }

    fn grid(ui: &AppWindow) -> Rc<GridCtx> {
        Rc::new(GridCtx::new(
            ui.as_weak(),
            Rc::new(RefCell::new(LibraryGridVm::new(
                FacetIndex::new(),
                Sorter::default(),
                1,
            ))),
            Rc::new(VecModel::default()),
            Rc::new(RefCell::new(None)),
            Rc::new(RefCell::new(ThumbCache::new(1))),
            Rc::new(RefCell::new(ShareVm::new())),
        ))
    }

    #[test]
    fn runtime_theme_switch_persists_restarts_reloads_and_rolls_back_failed_assets() {
        slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/theme-runtime-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = Rc::new(dir.join("settings.toml"));
        let settings = Rc::new(RefCell::new(AppSettings::default()));
        let peach_index = catalog(&dir.join("themes")).index("peach-cat", false) as i32;
        assert!(peach_index >= 2);
        let ui = AppWindow::new().unwrap();
        install(&ui, settings.clone(), path.clone(), grid(&ui));
        assert_eq!(ui.get_theme_index(), 0);
        let dark = ui.global::<Theme>().get_bg_app();
        ui.invoke_theme_selected(peach_index);
        assert_eq!(settings.borrow().theme_id, "peach-cat");
        assert_eq!(AppSettings::load(&path).theme_id, "peach-cat");
        assert_eq!(ui.global::<Theme>().get_radius(), 9.);
        assert_eq!(ui.get_theme_row_height(), 32.);
        assert_eq!(ui.get_theme_art().row_count(), 1);
        let peach = ui.global::<Theme>().get_bg_app();
        assert_ne!(peach, dark);

        let reopened = AppWindow::new().unwrap();
        let restored = Rc::new(RefCell::new(AppSettings::load(&path)));
        install(&reopened, restored, path, grid(&reopened));
        assert_eq!(reopened.get_theme_index(), peach_index);
        assert_eq!(reopened.global::<Theme>().get_bg_app(), peach);

        let mut invalid =
            ThemeCatalog::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../themes"))
                .themes
                .into_iter()
                .find(|t| t.pack.meta.id == "peach-cat")
                .unwrap();
        invalid.pack.decoration.corner = Some("assets/removed.png".into());
        assert!(apply(&ui, &invalid).is_err());
        assert_eq!(ui.global::<Theme>().get_bg_app(), peach);
        assert_eq!(ui.get_theme_art().row_count(), 1);
        ui.invoke_themes_reload();
        assert_eq!(ui.get_theme_index(), peach_index);
        ui.invoke_theme_selected(0);
        assert_eq!(ui.global::<Theme>().get_bg_app(), dark);
        assert_eq!(ui.global::<Theme>().get_radius(), 4.);
        assert_eq!(ui.get_theme_row_height(), 28.);
        assert_eq!(ui.get_theme_art().row_count(), 0);
        assert_eq!(ui.global::<Typography>().get_family(), "Segoe UI");

        let builtins = ThemeCatalog::load(&dir.join("absent"));
        let missing = AppSettings {
            theme_id: "removed-pack".into(),
            ..AppSettings::default()
        };
        refresh(&ui, &builtins, &missing);
        assert_eq!(ui.get_theme_index(), 0);
        assert!(ui.get_theme_error().contains("removed-pack"));
        let legacy = AppSettings {
            light_theme: true,
            ..AppSettings::default()
        };
        refresh(&ui, &builtins, &legacy);
        assert_eq!(ui.get_theme_index(), 1);

        // ComboBox writes its new index before emitting selected. A resource
        // removed since discovery must restore the actual previous selection.
        let failure_dir = dir.join("failure-data");
        let pack_dir = failure_dir.join("themes/broken-cat");
        std::fs::create_dir_all(pack_dir.join("assets")).unwrap();
        std::fs::write(
            pack_dir.join("theme.toml"),
            include_str!("../../../themes/peach-cat/theme.toml").replace("peach-cat", "broken-cat"),
        )
        .unwrap();
        let asset = pack_dir.join("assets/cat.png");
        std::fs::write(
            &asset,
            include_bytes!("../../../themes/peach-cat/assets/cat.png"),
        )
        .unwrap();
        let failed_ui = AppWindow::new().unwrap();
        let failed_settings = Rc::new(RefCell::new(AppSettings::default()));
        install(
            &failed_ui,
            failed_settings.clone(),
            Rc::new(failure_dir.join("settings.toml")),
            grid(&failed_ui),
        );
        std::fs::remove_file(&asset).unwrap();
        failed_ui.set_theme_index(2);
        failed_ui.invoke_theme_selected(2);
        assert_eq!(failed_ui.get_theme_index(), 0);
        assert_eq!(failed_ui.global::<Theme>().get_bg_app(), dark);
        assert!(!failed_ui.get_theme_error().is_empty());
        assert!(failed_settings.borrow().theme_id.is_empty());
        assert!(!failure_dir.join("settings.toml").exists());
        std::fs::remove_file(pack_dir.join("theme.toml")).unwrap();
        std::fs::remove_file(dir.join("settings.toml")).unwrap();
    }
}
