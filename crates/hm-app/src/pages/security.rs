//! Security: how protected this Mac is and what runs by itself. It explains, it never removes.

use gpui::{div, prelude::*, px, AnyElement, Context, ElementId, FontWeight, Hsla, Window};
use hm_core::{t, tf};
use hm_system::security::{Check, Level, StartupReview};

use super::{plural, scroll_pane};
use crate::assets::Icon;
use crate::state::Shell;
use crate::theme::{self, font, sp, GUTTER};
use crate::ui::chrome::{empty_state, notice, page_header, section_title, NoticeKind};
use crate::ui::controls::{pill, tag, Btn, BtnSize, ROW_GROUP};
use crate::ui::label::path_label;
use crate::ui::{col, icon, row, TextExt};

fn mark(level: Level) -> (Icon, Hsla) {
    match level {
        Level::Ok => (Icon::Check, theme::green()),
        Level::Info => (Icon::Info, theme::text_3()),
        Level::Review => (Icon::Alert, theme::amber()),
        Level::Risk => (Icon::Alert, theme::rose()),
    }
}

/// Words for the two levels that ask for attention. The others need no label: the icon says it.
fn level_pill(level: Level) -> Option<AnyElement> {
    match level {
        Level::Review => Some(pill(t!("Revisar", "Review"), theme::amber()).into_any_element()),
        Level::Risk => Some(pill(t!("Riesgo", "Risk"), theme::rose()).into_any_element()),
        Level::Ok | Level::Info => None,
    }
}

impl Shell {
    pub fn render_security(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let report = self.security.report.data.as_ref();
        let subtitle = match report {
            None => t!(
                "Cómo de protegido está tu Mac y qué se ejecuta solo.",
                "How protected your Mac is and what runs by itself."
            )
            .to_string(),
            Some(report) => match report.attention() {
                0 => t!(
                    "Todo en orden: nada que revisar.",
                    "All good: nothing to review."
                )
                .to_string(),
                count => tf!(
                    "{} que conviene revisar.",
                    "{} worth reviewing.",
                    plural(count, t!("Una cosa", "One thing"), t!("cosas", "things"))
                ),
            },
        };
        let refresh = Btn::secondary(t!("Volver a comprobar", "Check again"))
            .size(BtnSize::Sm)
            .icon(Icon::Refresh)
            .disabled(self.security.report.loading)
            .build(
                "security-refresh",
                cx.listener(|this, _, _, cx| this.load_security(cx)),
            )
            .into_any_element();
        let header = page_header(
            t!("Seguridad", "Security"),
            subtitle,
            report.is_some().then_some(refresh),
        );

        let body: AnyElement = match (report, &self.security.report.error) {
            (None, Some(error)) => div()
                .px(px(GUTTER))
                .child(notice(
                    NoticeKind::Error,
                    t!("No hemos podido comprobarlo", "We couldn't check"),
                    Some(error.clone().into()),
                    None,
                ))
                .into_any_element(),
            (None, None) => empty_state(
                Icon::Shield,
                theme::text_2(),
                t!("Comprobando la seguridad", "Checking security"),
                t!(
                    "Leemos los ajustes de macOS y la firma de lo que arranca solo. Unos segundos.",
                    "Reading macOS settings and the signature of what starts by itself. A few seconds."
                ),
                None,
            )
            .into_any_element(),
            (Some(report), _) => {
                let width = (super::content_width(window) - GUTTER * 2.0).min(760.0);
                let pane = self.security.scroll.clone();
                let startup_detail = tf!(
                    "{}. Lo firmado por un desarrollador identificado es lo esperable; lo demás dice por qué merece una mirada.",
                    "{}. Anything signed by an identified developer is what you'd expect; the rest says why it's worth a look.",
                    plural(report.startup.len(), t!("elemento", "item"), t!("elementos", "items"))
                );
                scroll_pane(
                    "security-scroll",
                    &pane,
                    col()
                        .w(px(width))
                        .ml(px(GUTTER))
                        .pb(sp(8.))
                        .gap(sp(7.))
                        .child(
                            div()
                                .fs(font::CAPTION)
                                .text_color(theme::text_2())
                                .child(t!(
                                    "Esto no es un antivirus: macOS ya busca malware conocido con XProtect. Aquí ves lo que conviene revisar con tus propios ojos, y no se toca nada.",
                                    "This isn't an antivirus: macOS already looks for known malware with XProtect. Here you see what's worth checking with your own eyes, and nothing is touched."
                                )),
                        )
                        .child(section(
                            t!("Protecciones de macOS", "macOS protections"),
                            None,
                            report
                                .protections
                                .iter()
                                .enumerate()
                                .map(|(index, check)| render_check("protection", index, check))
                                .collect(),
                        ))
                        .child(section(
                            t!("Lo que arranca solo", "What starts by itself"),
                            Some(startup_detail),
                            report
                                .startup
                                .iter()
                                .enumerate()
                                .map(|(index, review)| render_startup(index, review))
                                .collect(),
                        ))
                        .child(section(
                            t!("Otras cosas que mirar", "Other things to look at"),
                            None,
                            report
                                .other
                                .iter()
                                .enumerate()
                                .map(|(index, check)| render_check("other", index, check))
                                .collect(),
                        )),
                )
                .into_any_element()
            }
        };
        col()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

fn section(title: &'static str, detail: Option<String>, rows: Vec<AnyElement>) -> impl IntoElement {
    col()
        .w_full()
        .gap(sp(1.))
        .child(section_title(title))
        .children(detail.map(|detail| {
            div()
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(detail)
        }))
        .child(col().w_full().mt(sp(1.5)).children(rows))
}

/// One line of a list, separated from the next by a hairline.
fn list_row(id: ElementId, level: Level) -> gpui::Stateful<gpui::Div> {
    let (which, color) = mark(level);
    row()
        .id(id)
        .group(ROW_GROUP)
        .w_full()
        .items_start()
        .gap(sp(3.))
        .py(sp(2.5))
        .border_b_1()
        .border_color(theme::hairline())
        .child(div().mt(px(2.)).child(icon(which, 16.0, color)))
}

fn text_block(title: String, badge: Option<AnyElement>) -> gpui::Div {
    row()
        .w_full()
        .gap(sp(2.))
        .child(
            div()
                .flex_shrink()
                .min_w(px(0.))
                .fs(font::BODY)
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .children(badge)
}

fn render_check(group: &'static str, index: usize, check: &Check) -> AnyElement {
    let settings = check
        .settings
        .filter(|_| check.level != Level::Ok)
        .map(|url| {
            Btn::secondary(t!("Abrir Ajustes", "Open Settings"))
                .size(BtnSize::Sm)
                .icon(Icon::External)
                .build(
                    ElementId::NamedInteger(format!("{group}-settings").into(), index as u64),
                    move |_, _, cx| cx.open_url(url),
                )
                .into_any_element()
        });
    list_row(
        ElementId::NamedInteger(group.into(), index as u64),
        check.level,
    )
    .child(
        col()
            .flex_1()
            .min_w(px(0.))
            .gap(sp(0.5))
            .child(text_block(check.title.clone(), level_pill(check.level)))
            .child(
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(check.detail.clone()),
            )
            .children(check.lines.iter().map(|line| {
                div()
                    .fs(font::CAPTION)
                    .tabular()
                    .text_color(theme::text_1())
                    .child(line.clone())
            })),
    )
    .children(settings)
    .into_any_element()
}

fn render_startup(index: usize, review: &StartupReview) -> AnyElement {
    let item = &review.item;
    let plist = item.plist_path.clone();
    let program = item
        .program
        .clone()
        .unwrap_or_else(|| item.plist_path.display().to_string());
    list_row(
        ElementId::NamedInteger("security-startup".into(), index as u64),
        review.level,
    )
    .child(
        col()
            .flex_1()
            .min_w(px(0.))
            .gap(sp(0.5))
            .child(
                text_block(item.label.clone(), level_pill(review.level))
                    .when(item.disabled, |this| {
                        this.child(tag(t!("Desactivado", "Disabled")))
                    }),
            )
            .child(
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(review.signer.describe()),
            )
            .children(review.reasons.iter().map(|reason| {
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(reason.clone())
            }))
            .child(path_label(program, font::MICRO, theme::text_3())),
    )
    .child(
        Btn::row_action(t!("Mostrar en Finder", "Show in Finder")).build(
            ElementId::NamedInteger("security-reveal".into(), index as u64),
            move |_, _, cx| cx.reveal_path(&plist),
        ),
    )
    .into_any_element()
}
