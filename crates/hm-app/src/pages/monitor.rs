//! The live monitor. Refreshes once a second, only while it is on screen.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, relative, AnyElement, Background, Context, FontWeight, Hsla, SharedString,
    Window,
};
use hm_core::fmt::{human_memory, human_size};
use hm_core::{t, tf};
use hm_system::types::{BatteryStats, MemoryPressure, ProcessStats, SystemSnapshot, ThermalState};

use super::scroll_pane;
use crate::assets::Icon;
use crate::state::Shell;
use crate::theme::{self, font, sp, GUTTER};
use crate::ui::charts::{meter, sparkline, stack_bar};
use crate::ui::chrome::{big_number, card, empty_state, eyebrow, page_header};
use crate::ui::controls::{dot, live_dot, pill};
use crate::ui::{clip, col, icon, row, TextExt};

impl Shell {
    pub fn render_monitor(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> AnyElement {
        let uptime = self
            .live
            .snapshot
            .as_ref()
            .map(|s| format_uptime(s.uptime_secs));
        let subtitle: SharedString = match uptime {
            Some(uptime) => tf!(
                "Encendido desde hace {uptime}. Se actualiza cada segundo.",
                "Up for {uptime}. Updates every second."
            )
            .into(),
            None => t!(
                "Cómo va tu Mac, ahora mismo.",
                "How your Mac is doing, right now."
            )
            .into(),
        };
        let header = page_header(
            "Monitor",
            subtitle,
            vec![row()
                .gap(sp(1.5))
                .h(px(32.))
                .child(live_dot(theme::green()))
                .child(
                    div()
                        .fs(font::CAPTION)
                        .text_color(theme::text_2())
                        .child(t!("En directo", "Live")),
                )
                .into_any_element()],
        );
        let Some(snapshot) = self.live.snapshot.clone() else {
            return col()
                .size_full()
                .child(header)
                .child(empty_state(
                    Icon::Activity,
                    theme::teal(),
                    t!("Leyendo sensores", "Reading sensors"),
                    t!("Un segundo.", "One moment."),
                    None,
                ))
                .into_any_element();
        };
        let wide = super::content_width(window) > 860.0;
        let pane = self.monitor_scroll.clone();
        let memory = self.render_memory_card(&snapshot);
        let cpu = self.render_cpu_card(&snapshot);
        let network = self.render_network_card(&snapshot);
        let thermal = render_thermal_card(snapshot.thermal);
        let battery = render_battery_card(snapshot.battery.as_ref());
        let disks = render_disks_card(&snapshot);
        let by_cpu = render_process_card(
            t!("Más CPU", "Top CPU"),
            &snapshot.top_by_cpu,
            ProcessMetric::Cpu,
        );
        let by_memory = render_process_card(
            t!("Más memoria", "Top memory"),
            &snapshot.top_by_memory,
            ProcessMetric::Memory,
        );
        col()
            .size_full()
            .child(header)
            .child(scroll_pane(
                "monitor-scroll",
                &pane,
                col()
                    .w_full()
                    .px(px(GUTTER))
                    .pb(sp(6.))
                    .gap(sp(3.))
                    .child(cpu)
                    .child(memory)
                    .child(
                        row()
                            .w_full()
                            .gap(sp(3.))
                            .items_start()
                            .child(network)
                            .child(thermal)
                            .child(battery)
                            .when(wide, |this| this.child(disks)),
                    )
                    .child(
                        row()
                            .w_full()
                            .gap(sp(3.))
                            .items_start()
                            .child(by_cpu)
                            .child(by_memory),
                    ),
            ))
            .into_any_element()
    }

    fn render_cpu_card(&self, snapshot: &SystemSnapshot) -> impl IntoElement {
        let cpu = &snapshot.cpu;
        let history: Rc<Vec<f32>> = Rc::new(self.live.cpu.iter().copied().collect());
        let cores = &cpu.per_core_percent;
        // macOS lists the slowest cluster first, and core_clusters comes fastest first. Each cluster
        // gets its own shade, using macOS's own name for it rather than assuming "efficiency".
        let clusters = &cpu.core_clusters;
        let mut shades: Vec<Hsla> = Vec::with_capacity(cores.len());
        for (position, cluster) in clusters.iter().enumerate().rev() {
            let color = if position == 0 {
                theme::teal()
            } else {
                theme::slate()
            };
            shades.extend(std::iter::repeat_n(color, cluster.logical_cpus as usize));
        }
        shades.resize(cores.len(), theme::teal());
        let core_label = if clusters.is_empty() {
            tf!("{} núcleos", "{} cores", cores.len())
        } else {
            clusters
                .iter()
                .map(|cluster| format!("{} {}", cluster.logical_cpus, cluster_name(&cluster.name)))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        card().w_full().p(sp(4.)).child(
            row()
                .gap(sp(6.))
                .items_start()
                .child(
                    col()
                        .flex_none()
                        .w(px(170.))
                        .gap(sp(0.5))
                        .child(card_title(Icon::Cpu, t!("Procesador", "CPU")))
                        .child(big_number(
                            tf!("{:.0} %", "{:.0}%", cpu.usage_percent),
                            font::DISPLAY,
                            theme::text_1(),
                        ))
                        .child(
                            clip()
                                .fs(font::CAPTION)
                                .text_color(theme::text_2())
                                .child(cpu.brand.clone()),
                        )
                        .child(
                            div()
                                .fs(font::MICRO)
                                .tabular()
                                .text_color(theme::text_3())
                                .child(load_text(&cpu.load_average)),
                        ),
                )
                .child(
                    col()
                        .flex_1()
                        .min_w(px(0.))
                        .gap(sp(2.))
                        .child(div().h(px(60.)).w_full().child(sparkline(
                            history,
                            100.0,
                            theme::teal(),
                        )))
                        .child(
                            row().w_full().gap(px(3.)).h(px(28.)).items_end().children(
                                cores
                                    .iter()
                                    .zip(shades)
                                    .map(|(usage, color)| meter(usage / 100.0, color, 28.0)),
                            ),
                        )
                        .child(
                            clip()
                                .fs(font::MICRO)
                                .text_color(theme::text_3())
                                .child(core_label),
                        ),
                ),
        )
    }

    fn render_memory_card(&self, snapshot: &SystemSnapshot) -> impl IntoElement {
        let memory = &snapshot.memory;
        let total = memory.total.max(1);
        let accounted = memory
            .app
            .saturating_add(memory.wired)
            .saturating_add(memory.compressed);
        let other = memory.used.saturating_sub(accounted);
        let free = memory.total.saturating_sub(memory.used.max(accounted));
        let fraction = |bytes: u64| bytes as f32 / total as f32;
        let (app_color, wired_color, compressed_color) =
            (theme::slate(), theme::violet(), theme::orange());
        let segments: Vec<(f32, Background)> = vec![
            (fraction(memory.app), app_color.into()),
            (fraction(memory.wired), wired_color.into()),
            (fraction(memory.compressed), compressed_color.into()),
            (fraction(other), theme::surface_4().into()),
            (fraction(free), theme::surface_2().into()),
        ];
        let (pressure_label, pressure_color) = match memory.pressure {
            MemoryPressure::Normal => (t!("Presión normal", "Normal pressure"), theme::green()),
            MemoryPressure::Warning => (t!("Presión alta", "High pressure"), theme::amber()),
            MemoryPressure::Critical => (t!("Presión crítica", "Critical pressure"), theme::rose()),
        };
        card().w_full().p(sp(4.)).child(
            row()
                .gap(sp(6.))
                .items_start()
                .child(
                    col()
                        .flex_none()
                        .w(px(170.))
                        .gap(sp(0.5))
                        .child(card_title(Icon::Memory, t!("Memoria", "Memory")))
                        .child(big_number(
                            human_memory(memory.used),
                            font::DISPLAY,
                            theme::text_1(),
                        ))
                        .child(
                            div()
                                .fs(font::CAPTION)
                                .text_color(theme::text_2())
                                .child(tf!("de {}", "of {}", human_memory(memory.total))),
                        ),
                )
                .child(
                    col()
                        .flex_1()
                        .min_w(px(0.))
                        .gap(sp(2.5))
                        .pt(sp(1.))
                        .child(
                            row()
                                .justify_between()
                                .child(pill(pressure_label, pressure_color))
                                .child(
                                    div()
                                        .fs(font::CAPTION)
                                        .tabular()
                                        .text_color(theme::text_2())
                                        .child(if memory.swap_total > 0 {
                                            tf!(
                                                "Intercambio: {} de {}",
                                                "Swap: {} of {}",
                                                human_memory(memory.swap_used),
                                                human_memory(memory.swap_total)
                                            )
                                        } else {
                                            t!("Sin intercambio", "No swap").to_string()
                                        }),
                                ),
                        )
                        .child(stack_bar(&segments, 14.0))
                        .child(
                            row()
                                .flex_wrap()
                                .gap_x(sp(4.))
                                .gap_y(sp(0.5))
                                .child(legend(app_color, "Apps", memory.app))
                                .child(legend(wired_color, t!("Reservada", "Wired"), memory.wired))
                                .child(legend(
                                    compressed_color,
                                    t!("Comprimida", "Compressed"),
                                    memory.compressed,
                                ))
                                .when(other > 0, |this| {
                                    this.child(legend(
                                        theme::surface_4(),
                                        t!("Otra", "Other"),
                                        other,
                                    ))
                                })
                                .child(legend(theme::surface_3(), t!("Libre", "Free"), free)),
                        ),
                ),
        )
    }

    fn render_network_card(&self, snapshot: &SystemSnapshot) -> impl IntoElement {
        let rx: Rc<Vec<f32>> = Rc::new(self.live.rx.iter().copied().collect());
        let tx: Rc<Vec<f32>> = Rc::new(self.live.tx.iter().copied().collect());
        let peak = rx
            .iter()
            .chain(tx.iter())
            .copied()
            .fold(1_000_000.0_f32, f32::max);
        small_card(Icon::Network, t!("Red", "Network"))
            .child(
                row()
                    .gap(sp(3.5))
                    .child(rate(
                        t!("Baja", "Down"),
                        snapshot.network.rx_bytes_per_sec,
                        theme::blue(),
                    ))
                    .child(rate(
                        t!("Sube", "Up"),
                        snapshot.network.tx_bytes_per_sec,
                        theme::violet(),
                    )),
            )
            .child(
                div()
                    .relative()
                    .h(px(36.))
                    .w_full()
                    .child(
                        div()
                            .absolute()
                            .size_full()
                            .child(sparkline(rx, peak, theme::blue())),
                    )
                    .child(div().absolute().size_full().child(sparkline(
                        tx,
                        peak,
                        theme::violet(),
                    ))),
            )
    }
}

fn card_title(which: Icon, title: &'static str) -> impl IntoElement {
    row()
        .gap(px(7.))
        .mb(sp(1.5))
        .child(icon(which, 15.0, theme::text_2()))
        .child(
            div()
                .fs(font::CAPTION)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_2())
                .child(title),
        )
}

fn small_card(which: Icon, title: &'static str) -> gpui::Div {
    card()
        .flex_1()
        .min_w(px(0.))
        .h(px(136.))
        .p(sp(3.5))
        .flex()
        .flex_col()
        .gap(sp(2.))
        .child(card_title(which, title))
}

fn legend(color: Hsla, label: &'static str, bytes: u64) -> impl IntoElement {
    row()
        .gap(px(5.))
        .h(px(18.))
        .child(dot(color, 7.0))
        .child(
            div()
                .fs(font::CAPTION)
                .text_color(theme::text_3())
                .child(label),
        )
        .child(
            div()
                .fs(font::CAPTION)
                .tabular()
                .text_color(theme::text_2())
                .child(human_memory(bytes)),
        )
}

fn rate(label: &'static str, bytes_per_sec: u64, color: Hsla) -> impl IntoElement {
    col()
        .flex_1()
        .min_w(px(0.))
        .child(
            row().gap(sp(1.)).child(dot(color, 6.0)).child(
                div()
                    .fs(font::MICRO)
                    .text_color(theme::text_3())
                    .child(label),
            ),
        )
        .child(
            clip()
                .fs(font::BODY)
                .font_weight(FontWeight::MEDIUM)
                .tabular()
                .child(format!("{}/s", human_size(bytes_per_sec))),
        )
}

fn render_thermal_card(thermal: ThermalState) -> impl IntoElement {
    let (label, detail, color, fraction) = match thermal {
        ThermalState::Nominal => (
            t!("Fresco", "Cool"),
            t!("Sin ninguna limitación.", "Running at full speed."),
            theme::green(),
            0.2,
        ),
        ThermalState::Fair => (
            t!("Templado", "Warm"),
            t!(
                "Algo de calor, nada que notar.",
                "A little warm, nothing you'd notice."
            ),
            theme::lime(),
            0.45,
        ),
        ThermalState::Serious => (
            t!("Caliente", "Hot"),
            t!(
                "macOS está bajando el ritmo.",
                "macOS is slowing things down."
            ),
            theme::amber(),
            0.75,
        ),
        ThermalState::Critical => (
            t!("Muy caliente", "Very hot"),
            t!(
                "Rendimiento muy limitado.",
                "Performance is heavily limited."
            ),
            theme::rose(),
            1.0,
        ),
        ThermalState::Unknown => (
            t!("Sin datos", "No data"),
            t!("No hemos podido leerlo.", "Couldn't read it."),
            theme::text_3(),
            0.0,
        ),
    };
    small_card(Icon::Thermometer, t!("Temperatura", "Temperature"))
        .child(big_number(label, font::HEADLINE + 2.0, color))
        .child(
            div()
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(detail),
        )
        .child(
            div()
                .mt(sp(0.5))
                .h(px(4.))
                .w_full()
                .rounded(px(2.))
                .bg(theme::surface_2())
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded(px(2.))
                        .bg(color),
                ),
        )
}

fn render_battery_card(battery: Option<&BatteryStats>) -> impl IntoElement {
    let card = small_card(Icon::Battery, t!("Batería", "Battery"));
    let Some(battery) = battery else {
        return card
            .child(big_number(
                t!("Sin batería", "No battery"),
                font::HEADLINE + 2.0,
                theme::text_3(),
            ))
            .child(
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(t!(
                        "Este Mac va siempre enchufado.",
                        "This Mac always runs plugged in."
                    )),
            );
    };
    let state = if battery.charging {
        t!("Cargando", "Charging").to_string()
    } else if battery.plugged_in {
        t!("Enchufada", "Plugged in").to_string()
    } else if let Some(minutes) = battery.minutes_remaining {
        tf!(
            "Quedan {} h {} min",
            "{} h {} min left",
            minutes / 60,
            minutes % 60
        )
    } else {
        t!("Con batería", "On battery").to_string()
    };
    let mut facts: Vec<String> = Vec::new();
    if let Some(health) = battery.health_percent {
        facts.push(tf!("Salud {health} %", "Health {health}%"));
    }
    if let Some(cycles) = battery.cycle_count {
        facts.push(tf!("{cycles} ciclos", "{cycles} cycles"));
    }
    let condition_bad = battery
        .condition
        .as_deref()
        .is_some_and(|condition| condition != "Normal");
    let color = if battery.percent <= 15 && !battery.plugged_in {
        theme::rose()
    } else {
        theme::green()
    };
    card.child(
        row()
            .gap(sp(2.))
            .child(big_number(
                tf!("{} %", "{}%", battery.percent),
                font::HEADLINE + 2.0,
                theme::text_1(),
            ))
            .child(
                div()
                    .fs(font::CAPTION)
                    .text_color(theme::text_2())
                    .child(state),
            ),
    )
    .child(
        div()
            .h(px(4.))
            .w_full()
            .rounded(px(2.))
            .bg(theme::surface_2())
            .child(
                div()
                    .h_full()
                    .w(relative(battery.percent as f32 / 100.0))
                    .rounded(px(2.))
                    .bg(color),
            ),
    )
    .child(
        clip()
            .fs(font::CAPTION)
            .tabular()
            .text_color(if condition_bad {
                theme::amber()
            } else {
                theme::text_2()
            })
            .child(if condition_bad {
                tf!(
                    "macOS dice: {}",
                    "macOS says: {}",
                    battery.condition.clone().unwrap_or_default()
                )
            } else {
                facts.join(" · ")
            }),
    )
}

fn render_disks_card(snapshot: &SystemSnapshot) -> impl IntoElement {
    let disk = super::resumen::main_disk(snapshot);
    let card = small_card(Icon::Disk, t!("Disco", "Disk"));
    match disk {
        None => card.child(
            div()
                .fs(font::CAPTION)
                .text_color(theme::text_2())
                .child(t!("No hemos podido leerlo.", "Couldn't read it.")),
        ),
        Some(disk) => {
            let used = disk.total.saturating_sub(disk.available);
            let fraction = used as f32 / disk.total.max(1) as f32;
            let color = if fraction > 0.9 {
                theme::rose()
            } else if fraction > 0.8 {
                theme::amber()
            } else {
                theme::teal()
            };
            card.child(big_number(
                human_size(disk.available),
                font::HEADLINE + 2.0,
                theme::text_1(),
            ))
            .child(
                clip()
                    .fs(font::CAPTION)
                    .tabular()
                    .text_color(theme::text_2())
                    .child(tf!("libres de {}", "free of {}", human_size(disk.total))),
            )
            .child(
                div()
                    .h(px(4.))
                    .w_full()
                    .rounded(px(2.))
                    .bg(theme::surface_2())
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .rounded(px(2.))
                            .bg(color),
                    ),
            )
        }
    }
}

#[derive(Clone, Copy)]
enum ProcessMetric {
    Cpu,
    Memory,
}

fn render_process_card(
    title: &'static str,
    processes: &[ProcessStats],
    metric: ProcessMetric,
) -> impl IntoElement {
    let peak = processes
        .iter()
        .map(|process| match metric {
            ProcessMetric::Cpu => process.cpu_percent,
            ProcessMetric::Memory => process.memory_bytes as f32,
        })
        .fold(0.0_f32, f32::max)
        .max(1.0);
    card()
        .flex_1()
        .min_w(px(0.))
        .p(sp(3.5))
        .child(
            div()
                .mb(sp(2.))
                .fs(font::CAPTION)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_2())
                .child(title),
        )
        .children(if processes.is_empty() {
            vec![eyebrow(t!("Sin datos todavía", "No data yet")).into_any_element()]
        } else {
            processes
                .iter()
                .take(6)
                .map(|process| {
                    let (value, fraction) = match metric {
                        ProcessMetric::Cpu => (
                            tf!("{:.0} %", "{:.0}%", process.cpu_percent),
                            process.cpu_percent / peak,
                        ),
                        ProcessMetric::Memory => (
                            human_memory(process.memory_bytes),
                            process.memory_bytes as f32 / peak,
                        ),
                    };
                    col()
                        .h(px(30.))
                        .gap(sp(0.5))
                        .justify_center()
                        .child(
                            row()
                                .gap(sp(2.))
                                .child(
                                    clip()
                                        .flex_1()
                                        .fs(font::CAPTION)
                                        .child(process.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .fs(font::CAPTION)
                                        .tabular()
                                        .text_color(theme::text_2())
                                        .child(value),
                                ),
                        )
                        .child(
                            div()
                                .h(px(2.))
                                .w_full()
                                .rounded(px(1.))
                                .bg(theme::surface_1())
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(fraction.clamp(0.0, 1.0)))
                                        .rounded(px(1.))
                                        .bg(theme::surface_4()),
                                ),
                        )
                        .into_any_element()
                })
                .collect()
        })
}

fn cluster_name(name: &str) -> String {
    match name {
        "Performance" => t!("de rendimiento", "performance").to_string(),
        "Efficiency" => t!("de eficiencia", "efficiency").to_string(),
        "Super" => t!("de alto rendimiento", "super").to_string(),
        other => tf!("«{other}»", "“{other}”"),
    }
}

/// The load averages, with the decimal mark of the current language.
fn load_text(load: &[f64; 3]) -> String {
    let text = tf!(
        "Carga {:.1} · {:.1} · {:.1}",
        "Load {:.1} · {:.1} · {:.1}",
        load[0],
        load[1],
        load[2]
    );
    if hm_core::i18n::is_english() {
        text
    } else {
        text.replace('.', ",")
    }
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    if days > 0 {
        format!("{days} d {hours} h")
    } else if hours > 0 {
        format!("{hours} h {minutes} min")
    } else {
        format!("{minutes} min")
    }
}
