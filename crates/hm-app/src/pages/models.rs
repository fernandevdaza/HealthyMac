//! The flagship page: every local model, what fits in memory, and the copies you can merge.

use gpui::{
    div, list, prelude::*, px, AnyElement, Context, ElementId, FontWeight, Hsla, SharedString,
    Window,
};
use hm_ai::types::{
    DuplicateGroup, LocalModel, ModelFit, ModelFormat, ModelInventory, RuntimeStatus, UsageSignal,
};
use hm_core::fmt::{human_age, human_memory, human_size};
use hm_core::{t, tf, Action, Area, Finding, Recovery, Risk};

use super::{content_width, gutter_row, plural};
use crate::assets::Icon;
use crate::state::{ModelRow, ModelSort, Shell};
use crate::theme::{self, font, radius, sp, GUTTER};
use crate::ui::chrome::{empty_state, notice, page_header, section_title, NoticeKind};
use crate::ui::controls::{
    dot, icon_button, live_dot, pill, segmented, spinner, tag, Btn, BtnSize,
};
use crate::ui::scrollbar::{scrollbar, ScrollSource};
use crate::ui::{clip, col, icon, row, TextExt};

pub(super) const LOCAL_AI: Area = Area::LocalAi;

pub fn fit_color(fit: ModelFit) -> Hsla {
    match fit {
        ModelFit::Comfortable => theme::green(),
        ModelFit::Tight => theme::yellow(),
        ModelFit::CpuOnly => theme::amber(),
        ModelFit::TooLarge => theme::rose(),
        ModelFit::Unknown => theme::text_3(),
    }
}

fn format_label(format: ModelFormat) -> &'static str {
    match format {
        ModelFormat::Gguf => "GGUF",
        ModelFormat::Mlx => "MLX",
        ModelFormat::Safetensors => "Safetensors",
        ModelFormat::PyTorch => "PyTorch",
        ModelFormat::CoreMl => "Core ML",
        ModelFormat::Onnx => "ONNX",
        ModelFormat::CTranslate2 => "CTranslate2",
        ModelFormat::Ggml => "GGML",
        ModelFormat::Unknown => "—",
    }
}

impl Shell {
    pub fn render_models(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let loading = self.models.inventory.loading;
        let reload: AnyElement = if loading {
            row()
                .gap(sp(2.))
                .h(px(32.))
                .child(spinner("models-spin", 14.0, theme::text_2()))
                .child(
                    div()
                        .fs(font::CAPTION)
                        .text_color(theme::text_2())
                        .child(t!("Actualizando…", "Refreshing…")),
                )
                .into_any_element()
        } else {
            Btn::secondary(t!("Actualizar", "Refresh"))
                .icon(Icon::Refresh)
                .build(
                    "models-reload",
                    cx.listener(|this, _, _, cx| this.load_models(cx)),
                )
                .into_any_element()
        };
        let header = page_header(
            t!("Modelos", "Models"),
            t!(
                "Todos tus modelos locales, y cuáles caben de verdad en la memoria de este Mac.",
                "All your local models, and which ones really fit in this Mac's memory."
            ),
            vec![reload],
        );

        let body: AnyElement = if self.models.inventory.data.is_none() {
            if let Some(error) = self.models.inventory.error.clone() {
                let retry = Btn::secondary(t!("Reintentar", "Try again"))
                    .size(BtnSize::Sm)
                    .build(
                        "models-retry",
                        cx.listener(|this, _, _, cx| this.load_models(cx)),
                    )
                    .into_any_element();
                div()
                    .px(px(GUTTER))
                    .child(notice(
                        NoticeKind::Error,
                        t!(
                            "No hemos podido hacer el inventario de modelos",
                            "Couldn't list your models"
                        ),
                        Some(error.into()),
                        Some(retry),
                    ))
                    .into_any_element()
            } else {
                empty_state(
                    Icon::Cube,
                    theme::text_2(),
                    t!("Buscando modelos", "Looking for models"),
                    t!(
                        "Miramos en Ollama, LM Studio, Hugging Face, MLX y compañía. La primera vez puede tardar un poco, porque hay que comparar archivos muy grandes.",
                        "Checking Ollama, LM Studio, Hugging Face, MLX and the like. The first time can take a while, because very large files have to be compared."
                    ),
                    None,
                )
                .into_any_element()
            }
        } else {
            let state = self.models.list.clone();
            let bar = self.models.bar.clone();
            div()
                .relative()
                .flex_1()
                .min_h(px(0.))
                .child(
                    list(
                        state.clone(),
                        cx.processor(|this: &mut Shell, index: usize, window, cx| {
                            this.render_model_row(index, window, cx)
                        }),
                    )
                    .size_full(),
                )
                .child(scrollbar(ScrollSource::List(state), bar))
                .into_any_element()
        };

        col()
            .size_full()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_model_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row_data) = self.models.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        let Some(inventory) = self.models.inventory.data.as_ref() else {
            return div().into_any_element();
        };
        let wide = content_width(window) > 860.0;
        match row_data {
            ModelRow::Hero => super::models_budget::render_budget_hero(
                inventory,
                self.models.hovered_marker,
                self.models.inventory.error.clone(),
                cx,
            )
            .into_any_element(),
            ModelRow::Runtimes => render_runtimes(&inventory.runtimes).into_any_element(),
            ModelRow::TableHeader => self.render_table_header(wide, cx).into_any_element(),
            ModelRow::NoModels => col()
                .w_full()
                .items_center()
                .gap(sp(2.))
                .py(sp(10.))
                .child(icon(Icon::Cube, 24.0, theme::text_2()))
                .child(
                    div()
                        .fs(font::SUBHEAD)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("Aún no hay modelos en este Mac", "No models on this Mac yet")),
                )
                .child(
                    div()
                        .max_w(px(440.))
                        .text_center()
                        .fs(font::BODY)
                        .text_color(theme::text_2())
                        .child(t!(
                            "Cuando descargues alguno con Ollama, LM Studio, Hugging Face o MLX, aparecerá aquí, en el punto de la regla que le corresponde.",
                            "When you download one with Ollama, LM Studio, Hugging Face or MLX, it shows up here, at its place on the ruler."
                        )),
                )
                .into_any_element(),
            ModelRow::Model(position) => {
                let model_index = self.models.order[position];
                let model = inventory.models[model_index].clone();
                gutter_row().child(self.render_model(position, &model, wide, cx)).into_any_element()
            }
            ModelRow::DuplicatesHeader => render_duplicates_header(inventory, cx).into_any_element(),
            ModelRow::Duplicate(position) => {
                let group = inventory.duplicates[position].clone();
                gutter_row().child(render_duplicate(position, &group, cx)).into_any_element()
            }
            ModelRow::Gap => div().h(px(24.)).into_any_element(),
        }
    }

    fn render_table_header(&self, wide: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let labels: Vec<SharedString> = vec![
            t!("Tamaño", "Size").into(),
            t!("Último uso", "Last used").into(),
            t!("Nombre", "Name").into(),
        ];
        let selected = match self.models.sort {
            ModelSort::Size => 0,
            ModelSort::LastUsed => 1,
            ModelSort::Name => 2,
        };
        let entity = cx.entity().downgrade();
        let count = self
            .models
            .inventory
            .data
            .as_ref()
            .map_or(0, |inv| inv.models.len());
        col()
            .w_full()
            .pt(sp(6.))
            .gap(sp(2.5))
            .child(
                row()
                    .px(px(GUTTER))
                    .justify_between()
                    .child(section_title(plural(
                        count,
                        t!("modelo", "model"),
                        t!("modelos", "models"),
                    )))
                    .child(segmented(
                        "model-sort",
                        &labels,
                        selected,
                        move |index, _, cx| {
                            entity
                                .update(cx, |shell, cx| {
                                    shell.models.sort = match index {
                                        0 => ModelSort::Size,
                                        1 => ModelSort::LastUsed,
                                        _ => ModelSort::Name,
                                    };
                                    shell.rebuild_model_rows();
                                    cx.notify();
                                })
                                .ok();
                        },
                    )),
            )
            .child(
                row()
                    .h(px(26.))
                    .mx(px(GUTTER))
                    .gap(sp(3.))
                    .border_b_1()
                    .border_color(theme::hairline())
                    .fs(font::MICRO)
                    .text_color(theme::text_3())
                    .child(div().flex_1().min_w(px(0.)).child(t!("Modelo", "Model")))
                    .child(div().w(px(92.)).child("Runtime"))
                    .child(div().w(px(116.)).child(t!("Formato", "Format")))
                    .child(div().w(px(100.)).child(t!("En memoria", "In memory")))
                    .when(wide, |this| {
                        this.child(div().w(px(124.)).child(t!("Último uso", "Last used")))
                    })
                    .child(div().w(px(70.)).text_right().child(t!("Tamaño", "Size")))
                    .child(div().w(px(26.))),
            )
    }

    fn render_model(
        &self,
        position: usize,
        model: &LocalModel,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut facts: Vec<String> = Vec::new();
        if let Some(architecture) = &model.architecture {
            facts.push(architecture.clone());
        }
        if let Some(parameters) = model.parameter_count {
            facts.push(format_parameters(parameters));
        }
        if let Some(context) = model.context_length {
            facts.push(tf!("contexto {}", "{} context", format_context(context)));
        }
        if model.shared_bytes > 0 {
            facts.push(tf!(
                "{} compartidos",
                "{} shared",
                human_size(model.shared_bytes)
            ));
        }
        let format_text = match &model.quantization {
            Some(quantization) => format!("{} · {}", format_label(model.format), quantization),
            None => format_label(model.format).to_string(),
        };
        let now = std::time::SystemTime::now();
        let remove_model = model.clone();
        row()
            .id(ElementId::NamedInteger("model".into(), position as u64))
            .group("model-row")
            .min_h(px(48.))
            .w_full()
            .px(sp(2.))
            .gap(sp(3.))
            .rounded(px(radius::MD))
            .hover(|style| style.bg(theme::surface_1()))
            .child(
                col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        row()
                            .gap(px(7.))
                            .when(model.loaded, |this| this.child(live_dot(theme::green())))
                            .child(
                                clip()
                                    .fs(font::BODY)
                                    .text_color(theme::text_1())
                                    .child(model.name.clone()),
                            )
                            .when(model.incomplete, |this| {
                                this.child(pill(t!("Incompleto", "Incomplete"), theme::amber()))
                            }),
                    )
                    .when(!facts.is_empty(), |this| {
                        this.child(
                            clip()
                                .fs(font::MICRO)
                                .text_color(theme::text_3())
                                .child(facts.join(" · ")),
                        )
                    }),
            )
            .child(
                clip()
                    .flex_none()
                    .w(px(92.))
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(model.runtime.label()),
            )
            .child(
                clip()
                    .flex_none()
                    .w(px(116.))
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(format_text),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(100.))
                    .child(pill(model.fit.label(), fit_color(model.fit))),
            )
            .when(wide, |this| {
                this.child(
                    clip()
                        .flex_none()
                        .w(px(124.))
                        .fs(font::CAPTION)
                        .text_color(theme::text_3())
                        .child(usage_text(model, now)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(70.))
                    .text_right()
                    .fs(font::BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .tabular()
                    .child(human_size(model.size)),
            )
            .child(
                div()
                    .opacity(0.35)
                    .group_hover("model-row", |style| style.opacity(1.0))
                    .child(icon_button(
                        ElementId::NamedInteger("model-delete".into(), position as u64),
                        Icon::Trash,
                        theme::text_2(),
                        cx.listener(move |this, _, _, cx| {
                            let finding = if this.mode.demo {
                                crate::demo::ai::removal_finding(&remove_model)
                            } else {
                                hm_ai::inventory::model_removal_finding(&remove_model)
                            };
                            this.open_review(
                                tf!("Eliminar {}", "Remove {}", remove_model.name),
                                t!("Eliminar modelo", "Remove model"),
                                vec![finding],
                                cx,
                            );
                        }),
                    )),
            )
    }

    pub fn review_dedupe(&mut self, groups: Vec<DuplicateGroup>, cx: &mut Context<Self>) {
        let items: Vec<Finding> = groups
            .iter()
            .map(|group| {
                self.findings
                    .iter()
                    .find(|finding| finding.id == group.finding_id)
                    .cloned()
                    .unwrap_or_else(|| dedupe_finding(group))
            })
            .collect();
        self.open_review(
            t!("Deduplicar modelos", "Deduplicate models"),
            t!("Deduplicar", "Deduplicate"),
            items,
            cx,
        );
    }
}

/// The finding for a duplicate group, for when no scan has produced it yet.
fn dedupe_finding(group: &DuplicateGroup) -> Finding {
    let keep = group
        .keep
        .clone()
        .or_else(|| group.paths.first().cloned())
        .unwrap_or_default();
    let duplicates: Vec<_> = group
        .paths
        .iter()
        .filter(|path| **path != keep)
        .cloned()
        .collect();
    let copies = duplicates.len();
    let mut finding = Finding::new(
        "ai.duplicates",
        LOCAL_AI,
        &group.sha256,
        tf!(
            "Deduplicar {}",
            "Deduplicate {}",
            group.model_names.join(", ")
        ),
    )
    .subtitle(keep.display().to_string())
    .explanation(t!(
        "Copias idénticas del mismo archivo. Se sustituyen por clones APFS: todas las rutas siguen \
         funcionando y solo se liberan los bloques repetidos. No se pierde nada.",
        "Identical copies of the same file. They are replaced with APFS clones: every path keeps \
         working and only the repeated blocks are freed. Nothing is lost."
    ))
    .size(group.reclaimable)
    .items(copies as u64)
    .risk(Risk::Safe)
    .recovery(Recovery::NothingLost)
    .action(Action::CloneDedupe { keep, duplicates });
    finding.id = group.finding_id.clone();
    finding
}

/// "Last used" only when that is what we really know. Without a runtime's record, all we have is
/// when the files were written, so it says that instead.
fn usage_text(model: &LocalModel, now: std::time::SystemTime) -> String {
    match (model.last_used_source, model.last_used) {
        (UsageSignal::LoadedNow, _) => t!("Ahora", "Now").to_string(),
        (UsageSignal::RuntimeRecord, Some(when)) => human_age(when, now),
        (UsageSignal::Modified, Some(when)) => {
            tf!("Bajado {}", "Downloaded {}", human_age(when, now))
        }
        _ => "—".to_string(),
    }
}

fn format_parameters(parameters: u64) -> String {
    let value = parameters as f64;
    if value >= 1e9 {
        tf!(
            "{:.0} mil millones de parámetros",
            "{:.0} billion parameters",
            value / 1e9
        )
    } else if value >= 1e6 {
        tf!(
            "{:.0} millones de parámetros",
            "{:.0} million parameters",
            value / 1e6
        )
    } else {
        tf!("{parameters} parámetros", "{parameters} parameters")
    }
}

fn format_context(context: u64) -> String {
    if context >= 1000 {
        format!("{}K", context / 1000)
    } else {
        context.to_string()
    }
}

pub(super) fn runtime_loaded_bytes(runtime: &RuntimeStatus) -> u64 {
    let by_model: u64 = runtime
        .loaded
        .iter()
        .filter_map(|model| model.memory_bytes.or(model.gpu_memory_bytes))
        .sum();
    if by_model > 0 {
        by_model
    } else {
        runtime.loaded_memory_bytes.unwrap_or(0)
    }
}

fn render_runtimes(runtimes: &[RuntimeStatus]) -> impl IntoElement {
    let mut known: Vec<&RuntimeStatus> = runtimes
        .iter()
        .filter(|runtime| runtime.installed || runtime.storage_bytes > 0 || runtime.running)
        .collect();
    known.sort_by_key(|runtime| (!runtime.running, std::cmp::Reverse(runtime.storage_bytes)));
    row()
        .w_full()
        .px(px(GUTTER))
        .pt(sp(2.))
        .flex_wrap()
        .gap(sp(2.))
        .children(known.into_iter().map(runtime_chip))
}

fn runtime_chip(runtime: &RuntimeStatus) -> impl IntoElement {
    let loaded: Vec<String> = runtime
        .loaded
        .iter()
        .map(
            |model| match model.memory_bytes.or(model.gpu_memory_bytes) {
                Some(bytes) => format!("{} ({})", model.name, human_memory(bytes)),
                None => model.name.clone(),
            },
        )
        .collect();
    let detail = if !loaded.is_empty() {
        loaded.join(", ")
    } else if let Some(count) = runtime.loaded_count.filter(|count| *count > 0) {
        // Some runtimes only tell how many, not which.
        let models = plural(
            count as usize,
            t!("modelo cargado", "model loaded"),
            t!("modelos cargados", "models loaded"),
        );
        match runtime.loaded_memory_bytes {
            Some(bytes) => format!("{models} ({})", human_memory(bytes)),
            None => models,
        }
    } else if runtime.running {
        t!("Activo, sin modelos cargados", "Running, no models loaded").to_string()
    } else if !runtime.kind.has_server() {
        // A folder of files, not something that runs: "stopped" would mean nothing here.
        human_size(runtime.storage_bytes)
    } else if runtime.storage_bytes > 0 {
        tf!(
            "Parado · {}",
            "Stopped · {}",
            human_size(runtime.storage_bytes)
        )
    } else {
        t!("Parado", "Stopped").to_string()
    };
    row()
        .max_w(px(420.))
        .h(px(30.))
        .px(sp(2.5))
        .gap(px(7.))
        .rounded(px(radius::MD))
        .bg(if runtime.running {
            theme::surface_2()
        } else {
            theme::surface_1()
        })
        .border_1()
        .border_color(theme::hairline())
        .when(runtime.running, |this| this.child(live_dot(theme::green())))
        .when(!runtime.running && runtime.kind.has_server(), |this| {
            this.child(dot(theme::surface_4(), 7.0))
        })
        .child(
            div()
                .flex_none()
                .fs(font::CAPTION)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_1())
                .child(runtime.kind.label()),
        )
        .when_some(runtime.version.clone(), |this, version| {
            this.child(div().flex_none().child(tag(version)))
        })
        .child(
            clip()
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(detail),
        )
}

fn render_duplicates_header(
    inventory: &ModelInventory,
    cx: &mut Context<Shell>,
) -> impl IntoElement {
    let groups: Vec<DuplicateGroup> = inventory
        .duplicates
        .iter()
        .filter(|group| group.reclaimable > 0 && group.keep.is_some())
        .cloned()
        .collect();
    let can_dedupe = !groups.is_empty();
    col()
        .w_full()
        .px(px(GUTTER))
        .pt(sp(6.))
        .pb(sp(1.5))
        .gap(sp(2.))
        .child(
            row()
                .justify_between()
                .child(section_title(tf!(
                    "Copias repetidas: {} recuperables",
                    "Duplicate copies: {} reclaimable",
                    human_size(inventory.reclaimable_bytes)
                )))
                .child(
                    Btn::primary(t!("Deduplicar todo", "Deduplicate all"))
                        .size(BtnSize::Sm)
                        .icon(Icon::Copy)
                        .disabled(!can_dedupe)
                        .build(
                            "dedupe-all",
                            cx.listener(move |this, _, _, cx| this.review_dedupe(groups.clone(), cx)),
                        ),
                ),
        )
        .child(
            div()
                .max_w(px(640.))
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(t!(
                    "El mismo archivo guardado varias veces, normalmente por runtimes distintos. Deduplicar lo cambia por clones APFS: todas las rutas siguen funcionando, ningún modelo se rompe y el espacio se libera al momento. No se pierde nada.",
                    "The same file saved more than once, usually by different runtimes. Deduplicating swaps the copies for APFS clones: every path keeps working, no model breaks and the space is freed right away. Nothing is lost."
                )),
        )
}

fn render_duplicate(
    position: usize,
    group: &DuplicateGroup,
    cx: &mut Context<Shell>,
) -> impl IntoElement {
    let target = group.clone();
    let copies = group.paths.len();
    let actionable = group.reclaimable > 0 && group.keep.is_some();
    row()
        .min_h(px(44.))
        .w_full()
        .px(sp(2.))
        .gap(sp(3.))
        .rounded(px(radius::MD))
        .hover(|style| style.bg(theme::surface_1()))
        .child(icon(Icon::Copy, 15.0, theme::text_3()))
        .child(
            col()
                .flex_1()
                .min_w(px(0.))
                .child(clip().fs(font::BODY).child(group.model_names.join(", ")))
                .child(
                    clip()
                        .fs(font::MICRO)
                        .text_color(theme::text_3())
                        .child(tf!(
                            "{copies} copias de {} cada una",
                            "{copies} copies of {} each",
                            human_size(group.file_size)
                        )),
                ),
        )
        .child(
            div()
                .flex_none()
                .fs(font::BODY)
                .font_weight(FontWeight::MEDIUM)
                .tabular()
                .text_color(theme::amber())
                .child(human_size(group.reclaimable)),
        )
        .child(if actionable {
            Btn::secondary(t!("Deduplicar", "Deduplicate"))
                .size(BtnSize::Sm)
                .build(
                    ElementId::NamedInteger("dedupe".into(), position as u64),
                    cx.listener(move |this, _, _, cx| this.review_dedupe(vec![target.clone()], cx)),
                )
                .into_any_element()
        } else if group.reclaimable == 0 {
            tag(t!("Ya son clones", "Already clones")).into_any_element()
        } else {
            tag(t!("Fuera de tu carpeta", "Outside your home folder")).into_any_element()
        })
}
