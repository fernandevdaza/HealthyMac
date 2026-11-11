//! HealthyMac's desktop app.

mod assets;
mod demo;
mod dock_icon;
#[cfg(debug_assertions)]
mod drive;
mod flows;
mod logic;
mod pages;
mod state;
mod theme;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{
    actions, point, px, size, App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem,
    SystemMenuType, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions,
};
use hm_core::{t, Config, ScanContext};

use crate::state::{
    AppsState, DiskState, LiveState, ModelsState, Page, PortsState, ProjectsState, RunMode,
    ScanState, ScrollPane, SettingsState, Shell, StartupState,
};

actions!(
    healthymac,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        CloseWindow,
        OpenSettings
    ]
);

fn build_context(mode: &RunMode) -> (ScanContext, Option<String>) {
    let home = mode
        .home_override
        .clone()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"));
    if mode.demo {
        return (
            ScanContext::with_home("/Users/ana", Config::default()),
            None,
        );
    }
    match Config::load(&Config::default_path(&home)) {
        Ok(config) => (ScanContext::with_home(home, config), None),
        Err(error) => (
            ScanContext::with_home(home, Config::default()),
            Some(format!("{error:#}")),
        ),
    }
}

/// The menu bar, in the current language. Built again when the language changes.
pub fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "HealthyMac".into(),
            items: vec![
                MenuItem::action(t!("Ajustes…", "Settings…"), OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu(t!("Servicios", "Services"), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(t!("Ocultar HealthyMac", "Hide HealthyMac"), Hide),
                MenuItem::action(t!("Ocultar otros", "Hide Others"), HideOthers),
                MenuItem::action(t!("Mostrar todo", "Show All"), ShowAll),
                MenuItem::separator(),
                MenuItem::action(t!("Salir de HealthyMac", "Quit HealthyMac"), Quit),
            ],
        },
        Menu {
            name: t!("Ventana", "Window").into(),
            items: vec![
                MenuItem::action(t!("Minimizar", "Minimize"), Minimize),
                MenuItem::action(t!("Zoom", "Zoom"), Zoom),
                MenuItem::separator(),
                MenuItem::action(t!("Cerrar", "Close"), CloseWindow),
            ],
        },
    ]
}

fn main() {
    hm_core::threads::configure_global_pool();
    ui::motion::read_system_preference();
    let mode = RunMode::from_env();
    // Read the config before anything is drawn, so the first frame and the menu bar are already in
    // the right language.
    let (mut ctx, config_error) = build_context(&mode);
    // For screenshots and testing: force a language or theme without touching the saved settings.
    match std::env::var("HEALTHYMAC_LANG").as_deref() {
        Ok("es") => ctx.config.language = hm_core::LanguageChoice::Spanish,
        Ok("en") => ctx.config.language = hm_core::LanguageChoice::English,
        _ => {}
    }
    match std::env::var("HEALTHYMAC_THEME").as_deref() {
        Ok("light") => ctx.config.appearance = hm_core::Appearance::Light,
        Ok("dark") => ctx.config.appearance = hm_core::Appearance::Dark,
        _ => {}
    }
    hm_core::i18n::set_language(flows::language::resolve(ctx.config.language));
    let mut first_context = Some((ctx, config_error));
    // For automated testing: open without taking focus from whatever the user is doing.
    let background = std::env::var_os("HEALTHYMAC_BACKGROUND").is_some();

    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            dock_icon::apply(dock_icon::system_is_dark());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &Hide, cx| cx.hide());
            cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
            cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-h", Hide, None),
                KeyBinding::new("cmd-alt-h", HideOthers, None),
                KeyBinding::new("cmd-m", Minimize, None),
                KeyBinding::new("cmd-w", CloseWindow, None),
                KeyBinding::new("cmd-,", OpenSettings, None),
            ]);
            cx.set_menus(menus());
            // This is a one-window app: closing it means you are done.
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
            let mode = mode.clone();
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            title: Some("HealthyMac".into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(16.), px(17.))),
                        }),
                        window_min_size: Some(size(
                            px(theme::MIN_WINDOW_W),
                            px(theme::MIN_WINDOW_H),
                        )),
                        window_background: WindowBackgroundAppearance::Opaque,
                        focus: !background,
                        app_id: Some("dev.healthymac.app".into()),
                        ..Default::default()
                    },
                    move |window, cx| {
                        cx.new(|cx| {
                            let focus = cx.focus_handle();
                            focus.focus(window);
                            let (ctx, config_error) =
                                first_context.take().unwrap_or_else(|| build_context(&mode));
                            let scanners = flows::scan::scanners_for(&mode);
                            let mut shell = Shell {
                                mode,
                                ctx,
                                config_error,
                                focus,
                                page: Page::Resumen,
                                scanners,
                                scan: ScanState::default(),
                                findings: Vec::new(),
                                selection: Default::default(),
                                views: HashMap::new(),
                                live: LiveState::default(),
                                monitor_tx: None,
                                resumen_scroll: ScrollPane::default(),
                                monitor_scroll: ScrollPane::default(),
                                models: ModelsState::default(),
                                projects: ProjectsState::default(),
                                ports: PortsState::default(),
                                apps: AppsState::default(),
                                startup: StartupState::default(),
                                disk: DiskState::default(),
                                settings: SettingsState::default(),
                                sheet: None,
                                sheet_closing: false,
                                sheet_opened: 0,
                                toast: None,
                                toast_serial: 0,
                                titlebar_pressed: false,
                            };
                            shell.watch_appearance(window, cx);
                            shell.start_background_loops(cx);
                            #[cfg(debug_assertions)]
                            drive::start(window, cx);
                            shell
                        })
                    },
                )
                .expect("no se pudo abrir la ventana");

            // The root view handles these while it has focus. These are for when it does not, such as
            // the menu bar with nothing focused, and they keep the menu items enabled either way. They
            // cannot serve a key press: during one the window is busy dispatching it.
            cx.on_action(move |_: &OpenSettings, cx| {
                window
                    .update(cx, |shell, _, cx| shell.navigate(Page::Ajustes, cx))
                    .ok();
            });
            cx.on_action(move |_: &Minimize, cx| {
                window
                    .update(cx, |_, window, _| window.minimize_window())
                    .ok();
            });
            cx.on_action(move |_: &Zoom, cx| {
                window.update(cx, |_, window, _| window.zoom_window()).ok();
            });
            cx.on_action(move |_: &CloseWindow, cx| {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });

            if !background {
                cx.activate(true);
            }
        });
}
