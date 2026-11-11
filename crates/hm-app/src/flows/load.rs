//! Loading each page's data. Pages load when first opened, and on request after that.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui::{Context, Image, ImageFormat};
use hm_core::{CancelToken, Config};

use crate::state::{AppSort, ModelRow, ModelSort, Page, ProjectFilter, Shell};

impl Shell {
    pub fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.page = page;
        self.on_page_enter(cx);
        cx.notify();
    }

    /// Fetches what the page needs if it has not been fetched yet.
    pub fn on_page_enter(&mut self, cx: &mut Context<Self>) {
        match self.page {
            Page::Modelos if self.models.inventory.at.is_none() => self.load_models(cx),
            Page::Proyectos if self.projects.projects.at.is_none() => self.load_projects(cx),
            Page::Puertos if self.ports.ports.at.is_none() => self.load_ports(cx),
            Page::Apps if self.apps.apps.at.is_none() => self.load_apps(cx),
            Page::Arranque if self.startup.items.at.is_none() => self.load_startup(cx),
            Page::Mapa if self.disk.root.is_none() && !self.disk.loading => self.load_disk(cx),
            Page::Ajustes => self.load_settings(cx),
            _ => {}
        }
    }

    // ---- Models --------------------------------------------------------------------------------

    pub fn load_models(&mut self, cx: &mut Context<Self>) {
        if self.models.inventory.loading {
            return;
        }
        self.models.inventory.begin();
        let ctx = self.fresh_ctx();
        let demo = self.mode.demo;
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    std::thread::sleep(std::time::Duration::from_millis(350));
                    Ok(crate::demo::ai::inventory())
                } else {
                    hm_ai::inventory::scan(&ctx)
                }
            },
            |shell, result, _cx| {
                shell.models.inventory.finish(result);
                shell.models.runtimes_at = Some(Instant::now());
                shell.rebuild_model_rows();
            },
        );
        cx.notify();
    }

    pub fn rebuild_model_rows(&mut self) {
        let Some(inventory) = &self.models.inventory.data else {
            self.models.rows.clear();
            self.models.list.reset(0);
            return;
        };
        let mut order: Vec<usize> = (0..inventory.models.len()).collect();
        let models = &inventory.models;
        match self.models.sort {
            ModelSort::Size => order.sort_by(|&a, &b| models[b].size.cmp(&models[a].size)),
            ModelSort::LastUsed => {
                order.sort_by(|&a, &b| models[b].last_used.cmp(&models[a].last_used))
            }
            ModelSort::Name => order.sort_by(|&a, &b| {
                models[a]
                    .name
                    .to_lowercase()
                    .cmp(&models[b].name.to_lowercase())
            }),
        }
        let mut rows = vec![ModelRow::Hero, ModelRow::Runtimes];
        if !inventory.duplicates.is_empty() {
            rows.push(ModelRow::DuplicatesHeader);
            rows.extend((0..inventory.duplicates.len()).map(ModelRow::Duplicate));
        }
        if models.is_empty() {
            rows.push(ModelRow::NoModels);
        } else {
            rows.push(ModelRow::TableHeader);
        }
        rows.extend((0..order.len()).map(ModelRow::Model));
        rows.push(ModelRow::Gap);
        self.models.order = order;
        self.models.list.reset(rows.len());
        self.models.rows = rows;
    }

    // ---- Projects ------------------------------------------------------------------------------

    pub fn load_projects(&mut self, cx: &mut Context<Self>) {
        if self.projects.projects.loading {
            return;
        }
        self.projects.projects.begin();
        self.projects.docker.begin();
        let demo = self.mode.demo;
        let ctx = self.fresh_ctx();
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    Ok(crate::demo::dev::projects())
                } else {
                    hm_dev::projects::list_projects(&ctx)
                }
            },
            |shell, result, _cx| {
                shell.projects.projects.finish(result);
                shell.rebuild_project_rows();
            },
        );
        let ctx = self.fresh_ctx();
        Shell::run_bg(
            cx,
            move || {
                Ok(if demo {
                    Some(crate::demo::dev::docker())
                } else {
                    hm_dev::docker::summary(&ctx)
                })
            },
            |shell, result, _cx| {
                shell.projects.docker.finish(result);
                shell.rebuild_project_rows();
            },
        );
        cx.notify();
    }

    pub fn rebuild_project_rows(&mut self) {
        let projects = self.projects.projects.data.as_deref().unwrap_or(&[]);
        let idle_days = self.ctx.config.project_idle_days as u64;
        let filter = self.projects.filter;
        let mut order: Vec<usize> = (0..projects.len())
            .filter(|&index| match filter {
                ProjectFilter::All => true,
                ProjectFilter::Idle => projects[index].idle_days.is_some_and(|d| d >= idle_days),
            })
            .collect();
        order.sort_by(|&a, &b| projects[b].artifact_bytes.cmp(&projects[a].artifact_bytes));
        self.projects.order = order;
        // The header and the Docker card come before the projects.
        self.projects.list.reset(self.projects.order.len() + 2);
    }

    // ---- Ports ---------------------------------------------------------------------------------

    pub fn load_ports(&mut self, cx: &mut Context<Self>) {
        if self.ports.ports.loading {
            return;
        }
        self.ports.ports.begin();
        let demo = self.mode.demo;
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    Ok(crate::demo::dev::ports())
                } else {
                    hm_dev::ports::list_listening()
                }
            },
            |shell, result, _cx| {
                shell
                    .ports
                    .ports
                    .finish(result.map(crate::logic::ports::merge_addresses))
            },
        );
        cx.notify();
    }

    // ---- Apps ----------------------------------------------------------------------------------

    pub fn load_apps(&mut self, cx: &mut Context<Self>) {
        if self.apps.apps.loading {
            return;
        }
        self.apps.apps.begin();
        let demo = self.mode.demo;
        let ctx = self.fresh_ctx();
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    Ok(crate::demo::system::apps())
                } else {
                    hm_system::apps::list_apps(&ctx)
                }
            },
            |shell, result, cx| {
                shell.apps.apps.finish(result);
                shell.sort_apps();
                shell.load_app_icons(cx);
            },
        );
        cx.notify();
    }

    pub fn sort_apps(&mut self) {
        let apps = self.apps.apps.data.as_deref().unwrap_or(&[]);
        let mut order: Vec<usize> = (0..apps.len()).collect();
        match self.apps.sort {
            AppSort::Size => {
                let total = |index: usize| {
                    apps[index].size + apps[index].leftovers.iter().map(|l| l.size).sum::<u64>()
                };
                order.sort_by_key(|&index| std::cmp::Reverse(total(index)));
            }
            AppSort::LastUsed => order.sort_by(|&a, &b| apps[b].last_used.cmp(&apps[a].last_used)),
            AppSort::Name => {
                order.sort_by_key(|&index| apps[index].name.to_lowercase());
            }
        }
        self.apps.order = order;
    }

    fn load_app_icons(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<PathBuf> = self
            .apps
            .apps
            .data
            .iter()
            .flatten()
            .filter_map(|app| app.icon_path.clone())
            .filter(|path| !self.apps.icons.contains_key(path))
            .collect();
        if wanted.is_empty() {
            return;
        }
        Shell::run_bg(
            cx,
            move || {
                let mut icons = HashMap::new();
                for path in wanted {
                    if let Some(image) = read_icon(&path) {
                        icons.insert(path, Arc::new(image));
                    }
                }
                Ok(icons)
            },
            |shell, result, _cx| {
                if let Ok(icons) = result {
                    shell.apps.icons.extend(icons);
                }
            },
        );
    }

    // ---- Startup -------------------------------------------------------------------------------

    pub fn load_startup(&mut self, cx: &mut Context<Self>) {
        if self.startup.items.loading {
            return;
        }
        self.startup.items.begin();
        let demo = self.mode.demo;
        let ctx = self.fresh_ctx();
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    Ok(crate::demo::system::startup())
                } else {
                    hm_system::startup::list_items(&ctx)
                }
            },
            |shell, result, _cx| {
                let result = result.map(|mut items| {
                    // Orphans first: they are the ones worth looking at.
                    items.sort_by_key(|item| (!item.orphaned, item.label.to_lowercase()));
                    items
                });
                shell.startup.items.finish(result);
            },
        );
        cx.notify();
    }

    // ---- Disk map ------------------------------------------------------------------------------

    pub fn load_disk(&mut self, cx: &mut Context<Self>) {
        let root = self
            .disk
            .root_path
            .clone()
            .unwrap_or_else(|| self.ctx.home.clone());
        self.scan_disk_root(root, cx);
    }

    pub fn scan_disk_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if let Some(cancel) = self.disk.cancel.take() {
            cancel.cancel();
        }
        let cancel = CancelToken::new();
        self.disk.cancel = Some(cancel.clone());
        self.disk.loading = true;
        self.disk.error = None;
        self.disk.started = Some(Instant::now());
        self.disk.root_path = Some(root.clone());
        self.disk.serial += 1;
        let serial = self.disk.serial;
        let demo = self.mode.demo;
        let home = self.ctx.home.clone();
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    Ok(crate::demo::system::disk_tree())
                } else {
                    hm_system::space::scan_tree_in_home(&root, &home, 5, 20_000_000, &cancel)
                }
            },
            move |shell, result, _cx| {
                if shell.disk.serial != serial {
                    return;
                }
                shell.disk.loading = false;
                shell.disk.cancel = None;
                match result {
                    Ok(tree) => {
                        shell.disk.root = Some(tree);
                        shell.disk.trail.clear();
                        shell.disk.hover = None;
                    }
                    Err(error) => shell.disk.error = Some(error),
                }
            },
        );
        cx.notify();
    }

    pub fn cancel_disk_scan(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.disk.cancel.take() {
            cancel.cancel();
        }
        self.disk.serial += 1;
        self.disk.loading = false;
        cx.notify();
    }

    // ---- Settings ------------------------------------------------------------------------------

    pub fn load_settings(&mut self, cx: &mut Context<Self>) {
        let demo = self.mode.demo;
        let home = self.ctx.home.clone();
        self.settings.history.begin();
        Shell::run_bg(
            cx,
            move || {
                if demo {
                    return Ok((crate::demo::system::history(), Some(false)));
                }
                let log =
                    hm_core::audit::AuditLog::new(hm_core::audit::AuditLog::default_path(&home));
                let mut entries = log.read_all()?;
                entries.reverse();
                entries.truncate(200);
                Ok((entries, Some(hm_core::fsutil::has_full_disk_access(&home))))
            },
            |shell, result, _cx| match result {
                Ok((entries, access)) => {
                    shell.settings.history.finish(Ok(entries));
                    shell.settings.full_disk_access = access;
                }
                Err(error) => shell.settings.history.finish(Err(error)),
            },
        );
    }

    /// Applies a change to the config and saves it, unless this run must not write anything.
    pub fn update_config(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Config)) {
        change(&mut self.ctx.config);
        self.settings.save_error = None;
        if self.mode.persists_config() {
            let path = Config::default_path(&self.ctx.home);
            if let Err(error) = self.ctx.config.save(&path) {
                self.settings.save_error = Some(format!("{error:#}"));
            }
        }
        cx.notify();
    }
}

/// Reads an `.icns` and returns the PNG closest to the size we draw at.
fn read_icon(path: &Path) -> Option<Image> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("png") {
        return Some(Image::from_bytes(
            ImageFormat::Png,
            std::fs::read(path).ok()?,
        ));
    }
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let family = icns::IconFamily::read(file).ok()?;
    let mut types = family.available_icons();
    types.sort_by_key(|kind| {
        let size = kind.pixel_width();
        (size < 64, size.abs_diff(128))
    });
    for kind in types {
        if let Ok(image) = family.get_icon_with_type(kind) {
            let mut bytes = Vec::new();
            if image.write_png(&mut bytes).is_ok() {
                return Some(Image::from_bytes(ImageFormat::Png, bytes));
            }
        }
    }
    None
}
