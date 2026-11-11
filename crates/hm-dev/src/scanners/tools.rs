//! Caches of developer tools: browsers for testing, editors, JetBrains IDEs and Android.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use hm_core::{fsutil, t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};
use rayon::prelude::*;

use super::{table_findings, CacheSpec, HasTool, Removal};
use crate::util;

const ID: &str = "dev.tools";

/// The Android emulator runs as one of these, and holds its disk images open.
const EMULATOR: &[&str] = &["qemu-system-aarch64", "qemu-system-x86_64", "emulator"];
const EMULATOR_AND_STUDIO: &[&str] = &[
    "qemu-system-aarch64",
    "qemu-system-x86_64",
    "emulator",
    "studio",
];

/// Built on every scan rather than kept in a `const`, so its text follows the current language.
pub(super) fn specs() -> Vec<CacheSpec> {
    let browsers = t!("Navegadores de pruebas", "Test browsers");
    vec![
        CacheSpec {
            key: "playwright",
            group: browsers,
            title: t!("Navegadores de Playwright", "Playwright browsers"),
            paths: &["Library/Caches/ms-playwright"],
            removal: Removal::Contents,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Navegadores descargados por Playwright. Tus tests no funcionarán hasta que los descargues de nuevo con «playwright install».",
                "Browsers downloaded by Playwright. Your tests won't run until you download them again with “playwright install”."
            ),
        },
        CacheSpec {
            key: "puppeteer",
            group: browsers,
            title: t!("Navegadores de Puppeteer", "Puppeteer browsers"),
            paths: &[".cache/puppeteer"],
            removal: Removal::Contents,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Navegadores descargados por Puppeteer. Hay que volver a descargarlos para que funcionen tus tests.",
                "Browsers downloaded by Puppeteer. They have to be downloaded again for your tests to run."
            ),
        },
        CacheSpec {
            key: "selenium",
            group: browsers,
            title: t!("Controladores de Selenium", "Selenium drivers"),
            paths: &[".cache/selenium"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Navegadores y controladores descargados por Selenium Manager. Los vuelve a descargar solo cuando hacen falta.",
                "Browsers and drivers downloaded by Selenium Manager. It downloads them again on its own when needed."
            ),
        },
        CacheSpec {
            key: "electron",
            group: "Node",
            title: t!("Caché de Electron", "Electron cache"),
            paths: &["Library/Caches/electron", "Library/Caches/electron-builder"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Versiones de Electron descargadas al instalar dependencias o empaquetar apps. Se vuelven a descargar cuando hacen falta. Las apps de Electron que usas no están aquí.",
                "Electron versions downloaded when installing dependencies or packaging apps. They are downloaded again when needed. The Electron apps you use are not here."
            ),
        },
        CacheSpec {
            key: "node_gyp",
            group: "Node",
            title: t!("Cabeceras de node-gyp", "node-gyp headers"),
            paths: &["Library/Caches/node-gyp", ".node-gyp"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Cabeceras de Node para compilar módulos nativos. Se vuelven a descargar la próxima vez que compiles uno.",
                "Node headers for building native modules. They are downloaded again the next time you build one."
            ),
        },
        CacheSpec {
            key: "turborepo",
            group: "Node",
            title: t!("Caché global de Turborepo", "Turborepo global cache"),
            paths: &["Library/Caches/turborepo", "Library/Caches/turbo", ".cache/turbo"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Rebuild,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Resultados guardados de tareas de Turborepo. Vuelven al ejecutar esas tareas de nuevo. Tu sesión de Turborepo no se toca.",
                "Saved results of Turborepo tasks. They come back when you run those tasks again. Your Turborepo login is left alone."
            ),
        },
    ]
}

/// Editors built on VS Code. Only the folders that hold caches and logs are ever named, so `User`
/// (settings, snippets, history), `Backups` (unsaved files) and `~/.vscode/extensions` are out of
/// reach by construction.
struct Editor {
    name: &'static str,
    support_folder: &'static str,
    process: &'static str,
}

const EDITORS: &[Editor] = &[
    Editor {
        name: "VS Code",
        support_folder: "Code",
        process: "Code",
    },
    Editor {
        name: "VS Code Insiders",
        support_folder: "Code - Insiders",
        process: "Code - Insiders",
    },
    Editor {
        name: "Cursor",
        support_folder: "Cursor",
        process: "Cursor",
    },
    Editor {
        name: "VSCodium",
        support_folder: "VSCodium",
        process: "VSCodium",
    },
    Editor {
        name: "Windsurf",
        support_folder: "Windsurf",
        process: "Windsurf",
    },
];

const EDITOR_CACHES: &[&str] = &[
    "Cache",
    "CachedData",
    "CachedExtensionVSIXs",
    "Code Cache",
    "GPUCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
];

fn editor_findings(ctx: &ScanContext, editor: &Editor) -> Vec<Finding> {
    let support = ctx
        .home_path("Library/Application Support")
        .join(editor.support_folder);
    if !fsutil::is_real_dir(&support) {
        return Vec::new();
    }
    let mut found = Vec::new();
    let caches: Vec<PathBuf> = EDITOR_CACHES
        .iter()
        .map(|name| support.join(name))
        .filter(|path| fsutil::is_real_dir(path))
        .collect();
    let info = fsutil::measure_many(&caches, &ctx.cancel);
    if info.files > 0 {
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                format!("editor_cache:{}", editor.support_folder),
                tf!("Cachés de {}", "{} caches", editor.name),
            )
            .group(t!("Editores", "Editors"))
            .subtitle(support.display().to_string())
            .explanation(tf!(
                "Cachés internas de {}. Se regeneran solas, aunque puede tardar algo más en abrir la próxima vez. Tus ajustes, extensiones y archivos sin guardar no se tocan.",
                "Internal {} caches. They regenerate on their own, though it may take a little longer to open next time. Your settings, extensions and unsaved files are left alone.",
                editor.name
            ))
            .measured(&info)
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(true)
            .blocked_by(&[editor.process])
            .action(Action::Trash { paths: caches }),
        );
    }
    let logs = support.join("logs");
    let info = if fsutil::is_real_dir(&logs) {
        ctx.measure(&logs)
    } else {
        Default::default()
    };
    if info.files > 0 {
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                format!("editor_logs:{}", editor.support_folder),
                tf!("Registros de {}", "{} logs", editor.name),
            )
            .group(t!("Editores", "Editors"))
            .subtitle(logs.display().to_string())
            .explanation(t!(
                "Registros de sesiones anteriores. Solo sirven para investigar errores pasados.",
                "Logs from earlier sessions. They are only useful for looking into past errors."
            ))
            .measured(&info)
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .selected(true)
            .blocked_by(&[editor.process])
            .action(Action::TrashContents { dir: logs }),
        );
    }
    found
}

/// A folder family for versioned IDE data, with where each kind of data lives.
struct IdeFamily {
    caches: &'static str,
    logs: &'static str,
    config: &'static str,
    /// `None` for any product. Google's folders also hold Chrome, which has nothing to do with this.
    only_product: Option<&'static str>,
}

const IDE_FAMILIES: &[IdeFamily] = &[
    IdeFamily {
        caches: "Library/Caches/JetBrains",
        logs: "Library/Logs/JetBrains",
        config: "Library/Application Support/JetBrains",
        only_product: None,
    },
    IdeFamily {
        caches: "Library/Caches/Google",
        logs: "Library/Logs/Google",
        config: "Library/Application Support/Google",
        only_product: Some("AndroidStudio"),
    },
];

/// Splits `IntelliJIdea2026.2` into the product and its version. `Daemon` and `Toolbox`, which are
/// not version folders and hold shared state, give `None`.
fn parse_ide_folder(name: &str) -> Option<(&str, Vec<u64>)> {
    let split = name.find(|c: char| c.is_ascii_digit())?;
    let (product, version) = name.split_at(split);
    if product.is_empty() || !version.contains('.') {
        return None;
    }
    Some((product, util::parse_version(version)?))
}

fn ide_display_name(product: &str) -> &str {
    match product {
        "IntelliJIdea" => "IntelliJ IDEA",
        "IdeaIC" => "IntelliJ IDEA Community",
        "PyCharmCE" => "PyCharm Community",
        "JetBrainsGateway" => "JetBrains Gateway",
        "AndroidStudio" => "Android Studio",
        other => other,
    }
}

/// The IDE's executable, which is what `blocked_by` matches.
fn ide_process(product: &str) -> String {
    match product {
        "IntelliJIdea" | "IdeaIC" => "idea".to_string(),
        "PyCharmCE" => "pycharm".to_string(),
        "JetBrainsGateway" => "gateway".to_string(),
        "AndroidStudio" => "studio".to_string(),
        other => other.to_lowercase(),
    }
}

/// Every `(folder name, path)` of the IDE version folders under `relative`.
fn ide_folders(ctx: &ScanContext, relative: &str, family: &IdeFamily) -> Vec<(String, PathBuf)> {
    fsutil::children(&ctx.home_path(relative))
        .into_iter()
        .filter(|path| fsutil::is_real_dir(path))
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.to_string();
            let (product, _) = parse_ide_folder(&name)?;
            family
                .only_product
                .is_none_or(|only| only == product)
                .then_some((name, path))
        })
        .collect()
}

fn ide_findings(ctx: &ScanContext, family: &IdeFamily) -> Vec<Finding> {
    let caches = ide_folders(ctx, family.caches, family);
    let logs = ide_folders(ctx, family.logs, family);
    let config = ide_folders(ctx, family.config, family);
    let group = t!(
        "IDE de JetBrains y Android Studio",
        "JetBrains IDEs and Android Studio"
    );

    // A version only counts as old if a newer one has actually been used on this Mac.
    let mut latest: HashMap<String, Vec<u64>> = HashMap::new();
    for (name, _) in caches.iter().chain(&logs).chain(&config) {
        if let Some((product, version)) = parse_ide_folder(name) {
            let current = latest.entry(product.to_string()).or_default();
            if version > *current {
                *current = version;
            }
        }
    }
    let is_old = |name: &str| {
        parse_ide_folder(name)
            .is_some_and(|(product, version)| latest.get(product).is_some_and(|l| version < *l))
    };
    let label = |name: &str| match parse_ide_folder(name) {
        Some((product, _)) => format!("{} {}", ide_display_name(product), &name[product.len()..]),
        None => name.to_string(),
    };
    let process = |name: &str| {
        parse_ide_folder(name)
            .map(|(product, _)| ide_process(product))
            .unwrap_or_default()
    };

    let mut found: Vec<Finding> = caches
        .par_iter()
        .filter_map(|(name, path)| {
            let info = ctx.measure(path);
            if info.files == 0 {
                return None;
            }
            let old = is_old(name);
            let finding = Finding::new(
                ID,
                Area::Developer,
                format!("ide_cache:{name}"),
                tf!("Caché de {}", "{} cache", label(name)),
            )
            .group(group)
            .trash(path)
            .measured(&info)
            .recovery(Recovery::Automatic)
            .blocked_by(&[process(name).as_str()]);
            Some(if old {
                finding.risk(Risk::Safe).selected(true).tag("version_antigua").explanation(t!(
                    "Caché de una versión del IDE que ya actualizaste. Ya no se usa, salvo que vuelvas a abrir esa versión.",
                    "Cache of an IDE version you have since updated. It is no longer used unless you open that version again."
                ))
            } else {
                finding.risk(Risk::Moderate).selected(false).explanation(t!(
                    "Índices y caché de la versión del IDE que usas. Se regeneran solos, pero el IDE tendrá que volver a indexar tus proyectos, lo que puede llevar minutos.",
                    "Indexes and cache of the IDE version you use. They regenerate on their own, but the IDE will have to reindex your projects, which can take minutes."
                ))
            })
        })
        .collect();

    found.extend(
        logs.par_iter()
            .filter_map(|(name, path)| {
                let info = ctx.measure(path);
                if info.files == 0 {
                    return None;
                }
                Some(
                    Finding::new(
                        ID,
                        Area::Developer,
                        format!("ide_logs:{name}"),
                        tf!("Registros de {}", "{} logs", label(name)),
                    )
                    .group(group)
                    .trash(path)
                    .measured(&info)
                    .risk(Risk::Safe)
                    .recovery(Recovery::Automatic)
                    .selected(true)
                    .blocked_by(&[process(name).as_str()])
                    .explanation(t!(
                        "Registros del IDE. Solo sirven para investigar errores pasados.",
                        "IDE logs. They are only useful for looking into past errors."
                    )),
                )
            })
            .collect::<Vec<_>>(),
    );

    // Only old versions. The current one's folder holds the settings and plugins in use.
    found.extend(config.par_iter().filter(|(name, _)| is_old(name)).filter_map(|(name, path)| {
        let info = ctx.measure(path);
        if info.files == 0 {
            return None;
        }
        Some(
            Finding::new(
                ID,
                Area::Developer,
                format!("ide_config:{name}"),
                tf!("Ajustes de {}", "{} settings", label(name)),
            )
            .group(group)
            .trash(path)
            .measured(&info)
            .risk(Risk::Review)
            .recovery(Recovery::None)
            .selected(false)
            .blocked_by(&[process(name).as_str()])
            .tag("version_antigua")
            .explanation(t!(
                "Ajustes y plugins de una versión del IDE que ya actualizaste. La versión nueva suele importarlos al actualizar, pero no se regeneran: revísalo antes de quitarlo.",
                "Settings and plugins of an IDE version you have since updated. The new version usually imports them when it updates, but they don't regenerate: review them before removing them."
            )),
        )
    }).collect::<Vec<_>>());
    found
}

/// The AVDs on this Mac and the system image each one boots from.
struct AvdInventory {
    avds: Vec<(String, PathBuf)>,
    /// Image path relative to the SDK, without a trailing slash, mapped to the AVDs using it.
    image_users: HashMap<String, Vec<String>>,
}

fn avd_inventory(ctx: &ScanContext) -> AvdInventory {
    let mut inventory = AvdInventory {
        avds: Vec::new(),
        image_users: HashMap::new(),
    };
    for path in fsutil::children(&ctx.home_path(".android/avd")) {
        if path.extension().is_none_or(|ext| ext != "avd") || !fsutil::is_real_dir(&path) {
            continue;
        }
        let Some(name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if let Some(image) = fs::read_to_string(path.join("config.ini"))
            .ok()
            .as_deref()
            .and_then(image_of_avd)
        {
            inventory
                .image_users
                .entry(image)
                .or_default()
                .push(name.clone());
        }
        inventory.avds.push((name, path));
    }
    inventory
}

/// The SDK-relative system image an AVD's `config.ini` boots from.
fn image_of_avd(config: &str) -> Option<String> {
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "image.sysdir.1")
            .then(|| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
    })
}

fn avd_findings(ctx: &ScanContext, inventory: &AvdInventory) -> Vec<Finding> {
    inventory
        .avds
        .par_iter()
        .filter_map(|(name, path)| {
            let info = ctx.measure(path);
            if info.files == 0 {
                return None;
            }
            let mut targets = vec![path.clone()];
            // The `.ini` beside the folder is how Android Studio finds it. Left alone it would show
            // a broken device.
            let ini = path.with_extension("ini");
            if ini.is_file() {
                targets.push(ini);
            }
            let title = tf!("Emulador {name}", "Emulator {name}");
            Some(
                Finding::new(ID, Area::Developer, format!("avd:{name}"), title)
                    .group("Android")
                    .subtitle(path.display().to_string())
                    .measured(&info)
                    .risk(Risk::Review)
                    .recovery(Recovery::None)
                    .selected(false)
                    .blocked_by(EMULATOR)
                    .action(Action::Trash { paths: targets })
                    .explanation(t!(
                        "Un dispositivo virtual de Android con sus apps, datos e instantáneas. Lo que hayas instalado o configurado en él se pierde al quitarlo.",
                        "An Android virtual device with its apps, data and snapshots. Anything you installed or set up on it is lost when it is removed."
                    )),
            )
        })
        .collect()
}

fn system_image_findings(ctx: &ScanContext, inventory: &AvdInventory) -> Vec<Finding> {
    let sdk = ctx.home_path("Library/Android/sdk");
    let root = sdk.join("system-images");
    // system-images/android-35/google_apis/arm64-v8a
    let images: Vec<PathBuf> = fsutil::children(&root)
        .into_iter()
        .filter(|path| fsutil::is_real_dir(path))
        .flat_map(|api| fsutil::children(&api))
        .filter(|path| fsutil::is_real_dir(path))
        .flat_map(|tag| fsutil::children(&tag))
        .filter(|path| fsutil::is_real_dir(path))
        .collect();
    images
        .par_iter()
        .filter_map(|path| {
            let info = ctx.measure(path);
            if info.files == 0 {
                return None;
            }
            let relative = path.strip_prefix(&sdk).ok()?.to_str()?.to_string();
            let parts: Vec<&str> = relative.split('/').skip(1).collect();
            let title = tf!(
                "Imagen de sistema de Android: {}",
                "Android system image: {}",
                parts.join(" · ")
            );
            let users = inventory.image_users.get(&relative);
            let finding = Finding::new(
                ID,
                Area::Developer,
                format!("system_image:{relative}"),
                title,
            )
            .group("Android")
            .trash(path)
            .measured(&info)
            .recovery(Recovery::Redownload)
            .selected(false)
            .blocked_by(EMULATOR_AND_STUDIO);
            Some(match users {
                Some(users) => finding.risk(Risk::Review).tag("en_uso").explanation(tf!(
                    "Sistema operativo del emulador. Lo usa {}, que no arrancará hasta que lo vuelvas a descargar desde el SDK Manager de Android Studio.",
                    "The emulator's operating system. Used by {}, which won't start until you download it again from the SDK Manager in Android Studio.",
                    users.join(", ")
                )),
                None => finding.risk(Risk::Moderate).explanation(t!(
                    "Sistema operativo para emuladores, que ningún dispositivo virtual usa ahora. Se puede volver a descargar desde el SDK Manager de Android Studio.",
                    "An operating system for emulators that no virtual device uses right now. It can be downloaded again from the SDK Manager in Android Studio."
                )),
            })
        })
        .collect()
}

pub(super) struct ToolCacheScanner;

fn findings(ctx: &ScanContext, has_tool: HasTool) -> Vec<Finding> {
    let mut all = table_findings(ctx, ID, &specs(), has_tool);
    for editor in EDITORS {
        all.extend(editor_findings(ctx, editor));
    }
    for family in IDE_FAMILIES {
        all.extend(ide_findings(ctx, family));
    }
    let inventory = avd_inventory(ctx);
    all.extend(avd_findings(ctx, &inventory));
    all.extend(system_image_findings(ctx, &inventory));
    util::finalize(ctx, all)
}

impl Scanner for ToolCacheScanner {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        t!("Herramientas de desarrollo", "Developer tools")
    }

    fn description(&self) -> &'static str {
        t!(
            "Cachés y registros de editores e IDE, navegadores de pruebas y emuladores de Android.",
            "Caches and logs of editors and IDEs, test browsers and Android emulators."
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn scan(ctx: &ScanContext) -> Vec<Finding> {
        findings(ctx, &|_| false)
    }

    fn by_key<'a>(found: &'a [Finding], key: &str) -> &'a Finding {
        let id = format!("{ID}:{key}");
        found
            .iter()
            .find(|finding| finding.id == id)
            .unwrap_or_else(|| {
                panic!(
                    "no {id} in {:?}",
                    found.iter().map(|f| &f.id).collect::<Vec<_>>()
                )
            })
    }

    fn has_key(found: &[Finding], key: &str) -> bool {
        let id = format!("{ID}:{key}");
        found.iter().any(|finding| finding.id == id)
    }

    #[test]
    fn vscode_settings_and_extensions_are_out_of_reach() {
        let fx = Fixture::new();
        let support = "Library/Application Support/Code";
        fx.file(&format!("{support}/User/settings.json"), b"{}");
        fx.file(&format!("{support}/User/History/x"), b"x");
        fx.file(&format!("{support}/Backups/unsaved"), b"x");
        fx.file(&format!("{support}/Local Storage/x"), b"x");
        fx.file(&format!("{support}/Cache/a"), b"x");
        fx.file(&format!("{support}/CachedData/b"), b"x");
        fx.file(&format!("{support}/CachedExtensionVSIXs/c"), b"x");
        fx.file(&format!("{support}/logs/d"), b"x");
        fx.file(".vscode/extensions/ext/package.json", b"{}");

        let found = scan(&fx.ctx());
        assert_eq!(found.len(), 2, "{found:?}");
        let cache = by_key(&found, "editor_cache:Code");
        assert_eq!(cache.target_paths().len(), 3);
        assert_eq!(cache.blocked_by, vec!["Code".to_string()]);
        for finding in &found {
            for target in finding.target_paths() {
                let text = target.display().to_string();
                for forbidden in ["/User", "/Backups", "Local Storage", ".vscode"] {
                    assert!(!text.contains(forbidden), "{text}");
                }
            }
        }
    }

    #[test]
    fn cursor_is_handled_like_vscode() {
        let fx = Fixture::new();
        fx.file("Library/Application Support/Cursor/Cache/a", b"x");
        fx.file(
            "Library/Application Support/Cursor/User/settings.json",
            b"{}",
        );
        let found = scan(&fx.ctx());
        let cache = by_key(&found, "editor_cache:Cursor");
        assert_eq!(cache.blocked_by, vec!["Cursor".to_string()]);
    }

    #[test]
    fn ide_folder_names_parse() {
        assert_eq!(
            parse_ide_folder("IntelliJIdea2026.2"),
            Some(("IntelliJIdea", vec![2026, 2]))
        );
        assert_eq!(
            parse_ide_folder("AndroidStudio2026.1.2"),
            Some(("AndroidStudio", vec![2026, 1, 2]))
        );
        assert_eq!(parse_ide_folder("Daemon"), None);
        assert_eq!(parse_ide_folder("Toolbox"), None);
        assert_eq!(parse_ide_folder("acp-agents"), None);
        assert_eq!(parse_ide_folder("Chrome"), None);
    }

    #[test]
    fn old_ide_versions_are_flagged_and_current_data_is_treated_with_care() {
        let fx = Fixture::new();
        fx.file("Library/Caches/JetBrains/PyCharm2026.1/x", b"x");
        fx.file("Library/Caches/JetBrains/PyCharm2026.2/x", b"x");
        fx.file("Library/Logs/JetBrains/PyCharm2026.2/idea.log", b"x");
        fx.file(
            "Library/Application Support/JetBrains/PyCharm2026.1/options/x",
            b"x",
        );
        fx.file(
            "Library/Application Support/JetBrains/PyCharm2026.2/options/x",
            b"x",
        );
        fx.file("Library/Caches/JetBrains/Toolbox/x", b"x");

        let found = scan(&fx.ctx());
        let old_cache = by_key(&found, "ide_cache:PyCharm2026.1");
        assert!(old_cache.selected_by_default);
        assert_eq!(old_cache.risk, Risk::Safe);
        assert!(old_cache.tags.contains(&"version_antigua".to_string()));
        assert_eq!(old_cache.blocked_by, vec!["pycharm".to_string()]);

        let current_cache = by_key(&found, "ide_cache:PyCharm2026.2");
        assert!(
            !current_cache.selected_by_default,
            "reindexing is expensive"
        );
        assert_eq!(current_cache.risk, Risk::Moderate);

        let old_config = by_key(&found, "ide_config:PyCharm2026.1");
        assert_eq!(old_config.risk, Risk::Review);
        assert!(!old_config.selected_by_default);

        assert!(
            !has_key(&found, "ide_config:PyCharm2026.2"),
            "settings in use"
        );
        assert!(found.iter().all(|f| !f.id.contains("Toolbox")));
    }

    #[test]
    fn a_single_ide_version_is_never_called_old() {
        let fx = Fixture::new();
        fx.file(
            "Library/Application Support/JetBrains/GoLand2024.1/options/x",
            b"x",
        );
        assert!(scan(&fx.ctx()).is_empty());
    }

    #[test]
    fn google_folders_only_yield_android_studio() {
        let fx = Fixture::new();
        fx.file("Library/Caches/Google/AndroidStudio2026.1.2/x", b"x");
        fx.file("Library/Caches/Google/Chrome/Default/Cache/x", b"x");
        fx.file(
            "Library/Application Support/Google/Chrome/Default/Login Data",
            b"x",
        );
        let found = scan(&fx.ctx());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].blocked_by, vec!["studio".to_string()]);
    }

    #[test]
    fn avds_are_for_review_and_android_keys_are_never_touched() {
        let fx = Fixture::new();
        fx.file(".android/avd/pixel.avd/userdata.img", &[1u8; 4096]);
        fx.file(".android/avd/pixel.ini", b"path=x");
        fx.file(".android/debug.keystore", b"secret");
        fx.file(".android/adbkey", b"secret");
        let found = scan(&fx.ctx());
        assert_eq!(found.len(), 1);
        let avd = by_key(&found, "avd:pixel");
        assert_eq!(avd.risk, Risk::Review);
        assert!(!avd.selected_by_default);
        assert_eq!(avd.target_paths().len(), 2, "the folder and its .ini");
    }

    #[test]
    fn system_images_say_which_avd_depends_on_them() {
        let fx = Fixture::new();
        let used = "Library/Android/sdk/system-images/android-35/google_apis/arm64-v8a/system.img";
        let unused = "Library/Android/sdk/system-images/android-30/default/arm64-v8a/system.img";
        fx.file(used, &[1u8; 4096]);
        fx.file(unused, &[1u8; 4096]);
        fx.file(
            ".android/avd/pixel.avd/config.ini",
            b"avd.ini.encoding = UTF-8\nimage.sysdir.1 = system-images/android-35/google_apis/arm64-v8a/\n",
        );
        let found = scan(&fx.ctx());
        let in_use = by_key(
            &found,
            "system_image:system-images/android-35/google_apis/arm64-v8a",
        );
        assert_eq!(in_use.risk, Risk::Review);
        assert!(in_use.explanation.contains("pixel"));
        let free = by_key(
            &found,
            "system_image:system-images/android-30/default/arm64-v8a",
        );
        assert_eq!(free.risk, Risk::Moderate);
        assert!(!free.selected_by_default);
        assert_eq!(free.recovery, Recovery::Redownload);
    }

    #[test]
    fn test_browsers_are_not_preselected_since_tests_depend_on_them() {
        let fx = Fixture::new();
        fx.file("Library/Caches/ms-playwright/chromium-1228/chrome", b"x");
        fx.file("Library/Caches/electron/electron-v30.zip", b"x");
        let found = scan(&fx.ctx());
        assert!(!by_key(&found, "playwright").selected_by_default);
        assert!(by_key(&found, "electron:Library/Caches/electron").selected_by_default);
    }
}
