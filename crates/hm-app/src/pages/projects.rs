//! Development projects and the build artifacts they carry around.

use gpui::{
    div, list, prelude::*, px, AnyElement, Context, ElementId, FontWeight, SharedString, Window,
};
use hm_core::fmt::{human_age, human_size};
use hm_core::{t, tf, Area, Finding, Recovery, Risk};
use hm_dev::types::{DockerSummary, DockerUsage, ProjectInfo, ProjectKind};

use super::{content_width, plural};
use crate::assets::Icon;
use crate::state::{Page, ProjectFilter, Shell};
use crate::theme::{self, font, radius, sp, GUTTER};
use crate::ui::chrome::{
    big_number, card, empty_state, eyebrow, notice, page_header, section_title, NoticeKind,
};
use crate::ui::controls::{dot, live_dot, pill, segmented, spinner, tag, Btn, BtnSize};
use crate::ui::label::path_label;
use crate::ui::scrollbar::{scrollbar, ScrollSource};
use crate::ui::{clip, col, icon, row, TextExt};

const ACCENT_AREA: Area = Area::Developer;

fn kind_label(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::Node => "Node",
        ProjectKind::Rust => "Rust",
        ProjectKind::Python => "Python",
        ProjectKind::Swift => "Swift",
        ProjectKind::Xcode => "Xcode",
        ProjectKind::Go => "Go",
        ProjectKind::Gradle => "Gradle",
        ProjectKind::Maven => "Maven",
        ProjectKind::Flutter => "Flutter",
        ProjectKind::Ruby => "Ruby",
        ProjectKind::Php => "PHP",
        ProjectKind::DotNet => ".NET",
        ProjectKind::CMake => "CMake",
        ProjectKind::Other => t!("Otro", "Other"),
    }
}

/// One finding per artifact. A scan's finding knows more about it (how exactly it comes back), so
/// it wins when there is one. The fallback keeps the scanner's id scheme, so that cleaning from here
/// also clears it from the Developer page.
pub fn artifact_findings(project: &ProjectInfo, scanned: &[Finding]) -> Vec<Finding> {
    project
        .artifacts
        .iter()
        .map(|artifact| {
            let known = scanned
                .iter()
                .find(|finding| finding.target_paths() == [artifact.path.as_path()]);
            match known {
                Some(finding) => finding.clone(),
                None => Finding::new(
                    "dev.projects",
                    ACCENT_AREA,
                    artifact.path.display().to_string(),
                    format!("{} · {}", project.name, artifact.label),
                )
                .group(project.name.clone())
                .trash(&artifact.path)
                .explanation(tf!(
                    "{} de {}. Se vuelve a generar la próxima vez que instales dependencias o compiles.",
                    "{} in {}. It comes back the next time you install dependencies or build.",
                    artifact.label, project.name
                ))
                .size(artifact.size)
                .risk(Risk::Moderate)
                .recovery(Recovery::Rebuild),
            }
        })
        .collect()
}

impl Shell {
    pub fn render_projects(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let loading = self.projects.projects.loading;
        let action: AnyElement = if loading {
            row()
                .gap(sp(2.))
                .h(px(32.))
                .child(spinner("projects-spin", 14.0, theme::text_2()))
                .child(
                    div()
                        .fs(font::CAPTION)
                        .text_color(theme::text_2())
                        .child(t!("Buscando…", "Searching…")),
                )
                .into_any_element()
        } else {
            Btn::secondary(t!("Actualizar", "Refresh"))
                .icon(Icon::Refresh)
                .build(
                    "projects-reload",
                    cx.listener(|this, _, _, cx| this.load_projects(cx)),
                )
                .into_any_element()
        };
        let header = page_header(
            t!("Proyectos", "Projects"),
            t!(
                "Lo que tus proyectos guardan y puede volver a generarse. El código no se toca.",
                "What your projects keep that can be generated again. Your code is never touched."
            ),
            vec![action],
        );

        let projects = self.projects.projects.data.as_deref();
        let body: AnyElement = match projects {
            None => {
                if let Some(error) = self.projects.projects.error.clone() {
                    div()
                        .px(px(GUTTER))
                        .child(notice(
                            NoticeKind::Error,
                            t!(
                                "No hemos podido buscar tus proyectos",
                                "Couldn't search for your projects"
                            ),
                            Some(error.into()),
                            None,
                        ))
                        .into_any_element()
                } else {
                    empty_state(
                        Icon::Folder,
                        theme::text_2(),
                        t!("Buscando proyectos", "Looking for projects"),
                        t!(
                            "Recorremos tus carpetas de código midiendo node_modules, target y compañía.",
                            "Going through your code folders, measuring node_modules, target and the like."
                        ),
                        None,
                    )
                    .into_any_element()
                }
            }
            Some(_) => {
                let state = self.projects.list.clone();
                let bar = self.projects.bar.clone();
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        list(
                            state.clone(),
                            cx.processor(|this: &mut Shell, index: usize, window, cx| {
                                this.render_project_row(index, window, cx)
                            }),
                        )
                        .size_full(),
                    )
                    .child(scrollbar(ScrollSource::List(state), bar))
                    .into_any_element()
            }
        };
        col()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_project_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match index {
            0 => self.render_docker(cx).into_any_element(),
            1 => self.render_projects_toolbar(cx).into_any_element(),
            _ => {
                let Some(&project_index) = self.projects.order.get(index - 2) else {
                    return div().h(px(16.)).into_any_element();
                };
                let Some(project) = self
                    .projects
                    .projects
                    .data
                    .as_ref()
                    .and_then(|projects| projects.get(project_index))
                    .cloned()
                else {
                    return div().into_any_element();
                };
                self.render_project(index, &project, window, cx)
                    .into_any_element()
            }
        }
    }

    fn render_projects_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let total: u64 = self
            .projects
            .order
            .iter()
            .filter_map(|&index| self.projects.projects.data.as_ref()?.get(index))
            .map(|project| project.artifact_bytes)
            .fold(0, u64::saturating_add);
        let idle_days = self.ctx.config.project_idle_days;
        let labels: Vec<SharedString> = vec![
            t!("Todos", "All").into(),
            tf!("Parados {idle_days}+ días", "Idle {idle_days}+ days").into(),
        ];
        let selected = usize::from(self.projects.filter == ProjectFilter::Idle);
        let entity = cx.entity().downgrade();
        let count = self.projects.order.len();
        let empty = count == 0;
        col()
            .w_full()
            .pt(sp(3.))
            .child(
                row()
                    .px(px(GUTTER))
                    .pb(sp(2.5))
                    .gap(sp(3.5))
                    .items_end()
                    .child(if total == 0 {
                        big_number(t!("Nada", "Nothing"), font::DISPLAY, theme::text_3())
                    } else {
                        big_number(human_size(total), font::DISPLAY, theme::text_1())
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .pb(px(5.))
                            .fs(font::CAPTION)
                            .text_color(theme::text_2())
                            .child(tf!(
                                "en artefactos, {}",
                                "in build artifacts, {}",
                                plural(count, t!("proyecto", "project"), t!("proyectos", "projects"))
                            )),
                    )
                    .child(div().pb(sp(1.)).child(segmented(
                        "project-filter",
                        &labels,
                        selected,
                        move |index, _, cx| {
                            entity
                                .update(cx, |shell, cx| {
                                    shell.projects.filter =
                                        if index == 0 { ProjectFilter::All } else { ProjectFilter::Idle };
                                    shell.rebuild_project_rows();
                                    cx.notify();
                                })
                                .ok();
                        },
                    ))),
            )
            .child(div().h(px(1.)).mx(px(GUTTER)).bg(theme::hairline()))
            .when(empty, |this| {
                this.child(
                    div()
                        .px(px(GUTTER))
                        .py(sp(6.))
                        .fs(font::BODY)
                        .text_color(theme::text_2())
                        .child(if self.projects.filter == ProjectFilter::Idle {
                            t!(
                                "Ningún proyecto lleva tanto tiempo parado.",
                                "No project has been idle that long."
                            )
                        } else {
                            t!(
                                "No hemos encontrado proyectos. Si tu código vive en otra carpeta, añádela en Ajustes.",
                                "No projects found. If your code lives in another folder, add it in Settings."
                            )
                        }),
                )
            })
    }

    fn render_project(
        &self,
        row_index: usize,
        project: &ProjectInfo,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.projects.expanded.contains(&project.path);
        let wide = content_width(window) > 860.0;
        let idle_days = self.ctx.config.project_idle_days as u64;
        let idle = project.idle_days.is_some_and(|days| days >= idle_days);
        let path = project.path.clone();
        let target = project.clone();
        let list = self.projects.list.clone();
        let now = std::time::SystemTime::now();
        col().w_full().px(px(GUTTER - 8.0)).child(
            col()
                .w_full()
                .rounded(px(radius::MD))
                .when(expanded, |this| this.bg(theme::surface_1()))
                .child(
                    row()
                        .id(ElementId::NamedInteger("project".into(), row_index as u64))
                        .min_h(px(54.))
                        .px(sp(2.))
                        .gap(sp(3.))
                        .rounded(px(radius::MD))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_1()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.projects.expanded.remove(&path) {
                                this.projects.expanded.insert(path.clone());
                            }
                            list.splice(row_index..row_index + 1, 1);
                            cx.notify();
                        }))
                        .child(icon(
                            if expanded {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            },
                            13.0,
                            theme::text_3(),
                        ))
                        .child(
                            col()
                                .flex_1()
                                .min_w(px(0.))
                                .gap(px(1.))
                                .child(
                                    row()
                                        .gap(sp(2.))
                                        .child(
                                            clip()
                                                .fs(font::BODY)
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(project.name.clone()),
                                        )
                                        .children(
                                            project
                                                .kinds
                                                .iter()
                                                .take(3)
                                                .map(|kind| tag(kind_label(*kind))),
                                        )
                                        .when_some(project.git.as_ref(), |this, git| {
                                            this.child(
                                                row()
                                                    .flex_none()
                                                    .gap(sp(1.))
                                                    .fs(font::MICRO)
                                                    .text_color(theme::text_3())
                                                    .child(icon(
                                                        Icon::Branch,
                                                        12.0,
                                                        theme::text_3(),
                                                    ))
                                                    .when_some(
                                                        git.branch.clone(),
                                                        |this, branch| this.child(branch),
                                                    )
                                                    .when(!git.status_known, |this| {
                                                        this.child(t!(
                                                            "estado desconocido",
                                                            "status unknown"
                                                        ))
                                                    })
                                                    .when(git.dirty, |this| {
                                                        this.child(dot(theme::amber(), 6.0))
                                                    })
                                                    .when_some(
                                                        git.unpushed_commits
                                                            .filter(|count| *count > 0),
                                                        |this, count| {
                                                            this.child(
                                                                row()
                                                                    .text_color(theme::text_2())
                                                                    .child(icon(
                                                                        Icon::ArrowUp,
                                                                        11.0,
                                                                        theme::text_2(),
                                                                    ))
                                                                    .child(count.to_string()),
                                                            )
                                                        },
                                                    ),
                                            )
                                        }),
                                )
                                .child(path_label(
                                    project.path.display().to_string(),
                                    font::MICRO,
                                    theme::text_3(),
                                )),
                        )
                        .when(wide || idle, |this| {
                            this.child(
                                div().flex_none().w(px(92.)).text_right().child(
                                    div()
                                        .fs(font::CAPTION)
                                        .whitespace_nowrap()
                                        .text_color(if idle {
                                            theme::amber()
                                        } else {
                                            theme::text_3()
                                        })
                                        .child(
                                            project
                                                .last_activity
                                                .map_or("—".to_string(), |when| {
                                                    human_age(when, now)
                                                }),
                                        ),
                                ),
                            )
                        })
                        .child(
                            div()
                                .flex_none()
                                .w(px(72.))
                                .text_right()
                                .fs(font::BODY)
                                .font_weight(FontWeight::MEDIUM)
                                .tabular()
                                .child(if project.artifact_bytes > 0 {
                                    human_size(project.artifact_bytes)
                                } else {
                                    "—".to_string()
                                }),
                        )
                        .child(
                            Btn::secondary(t!("Limpiar", "Clean"))
                                .size(BtnSize::Sm)
                                .disabled(project.artifacts.is_empty())
                                .build(
                                    ElementId::NamedInteger(
                                        "project-clean".into(),
                                        row_index as u64,
                                    ),
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        let items = artifact_findings(&target, &this.findings);
                                        this.open_review(
                                            tf!("Limpiar {}", "Clean {}", target.name),
                                            t!("Limpiar artefactos", "Clean build artifacts"),
                                            items,
                                            cx,
                                        );
                                    }),
                                ),
                        ),
                )
                .when(expanded, |this| this.child(project_details(project))),
        )
    }

    fn render_docker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let summary: Option<&DockerSummary> =
            self.projects.docker.data.as_ref().and_then(|d| d.as_ref());
        div()
            .w_full()
            .px(px(GUTTER))
            .when_some(summary, |this, docker| {
                let reclaimable = docker.images.reclaimable
                    + docker.containers.reclaimable
                    + docker.volumes.reclaimable
                    + docker.build_cache.reclaimable;
                let image_note = if docker.disk_image_readable && docker.disk_image_bytes > 0 {
                    Some(tf!(
                        "La imagen de disco de la máquina virtual ocupa {}, a menudo bastante más de lo que Docker declara.",
                        "The virtual machine's disk image takes {}, often a lot more than Docker reports.",
                        human_size(docker.disk_image_bytes)
                    ))
                } else {
                    docker.disk_image_note.clone()
                };
                this.child(
                    card()
                        .w_full()
                        .p(sp(3.5))
                        .child(
                            row()
                                .gap(sp(2.5))
                                .mb(sp(2.5))
                                .child(icon(Icon::Container, 16.0, theme::text_2()))
                                .child(section_title(docker.runtime.clone()))
                                .child(if docker.daemon_running {
                                    row()
                                        .gap(sp(1.5))
                                        .child(live_dot(theme::green()))
                                        .child(div().fs(font::CAPTION).text_color(theme::text_2()).child(t!("Activo", "Running")))
                                        .into_any_element()
                                } else {
                                    div()
                                        .fs(font::CAPTION)
                                        .text_color(theme::text_3())
                                        .child(t!("Parado", "Stopped"))
                                        .into_any_element()
                                })
                                .child(div().flex_1())
                                .when(reclaimable > 0, |this| {
                                    this.child(
                                        Btn::ghost(tf!("{} recuperables en Developer", "{} reclaimable in Developer", human_size(reclaimable)))
                                            .size(BtnSize::Sm)
                                            .build(
                                                "docker-goto",
                                                cx.listener(|this, _, _, cx| {
                                                    this.navigate(Page::Area(Area::Developer), cx)
                                                }),
                                            ),
                                    )
                                }),
                        )
                        .when(docker.usage_known, |this| {
                            this.child(
                                row()
                                    .w_full()
                                    .gap(sp(2.5))
                                    .child(docker_stat(t!("Imágenes", "Images"), &docker.images))
                                    .child(docker_stat(t!("Contenedores", "Containers"), &docker.containers))
                                    .child(docker_stat(t!("Volúmenes", "Volumes"), &docker.volumes))
                                    .child(docker_stat(t!("Caché de build", "Build cache"), &docker.build_cache)),
                            )
                        })
                        .when(!docker.usage_known, |this| {
                            this.child(
                                div()
                                    .fs(font::CAPTION)
                                    .text_color(theme::text_2())
                                    .child(t!(
                                        "Con el motor parado no sabemos cuánto ocupa cada cosa. Arráncalo y actualiza para verlo.",
                                        "With the engine stopped we can't tell how much each part takes. Start it and refresh to see."
                                    )),
                            )
                        })
                        .when_some(image_note, |this, note| {
                            this.child(
                                div()
                                    .mt(sp(2.5))
                                    .fs(font::CAPTION)
                                    .text_color(theme::text_2())
                                    .child(note),
                            )
                        }),
                )
            })
    }
}

fn docker_stat(label: &'static str, usage: &DockerUsage) -> impl IntoElement {
    col()
        .flex_1()
        .min_w(px(0.))
        .gap(px(1.))
        .child(eyebrow(label))
        .child(big_number(
            human_size(usage.bytes),
            font::HEADLINE,
            theme::text_1(),
        ))
        .child(
            clip()
                .fs(font::MICRO)
                .tabular()
                .text_color(theme::text_3())
                .child(if usage.reclaimable > 0 {
                    tf!(
                        "{} · {} libres",
                        "{} · {} reclaimable",
                        usage.count,
                        human_size(usage.reclaimable)
                    )
                } else {
                    tf!("{} en total", "{} in total", usage.count)
                }),
        )
}

fn project_details(project: &ProjectInfo) -> impl IntoElement {
    col()
        .pl(px(33.))
        .pr(sp(3.))
        .pb(sp(3.))
        .gap(sp(1.))
        .when_some(project.git.as_ref(), |this, git| {
            this.when(git.dirty || git.unpushed_commits.unwrap_or(0) > 0, |this| {
                this.child(
                    row()
                        .mb(sp(1.))
                        .gap(sp(1.5))
                        .when(git.dirty, |this| {
                            this.child(pill(
                                t!("Cambios sin confirmar", "Uncommitted changes"),
                                theme::amber(),
                            ))
                        })
                        .when(git.unpushed_commits.unwrap_or(0) > 0, |this| {
                            this.child(pill(
                                plural(
                                    git.unpushed_commits.unwrap_or(0) as usize,
                                    t!("commit sin subir", "unpushed commit"),
                                    t!("commits sin subir", "unpushed commits"),
                                ),
                                theme::text_2(),
                            ))
                        }),
                )
            })
            .when(git.git_dir_bytes > 0, |this| {
                this.child(
                    div()
                        .fs(font::CAPTION)
                        .text_color(theme::text_3())
                        .child(tf!(
                            "Historial de git: {} (no se toca)",
                            "Git history: {} (not touched)",
                            human_size(git.git_dir_bytes)
                        )),
                )
            })
        })
        .when(project.artifacts.is_empty(), |this| {
            this.child(
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(t!(
                        "Este proyecto no tiene artefactos que limpiar.",
                        "This project has no build artifacts to clean."
                    )),
            )
        })
        .children(project.artifacts.iter().map(|artifact| {
            row()
                .h(px(24.))
                .gap(sp(2.5))
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .fs(font::CAPTION)
                        .child(artifact.label.clone()),
                )
                .child(div().flex_1().min_w(px(0.)).child(path_label(
                    artifact.path.display().to_string(),
                    font::MICRO,
                    theme::text_3(),
                )))
                .child(
                    div()
                        .flex_none()
                        .fs(font::CAPTION)
                        .tabular()
                        .text_color(theme::text_2())
                        .child(human_size(artifact.size)),
                )
        }))
}
