//! Time Machine local snapshots. Report-only: macOS manages them, and removing them is not a
//! decision a cleaner should make.

use std::time::Duration;

use anyhow::{anyhow, Result};
use hm_core::{exec, t, tf, Area, Finding, ScanContext, Scanner};

const SCANNER_ID: &str = "sys.snapshots";
const TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct LocalSnapshots;

impl Scanner for LocalSnapshots {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Instantáneas de Time Machine", "Time Machine snapshots")
    }
    fn description(&self) -> &'static str {
        t!(
            "Copias locales que macOS guarda y libera por su cuenta cuando hace falta espacio.",
            "Local copies that macOS keeps and frees on its own when space is needed."
        )
    }
    fn area(&self) -> Area {
        Area::System
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let tmutil = ctx
            .which("tmutil")
            .ok_or_else(|| anyhow!(t!("no se encontró tmutil", "tmutil was not found")))?;
        // Listing only. `listlocalsnapshots` changes nothing.
        let output = exec::run(&tmutil, &["listlocalsnapshots", "/"], TIMEOUT)?;
        if !output.success() {
            return Err(anyhow!(tf!(
                "tmutil falló: {}",
                "tmutil failed: {}",
                output
                    .stderr
                    .lines()
                    .next()
                    .unwrap_or(t!("sin detalles", "no details"))
            )));
        }
        Ok(findings_from_output(&output.stdout))
    }
}

/// The date part of each snapshot, oldest first: `2026-11-05-093000`.
fn parse_snapshots(output: &str) -> Vec<String> {
    let mut dates: Vec<String> = output
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("com.apple.TimeMachine.")?
                .strip_suffix(".local")
                .map(str::to_string)
        })
        .collect();
    dates.sort();
    dates
}

/// `2026-11-05-093000` to `2026-11-05`.
fn day_of(date: &str) -> &str {
    date.get(..10).unwrap_or(date)
}

pub(crate) fn findings_from_output(output: &str) -> Vec<Finding> {
    let dates = parse_snapshots(output);
    let (Some(oldest), Some(newest)) = (dates.first(), dates.last()) else {
        return Vec::new();
    };
    let title = if dates.len() == 1 {
        t!(
            "1 instantánea local de Time Machine",
            "1 local Time Machine snapshot"
        )
        .to_string()
    } else {
        tf!(
            "{} instantáneas locales de Time Machine",
            "{} local Time Machine snapshots",
            dates.len()
        )
    };
    vec![Finding::new(SCANNER_ID, Area::System, "instantaneas", title)
        .items(dates.len() as u64)
        .explanation(tf!(
            "Van del {} al {}. macOS no informa de cuánto ocupan, pero las libera solo cuando falta \
             espacio, y Time Machine las necesita para restaurar sin tener el disco de copias a \
             mano. Por eso HealthyMac solo las muestra.",
            "They range from {} to {}. macOS does not report how much space they take, but it frees \
             them on its own when space runs low, and Time Machine needs them to restore without \
             the backup disk at hand. That is why HealthyMac only shows them.",
            day_of(oldest),
            day_of(newest)
        ))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use hm_core::Action;

    const OUTPUT: &str = "Snapshots for disk /:\n\
        com.apple.TimeMachine.2026-11-05-093000.local\n\
        com.apple.TimeMachine.2026-11-03-083000.local\n\
        com.apple.os.update-ABC123\n";

    #[test]
    fn parses_and_sorts_only_time_machine_snapshots() {
        assert_eq!(
            parse_snapshots(OUTPUT),
            ["2026-11-03-083000", "2026-11-05-093000"]
        );
    }

    #[test]
    fn no_snapshots_means_no_finding() {
        assert!(findings_from_output("Snapshots for disk /:\n").is_empty());
        assert!(findings_from_output("").is_empty());
    }

    #[test]
    fn is_report_only_and_claims_no_size() {
        let findings = findings_from_output(OUTPUT);
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.action, Action::ReportOnly);
        assert_eq!(finding.size, 0);
        assert_eq!(finding.item_count, 2);
        assert!(finding.title.starts_with("2 instantáneas"));
        assert!(finding.explanation.contains("2026-11-03"));
    }

    #[test]
    fn garbage_output_is_ignored() {
        assert!(findings_from_output("com.apple.TimeMachine.\nrandom\n\n").is_empty());
    }
}
