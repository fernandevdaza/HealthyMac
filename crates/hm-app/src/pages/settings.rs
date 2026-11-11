//! Settings: how things get removed, where to look, and what has been done so far.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, AnyElement, Context, ElementId, FontWeight, SharedString, Window};
use hm_core::fmt::{human_age, human_size};
use hm_core::{t, tf, Appearance, CleanStatus, LanguageChoice, RemovalMode};

use super::{plural, scroll_pane};
use crate::assets::Icon;
use crate::state::Shell;
use crate::theme::{self, font, radius, sp, GUTTER};
use crate::ui::chrome::{card, notice, page_header, section_title, NoticeKind};
use crate::ui::controls::{icon_button, pill, segmented, stepper, Btn, BtnSize};
use crate::ui::label::path_label;
use crate::ui::{col, icon, row, TextExt};

impl Shell {
    pub fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let width = (super::content_width(window) - GUTTER * 2.0).min(720.0);
        let subtitle = if self.mode.persists_config() {
            t!("Se guarda al momento.", "Changes are saved right away.").to_string()
        } else {
            t!(
                "En este modo los cambios duran hasta que cierres la app: no se guardan.",
                "In this mode, changes last until you quit the app. They are not saved."
            )
            .to_string()
        };
        let pane = self.settings.scroll.clone();
        col()
            .size_full()
            .child(page_header(t!("Ajustes", "Settings"), subtitle, Vec::new()))
            .child(scroll_pane(
                "settings-scroll",
                &pane,
                col()
                    .w(px(width))
                    .ml(px(GUTTER))
                    .pb(sp(8.))
                    .gap(sp(7.))
                    .when_some(self.config_error.clone(), |this, error| {
                        this.child(notice(
                            NoticeKind::Warning,
                            t!(
                                "Tu configuración no se ha podido leer, así que usamos la de fábrica",
                                "Your settings couldn't be read, so we're using the defaults"
                            ),
                            Some(error.into()),
                            None,
                        ))
                    })
                    .when_some(self.settings.save_error.clone(), |this, error| {
                        this.child(notice(
                            NoticeKind::Error,
                            t!("No se ha podido guardar el cambio", "The change couldn't be saved"),
                            Some(error.into()),
                            None,
                        ))
                    })
                    .child(self.render_appearance(cx))
                    .child(self.render_removal_mode(cx))
                    .child(self.render_access(cx))
                    .child(self.render_project_settings(cx))
                    .child(self.render_excluded(cx))
                    .child(self.render_history()),
            ))
            .into_any_element()
    }

    fn render_removal_mode(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.ctx.config.removal_mode;
        section(t!("Al limpiar", "When cleaning"))
            .child(self.mode_option(
                RemovalMode::Trash,
                mode,
                t!("Mover a la Papelera", "Move to Trash"),
                t!(
                    "Lo más prudente: todo se puede recuperar. A cambio, el espacio no se libera hasta que vacías la Papelera.",
                    "The safest choice: everything can be recovered. The catch is that no space is freed until you empty the Trash."
                ),
                cx,
            ))
            .child(self.mode_option(
                RemovalMode::DeleteRegenerable,
                mode,
                t!("Borrar lo que se regenera solo", "Delete what rebuilds itself"),
                t!(
                    "Lo seguro que se regenera (cachés, compilaciones) se borra al momento y libera espacio ya. Todo lo demás sigue yendo a la Papelera.",
                    "Safe items that rebuild themselves (caches, builds) are deleted at once and free space now. Everything else still goes to the Trash."
                ),
                cx,
            ))
    }

    fn mode_option(
        &self,
        value: RemovalMode,
        current: RemovalMode,
        title: &'static str,
        detail: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = value == current;
        let accent = theme::primary();
        row()
            .id(SharedString::from(format!("removal-{value:?}")))
            .gap(sp(3.))
            .items_start()
            .p(sp(3.))
            .rounded(px(radius::MD))
            .border_1()
            .cursor_pointer()
            .when(selected, |this| {
                this.bg(theme::surface_1()).border_color(accent.alpha(0.55))
            })
            .when(!selected, |this| {
                this.bg(theme::surface_1())
                    .border_color(theme::hairline())
                    .hover(|style| style.bg(theme::surface_2()))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.update_config(cx, |config| config.removal_mode = value);
            }))
            .child(
                row()
                    .mt(sp(0.5))
                    .flex_none()
                    .size(px(16.))
                    .justify_center()
                    .rounded_full()
                    .when(selected, |this| this.bg(accent))
                    .when(!selected, |this| {
                        this.border_1().border_color(theme::hairline_strong())
                    })
                    .when(selected, |this| {
                        this.child(div().size(px(6.)).rounded_full().bg(theme::on_primary()))
                    }),
            )
            .child(
                col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(sp(0.5))
                    .child(
                        div()
                            .fs(font::BODY)
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .fs(font::CAPTION)
                            .text_color(theme::text_2())
                            .child(detail),
                    ),
            )
    }

    fn render_access(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let access = self.settings.full_disk_access;
        section(t!("Acceso total al disco", "Full Disk Access")).child(
            card()
                .w_full()
                .p(sp(3.5))
                .child(
                    row()
                        .gap(sp(3.))
                        .items_start()
                        .child(icon(
                            if access == Some(true) { Icon::Shield } else { Icon::Lock },
                            18.0,
                            if access == Some(true) { theme::green() } else { theme::amber() },
                        ))
                        .child(
                            col()
                                .flex_1()
                                .min_w(px(0.))
                                .gap(sp(0.5))
                                .child(
                                    div().fs(font::BODY).font_weight(FontWeight::MEDIUM).child(match access {
                                        Some(true) => t!("Concedido", "Granted"),
                                        Some(false) => t!("Sin conceder", "Not granted"),
                                        None => t!("Comprobando…", "Checking…"),
                                    }),
                                )
                                .child(
                                    div()
                                        .fs(font::CAPTION)
                                        .text_color(theme::text_2())
                                        .child(match access {
                                            Some(true) => t!(
                                                "HealthyMac ve todo el disco: Documentos, Descargas, Escritorio y los datos de las apps. Sigue sin borrar nada que no le pidas.",
                                                "HealthyMac can see the whole disk: Documents, Downloads, Desktop and app data. It still deletes nothing you don't ask for."
                                            ),
                                            _ => t!(
                                                "Sin este permiso no miramos Documentos, Descargas, Escritorio ni los datos internos de las apps, así que el mapa de disco y los archivos grandes se quedan cortos. Con él lo vemos todo, y sigue sin borrarse nada que no pidas. Se concede en Ajustes del Sistema y se puede retirar cuando quieras.",
                                                "Without this permission we can't look in Documents, Downloads, Desktop or app data, so the disk map and large files come up short. With it we see everything, and still nothing is deleted unless you ask. You grant it in System Settings and can take it back at any time."
                                            ),
                                        }),
                                ),
                        )
                        .when(access != Some(true), |this| {
                            this.child(
                                Btn::secondary(t!("Abrir Ajustes", "Open Settings"))
                                    .size(BtnSize::Sm)
                                    .icon(Icon::External)
                                    .build(
                                        "fda-open",
                                        cx.listener(|_, _, _, cx| cx.open_url(super::FULL_DISK_ACCESS_URL)),
                                    ),
                            )
                        }),
                ),
        )
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let labels: [SharedString; 3] = [
            t!("Sistema", "System").into(),
            t!("Claro", "Light").into(),
            t!("Oscuro", "Dark").into(),
        ];
        let selected = match self.ctx.config.appearance {
            Appearance::System => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        };
        let entity = cx.entity().downgrade();
        let languages: [SharedString; 3] = [
            t!("Sistema", "System").into(),
            "Español".into(),
            "English".into(),
        ];
        let language = match self.ctx.config.language {
            LanguageChoice::System => 0,
            LanguageChoice::Spanish => 1,
            LanguageChoice::English => 2,
        };
        let language_entity = cx.entity().downgrade();
        section(t!("Apariencia e idioma", "Appearance and language"))
            .child(setting_row(
                t!("Idioma", "Language"),
                t!(
                    "Sistema usa el primer idioma de macOS. Lo ya escaneado cambia al volver a escanear.",
                    "System uses your first macOS language. What was already scanned changes when you scan again."
                ),
                segmented("language", &languages, language, move |index, window, cx| {
                    let value = match index {
                        1 => LanguageChoice::Spanish,
                        2 => LanguageChoice::English,
                        _ => LanguageChoice::System,
                    };
                    language_entity
                        .update(cx, |shell, cx| shell.set_language_choice(value, window, cx))
                        .ok();
                })
                .into_any_element(),
            ))
            .child(setting_row(
            t!("Tema", "Theme"),
            t!(
                "Sistema sigue el modo claro u oscuro de macOS.",
                "System follows the light or dark mode of macOS."
            ),
            segmented("appearance", &labels, selected, move |index, window, cx| {
                let value = match index {
                    1 => Appearance::Light,
                    2 => Appearance::Dark,
                    _ => Appearance::System,
                };
                entity
                    .update(cx, |shell, cx| shell.set_appearance(value, window, cx))
                    .ok();
            })
            .into_any_element(),
        ))
    }

    fn render_project_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let days = self.ctx.config.project_idle_days;
        let min_large = self.ctx.config.large_file_min_bytes;
        let roots = self.ctx.config.project_roots.clone();
        let effective = self.ctx.config.effective_project_roots(&self.ctx.home);
        section(t!("Proyectos y archivos", "Projects and files"))
            .child(setting_row(
                t!("Un proyecto está parado tras", "A project is idle after"),
                t!(
                    "A partir de ahí se marca como parado en Proyectos.",
                    "After that, Projects marks it as idle."
                ),
                stepper(
                    "idle-days",
                    if days == 1 {
                        t!("1 día", "1 day").to_string()
                    } else {
                        tf!("{days} días", "{days} days")
                    },
                    cx.listener(|this, _, _, cx| {
                        this.update_config(cx, |config| {
                            config.project_idle_days =
                                config.project_idle_days.saturating_sub(1).max(1);
                        });
                        this.rebuild_project_rows();
                    }),
                    cx.listener(|this, _, _, cx| {
                        this.update_config(cx, |config| {
                            config.project_idle_days = (config.project_idle_days + 1).min(365);
                        });
                        this.rebuild_project_rows();
                    }),
                )
                .into_any_element(),
            ))
            .child(setting_row(
                t!("Un archivo es grande desde", "A file counts as large from"),
                t!(
                    "Desde ese tamaño aparece en Archivos. Se aplica en el siguiente escaneo.",
                    "From that size on, it shows up in Files. Applies on the next scan."
                ),
                stepper(
                    "large-file",
                    human_size(min_large),
                    cx.listener(|this, _, _, cx| {
                        this.update_config(cx, |config| {
                            config.large_file_min_bytes =
                                (config.large_file_min_bytes / 2).max(100_000_000);
                        });
                    }),
                    cx.listener(|this, _, _, cx| {
                        this.update_config(cx, |config| {
                            config.large_file_min_bytes =
                                (config.large_file_min_bytes * 2).min(64_000_000_000);
                        });
                    }),
                )
                .into_any_element(),
            ))
            .child(path_list(
                t!("Carpetas de proyectos", "Project folders"),
                if roots.is_empty() {
                    t!(
                        "Miramos en las carpetas habituales. Añade las tuyas para mirar solo en ellas.",
                        "We look in the usual folders. Add your own to look only in those."
                    )
                } else {
                    t!("Solo miramos en estas.", "We only look in these.")
                },
                "project-root",
                if roots.is_empty() { &effective } else { &roots },
                !roots.is_empty(),
                cx,
                |config, path| config.project_roots.retain(|root| root != path),
            ))
            .child(
                row().child(
                    Btn::secondary(t!("Añadir carpeta…", "Add folder…"))
                        .size(BtnSize::Sm)
                        .icon(Icon::Plus)
                        .build(
                            "add-root",
                            cx.listener(|this, _, _, cx| {
                                this.pick_paths(cx, |config, paths| {
                                    for path in paths {
                                        if !config.project_roots.contains(&path) {
                                            config.project_roots.push(path);
                                        }
                                    }
                                });
                            }),
                        ),
                ),
            )
    }

    fn render_excluded(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let excluded = self.ctx.config.excluded_paths.clone();
        section(t!("Nunca tocar", "Never touch"))
            .child(path_list(
                t!("Rutas excluidas", "Excluded paths"),
                if excluded.is_empty() {
                    t!(
                        "Nada excluido. Lo que añadas aquí queda fuera de cualquier limpieza, pase lo que pase.",
                        "Nothing excluded. Anything you add here is left out of every cleanup, no matter what."
                    )
                } else {
                    t!(
                        "Quedan fuera de cualquier limpieza, pase lo que pase.",
                        "These are left out of every cleanup, no matter what."
                    )
                },
                "excluded",
                &excluded,
                true,
                cx,
                |config, path| config.excluded_paths.retain(|item| item != path),
            ))
            .child(row().child(Btn::secondary(t!("Excluir una ruta…", "Exclude a path…")).size(BtnSize::Sm).icon(Icon::Plus).build(
                "add-excluded",
                cx.listener(|this, _, _, cx| {
                    this.pick_paths(cx, |config, paths| {
                        for path in paths {
                            if !config.excluded_paths.contains(&path) {
                                config.excluded_paths.push(path);
                            }
                        }
                    });
                }),
            )))
    }

    /// Asks macOS for folders, then applies them to the config.
    fn pick_paths(
        &mut self,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut hm_core::Config, Vec<PathBuf>) + 'static,
    ) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some(t!("Añadir", "Add").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                this.update(cx, |shell, cx| {
                    shell.update_config(cx, |config| apply(config, paths));
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_history(&self) -> impl IntoElement {
        let now = std::time::SystemTime::now();
        let entries = self.settings.history.data.as_deref().unwrap_or(&[]);
        section(t!("Historial de limpieza", "Cleanup history")).child(if entries.is_empty() {
            div()
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(match &self.settings.history.error {
                    Some(error) => tf!(
                        "No hemos podido leer el historial: {error}",
                        "We couldn't read the history: {error}"
                    ),
                    None => t!(
                        "Todavía no has limpiado nada. Aquí quedará constancia de todo lo que hagas.",
                        "You haven't cleaned anything yet. Everything you do will be recorded here."
                    )
                    .to_string(),
                })
                .into_any_element()
        } else {
            let trashed: u64 = entries.iter().map(|entry| entry.outcome.trashed_bytes).sum();
            let freed: u64 = entries.iter().map(|entry| entry.outcome.freed_bytes).sum();
            col()
                .gap(sp(2.))
                .child(
                    div()
                        .fs(font::CAPTION)
                        .text_color(theme::text_2())
                        .child(tf!(
                            "{}: {} liberados y {} enviados a la Papelera.",
                            "{}: {} freed and {} moved to the Trash.",
                            plural(entries.len(), t!("acción", "action"), t!("acciones", "actions")),
                            human_size(freed),
                            human_size(trashed)
                        )),
                )
                .child(
                    col().children(entries.iter().take(60).enumerate().map(|(index, entry)| {
                        let (label, color) = match entry.outcome.status {
                            CleanStatus::Done | CleanStatus::WouldClean => (t!("Hecho", "Done"), theme::green()),
                            CleanStatus::Partial => (t!("A medias", "Partial"), theme::amber()),
                            CleanStatus::Skipped => (t!("Omitido", "Skipped"), theme::text_3()),
                            CleanStatus::Failed => (t!("Falló", "Failed"), theme::rose()),
                        };
                        let bytes = entry.outcome.freed_bytes + entry.outcome.trashed_bytes;
                        row()
                            .id(ElementId::NamedInteger("history".into(), index as u64))
                            .h(px(32.))
                            .gap(sp(2.5))
                            .border_b_1()
                            .border_color(theme::hairline())
                            .child(crate::ui::clip().flex_1().fs(font::CAPTION).child(entry.title.clone()))
                            .child(pill(label, color))
                            .child(
                                div()
                                    .w(px(88.))
                                    .text_right()
                                    .fs(font::CAPTION)
                                    .whitespace_nowrap()
                                    .text_color(theme::text_3())
                                    .child(human_age(entry.at, now)),
                            )
                            .child(
                                div()
                                    .w(px(64.))
                                    .text_right()
                                    .fs(font::CAPTION)
                                    .tabular()
                                    .child(if bytes > 0 { human_size(bytes) } else { "—".to_string() }),
                            )
                    })),
                )
                .into_any_element()
        })
    }
}

fn section(title: &'static str) -> gpui::Div {
    col().w_full().gap(sp(2.5)).child(section_title(title))
}

fn setting_row(title: &'static str, detail: &'static str, control: AnyElement) -> impl IntoElement {
    row()
        .w_full()
        .gap(sp(4.))
        .py(sp(1.))
        .child(
            col()
                .flex_1()
                .min_w(px(0.))
                .child(div().fs(font::BODY).child(title))
                .child(
                    div()
                        .fs(font::CAPTION)
                        .text_color(theme::text_2())
                        .child(detail),
                ),
        )
        .child(control)
}

fn path_list(
    title: &'static str,
    detail: &'static str,
    id: &'static str,
    paths: &[PathBuf],
    removable: bool,
    cx: &mut Context<Shell>,
    remove: impl Fn(&mut hm_core::Config, &PathBuf) + Clone + 'static,
) -> impl IntoElement {
    col()
        .w_full()
        .gap(sp(1.5))
        .mt(sp(1.))
        .child(
            col().child(div().fs(font::BODY).child(title)).child(
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(detail),
            ),
        )
        .when(!paths.is_empty(), |this| {
            this.child(
                card()
                    .w_full()
                    .py(sp(1.))
                    .children(paths.iter().enumerate().map(|(index, path)| {
                        let target = path.clone();
                        let remove = remove.clone();
                        row()
                            .h(px(32.))
                            .px(sp(2.5))
                            .gap(sp(2.))
                            .child(icon(Icon::Folder, 14.0, theme::text_3()))
                            .child(div().flex_1().min_w(px(0.)).child(path_label(
                                path.display().to_string(),
                                font::CAPTION,
                                theme::text_1(),
                            )))
                            .when(removable, |this| {
                                this.child(icon_button(
                                    ElementId::NamedInteger(id.into(), index as u64),
                                    Icon::X,
                                    theme::text_3(),
                                    cx.listener(move |this, _, _, cx| {
                                        let remove = remove.clone();
                                        let target = target.clone();
                                        this.update_config(cx, move |config| {
                                            remove(config, &target)
                                        });
                                    }),
                                ))
                            })
                    })),
            )
        })
}
