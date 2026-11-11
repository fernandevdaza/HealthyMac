//! `healthymac`: the engine without the UI. Scriptable, and how the engine is exercised in testing.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hm_core::fmt::human_size;
use hm_core::i18n::{set_language, Language};
use hm_core::{
    run_scan, t, tf, Area, CancelToken, CleanOptions, Cleaner, Finding, ScanContext, ScanEvent,
    Scanner,
};

#[derive(Parser)]
#[command(
    name = "healthymac",
    version,
    about = "Mantén tu Mac sano, desde la terminal"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum AreaArg {
    System,
    Dev,
    Ai,
    Apps,
    Files,
}

impl From<AreaArg> for Area {
    fn from(value: AreaArg) -> Self {
        match value {
            AreaArg::System => Area::System,
            AreaArg::Dev => Area::Developer,
            AreaArg::Ai => Area::LocalAi,
            AreaArg::Apps => Area::Apps,
            AreaArg::Files => Area::Files,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Busca lo que se puede limpiar. Nunca modifica nada.
    Scan {
        #[arg(long, value_enum)]
        area: Option<AreaArg>,
        #[arg(long)]
        json: bool,
    },
    /// Limpia hallazgos por id. Mueve a la Papelera. Sin --yes solo simula.
    Clean {
        /// Ids tal como los muestra `scan --json`.
        ids: Vec<String>,
        /// Ejecutar de verdad.
        #[arg(long)]
        yes: bool,
    },
    /// Inventario de modelos de IA locales.
    Models,
    /// Estado de los runtimes de IA.
    Runtimes,
    /// Procesos escuchando en puertos.
    Ports,
    /// Proyectos de desarrollo y sus artefactos.
    Projects,
    /// Uso de disco de Docker.
    Docker,
    /// Apps instaladas y sus restos.
    Apps,
    /// Agentes y daemons de arranque.
    Startup,
    /// Una lectura del monitor del sistema y la puntuación de salud.
    Monitor,
}

fn all_scanners() -> Vec<Box<dyn Scanner>> {
    let mut scanners = hm_system::scanners();
    scanners.extend(hm_dev::scanners());
    scanners.extend(hm_ai::scanners());
    scanners
}

fn scan(ctx: &ScanContext, area: Option<Area>, verbose: bool) -> Vec<Finding> {
    let scanners: Vec<Box<dyn Scanner>> = all_scanners()
        .into_iter()
        .filter(|scanner| area.is_none_or(|area| scanner.area() == area))
        .collect();
    let failures = Mutex::new(Vec::new());
    let raw_count = AtomicUsize::new(0);
    let mut findings = run_scan(&scanners, ctx, &|event| match event {
        ScanEvent::Failed { scanner_id, error } => {
            if let Ok(mut failures) = failures.lock() {
                failures.push(format!("{scanner_id}: {error}"));
            }
        }
        ScanEvent::Finished {
            scanner_id,
            findings,
            elapsed,
        } => {
            raw_count.fetch_add(findings.len(), Ordering::Relaxed);
            if verbose {
                eprintln!(
                    "{}",
                    tf!(
                        "  {scanner_id}: {} hallazgos en {elapsed:.1?}",
                        "  {scanner_id}: {} items in {elapsed:.1?}",
                        findings.len()
                    )
                );
            }
        }
        _ => {}
    });
    let dropped = raw_count.into_inner().saturating_sub(findings.len());
    if verbose && dropped > 0 {
        eprintln!(
            "{}",
            tf!(
                "  {dropped} hallazgos descartados por solaparse con otros",
                "  {dropped} items dropped because they overlap others"
            )
        );
    }
    for failure in failures.into_inner().unwrap_or_default() {
        eprintln!(
            "{}",
            tf!("error en escáner: {failure}", "scanner error: {failure}")
        );
    }
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.size));
    findings
}

fn dump(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// The terminal's locale, in POSIX order: `LC_ALL` over `LC_MESSAGES` over `LANG`.
fn terminal_language() -> Language {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty());
    language_for_locale(locale.as_deref())
}

/// Spanish when the locale is Spanish or there is none, English for any other.
fn language_for_locale(locale: Option<&str>) -> Language {
    match locale {
        Some(locale) if !locale.starts_with("es") => Language::English,
        _ => Language::Spanish,
    }
}

fn main() -> Result<()> {
    set_language(terminal_language());
    hm_core::threads::configure_global_pool();
    let cli = Cli::parse();
    let ctx = ScanContext::for_current_user()?;

    match cli.command {
        Command::Scan { area, json } => {
            let findings = scan(&ctx, area.map(Area::from), !json);
            if json {
                return dump(&findings);
            }
            for area in Area::ALL {
                let in_area: Vec<&Finding> = findings.iter().filter(|f| f.area == area).collect();
                if in_area.is_empty() {
                    continue;
                }
                let total: u64 = in_area.iter().map(|f| f.size).sum();
                println!("\n{} · {}", area.label(), human_size(total));
                for finding in in_area {
                    println!(
                        "  {:>9}  {:<9} {}{}",
                        human_size(finding.size),
                        finding.risk.label(),
                        finding.title,
                        finding
                            .subtitle
                            .as_ref()
                            .map(|s| format!("  ({s})"))
                            .unwrap_or_default()
                    );
                }
            }
            let selected: u64 = findings
                .iter()
                .filter(|f| f.selected_by_default)
                .map(|f| f.size)
                .sum();
            let total: u64 = findings.iter().map(|f| f.size).sum();
            println!(
                "{}",
                tf!(
                    "\nTotal encontrado: {} · Seleccionado por defecto: {}",
                    "\nTotal found: {} · Selected by default: {}",
                    human_size(total),
                    human_size(selected)
                )
            );
        }
        Command::Clean { ids, yes } => {
            if ids.is_empty() {
                bail!(t!(
                    "indica al menos un id (usa `healthymac scan --json` para verlos)",
                    "give at least one id (run `healthymac scan --json` to see them)"
                ));
            }
            let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let findings: Vec<Finding> = scan(&ctx, None, false)
                .into_iter()
                .filter(|finding| wanted.contains(finding.id.as_str()))
                .collect();
            if findings.len() != wanted.len() {
                let found: HashSet<&str> = findings.iter().map(|f| f.id.as_str()).collect();
                for id in wanted.difference(&found) {
                    eprintln!("{}", tf!("no encontrado: {id}", "not found: {id}"));
                }
            }
            let cleaner = Cleaner::new(
                &ctx.home,
                &ctx.config.excluded_paths,
                CleanOptions {
                    mode: ctx.config.removal_mode,
                    dry_run: !yes,
                },
            )
            .with_audit(hm_core::audit::AuditLog::new(
                hm_core::audit::AuditLog::default_path(&ctx.home),
            ));
            let report = cleaner.clean(&findings, &CancelToken::new(), &|_| {});
            for outcome in &report.outcomes {
                println!(
                    "{:?}  {}  {}{}",
                    outcome.status,
                    outcome.finding_id,
                    human_size(outcome.freed_bytes + outcome.trashed_bytes),
                    outcome
                        .message
                        .as_ref()
                        .map(|m| format!("  ({m})"))
                        .unwrap_or_default()
                );
                for error in &outcome.errors {
                    println!("    error: {error}");
                }
            }
            println!(
                "{}",
                tf!(
                    "\n{}Liberado: {} · En la Papelera: {}",
                    "\n{}Freed: {} · In the Trash: {}",
                    if yes {
                        ""
                    } else {
                        t!("[simulación] ", "[dry run] ")
                    },
                    human_size(report.freed_bytes()),
                    human_size(report.trashed_bytes())
                )
            );
        }
        Command::Models => dump(&hm_ai::inventory::scan(&ctx)?)?,
        Command::Runtimes => dump(&hm_ai::runtime::status(&ctx))?,
        Command::Ports => dump(&hm_dev::ports::list_listening()?)?,
        Command::Projects => dump(&hm_dev::projects::list_projects(&ctx)?)?,
        Command::Docker => dump(&hm_dev::docker::summary(&ctx))?,
        Command::Apps => dump(&hm_system::apps::list_apps(&ctx)?)?,
        Command::Startup => dump(&hm_system::startup::list_items(&ctx)?)?,
        Command::Monitor => {
            let mut monitor = hm_system::monitor::Monitor::new();
            monitor.refresh();
            // Rates and CPU usage need two samples.
            std::thread::sleep(Duration::from_millis(500));
            let snapshot = monitor.refresh();
            let reclaimable = hm_system::health::reclaimable_bytes(&scan(&ctx, None, false));
            dump(&serde_json::json!({
                "snapshot": snapshot,
                "health": hm_system::health::compute(&snapshot, reclaimable),
            }))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spanish_locale_or_none_keeps_spanish_and_any_other_is_english() {
        assert_eq!(language_for_locale(None), Language::Spanish);
        assert_eq!(language_for_locale(Some("es_BO.UTF-8")), Language::Spanish);
        assert_eq!(language_for_locale(Some("es")), Language::Spanish);
        assert_eq!(language_for_locale(Some("en_US.UTF-8")), Language::English);
        assert_eq!(language_for_locale(Some("C")), Language::English);
    }
}
