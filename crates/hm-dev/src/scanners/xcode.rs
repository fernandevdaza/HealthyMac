//! Xcode's build products, archives, device support files and simulators.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use hm_core::fmt::{age_days, human_size};
use hm_core::{fsutil, t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};
use plist::Value;
use rayon::prelude::*;

use super::{table_findings, CacheSpec, Fallback, HasTool, Removal};
use crate::util::{self, PathState};

const ID: &str = "dev.xcode";
/// `xcrun simctl` talks to a launchd service, which can take a moment on first use.
const SIMCTL_TIMEOUT: Duration = Duration::from_secs(20);
/// Device support older than this is flagged even for the newest OS seen.
const DEVICE_SUPPORT_STALE_DAYS: u64 = 180;

const XCODE: &[&str] = &["Xcode", "xcodebuild"];
const XCODE_AND_SIMULATOR: &[&str] = &["Xcode", "Simulator"];

/// Shared caches at the top level of DerivedData, not tied to any project.
const SHARED_DERIVED_DATA: &[&str] = &[
    "ModuleCache.noindex",
    "CompilationCache.noindex",
    "SDKStatCaches.noindex",
    "SDKExplicitPrecompiledModules",
];

const DEVICE_SUPPORT_PLATFORMS: &[(&str, &str)] = &[
    ("iOS", "iOS DeviceSupport"),
    ("watchOS", "watchOS DeviceSupport"),
    ("tvOS", "tvOS DeviceSupport"),
    ("visionOS", "xrOS DeviceSupport"),
    ("visionOS", "visionOS DeviceSupport"),
];

/// Built on every scan rather than kept in a `const`, so its text follows the current language.
pub(super) fn specs() -> Vec<CacheSpec> {
    let simulators = t!("Simuladores", "Simulators");
    vec![
        CacheSpec {
            key: "coresimulator_caches",
            group: simulators,
            title: t!("Cachés de CoreSimulator", "CoreSimulator caches"),
            paths: &["Library/Developer/CoreSimulator/Caches"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: XCODE_AND_SIMULATOR,
            explanation: t!(
                "Cachés que los simuladores generan al arrancar. Se regeneran solas: el próximo arranque de un simulador tardará algo más. Tus apps y datos en los simuladores no están aquí.",
                "Caches the simulators build when they start. They regenerate on their own: the next simulator launch takes a little longer. Your apps and data in the simulators are not here."
            ),
        },
        CacheSpec {
            key: "coresimulator_logs",
            group: simulators,
            title: t!("Registros de CoreSimulator", "CoreSimulator logs"),
            paths: &["Library/Logs/CoreSimulator"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: XCODE_AND_SIMULATOR,
            explanation: t!(
                "Registros de los simuladores. Solo sirven para investigar errores pasados.",
                "Simulator logs. They are only useful for looking into past errors."
            ),
        },
        CacheSpec {
            key: "xcode_cache",
            group: "Xcode",
            title: t!("Caché de Xcode", "Xcode cache"),
            paths: &["Library/Caches/com.apple.dt.Xcode"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: XCODE,
            explanation: t!(
                "Caché interna de la app Xcode. La regenera sola.",
                "The Xcode app's internal cache. It regenerates on its own."
            ),
        },
        CacheSpec {
            key: "swiftui_previews",
            group: "Xcode",
            title: t!("Datos de las vistas previas de SwiftUI", "SwiftUI preview data"),
            paths: &["Library/Developer/Xcode/UserData/Previews"],
            removal: Removal::Command {
                program: "xcrun",
                args: &["simctl", "--set", "previews", "delete", "all"],
                fallback: Fallback::TrashContents,
            },
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: XCODE_AND_SIMULATOR,
            explanation: t!(
                "Simuladores que Xcode crea solo para las vistas previas de SwiftUI. Xcode los vuelve a crear cuando hacen falta, aunque la primera vista previa tardará más.",
                "Simulators Xcode creates just for SwiftUI previews. Xcode creates them again when needed, though the first preview takes longer."
            ),
        },
    ]
}

pub(super) struct XcodeScanner;

impl Scanner for XcodeScanner {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        t!("Xcode y simuladores", "Xcode and simulators")
    }

    fn description(&self) -> &'static str {
        t!(
            "DerivedData, Archives, soporte de dispositivos y simuladores que ya no se pueden usar.",
            "DerivedData, Archives, device support and simulators that can no longer be used."
        )
    }

    fn area(&self) -> Area {
        Area::Developer
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let has_tool = |name: &str| util::tool(ctx, name).is_some();
        Ok(findings(ctx, &has_tool))
    }
}

fn findings(ctx: &ScanContext, has_tool: HasTool) -> Vec<Finding> {
    let mut all = table_findings(ctx, ID, &specs(), has_tool);
    all.extend(derived_data(ctx));
    all.extend(archives(ctx));
    all.extend(device_support(ctx));
    all.extend(unavailable_simulators(ctx));
    util::finalize(ctx, all)
}

fn plist_dict(path: &Path) -> Option<plist::Dictionary> {
    Value::from_file(path).ok()?.into_dictionary()
}

fn plist_date(dict: &plist::Dictionary, key: &str) -> Option<SystemTime> {
    dict.get(key)?.as_date().map(SystemTime::from)
}

/// `Project-abcdefgh...` becomes `Project`. A name with no hash is left alone.
fn name_from_folder(folder: &str) -> &str {
    match folder.rsplit_once('-') {
        Some((name, hash))
            if !name.is_empty()
                && hash.len() >= 16
                && hash.chars().all(|c| c.is_ascii_lowercase()) =>
        {
            name
        }
        _ => folder,
    }
}

fn derived_data(ctx: &ScanContext) -> Vec<Finding> {
    let root = ctx.home_path("Library/Developer/Xcode/DerivedData");
    let children = fsutil::children(&root);
    let (shared, projects): (Vec<PathBuf>, Vec<PathBuf>) = children
        .into_iter()
        .filter(|path| fsutil::is_real_dir(path))
        .partition(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SHARED_DERIVED_DATA.contains(&name))
        });

    let mut found: Vec<Finding> = projects
        .par_iter()
        .filter_map(|path| derived_data_project(ctx, path))
        .collect();

    let info = fsutil::measure_many(&shared, &ctx.cancel);
    if info.files > 0 {
        let title = t!("Cachés compartidas de módulos", "Shared module caches");
        found.push(
            Finding::new(ID, Area::Developer, "derived_data_shared", title)
                .group("DerivedData")
                .subtitle(root.display().to_string())
                .explanation(t!(
                    "Módulos precompilados de Swift y Clang que usan todos tus proyectos. Se regeneran solos: la próxima compilación de cada proyecto tardará algo más.",
                    "Precompiled Swift and Clang modules that all your projects use. They regenerate on their own: each project's next build takes a little longer."
                ))
                .measured(&info)
                .risk(Risk::Safe)
                .recovery(Recovery::Automatic)
                .selected(true)
                .blocked_by(XCODE)
                .action(Action::Trash { paths: shared }),
        );
    }
    found
}

fn derived_data_project(ctx: &ScanContext, path: &Path) -> Option<Finding> {
    if ctx.is_cancelled() {
        return None;
    }
    let info = ctx.measure(path);
    if info.files == 0 {
        return None;
    }
    let folder = path.file_name()?.to_str()?;
    let metadata = plist_dict(&path.join("info.plist"));
    let workspace = metadata
        .as_ref()
        .and_then(|dict| dict.get("WorkspacePath")?.as_string())
        .map(PathBuf::from);
    let last_accessed = metadata
        .as_ref()
        .and_then(|dict| plist_date(dict, "LastAccessedDate"));
    let last_used = util::newest(info.newest_modified, last_accessed);

    let name = workspace
        .as_deref()
        .and_then(|workspace| workspace.file_stem()?.to_str())
        .unwrap_or_else(|| name_from_folder(folder))
        .to_string();
    let state = workspace
        .as_deref()
        .map(|workspace| util::path_state(ctx, workspace));
    let idle_for = last_used.and_then(|last_used| age_days(last_used, ctx.now));
    let idle = idle_for.is_some_and(|days| days >= u64::from(ctx.config.project_idle_days));

    let shown = workspace
        .as_deref()
        .map(|workspace| util::tilde(ctx, workspace))
        .unwrap_or_else(|| t!("un proyecto desconocido", "an unknown project").to_string());
    let finding = Finding::new(ID, Area::Developer, format!("derived_data:{folder}"), name)
        .group("DerivedData")
        .trash(path)
        .measured(&info)
        .last_used(last_used)
        .recovery(Recovery::Rebuild)
        .blocked_by(XCODE);

    Some(match state {
        Some(PathState::Missing) => finding
            .risk(Risk::Safe)
            .selected(true)
            .tag("huerfano")
            .explanation(tf!(
                "Compilados de {shown}, que ya no existe en esa ubicación. Si lo moviste, Xcode los regenerará al compilarlo de nuevo.",
                "Build output of {shown}, which no longer exists at that location. If you moved it, Xcode will regenerate it when you build the project again."
            )),
        Some(PathState::Unknown) => finding
            .risk(Risk::Moderate)
            .selected(false)
            .explanation(tf!(
                "Compilados de {shown}. No se comprobó si el proyecto sigue ahí porque hacerlo pediría permisos. Xcode los regenera al compilar, lo que lleva su tiempo.",
                "Build output of {shown}. HealthyMac didn't check whether the project is still there, because checking would ask for permission. Xcode regenerates it when you build, which takes time."
            )),
        Some(PathState::Exists) => finding
            .risk(Risk::Moderate)
            .selected(idle)
            .tag(if idle { "inactivo" } else { "activo" })
            .explanation(match idle_for {
                Some(days) if idle => tf!(
                    "Compilados de {shown}, sin usar desde hace {days} días. Xcode los regenera al compilar, lo que lleva su tiempo.",
                    "Build output of {shown}, unused for {days} days. Xcode regenerates it when you build, which takes time."
                ),
                _ => tf!(
                    "Compilados de {shown}, usados hace poco, así que no van preseleccionados. Xcode los regenera al compilar, lo que lleva su tiempo.",
                    "Build output of {shown}, used recently, so it is not preselected. Xcode regenerates it when you build, which takes time."
                ),
            }),
        None => finding
            .risk(Risk::Moderate)
            .selected(false)
            .explanation(t!(
                "Compilados de un proyecto que no se pudo identificar. Xcode los regenera al compilar, lo que lleva su tiempo.",
                "Build output of a project that could not be identified. Xcode regenerates it when you build, which takes time."
            )),
    })
}

fn archives(ctx: &ScanContext) -> Vec<Finding> {
    let root = ctx.home_path("Library/Developer/Xcode/Archives");
    let archives: Vec<PathBuf> = fsutil::children(&root)
        .into_iter()
        .filter(|path| fsutil::is_real_dir(path))
        .flat_map(|day| fsutil::children(&day))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "xcarchive") && fsutil::is_real_dir(path)
        })
        .collect();
    archives
        .par_iter()
        .filter_map(|path| {
            if ctx.is_cancelled() {
                return None;
            }
            let info = ctx.measure(path);
            let metadata = plist_dict(&path.join("Info.plist"));
            let name = metadata
                .as_ref()
                .and_then(|dict| dict.get("Name")?.as_string())
                .map(str::to_string)
                .or_else(|| Some(path.file_stem()?.to_string_lossy().into_owned()))?;
            let version = metadata.as_ref().and_then(|dict| {
                dict.get("ApplicationProperties")?
                    .as_dictionary()?
                    .get("CFBundleShortVersionString")?
                    .as_string()
            });
            let title = match version {
                Some(version) => format!("{name} {version}"),
                None => name,
            };
            let created = metadata.as_ref().and_then(|dict| plist_date(dict, "CreationDate"));
            Some(
                Finding::new(ID, Area::Developer, format!("archive:{}", path.display()), title)
                    .group("Archives")
                    .trash(path)
                    .measured(&info)
                    .last_used(created.or(info.newest_modified))
                    .risk(Risk::Review)
                    .recovery(Recovery::None)
                    .selected(false)
                    .blocked_by(XCODE)
                    .explanation(t!(
                        "Una versión que compilaste para distribuirla, con sus dSYM. Sin ellos no podrás simbolizar los informes de fallos de esa versión, y no se pueden regenerar sin el código exacto de entonces.",
                        "A build you made for distribution, with its dSYMs. Without them you can't symbolicate crash reports from that version, and they can't be regenerated without the exact code from back then."
                    )),
            )
        })
        .collect()
}

/// The OS version in a device support folder: `18.2 (22C152)`, `iPhone15,2 18.2 (22C152)` or
/// `16.4.1 (20E252) arm64e`.
fn device_support_version(folder: &str) -> Option<Vec<u64>> {
    folder.split_whitespace().find_map(|token| {
        if token.contains(',') || !token.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        util::parse_version(token)
    })
}

fn device_support(ctx: &ScanContext) -> Vec<Finding> {
    DEVICE_SUPPORT_PLATFORMS
        .iter()
        .flat_map(|(platform, folder)| {
            let root = ctx.home_path("Library/Developer/Xcode").join(folder);
            device_support_platform(ctx, platform, &root)
        })
        .collect()
}

fn device_support_platform(ctx: &ScanContext, platform: &str, root: &Path) -> Vec<Finding> {
    let versions: Vec<(PathBuf, Option<Vec<u64>>)> = fsutil::children(root)
        .into_iter()
        .filter(|path| fsutil::is_real_dir(path))
        .map(|path| {
            let version = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(device_support_version);
            (path, version)
        })
        .collect();
    let newest_major = versions
        .iter()
        .filter_map(|(_, version)| version.as_ref()?.first().copied())
        .max();

    versions
        .par_iter()
        .filter_map(|(path, version)| {
            let info = ctx.measure(path);
            if info.files == 0 {
                return None;
            }
            let folder = path.file_name()?.to_string_lossy().into_owned();
            let major = version.as_ref().and_then(|version| version.first().copied());
            let superseded = matches!((major, newest_major), (Some(major), Some(newest)) if major < newest);
            let stale = info
                .newest_modified
                .and_then(|modified| age_days(modified, ctx.now))
                .is_some_and(|days| days >= DEVICE_SUPPORT_STALE_DAYS);
            let old = superseded || stale;
            let finding = Finding::new(
                ID,
                Area::Developer,
                format!("device_support:{platform}:{folder}"),
                format!("{platform} {folder}"),
            )
            .group(t!("Soporte de dispositivos", "Device support"))
            .trash(path)
            .measured(&info)
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(old)
            .blocked_by(XCODE);
            Some(if old {
                finding.tag("antiguo").explanation(t!(
                    "Símbolos de depuración de una versión del sistema antigua. Si vuelves a conectar un dispositivo con esa versión, Xcode los copia de nuevo, lo que tarda unos minutos.",
                    "Debug symbols for an old OS version. If you connect a device with that version again, Xcode copies them back, which takes a few minutes."
                ))
            } else {
                finding.explanation(t!(
                    "Símbolos de depuración de una versión del sistema reciente, así que no van preseleccionados. Si los quitas, Xcode los copia de nuevo la próxima vez que conectes ese dispositivo, lo que tarda unos minutos.",
                    "Debug symbols for a recent OS version, so they are not preselected. If you remove them, Xcode copies them back the next time you connect that device, which takes a few minutes."
                ))
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SimDevice {
    name: String,
    data_path: Option<PathBuf>,
    available: bool,
}

/// Parses `xcrun simctl list -j devices`. `None` if it is not what was expected.
fn parse_simctl_devices(json: &str) -> Option<Vec<SimDevice>> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let runtimes = value.get("devices")?.as_object()?;
    let mut devices = Vec::new();
    for entries in runtimes.values() {
        for device in entries.as_array()? {
            let name = device
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or("?");
            // Older Xcode versions wrote an `availability` string instead of `isAvailable`.
            let available = device
                .get("isAvailable")
                .and_then(|flag| flag.as_bool())
                .or_else(|| {
                    device
                        .get("availability")
                        .and_then(|text| text.as_str())
                        .map(|text| !text.contains("unavailable"))
                })
                // No statement either way is not evidence that it is unusable.
                .unwrap_or(true);
            devices.push(SimDevice {
                name: name.to_string(),
                data_path: device
                    .get("dataPath")
                    .and_then(|path| path.as_str())
                    .map(PathBuf::from),
                available,
            });
        }
    }
    Some(devices)
}

fn unavailable_simulators(ctx: &ScanContext) -> Vec<Finding> {
    // Only ask if this home has simulators at all, so a Mac without Xcode never runs `xcrun`.
    let devices_root = ctx.home_path("Library/Developer/CoreSimulator/Devices");
    if !fsutil::is_real_dir(&devices_root) {
        return Vec::new();
    }
    let Some(xcrun) = util::tool(ctx, "xcrun") else {
        return Vec::new();
    };
    let Some(output) = util::run_tool(&xcrun, &["simctl", "list", "-j", "devices"], SIMCTL_TIMEOUT)
    else {
        return Vec::new();
    };
    if !output.success() {
        return Vec::new();
    }
    let Some(devices) = parse_simctl_devices(&output.stdout) else {
        return Vec::new();
    };
    unavailable_finding(ctx, &devices_root, &devices)
        .into_iter()
        .collect()
}

fn unavailable_finding(
    ctx: &ScanContext,
    devices_root: &Path,
    devices: &[SimDevice],
) -> Option<Finding> {
    // simctl reports the real machine, so a fixture home must not pick up the real user's devices.
    let folders: Vec<PathBuf> = devices
        .iter()
        .filter(|device| !device.available)
        .filter_map(|device| {
            let folder = device.data_path.as_deref()?.parent()?;
            folder
                .starts_with(devices_root)
                .then(|| folder.to_path_buf())
        })
        .collect();
    if folders.is_empty() {
        return None;
    }
    let info = fsutil::measure_many(&folders, &ctx.cancel);
    let mut names: Vec<&str> = devices
        .iter()
        .filter(|device| !device.available)
        .map(|device| device.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let shown = names.iter().take(5).copied().collect::<Vec<_>>().join(", ");
    Some(
        Finding::new(
            ID,
            Area::Developer,
            "simulators_unavailable",
            // The English puts "unavailable" before the noun, so it goes into the counted word.
            tf!(
                "{} no disponibles",
                "{}",
                util::count(
                    folders.len() as u64,
                    t!("simulador", "unavailable simulator"),
                    t!("simuladores", "unavailable simulators")
                )
            ),
        )
        .group(t!("Simuladores", "Simulators"))
        .subtitle(devices_root.display().to_string())
        .size(info.exclusive())
        .shared_bytes(info.shared)
        .items(folders.len() as u64)
        .risk(Risk::Moderate)
        .recovery(Recovery::None)
        .selected(false)
        .blocked_by(XCODE_AND_SIMULATOR)
        .action(Action::Command {
            program: "xcrun".to_string(),
            args: ["simctl", "delete", "unavailable"]
                .iter()
                .map(|arg| arg.to_string())
                .collect(),
        })
        .tag("no_disponible")
        .explanation(tf!(
            "Simuladores cuyo sistema ya no está instalado, así que no pueden arrancar: {shown}. Ocupan {} con sus apps y datos, que se pierden al eliminarlos. Solo volverían a funcionar si reinstalas ese sistema en Xcode.",
            "Simulators whose OS is no longer installed, so they can't start: {shown}. They take up {} with their apps and data, which are lost when they are deleted. They would only work again if you reinstalled that OS in Xcode.",
            human_size(info.bytes)
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    const CAPTURED_SIMCTL: &str = r#"{
      "devices" : {
        "com.apple.CoreSimulator.SimRuntime.iOS-26-4" : [
          {
            "availabilityError" : "runtime profile not found using “System” match policy",
            "dataPath" : "/Users/x/Library/Developer/CoreSimulator/Devices/EEF0CC41/data",
            "dataPathSize" : 293228544,
            "udid" : "EEF0CC41",
            "isAvailable" : false,
            "state" : "Shutdown",
            "name" : "iPhone 17 Pro"
          }
        ],
        "com.apple.CoreSimulator.SimRuntime.iOS-27-0" : [
          {
            "dataPath" : "/Users/x/Library/Developer/CoreSimulator/Devices/AAAA/data",
            "udid" : "AAAA",
            "isAvailable" : true,
            "state" : "Booted",
            "name" : "iPhone 18"
          }
        ]
      }
    }"#;

    fn plist_file(fx: &Fixture, relative: &str, body: &str) {
        fx.file(
            relative,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>{body}</dict></plist>"#
            )
            .as_bytes(),
        );
    }

    fn derived(fx: &Fixture, folder: &str, workspace: &str) {
        let base = format!("Library/Developer/Xcode/DerivedData/{folder}");
        plist_file(
            fx,
            &format!("{base}/info.plist"),
            &format!("<key>WorkspacePath</key><string>{workspace}</string>"),
        );
        fx.file(&format!("{base}/Build/Products/app"), &[1u8; 4096]);
    }

    fn scan(ctx: &ScanContext) -> Vec<Finding> {
        findings(ctx, &|_| false)
    }

    fn by_title<'a>(found: &'a [Finding], title: &str) -> &'a Finding {
        found
            .iter()
            .find(|finding| finding.title == title)
            .unwrap_or_else(|| panic!("no {title} in {found:?}"))
    }

    #[test]
    fn derived_data_is_named_from_info_plist_and_orphans_are_flagged() {
        let fx = Fixture::new();
        let workspace = fx.file(
            "Coding/Alive/Alive.xcworkspace/contents.xcworkspacedata",
            b"x",
        );
        derived(
            &fx,
            "Alive-abcdefghijklmnopqrstuvwxyz",
            workspace.parent().unwrap().to_str().unwrap(),
        );
        let gone = fx.home.join("Coding/Gone/Gone.xcodeproj");
        derived(
            &fx,
            "Gone-abcdefghijklmnopqrstuvwxyz",
            gone.to_str().unwrap(),
        );

        let found = scan(&fx.ctx());
        let alive = by_title(&found, "Alive");
        let gone = by_title(&found, "Gone");
        assert!(gone.tags.contains(&"huerfano".to_string()));
        assert!(gone.selected_by_default);
        assert_eq!(gone.risk, Risk::Safe);
        assert!(!alive.tags.contains(&"huerfano".to_string()));
        assert!(!alive.selected_by_default, "just used");
        for finding in [alive, gone] {
            assert_eq!(finding.group.as_deref(), Some("DerivedData"));
            assert!(finding.blocked_by.contains(&"Xcode".to_string()));
            assert_eq!(finding.recovery, Recovery::Rebuild);
        }
    }

    #[test]
    fn derived_data_of_an_idle_project_is_preselected() {
        let fx = Fixture::new();
        let workspace = fx.file(
            "Coding/Idle/Idle.xcworkspace/contents.xcworkspacedata",
            b"x",
        );
        derived(
            &fx,
            "Idle-abcdefghijklmnopqrstuvwxyz",
            workspace.parent().unwrap().to_str().unwrap(),
        );
        let mut ctx = fx.ctx();
        ctx.now += Duration::from_secs(86_400 * 60);
        assert!(by_title(&scan(&ctx), "Idle").selected_by_default);
    }

    #[test]
    fn a_workspace_in_a_prompting_folder_is_not_touched_or_called_an_orphan() {
        let fx = Fixture::new();
        // The project does not exist, but the scanner may not find that out by touching Documents.
        let hidden = fx.home.join("Documents/Secret/Secret.xcodeproj");
        derived(
            &fx,
            "Secret-abcdefghijklmnopqrstuvwxyz",
            hidden.to_str().unwrap(),
        );
        let found = scan(&fx.ctx());
        let secret = by_title(&found, "Secret");
        assert!(!secret.tags.contains(&"huerfano".to_string()));
        assert!(!secret.selected_by_default);
        assert!(secret.explanation.contains("pediría permisos"));
    }

    #[test]
    fn a_malformed_info_plist_falls_back_to_the_folder_name() {
        let fx = Fixture::new();
        let base = "Library/Developer/Xcode/DerivedData/Odd-abcdefghijklmnopqrstuvwxyz";
        fx.file(&format!("{base}/info.plist"), b"not a plist at all");
        fx.file(&format!("{base}/Build/x"), &[1u8; 4096]);
        let found = scan(&fx.ctx());
        let odd = by_title(&found, "Odd");
        assert!(!odd.selected_by_default);
    }

    #[test]
    fn shared_module_caches_are_one_finding_of_their_own() {
        let fx = Fixture::new();
        fx.file(
            "Library/Developer/Xcode/DerivedData/ModuleCache.noindex/a",
            b"x",
        );
        fx.file(
            "Library/Developer/Xcode/DerivedData/CompilationCache.noindex/b",
            b"x",
        );
        let found = scan(&fx.ctx());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target_paths().len(), 2);
        assert!(found[0].selected_by_default);
    }

    #[test]
    fn archives_are_for_review_with_their_version_in_the_title() {
        let fx = Fixture::new();
        let base = "Library/Developer/Xcode/Archives/2026-03-01/Shop 1-3-26.xcarchive";
        plist_file(
            &fx,
            &format!("{base}/Info.plist"),
            "<key>Name</key><string>Shop</string>\
             <key>ApplicationProperties</key><dict>\
             <key>CFBundleShortVersionString</key><string>2.4.0</string></dict>",
        );
        fx.file(&format!("{base}/dSYMs/Shop.app.dSYM/data"), b"x");
        let found = scan(&fx.ctx());
        let archive = by_title(&found, "Shop 2.4.0");
        assert_eq!(archive.risk, Risk::Review);
        assert_eq!(archive.recovery, Recovery::None);
        assert!(!archive.selected_by_default);
    }

    #[test]
    fn device_support_versions_parse_in_every_known_form() {
        assert_eq!(device_support_version("18.2 (22C152)"), Some(vec![18, 2]));
        assert_eq!(
            device_support_version("iPhone15,2 18.2 (22C152)"),
            Some(vec![18, 2])
        );
        assert_eq!(
            device_support_version("16.4.1 (20E252) arm64e"),
            Some(vec![16, 4, 1])
        );
        assert_eq!(
            device_support_version("Watch6,1 10.0 (21R356)"),
            Some(vec![10, 0])
        );
        assert_eq!(device_support_version("whatever"), None);
    }

    #[test]
    fn old_device_support_is_flagged_and_current_is_left_unselected() {
        let fx = Fixture::new();
        fx.file(
            "Library/Developer/Xcode/iOS DeviceSupport/17.4 (21E219)/Symbols/a",
            b"x",
        );
        fx.file(
            "Library/Developer/Xcode/iOS DeviceSupport/iPhone15,2 18.2 (22C152)/Symbols/a",
            b"x",
        );
        fx.file(
            "Library/Developer/Xcode/watchOS DeviceSupport/10.0 (21R356)/Symbols/a",
            b"x",
        );
        let found = scan(&fx.ctx());
        let old = by_title(&found, "iOS 17.4 (21E219)");
        assert!(old.selected_by_default && old.tags.contains(&"antiguo".to_string()));
        assert!(!by_title(&found, "iOS iPhone15,2 18.2 (22C152)").selected_by_default);
        // Other platforms are judged against their own newest version.
        assert!(!by_title(&found, "watchOS 10.0 (21R356)").selected_by_default);
    }

    #[test]
    fn parses_captured_simctl_output() {
        let devices = parse_simctl_devices(CAPTURED_SIMCTL).unwrap();
        assert_eq!(devices.len(), 2);
        let unavailable: Vec<_> = devices.iter().filter(|device| !device.available).collect();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].name, "iPhone 17 Pro");
    }

    #[test]
    fn malformed_simctl_output_is_not_a_failure() {
        assert_eq!(parse_simctl_devices("not json"), None);
        assert_eq!(parse_simctl_devices("{}"), None);
        assert_eq!(parse_simctl_devices(r#"{"devices": {}}"#), Some(Vec::new()));
    }

    #[test]
    fn unavailable_simulators_use_simctl_and_only_count_this_home() {
        let fx = Fixture::new();
        let devices_root = fx.home.join("Library/Developer/CoreSimulator/Devices");
        fx.file(
            "Library/Developer/CoreSimulator/Devices/GONE/data/x",
            &[1u8; 8192],
        );
        let devices = vec![
            SimDevice {
                name: "iPhone viejo".to_string(),
                data_path: Some(devices_root.join("GONE/data")),
                available: false,
            },
            SimDevice {
                name: "Otro home".to_string(),
                data_path: Some(PathBuf::from(
                    "/Users/real/Library/Developer/CoreSimulator/Devices/R/data",
                )),
                available: false,
            },
            SimDevice {
                name: "Disponible".to_string(),
                data_path: Some(devices_root.join("OK/data")),
                available: true,
            },
        ];
        let finding = unavailable_finding(&fx.ctx(), &devices_root, &devices).unwrap();
        assert_eq!(finding.item_count, 1);
        assert!(finding.size >= 8192);
        assert!(!finding.selected_by_default);
        assert_eq!(
            finding.action,
            Action::Command {
                program: "xcrun".to_string(),
                args: vec!["simctl".into(), "delete".into(), "unavailable".into()],
            }
        );
        assert!(finding.blocked_by.contains(&"Simulator".to_string()));
    }

    #[test]
    fn previews_use_xcrun_when_available_and_the_trash_otherwise() {
        let fx = Fixture::new();
        fx.file(
            "Library/Developer/Xcode/UserData/Previews/Simulator Devices/x",
            b"x",
        );
        let with = findings(&fx.ctx(), &|_| true);
        assert!(matches!(with[0].action, Action::Command { .. }));
        let without = scan(&fx.ctx());
        assert!(matches!(without[0].action, Action::TrashContents { .. }));
    }

    #[test]
    fn a_machine_with_no_xcode_folders_finds_nothing() {
        let fx = Fixture::new();
        assert!(scan(&fx.ctx()).is_empty());
    }

    #[test]
    fn simulator_caches_keep_simulator_data_out_of_the_finding() {
        let fx = Fixture::new();
        fx.file("Library/Developer/CoreSimulator/Caches/dyld/x", b"x");
        fx.file(
            "Library/Developer/CoreSimulator/Devices/ABC/data/photo.jpg",
            b"x",
        );
        let found = scan(&fx.ctx());
        assert_eq!(found.len(), 1);
        let targets = found[0].target_paths();
        assert!(targets[0].ends_with("CoreSimulator/Caches"));
    }
}
