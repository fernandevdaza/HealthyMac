//! Installed apps: what they weigh with their leftovers, and how to get rid of them properly.

use gpui::{
    div, img, prelude::*, px, uniform_list, AnyElement, Context, ElementId, FontWeight, ObjectFit,
    SharedString, StyledImage, Window,
};
use hm_core::fmt::{human_age, human_size};
use hm_core::{t, tf, Area};
use hm_system::types::AppArch;

use super::{content_width, gutter_row, plural};
use crate::assets::Icon;
use crate::state::{AppSort, AppsTab, Shell};
use crate::theme::{self, font, radius, sp, GUTTER};
use crate::ui::chrome::{empty_state, notice, page_header, NoticeKind};
use crate::ui::controls::{dot, pill, segmented, tag, Btn, ROW_GROUP};
use crate::ui::scrollbar::{scrollbar, ScrollSource};
use crate::ui::{clip, col, row, TextExt};

const ROW_HEIGHT: f32 = 56.0;

impl Shell {
    pub fn render_apps(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let leftovers_count = self
            .findings
            .iter()
            .filter(|finding| finding.area == Area::Apps)
            .count();
        let tab_labels: Vec<SharedString> = vec![
            t!("Instaladas", "Installed").into(),
            if leftovers_count > 0 {
                tf!(
                    "Restos ({leftovers_count})",
                    "Leftovers ({leftovers_count})"
                )
                .into()
            } else {
                t!("Restos", "Leftovers").into()
            },
        ];
        let tab = usize::from(self.apps.tab == AppsTab::Leftovers);
        let tab_entity = cx.entity().downgrade();
        let tabs = segmented("apps-tab", &tab_labels, tab, move |index, _, cx| {
            tab_entity
                .update(cx, |shell, cx| {
                    shell.apps.tab = if index == 0 {
                        AppsTab::Installed
                    } else {
                        AppsTab::Leftovers
                    };
                    cx.notify();
                })
                .ok();
        })
        .into_any_element();

        let count = self.apps.apps.data.as_ref().map_or(0, Vec::len);
        let subtitle = if self.apps.tab == AppsTab::Leftovers {
            t!(
                "Lo que dejaron atrás las apps que ya no tienes.",
                "What apps you no longer have left behind."
            )
            .to_string()
        } else if self.apps.apps.data.is_some() {
            tf!(
                "{} en este Mac. Desinstalar también se lleva sus restos.",
                "{} on this Mac. Uninstalling also removes their leftovers.",
                plural(count, "app", "apps")
            )
        } else {
            t!(
                "Las apps de este Mac y lo que pesan de verdad, con sus restos.",
                "The apps on this Mac and their real size, leftovers included."
            )
            .to_string()
        };
        let mut actions = vec![tabs];
        if self.apps.tab == AppsTab::Installed && self.apps.apps.data.is_some() {
            let sort_labels: Vec<SharedString> = vec![
                t!("Tamaño", "Size").into(),
                t!("Último uso", "Last used").into(),
                t!("Nombre", "Name").into(),
            ];
            let selected = match self.apps.sort {
                AppSort::Size => 0,
                AppSort::LastUsed => 1,
                AppSort::Name => 2,
            };
            let entity = cx.entity().downgrade();
            actions.push(
                segmented("apps-sort", &sort_labels, selected, move |index, _, cx| {
                    entity
                        .update(cx, |shell, cx| {
                            shell.apps.sort = match index {
                                0 => AppSort::Size,
                                1 => AppSort::LastUsed,
                                _ => AppSort::Name,
                            };
                            shell.sort_apps();
                            cx.notify();
                        })
                        .ok();
                })
                .into_any_element(),
            );
        }
        let header = page_header("Apps", subtitle, actions);

        let body: AnyElement = if self.apps.tab == AppsTab::Leftovers {
            self.render_findings_body(Area::Apps, window, cx)
        } else if let (None, Some(error)) = (&self.apps.apps.data, &self.apps.apps.error) {
            div()
                .px(px(GUTTER))
                .child(notice(
                    NoticeKind::Error,
                    t!(
                        "No hemos podido leer tus apps",
                        "We couldn't read your apps"
                    ),
                    Some(error.clone().into()),
                    None,
                ))
                .into_any_element()
        } else if self.apps.apps.data.is_none() {
            empty_state(
                Icon::Grid,
                theme::text_2(),
                t!("Pesando tus apps", "Measuring your apps"),
                t!(
                    "Medimos cada app y buscamos lo que tiene repartido por la Biblioteca. Puede tardar un poco.",
                    "We measure each app and look for what it has spread across the Library. This can take a little while."
                ),
                None,
            )
            .into_any_element()
        } else if count == 0 {
            empty_state(
                Icon::Grid,
                theme::text_2(),
                t!("No hemos encontrado apps", "We found no apps"),
                t!(
                    "Qué raro. Si tus apps están fuera de /Applications, no las vemos.",
                    "That's odd. If your apps are outside /Applications, we can't see them."
                ),
                None,
            )
            .into_any_element()
        } else {
            let handle = self.apps.scroll.clone();
            let bar = self.apps.bar.clone();
            let wide = content_width(window) > 860.0;
            col()
                .flex_1()
                .min_h(px(0.))
                .child(
                    row()
                        .flex_none()
                        .h(px(26.))
                        .mx(px(GUTTER))
                        .gap(sp(3.))
                        .border_b_1()
                        .border_color(theme::hairline())
                        .fs(font::MICRO)
                        .text_color(theme::text_3())
                        .child(div().flex_1().min_w(px(0.)).child("App"))
                        .child(div().w(px(116.)).child(t!("Arquitectura", "Architecture")))
                        .when(wide, |this| {
                            this.child(div().w(px(92.)).child(t!("Último uso", "Last used")))
                        })
                        .child(div().w(px(110.)).text_right().child(t!("Tamaño", "Size")))
                        .child(div().w(px(94.))),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(
                            uniform_list(
                                "apps-list",
                                count,
                                cx.processor(
                                    move |this: &mut Shell,
                                          range: std::ops::Range<usize>,
                                          _window,
                                          cx| {
                                        range
                                            .map(|index| this.render_app(index, wide, cx))
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(handle.clone())
                            .size_full(),
                        )
                        .child(scrollbar(ScrollSource::Uniform(handle), bar)),
                )
                .into_any_element()
        };
        col()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_app(&mut self, index: usize, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(app) = self
            .apps
            .order
            .get(index)
            .and_then(|&position| self.apps.apps.data.as_ref()?.get(position))
            .cloned()
        else {
            return div().into_any_element();
        };
        let now = std::time::SystemTime::now();
        let leftovers: u64 = app.leftovers.iter().map(|l| l.size).sum();
        let icon_image = app
            .icon_path
            .as_ref()
            .and_then(|path| self.apps.icons.get(path))
            .cloned();
        let initial = app
            .name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string();
        let target = app.clone();
        gutter_row()
            .child(
                row()
                    .id(ElementId::NamedInteger("app".into(), index as u64))
                    .group(ROW_GROUP)
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .px(sp(2.))
                    .gap(sp(3.))
                    .rounded(px(radius::MD))
                    .hover(|style| style.bg(theme::surface_1()))
                    .child(match icon_image {
                        Some(image) => img(image)
                            .flex_none()
                            .size(px(36.))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element(),
                        None => row()
                            .flex_none()
                            .size(px(36.))
                            .justify_center()
                            .rounded(px(9.))
                            .bg(theme::surface_2())
                            .border_1()
                            .border_color(theme::hairline())
                            .fs(font::SUBHEAD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_2())
                            .child(initial)
                            .into_any_element(),
                    })
                    .child(
                        col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                row()
                                    .gap(px(7.))
                                    .child(
                                        clip()
                                            .fs(font::BODY)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(app.name.clone()),
                                    )
                                    .when(app.running, |this| {
                                        this.child(
                                            row()
                                                .flex_none()
                                                .gap(sp(1.))
                                                .fs(font::MICRO)
                                                .text_color(theme::green())
                                                .child(dot(theme::green(), 6.0))
                                                .child(t!("Abierta", "Open")),
                                        )
                                    }),
                            )
                            .child(clip().fs(font::MICRO).text_color(theme::text_3()).child(
                                match (&app.uninstall_blocked, &app.version) {
                                    (Some(reason), _) if !app.is_system => reason.clone(),
                                    (_, Some(version)) => {
                                        tf!("Versión {version}", "Version {version}")
                                    }
                                    _ => String::new(),
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .w(px(116.))
                            .child(arch_pill(app.arch)),
                    )
                    .when(wide, |this| {
                        this.child(
                            clip()
                                .flex_none()
                                .w(px(104.))
                                .fs(font::CAPTION)
                                .text_color(theme::text_3())
                                .child(
                                    app.last_used
                                        .map_or("—".to_string(), |when| human_age(when, now)),
                                ),
                        )
                    })
                    .child(
                        col()
                            .flex_none()
                            .w(px(110.))
                            .items_end()
                            .child(
                                div()
                                    .fs(font::BODY)
                                    .font_weight(FontWeight::MEDIUM)
                                    .tabular()
                                    .child(human_size(app.size + leftovers)),
                            )
                            .when(leftovers >= 1_000_000, |this| {
                                this.child(
                                    div()
                                        .fs(font::MICRO)
                                        .tabular()
                                        .text_color(theme::text_3())
                                        .child(tf!(
                                            "{} de restos",
                                            "{} of leftovers",
                                            human_size(leftovers)
                                        )),
                                )
                            }),
                    )
                    .child(div().flex_none().w(px(94.)).flex().justify_end().child(
                        if app.is_system {
                            tag(t!("Del sistema", "System")).into_any_element()
                        } else if app.uninstall_blocked.is_some() {
                            tag(t!("No se puede", "Blocked")).into_any_element()
                        } else {
                            Btn::row_danger(t!("Desinstalar", "Uninstall"))
                                .build(
                                    ElementId::NamedInteger("app-uninstall".into(), index as u64),
                                    cx.listener(move |this, _, _, cx| {
                                        this.review_uninstall(target.clone(), cx)
                                    }),
                                )
                                .into_any_element()
                        },
                    )),
            )
            .into_any_element()
    }
}

fn arch_pill(arch: AppArch) -> AnyElement {
    match arch {
        AppArch::Intel => pill("Intel · Rosetta", theme::rose()).into_any_element(),
        AppArch::AppleSilicon => tag("Apple Silicon").into_any_element(),
        AppArch::Universal => tag("Universal").into_any_element(),
        AppArch::Unknown => div()
            .fs(font::CAPTION)
            .text_color(theme::text_3())
            .child("—")
            .into_any_element(),
    }
}
