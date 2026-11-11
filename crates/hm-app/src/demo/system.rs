//! Invented monitor readings, apps, startup items and a disk tree.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hm_core::audit::AuditEntry;
use hm_core::fmt::human_size;
use hm_core::{t, tf, Area, CleanOutcome, CleanStatus, Finding, Recovery, Risk};
use hm_system::types::*;

use super::{days_ago, hours_ago, GB, GIB, HOME, MB};

fn wave(tick: u64, period: f32, phase: f32) -> f32 {
    ((tick as f32 / period + phase).sin() + 1.0) / 2.0
}

pub fn snapshot(tick: u64) -> SystemSnapshot {
    let usage = 14.0 + 26.0 * wave(tick, 6.0, 0.0) + 16.0 * wave(tick, 2.3, 1.0);
    let per_core: Vec<f32> = (0..14)
        .map(|core| {
            let base = if core < 4 { 8.0 } else { 18.0 };
            (base + 58.0 * wave(tick, 3.0 + core as f32 * 0.7, core as f32)).min(100.0)
        })
        .collect();
    let total = 64 * GIB;
    let app = 21 * GIB + (3.0 * GIB as f32 * wave(tick, 20.0, 0.3)) as u64;
    let wired = 7 * GIB;
    let compressed = 4 * GIB + (GIB as f32 * wave(tick, 30.0, 1.1)) as u64;
    SystemSnapshot {
        taken_at: SystemTime::now(),
        cpu: CpuStats {
            brand: "Apple M4 Pro".to_string(),
            usage_percent: usage,
            per_core_percent: per_core,
            performance_cores: 10,
            efficiency_cores: 4,
            load_average: [3.42, 2.91, 2.57],
            core_clusters: vec![
                CoreCluster {
                    name: "Performance".to_string(),
                    logical_cpus: 10,
                },
                CoreCluster {
                    name: "Efficiency".to_string(),
                    logical_cpus: 4,
                },
            ],
        },
        memory: MemoryStats {
            total,
            used: app + wired + compressed + 2 * GIB,
            app,
            wired,
            compressed,
            swap_total: 3 * GIB,
            swap_used: 840 * MB,
            pressure: MemoryPressure::Normal,
            gpu_wired_limit: Some(48 * GIB),
        },
        disks: vec![DiskStats {
            name: "Macintosh HD".to_string(),
            mount_point: PathBuf::from("/"),
            total: 994 * GB,
            available: 118 * GB,
            is_removable: false,
            file_system: "apfs".to_string(),
        }],
        battery: Some(BatteryStats {
            percent: 82,
            charging: false,
            plugged_in: true,
            cycle_count: Some(143),
            health_percent: Some(96),
            condition: Some("Normal".to_string()),
            minutes_remaining: None,
        }),
        thermal: ThermalState::Nominal,
        network: NetworkStats {
            rx_bytes_per_sec: (2.4e6 * wave(tick, 4.0, 0.0) as f64 + 120_000.0) as u64,
            tx_bytes_per_sec: (5.5e5 * wave(tick, 5.0, 1.0) as f64 + 30_000.0) as u64,
        },
        uptime_secs: 6 * 86_400 + 4 * 3_600,
        top_by_cpu: vec![
            process(
                812,
                "ollama runner",
                41.0 + 10.0 * wave(tick, 3.0, 0.0),
                21 * GIB,
            ),
            process(1204, "Xcode", 18.2, 4_100 * MB),
            process(233, "WindowServer", 12.4, 900 * MB),
            process(2210, "Google Chrome Helper (Renderer)", 8.7, 1_300 * MB),
            process(3401, "rust-analyzer", 6.1, 3_200 * MB),
            process(99, "mds_stores", 3.2, 310 * MB),
        ],
        top_by_memory: vec![
            process(812, "ollama runner", 41.0, 21 * GIB),
            process(1204, "Xcode", 18.2, 4_100 * MB),
            process(3401, "rust-analyzer", 6.1, 3_200 * MB),
            process(2210, "Google Chrome Helper (Renderer)", 8.7, 1_300 * MB),
            process(4100, "Slack Helper", 1.1, 1_100 * MB),
            process(233, "WindowServer", 12.4, 900 * MB),
        ],
    }
}

fn process(pid: u32, name: &str, cpu: f32, memory: u64) -> ProcessStats {
    ProcessStats {
        pid,
        name: name.to_string(),
        cpu_percent: cpu,
        memory_bytes: memory,
    }
}

/// `reclaimable` is what `hm_system::health::reclaimable_bytes` counts, and the factor it adds reads
/// like the real one, so the demo shows what the app would.
pub fn health(reclaimable: u64) -> HealthReport {
    let mut factors = vec![
        HealthFactor {
            title: tf!(
                "El disco va justo: {} libres de {}",
                "Disk space is running low: {} free of {}",
                human_size(118 * GB),
                human_size(994 * GB)
            ),
            detail: t!(
                "Por debajo del 15 % libre, macOS empieza a ir más lento y las actualizaciones pueden fallar.",
                "Below 15% free, macOS starts to slow down and updates can fail."
            )
            .to_string(),
            severity: Severity::Warning,
            penalty: 14,
        },
        HealthFactor {
            title: t!("El intercambio está en uso", "Swap is in use").to_string(),
            detail: tf!(
                "{} en disco. Nada grave, pero ese modelo cargado está apretando la memoria.",
                "{} on disk. Nothing serious, but that loaded model is putting pressure on memory.",
                human_size(840 * MB)
            ),
            severity: Severity::Info,
            penalty: 3,
        },
    ];
    let mut score: u8 = 83;
    if reclaimable > 10 * GB {
        factors.insert(
            1,
            HealthFactor {
                title: t!("Espacio recuperable", "Reclaimable space").to_string(),
                detail: tf!(
                    "{} de cachés y archivos que se pueden regenerar. Limpiarlos no pierde nada importante.",
                    "{} of caches and files that can be rebuilt. Cleaning them up loses nothing important.",
                    human_size(reclaimable)
                ),
                severity: Severity::Info,
                penalty: 6,
            },
        );
        score = 77;
    }
    HealthReport {
        score,
        label: t!("Buena", "Good").to_string(),
        factors,
    }
}

/// The demo's apps are invented, but when one of them is installed on this Mac its real icon is
/// shown, read from the bundle on disk the same way the real Apps page does. Nothing is downloaded:
/// the demo stays as offline as the rest of the app.
fn installed_icon(name: &str) -> Option<PathBuf> {
    // The demo names Photos in the interface language; the bundle is always Photos.app.
    let bundle = if name == "Fotos" { "Photos" } else { name };
    [
        "/Applications",
        "/Applications/Utilities",
        "/System/Applications",
        "/System/Applications/Utilities",
    ]
    .iter()
    .map(|dir| Path::new(dir).join(format!("{bundle}.app")))
    .find(|path| path.is_dir())
    .and_then(|path| hm_system::apps::icon_path_of(&path))
}

#[allow(clippy::too_many_arguments)]
fn app(
    name: &str,
    version: &str,
    size: u64,
    leftovers: &[(&str, u64, &str)],
    days: u64,
    running: bool,
    is_system: bool,
    arch: AppArch,
) -> AppInfo {
    AppInfo {
        name: name.to_string(),
        bundle_id: Some(format!(
            "com.example.{}",
            name.to_lowercase().replace(' ', "")
        )),
        version: Some(version.to_string()),
        path: PathBuf::from(format!("/Applications/{name}.app")),
        size,
        leftovers: leftovers
            .iter()
            .map(|(path, size, kind)| Leftover {
                path: PathBuf::from(format!("{HOME}/Library/{path}")),
                size: *size,
                kind: kind.to_string(),
            })
            .collect(),
        last_used: days_ago(days),
        running,
        is_system,
        arch,
        icon_path: installed_icon(name),
        executable: Some(name.to_string()),
        uninstall_blocked: is_system
            .then(|| t!("Forma parte de macOS.", "It's part of macOS.").to_string()),
    }
}

pub fn apps() -> Vec<AppInfo> {
    vec![
        app(
            "Xcode",
            "16.2",
            12_400 * MB,
            &[(
                "Developer/Xcode/UserData",
                4_200 * MB,
                t!("Datos de usuario", "User data"),
            )],
            0,
            true,
            false,
            AppArch::Universal,
        ),
        app(
            "Docker",
            "4.37.1",
            2_100 * MB,
            &[(
                "Containers/com.docker.docker",
                58_000 * MB,
                t!("Contenedor", "Container"),
            )],
            3,
            false,
            false,
            AppArch::AppleSilicon,
        ),
        app(
            "LM Studio",
            "0.3.9",
            1_400 * MB,
            &[(
                "Application Support/LM Studio",
                620 * MB,
                t!("Soporte de aplicación", "Application Support"),
            )],
            1,
            false,
            false,
            AppArch::AppleSilicon,
        ),
        app(
            "Final Cut Pro",
            "11.0",
            7_800 * MB,
            &[(
                "Caches/com.apple.FinalCut",
                1_800 * MB,
                t!("Caché", "Cache"),
            )],
            92,
            false,
            false,
            AppArch::Universal,
        ),
        app(
            "Microsoft Excel",
            "16.93",
            2_400 * MB,
            &[(
                "Containers/com.microsoft.Excel",
                340 * MB,
                t!("Contenedor", "Container"),
            )],
            15,
            false,
            false,
            AppArch::Universal,
        ),
        app(
            "Adobe Photoshop 2022",
            "23.5",
            4_900 * MB,
            &[
                ("Caches/Adobe", 2_600 * MB, t!("Caché", "Cache")),
                (
                    "Preferences/Adobe",
                    4 * MB,
                    t!("Preferencias", "Preferences"),
                ),
            ],
            410,
            false,
            false,
            AppArch::Intel,
        ),
        app(
            "Steam",
            "3.6",
            780 * MB,
            &[(
                "Application Support/Steam",
                48_000 * MB,
                t!("Soporte de aplicación", "Application Support"),
            )],
            44,
            false,
            false,
            AppArch::Intel,
        ),
        app(
            "Google Chrome",
            "131.0",
            1_900 * MB,
            &[(
                "Application Support/Google/Chrome",
                3_400 * MB,
                t!("Soporte de aplicación", "Application Support"),
            )],
            0,
            true,
            false,
            AppArch::Universal,
        ),
        app(
            "Slack",
            "4.41",
            580 * MB,
            &[(
                "Application Support/Slack",
                1_200 * MB,
                t!("Soporte de aplicación", "Application Support"),
            )],
            0,
            true,
            false,
            AppArch::AppleSilicon,
        ),
        app(
            "Notion",
            "3.14",
            460 * MB,
            &[],
            7,
            false,
            false,
            AppArch::Universal,
        ),
        app(
            "Safari",
            "18.2",
            18 * MB,
            &[],
            0,
            false,
            true,
            AppArch::Universal,
        ),
        // Finder shows Apple's own apps under their localized names.
        app(
            t!("Fotos", "Photos"),
            "10.0",
            62 * MB,
            &[],
            12,
            false,
            true,
            AppArch::Universal,
        ),
        app(
            "Spotify",
            "1.2.52",
            410 * MB,
            &[(
                "Caches/com.spotify.client",
                7_400 * MB,
                t!("Caché", "Cache"),
            )],
            0,
            true,
            false,
            AppArch::AppleSilicon,
        ),
        app(
            "VLC",
            "3.0.21",
            190 * MB,
            &[],
            203,
            false,
            false,
            AppArch::Universal,
        ),
    ]
}

pub fn uninstall_finding(app: &AppInfo, include_leftovers: bool) -> Finding {
    let mut paths = vec![app.path.clone()];
    let mut size = app.size;
    if include_leftovers {
        for leftover in &app.leftovers {
            paths.push(leftover.path.clone());
            size += leftover.size;
        }
    }
    Finding::new(
        "apps.uninstall",
        Area::Apps,
        &app.name,
        tf!("Desinstalar {}", "Uninstall {}", app.name),
    )
    .subtitle(app.path.display().to_string())
    .explanation(t!(
        "Mueve la app y sus datos a la Papelera.",
        "Moves the app and its data to the Trash."
    ))
    .size(size)
    .items(paths.len() as u64)
    .risk(Risk::Review)
    .recovery(Recovery::Redownload)
    .action(hm_core::Action::Trash { paths })
    .blocked_by(&[app.name.as_str()])
}

pub fn startup() -> Vec<StartupItem> {
    let item = |label: &str, kind, program: &str, run_at_load, keep_alive, disabled, orphaned| {
        let directory = match kind {
            StartupKind::UserAgent => format!("{HOME}/Library/LaunchAgents"),
            StartupKind::GlobalAgent => "/Library/LaunchAgents".to_string(),
            StartupKind::GlobalDaemon => "/Library/LaunchDaemons".to_string(),
        };
        StartupItem {
            label: label.to_string(),
            kind,
            plist_path: PathBuf::from(format!("{directory}/{label}.plist")),
            program: Some(program.to_string()),
            run_at_load,
            keep_alive,
            disabled,
            orphaned,
            malformed: false,
        }
    };
    vec![
        item("com.google.keystone.agent", StartupKind::UserAgent, "/Users/ana/Library/Google/GoogleSoftwareUpdate/GoogleSoftwareUpdate.bundle/Contents/Resources/GoogleSoftwareUpdateAgent.app/Contents/MacOS/GoogleSoftwareUpdateAgent", true, false, false, false),
        item("homebrew.mxcl.postgresql@16", StartupKind::UserAgent, "/opt/homebrew/opt/postgresql@16/bin/postgres", true, true, false, false),
        item("homebrew.mxcl.ollama", StartupKind::UserAgent, "/opt/homebrew/opt/ollama/bin/ollama", true, true, false, false),
        item("com.bohemiancoding.sketch.helper", StartupKind::UserAgent, "/Applications/Sketch.app/Contents/Helpers/Sketch Helper.app", true, false, false, true),
        item("com.docker.vmnetd", StartupKind::GlobalDaemon, "/Library/PrivilegedHelperTools/com.docker.vmnetd", true, true, false, false),
        item("com.adobe.ccxprocess", StartupKind::GlobalAgent, "/Applications/Utilities/Adobe Creative Cloud Experience/CCXProcess/CCXProcess.app", true, false, true, false),
        item("com.teamviewer.service", StartupKind::GlobalDaemon, "/Applications/TeamViewer.app/Contents/Library/TeamViewerService", true, true, false, true),
        item("com.microsoft.update.agent", StartupKind::GlobalAgent, "/Library/Application Support/Microsoft/MAU2.0/Microsoft AutoUpdate.app", false, false, false, false),
    ]
}

fn folder(name: &str, parent: &str, children: Vec<DirNode>, other: u64) -> DirNode {
    let path = format!("{parent}/{name}");
    let size = children.iter().map(|child| child.size).sum::<u64>() + other;
    DirNode {
        name: name.to_string(),
        path: PathBuf::from(path),
        size,
        is_dir: true,
        file_count: size / 90_000,
        last_modified: days_ago(size % 40),
        children,
        other_size: other,
        partial: false,
    }
}

fn leaf(name: &str, parent: &str, size: u64) -> DirNode {
    DirNode {
        name: name.to_string(),
        path: PathBuf::from(format!("{parent}/{name}")),
        size,
        is_dir: !name.contains('.'),
        file_count: size / 90_000 + 1,
        last_modified: days_ago(size % 90),
        children: Vec::new(),
        other_size: 0,
        partial: false,
    }
}

pub fn disk_tree() -> DirNode {
    let home = HOME;
    let library = format!("{home}/Library");
    let coding = format!("{home}/Coding");
    folder(
        "ana",
        "/Users",
        vec![
            folder(
                "Library",
                home,
                vec![
                    folder(
                        "Containers",
                        &library,
                        vec![
                            leaf(
                                "com.docker.docker",
                                &format!("{library}/Containers"),
                                58 * GB,
                            ),
                            leaf("com.apple.mail", &format!("{library}/Containers"), 6 * GB),
                            leaf(
                                "com.microsoft.Excel",
                                &format!("{library}/Containers"),
                                340 * MB,
                            ),
                        ],
                        7 * GB,
                    ),
                    folder(
                        "Developer",
                        &library,
                        vec![
                            leaf("Xcode", &format!("{library}/Developer"), 62 * GB),
                            leaf("CoreSimulator", &format!("{library}/Developer"), 34 * GB),
                        ],
                        2 * GB,
                    ),
                    folder(
                        "Application Support",
                        &library,
                        vec![
                            leaf("Steam", &format!("{library}/Application Support"), 48 * GB),
                            leaf(
                                "Google",
                                &format!("{library}/Application Support"),
                                3_400 * MB,
                            ),
                            leaf(
                                "Slack",
                                &format!("{library}/Application Support"),
                                1_200 * MB,
                            ),
                            leaf(
                                "LM Studio",
                                &format!("{library}/Application Support"),
                                620 * MB,
                            ),
                        ],
                        9 * GB,
                    ),
                    folder(
                        "Caches",
                        &library,
                        vec![
                            leaf(
                                "com.spotify.client",
                                &format!("{library}/Caches"),
                                7_400 * MB,
                            ),
                            leaf("Google", &format!("{library}/Caches"), 3_100 * MB),
                            leaf("Homebrew", &format!("{library}/Caches"), 2_900 * MB),
                        ],
                        4 * GB,
                    ),
                ],
                5 * GB,
            ),
            folder(
                "Coding",
                home,
                vec![
                    folder(
                        "motor-audio",
                        &coding,
                        vec![
                            leaf("target", &format!("{coding}/motor-audio"), 18 * GB),
                            leaf("assets", &format!("{coding}/motor-audio"), 6 * GB),
                        ],
                        400 * MB,
                    ),
                    folder(
                        "tienda-web",
                        &coding,
                        vec![
                            leaf("node_modules", &format!("{coding}/tienda-web"), 2_100 * MB),
                            leaf("admin", &format!("{coding}/tienda-web"), 1_500 * MB),
                        ],
                        200 * MB,
                    ),
                    folder(
                        "herramientas",
                        &coding,
                        vec![leaf(
                            "rsync-mejor",
                            &format!("{coding}/herramientas"),
                            4_300 * MB,
                        )],
                        900 * MB,
                    ),
                ],
                3 * GB,
            ),
            folder(
                ".ollama",
                home,
                vec![folder(
                    "models",
                    &format!("{home}/.ollama"),
                    vec![leaf("blobs", &format!("{home}/.ollama/models"), 112 * GB)],
                    40 * MB,
                )],
                0,
            ),
            folder(
                ".lmstudio",
                home,
                vec![leaf("models", &format!("{home}/.lmstudio"), 74 * GB)],
                800 * MB,
            ),
            folder(
                ".cache",
                home,
                vec![
                    leaf("huggingface", &format!("{home}/.cache"), 41 * GB),
                    leaf("pip", &format!("{home}/.cache"), 2 * GB),
                ],
                300 * MB,
            ),
            folder(
                "Movies",
                home,
                vec![
                    leaf(
                        "Grabación de pantalla - reunión.mov",
                        &format!("{home}/Movies"),
                        24_800 * MB,
                    ),
                    leaf(
                        "Proyecto Final Cut.fcpbundle",
                        &format!("{home}/Movies"),
                        31 * GB,
                    ),
                ],
                GB,
            ),
            folder(
                "Pictures",
                home,
                vec![leaf(
                    "Biblioteca de Fotos.photoslibrary",
                    &format!("{home}/Pictures"),
                    88 * GB,
                )],
                500 * MB,
            ),
            folder(
                "Downloads",
                home,
                vec![
                    leaf(
                        "ubuntu-24.04-desktop-arm64.iso",
                        &format!("{home}/Downloads"),
                        6_100 * MB,
                    ),
                    leaf("Docker.dmg", &format!("{home}/Downloads"), 1_400 * MB),
                ],
                3 * GB,
            ),
            leaf("Documents", home, 12 * GB),
            leaf("Desktop", home, 19 * GB),
        ],
        2 * GB,
    )
}

pub fn history() -> Vec<AuditEntry> {
    let entry = |title: &str,
                 freed: u64,
                 trashed: u64,
                 status: CleanStatus,
                 hours: u64,
                 message: Option<&str>| AuditEntry {
        at: hours_ago(hours).unwrap_or_else(SystemTime::now),
        title: title.to_string(),
        outcome: CleanOutcome {
            finding_id: title.to_string(),
            status,
            freed_bytes: freed,
            trashed_bytes: trashed,
            freed_is_estimate: false,
            message: message.map(str::to_string),
            errors: Vec::new(),
        },
    };
    vec![
        entry(
            t!("Caché de npm", "npm cache"),
            0,
            5_100 * MB,
            CleanStatus::Done,
            3,
            None,
        ),
        entry(
            t!("Homebrew: descargas viejas", "Homebrew: old downloads"),
            2_400 * MB,
            0,
            CleanStatus::Done,
            3,
            None,
        ),
        entry(
            "DerivedData",
            0,
            0,
            CleanStatus::Skipped,
            3,
            Some(t!("Xcode está abierto", "Xcode is open")),
        ),
        entry(
            t!("Deduplicar qwen3:32b", "Deduplicate qwen3:32b"),
            19_000 * MB,
            0,
            CleanStatus::Done,
            76,
            None,
        ),
        entry(
            t!("Vaciar la Papelera", "Empty Trash"),
            31_000 * MB,
            0,
            CleanStatus::Done,
            76,
            None,
        ),
        entry(
            t!("Registros de apps", "App logs"),
            0,
            980 * MB,
            CleanStatus::Partial,
            240,
            None,
        ),
    ]
}
