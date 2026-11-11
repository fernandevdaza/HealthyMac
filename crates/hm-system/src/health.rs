//! The health score shown on the dashboard.
//!
//! Transparent by design: the score is 100 minus the listed penalties and nothing else, so what the
//! UI shows always adds up. The most any factor can take off:
//!
//! | Factor | Max |
//! |---|---|
//! | Disk free | 35 |
//! | Memory pressure | 20 |
//! | Thermal state | 15 |
//! | Swap | 10 |
//! | Battery | 10 |
//! | Reclaimable space | 6 |
//!
//! Reclaimable space weighs little on purpose. It is an opportunity, not a fault: a healthy Mac with
//! a few GB of caches should not be told otherwise, which is also why the score stays in the 90s.

use std::path::Path;

use hm_core::fmt::human_size;
use hm_core::{t, tf, Action, Finding, Risk};

use crate::types::*;

const GB: u64 = 1_000_000_000;

/// Scores the machine from a monitor snapshot and the bytes a scan found reclaimable.
pub fn compute(snapshot: &SystemSnapshot, reclaimable_bytes: u64) -> HealthReport {
    let mut factors = vec![
        disk_factor(&snapshot.disks),
        pressure_factor(snapshot.memory.pressure),
        swap_factor(&snapshot.memory),
        thermal_factor(snapshot.thermal),
        reclaimable_factor(reclaimable_bytes),
    ];
    factors.extend(snapshot.battery.as_ref().map(battery_factor));
    factors.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| b.penalty.cmp(&a.penalty))
    });
    let lost: u32 = factors.iter().map(|factor| u32::from(factor.penalty)).sum();
    let score = 100u32.saturating_sub(lost) as u8;
    HealthReport {
        score,
        label: label_for(score).to_string(),
        factors,
    }
}

/// What a scan's findings would really give back, for [`compute`]. Leaves out report-only findings
/// and everything that needs a human decision: a 90 GB video is not "reclaimable" until its owner
/// says so, and counting it would nag people about files they want.
pub fn reclaimable_bytes(findings: &[Finding]) -> u64 {
    findings
        .iter()
        .filter(|finding| finding.risk != Risk::Review && finding.action != Action::ReportOnly)
        .map(|finding| finding.size)
        .sum()
}

fn label_for(score: u8) -> &'static str {
    match score {
        90.. => t!("Excelente", "Excellent"),
        75..=89 => t!("Buena", "Good"),
        50..=74 => t!("Regular", "Fair"),
        _ => t!("Atención", "Needs attention"),
    }
}

fn factor(title: &str, detail: String, severity: Severity, penalty: u8) -> HealthFactor {
    HealthFactor {
        title: title.to_string(),
        detail,
        severity,
        penalty,
    }
}

/// The boot volume. Everything else on the machine lives or dies by this one.
fn boot_disk(disks: &[DiskStats]) -> Option<&DiskStats> {
    disks
        .iter()
        .find(|disk| disk.mount_point == Path::new("/"))
        .or_else(|| {
            disks
                .iter()
                .find(|disk| disk.mount_point == Path::new("/System/Volumes/Data"))
        })
        .or_else(|| disks.iter().find(|disk| !disk.is_removable))
}

/// The percentage matters on a small disk and the absolute figure on a large one: 10 % of 8 TB is
/// plenty, so each threshold is capped, with a floor below which macOS itself starts to struggle.
fn disk_factor(disks: &[DiskStats]) -> HealthFactor {
    let title = t!("Espacio en disco", "Disk space");
    let Some(disk) = boot_disk(disks).filter(|disk| disk.total > 0) else {
        return factor(
            title,
            t!(
                "No se pudo leer el disco de arranque, así que no cuenta para la puntuación.",
                "The startup disk could not be read, so it does not count toward the score."
            )
            .to_string(),
            Severity::Info,
            0,
        );
    };
    let threshold =
        |percent: u64, cap: u64, floor: u64| (disk.total / 100 * percent).min(cap).max(floor);
    let free = disk.available;
    let percent = free.saturating_mul(100) / disk.total;
    let status = tf!(
        "Quedan {} libres de {} ({percent} %).",
        "{} free of {} ({percent}%).",
        human_size(free),
        human_size(disk.total)
    );
    if free < threshold(5, 25 * GB, 5 * GB) {
        factor(
            title,
            tf!(
                "{status} Con tan poco espacio, macOS no puede actualizarse ni usar bien la memoria virtual.",
                "{status} With so little space, macOS cannot update or make good use of virtual memory."
            ),
            Severity::Critical,
            35,
        )
    } else if free < threshold(10, 50 * GB, 10 * GB) {
        factor(
            title,
            tf!(
                "{status} Conviene liberar espacio pronto: el sistema empezará a ir más lento.",
                "{status} Free up space soon: the system will start to slow down."
            ),
            Severity::Warning,
            20,
        )
    } else if free < threshold(20, 100 * GB, 20 * GB) {
        factor(
            title,
            tf!(
                "{status} Todavía hay margen, pero se está acabando.",
                "{status} There is still room, but it is running out."
            ),
            Severity::Info,
            6,
        )
    } else {
        factor(title, status, Severity::Good, 0)
    }
}

fn pressure_factor(pressure: MemoryPressure) -> HealthFactor {
    let title = t!("Presión de memoria", "Memory pressure");
    match pressure {
        MemoryPressure::Normal => factor(
            title,
            t!(
                "Normal: hay memoria de sobra para lo que tienes abierto.",
                "Normal: there is plenty of memory for what you have open."
            )
            .to_string(),
            Severity::Good,
            0,
        ),
        MemoryPressure::Warning => factor(
            title,
            t!(
                "Alta: macOS está comprimiendo memoria para hacer sitio. Cerrar apps que no uses lo \
                 aliviaría.",
                "High: macOS is compressing memory to make room. Quitting apps you are not using \
                 would help."
            )
            .to_string(),
            Severity::Warning,
            10,
        ),
        MemoryPressure::Critical => factor(
            title,
            t!(
                "Crítica: no hay suficiente memoria para lo que tienes abierto y todo irá lento. Cierra \
                 apps cuanto antes.",
                "Critical: there is not enough memory for what you have open, and everything will \
                 be slow. Quit apps as soon as you can."
            )
            .to_string(),
            Severity::Critical,
            20,
        ),
    }
}

/// Some swap is normal on macOS, so only a lot of it, relative to RAM, costs anything real.
fn swap_factor(memory: &MemoryStats) -> HealthFactor {
    let title = t!("Memoria de intercambio", "Swap memory");
    let used = human_size(memory.swap_used);
    let percent = memory
        .swap_used
        .saturating_mul(100)
        .checked_div(memory.total)
        .unwrap_or(0);
    match percent {
        0..=9 => factor(
            title,
            tf!(
                "{used} en uso, lo normal en macOS.",
                "{used} in use, which is normal on macOS."
            ),
            Severity::Good,
            0,
        ),
        10..=24 => factor(
            title,
            tf!(
                "{used} en uso. No es un problema, aunque el sistema ya está tirando de disco.",
                "{used} in use. Not a problem, though the system is already leaning on the disk."
            ),
            Severity::Info,
            1,
        ),
        25..=49 => factor(
            title,
            tf!(
                "{used} en uso, una parte importante de tu memoria. Puede notarse al cambiar de app.",
                "{used} in use, a large share of your memory. You may notice it when switching apps."
            ),
            Severity::Info,
            3,
        ),
        50..=99 => factor(
            title,
            tf!(
                "{used} en uso. El sistema depende bastante del disco, lo que lo hace más lento.",
                "{used} in use. The system relies heavily on the disk, which slows it down."
            ),
            Severity::Warning,
            6,
        ),
        _ => factor(
            title,
            tf!(
                "{used} en uso, más que toda tu memoria. Reiniciar o cerrar apps se notará mucho.",
                "{used} in use, more than all of your memory. Restarting or quitting apps will \
                 make a big difference."
            ),
            Severity::Warning,
            10,
        ),
    }
}

fn thermal_factor(state: ThermalState) -> HealthFactor {
    let title = t!("Temperatura", "Temperature");
    match state {
        ThermalState::Unknown => factor(
            title,
            t!(
                "macOS no ha dado su estado térmico, así que no cuenta para la puntuación.",
                "macOS has not reported its thermal state, so it does not count toward the score."
            )
            .to_string(),
            Severity::Info,
            0,
        ),
        ThermalState::Nominal => factor(
            title,
            t!("Normal, sin limitación.", "Normal, not throttled.").to_string(),
            Severity::Good,
            0,
        ),
        ThermalState::Fair => factor(
            title,
            t!(
                "Algo elevada, pero sin que afecte al rendimiento todavía.",
                "Somewhat high, but not affecting performance yet."
            )
            .to_string(),
            Severity::Info,
            2,
        ),
        ThermalState::Serious => factor(
            title,
            t!(
                "Alta: macOS está reduciendo el rendimiento para enfriarse.",
                "High: macOS is reducing performance to cool down."
            )
            .to_string(),
            Severity::Warning,
            8,
        ),
        ThermalState::Critical => factor(
            title,
            t!(
                "Muy alta: el rendimiento está muy limitado. Deja que el Mac descanse y revisa qué lo \
                 está forzando.",
                "Very high: performance is heavily limited. Let the Mac rest and check what is \
                 pushing it so hard."
            )
            .to_string(),
            Severity::Critical,
            15,
        ),
    }
}

fn battery_factor(battery: &BatteryStats) -> HealthFactor {
    let title = t!("Batería", "Battery");
    let cycles = battery
        .cycle_count
        .map(|cycles| tf!(" {cycles} ciclos.", " {cycles} cycles."))
        .unwrap_or_default();
    let capacity = battery
        .health_percent
        .map(|health| {
            tf!(
                "Capacidad máxima del {health} %.",
                "Maximum capacity {health}%."
            )
        })
        .unwrap_or_else(|| {
            t!("Capacidad máxima desconocida.", "Maximum capacity unknown.").to_string()
        });
    let verdict = battery.condition.as_deref().unwrap_or("Normal");
    if !verdict.eq_ignore_ascii_case("normal") && !verdict.eq_ignore_ascii_case("good") {
        return factor(
            title,
            tf!(
                "macOS indica «{verdict}». {capacity}{cycles} Conviene llevarla a revisar.",
                "macOS reports “{verdict}”. {capacity}{cycles} Consider having it checked."
            ),
            Severity::Critical,
            10,
        );
    }
    match battery.health_percent {
        Some(health) if health < 70 => factor(
            title,
            tf!(
                "{capacity}{cycles} Se notará en la autonomía, y quizá sea hora de cambiarla.",
                "{capacity}{cycles} You will notice it in battery life, and it may be time to \
                 replace it."
            ),
            Severity::Warning,
            7,
        ),
        Some(health) if health < 80 => factor(
            title,
            tf!(
                "{capacity}{cycles} Ya aguanta bastante menos que nueva.",
                "{capacity}{cycles} It already lasts quite a bit less than when it was new."
            ),
            Severity::Warning,
            4,
        ),
        _ => factor(title, format!("{capacity}{cycles}"), Severity::Good, 0),
    }
}

fn reclaimable_factor(bytes: u64) -> HealthFactor {
    let title = t!("Espacio recuperable", "Reclaimable space");
    let size = human_size(bytes);
    let (severity, penalty) = match bytes {
        b if b < 2 * GB => (Severity::Good, 0),
        b if b < 20 * GB => (Severity::Info, 1),
        b if b < 50 * GB => (Severity::Info, 2),
        b if b < 100 * GB => (Severity::Info, 4),
        _ => (Severity::Info, 6),
    };
    let detail = if penalty == 0 {
        tf!(
            "Solo {size} de cachés y archivos que se pueden regenerar.",
            "Only {size} of caches and files that can be regenerated."
        )
    } else {
        tf!(
            "{size} de cachés y archivos que se pueden regenerar. Limpiarlos no pierde nada importante.",
            "{size} of caches and files that can be regenerated. Cleaning them loses nothing important."
        )
    };
    factor(title, detail, severity, penalty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hm_core::Area;
    use std::path::PathBuf;
    use std::time::SystemTime;

    const TB: u64 = 1_000 * GB;

    fn healthy() -> SystemSnapshot {
        SystemSnapshot {
            taken_at: SystemTime::now(),
            cpu: CpuStats::default(),
            memory: MemoryStats {
                total: 48 * GB,
                swap_used: GB,
                ..MemoryStats::default()
            },
            disks: vec![disk("/", TB, 500 * GB)],
            battery: Some(BatteryStats {
                health_percent: Some(92),
                cycle_count: Some(151),
                condition: Some("Normal".to_string()),
                ..BatteryStats::default()
            }),
            thermal: ThermalState::Nominal,
            network: NetworkStats::default(),
            uptime_secs: 3_600,
            top_by_cpu: Vec::new(),
            top_by_memory: Vec::new(),
        }
    }

    fn disk(mount: &str, total: u64, available: u64) -> DiskStats {
        DiskStats {
            name: "Macintosh HD".to_string(),
            mount_point: PathBuf::from(mount),
            total,
            available,
            ..DiskStats::default()
        }
    }

    fn penalty_of(report: &HealthReport, title: &str) -> u8 {
        report
            .factors
            .iter()
            .find(|factor| factor.title == title)
            .map(|factor| factor.penalty)
            .expect("factor present")
    }

    #[test]
    fn a_healthy_mac_with_some_caches_scores_in_the_nineties() {
        let report = compute(&healthy(), 10 * GB);
        assert!((90..=100).contains(&report.score), "{}", report.score);
        assert_eq!(report.label, "Excelente");
    }

    #[test]
    fn the_score_is_exactly_100_minus_the_penalties() {
        let mut snapshot = healthy();
        snapshot.memory.pressure = MemoryPressure::Warning;
        snapshot.thermal = ThermalState::Fair;
        let report = compute(&snapshot, 60 * GB);
        let lost: u32 = report.factors.iter().map(|f| u32::from(f.penalty)).sum();
        assert_eq!(u32::from(report.score), 100 - lost);
        assert!(lost > 0);
    }

    #[test]
    fn every_factor_explains_itself_and_the_worst_comes_first() {
        let mut snapshot = healthy();
        snapshot.disks = vec![disk("/", TB, 8 * GB)];
        snapshot.memory.pressure = MemoryPressure::Warning;
        let report = compute(&snapshot, 0);
        assert!(report
            .factors
            .iter()
            .all(|factor| !factor.detail.is_empty()));
        assert_eq!(report.factors[0].severity, Severity::Critical);
        assert_eq!(report.factors[0].title, "Espacio en disco");
        for pair in report.factors.windows(2) {
            assert!(pair[0].severity >= pair[1].severity);
        }
    }

    #[test]
    fn even_the_worst_case_stays_in_range() {
        let snapshot = SystemSnapshot {
            disks: vec![disk("/", TB, 0)],
            memory: MemoryStats {
                total: 8 * GB,
                swap_used: 64 * GB,
                pressure: MemoryPressure::Critical,
                ..MemoryStats::default()
            },
            thermal: ThermalState::Critical,
            battery: Some(BatteryStats {
                condition: Some("Service Recommended".to_string()),
                ..BatteryStats::default()
            }),
            ..healthy()
        };
        let report = compute(&snapshot, u64::MAX);
        assert_eq!(report.score, 4);
        assert_eq!(report.label, "Atención");
    }

    #[test]
    fn labels_follow_the_score() {
        assert_eq!(label_for(100), "Excelente");
        assert_eq!(label_for(90), "Excelente");
        assert_eq!(label_for(89), "Buena");
        assert_eq!(label_for(75), "Buena");
        assert_eq!(label_for(74), "Regular");
        assert_eq!(label_for(50), "Regular");
        assert_eq!(label_for(49), "Atención");
        assert_eq!(label_for(0), "Atención");
    }

    #[test]
    fn disk_thresholds_scale_but_stay_sensible() {
        let penalty = |total, free| disk_factor(&[disk("/", total, free)]).penalty;
        // 256 GB disk: 25 GB free is 10 %, right on the edge of a warning.
        assert_eq!(penalty(256 * GB, 60 * GB), 0);
        assert_eq!(penalty(256 * GB, 40 * GB), 6);
        assert_eq!(penalty(256 * GB, 20 * GB), 20);
        assert_eq!(penalty(256 * GB, 8 * GB), 35);
        // 8 TB disk: 200 GB free is only 2.5 %, but it is not a problem.
        assert_eq!(penalty(8 * TB, 200 * GB), 0);
    }

    #[test]
    fn a_missing_disk_costs_nothing_but_says_so() {
        let factor = disk_factor(&[]);
        assert_eq!(factor.penalty, 0);
        assert_eq!(factor.severity, Severity::Info);
        assert!(factor.detail.contains("No se pudo leer"));
    }

    #[test]
    fn prefers_the_boot_volume_over_external_disks() {
        let disks = [disk("/Volumes/Backup", TB, 900 * GB), disk("/", TB, 8 * GB)];
        assert_eq!(disk_factor(&disks).penalty, 35);
    }

    #[test]
    fn swap_scales_with_memory_and_handles_zero_ram() {
        let mut memory = MemoryStats {
            total: 48 * GB,
            swap_used: GB,
            ..MemoryStats::default()
        };
        assert_eq!(swap_factor(&memory).penalty, 0);
        memory.swap_used = 60 * GB;
        assert_eq!(swap_factor(&memory).penalty, 10);
        memory.total = 0;
        assert_eq!(swap_factor(&memory).penalty, 0);
    }

    #[test]
    fn unknown_thermal_state_costs_nothing() {
        assert_eq!(thermal_factor(ThermalState::Unknown).penalty, 0);
        assert_eq!(
            thermal_factor(ThermalState::Serious).severity,
            Severity::Warning
        );
    }

    #[test]
    fn a_desktop_with_no_battery_has_no_battery_factor() {
        let mut snapshot = healthy();
        snapshot.battery = None;
        let report = compute(&snapshot, 0);
        assert!(report
            .factors
            .iter()
            .all(|factor| factor.title != "Batería"));
    }

    #[test]
    fn battery_verdict_from_macos_outranks_the_percentage() {
        let report = compute(
            &SystemSnapshot {
                battery: Some(BatteryStats {
                    health_percent: Some(95),
                    condition: Some("Service Recommended".to_string()),
                    ..BatteryStats::default()
                }),
                ..healthy()
            },
            0,
        );
        assert_eq!(penalty_of(&report, "Batería"), 10);
    }

    #[test]
    fn worn_battery_is_penalised_by_steps() {
        let with_health = |health| {
            battery_factor(&BatteryStats {
                health_percent: Some(health),
                ..BatteryStats::default()
            })
            .penalty
        };
        assert_eq!(with_health(92), 0);
        assert_eq!(with_health(75), 4);
        assert_eq!(with_health(60), 7);
    }

    #[test]
    fn reclaimable_only_counts_what_needs_no_decision() {
        let safe = Finding::new("t", Area::System, "a", "a")
            .risk(Risk::Safe)
            .size(100)
            .action(Action::Trash { paths: Vec::new() });
        let moderate = Finding::new("t", Area::System, "b", "b")
            .risk(Risk::Moderate)
            .size(10)
            .action(Action::Trash { paths: Vec::new() });
        let review = Finding::new("t", Area::Files, "c", "c")
            .size(90_000)
            .action(Action::Trash { paths: Vec::new() });
        let report = Finding::new("t", Area::System, "d", "d")
            .risk(Risk::Safe)
            .size(70_000);
        assert_eq!(reclaimable_bytes(&[safe, moderate, review, report]), 110);
    }
}
