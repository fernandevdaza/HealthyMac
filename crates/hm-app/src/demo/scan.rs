//! A pretend scan: the same events as the real one, on a similar schedule.

use std::time::Duration;

use hm_core::{t, Action, Area, CancelToken, Finding, Recovery, Risk, ScanEvent};

use super::{days_ago, hours_ago, HOME, MB};
use crate::state::ScannerMeta;

/// Built on every call rather than kept in a `const`, so the names and descriptions come out in the
/// language in use at that moment.
fn scanners() -> Vec<(&'static str, &'static str, &'static str, Area)> {
    vec![
        (
            "sys.user_caches",
            t!("Cachés de apps", "App caches"),
            t!(
                "Lo que las apps guardan para ir más rápido. Lo regeneran solas.",
                "What apps keep around to run faster. They rebuild it on their own."
            ),
            Area::System,
        ),
        (
            "sys.logs",
            t!("Registros", "Logs"),
            t!(
                "Diarios que ya nadie va a leer.",
                "Records nobody is going to read anymore."
            ),
            Area::System,
        ),
        (
            "sys.updates",
            t!("Actualizaciones descargadas", "Downloaded updates"),
            t!(
                "Instaladores que ya hicieron su trabajo.",
                "Installers that have already done their job."
            ),
            Area::System,
        ),
        (
            "sys.trash",
            t!("Papelera", "Trash"),
            t!(
                "Lo que ya tiraste y sigue ocupando espacio.",
                "What you threw away, still taking up space."
            ),
            Area::System,
        ),
        (
            "sys.time_machine",
            t!("Instantáneas locales", "Local snapshots"),
            t!(
                "Copias de Time Machine guardadas en el propio disco.",
                "Time Machine copies kept on this Mac's own disk."
            ),
            Area::System,
        ),
        (
            "dev.xcode.derived_data",
            "Xcode: DerivedData",
            t!(
                "Compilaciones intermedias. Xcode las rehace.",
                "Intermediate build files. Xcode rebuilds them."
            ),
            Area::Developer,
        ),
        (
            "dev.xcode.device_support",
            t!("Xcode: soporte de dispositivos", "Xcode: device support"),
            t!(
                "Símbolos de versiones de iOS que ya no usas.",
                "Symbols for iOS versions you no longer use."
            ),
            Area::Developer,
        ),
        (
            "dev.xcode.simulators",
            t!("Simuladores", "Simulators"),
            t!(
                "Runtimes y dispositivos simulados que no se usan.",
                "Runtimes and simulated devices you don't use."
            ),
            Area::Developer,
        ),
        (
            "dev.node_modules",
            t!("node_modules abandonados", "Abandoned node_modules"),
            t!(
                "Dependencias de proyectos parados. npm install las trae de vuelta.",
                "Dependencies of idle projects. npm install brings them back."
            ),
            Area::Developer,
        ),
        (
            "dev.rust_target",
            t!("Compilaciones de Rust", "Rust builds"),
            t!(
                "Carpetas target de proyectos parados.",
                "The target folders of idle projects."
            ),
            Area::Developer,
        ),
        (
            "dev.pkg_caches",
            t!("Cachés de paquetes", "Package caches"),
            t!(
                "npm, pnpm, Cargo, pip y Homebrew.",
                "npm, pnpm, Cargo, pip and Homebrew."
            ),
            Area::Developer,
        ),
        (
            "dev.docker",
            "Docker",
            t!(
                "Imágenes sin usar y caché de build.",
                "Unused images and build cache."
            ),
            Area::Developer,
        ),
        (
            "ai.hf_cache",
            t!("Caché de Hugging Face", "Hugging Face cache"),
            t!(
                "Modelos descargados por scripts y que puede que no vuelvas a usar.",
                "Models that scripts downloaded and you may never use again."
            ),
            Area::LocalAi,
        ),
        (
            "ai.duplicates",
            t!("Modelos repetidos", "Duplicate models"),
            t!(
                "El mismo archivo guardado por runtimes distintos.",
                "The same file, saved by different runtimes."
            ),
            Area::LocalAi,
        ),
        (
            "ai.partial",
            t!("Descargas a medias", "Unfinished downloads"),
            t!(
                "Modelos que se quedaron sin terminar.",
                "Models that never finished downloading."
            ),
            Area::LocalAi,
        ),
        (
            "ai.comfy",
            t!("Salidas de ComfyUI", "ComfyUI outputs"),
            t!(
                "Imágenes generadas que se acumulan.",
                "Generated images that pile up."
            ),
            Area::LocalAi,
        ),
        (
            "apps.leftovers",
            t!("Restos de apps", "App leftovers"),
            t!(
                "Lo que dejaron las apps que ya no están.",
                "Files left behind by apps you removed."
            ),
            Area::Apps,
        ),
        (
            "files.large",
            t!("Archivos grandes", "Large files"),
            t!(
                "Lo más voluminoso fuera de tus proyectos.",
                "The biggest files outside your projects."
            ),
            Area::Files,
        ),
        (
            "files.installers",
            t!("Instaladores en Descargas", "Installers in Downloads"),
            t!(
                "Los .dmg y .pkg que ya no hacen falta.",
                "The .dmg and .pkg files you no longer need."
            ),
            Area::Files,
        ),
        (
            "files.fda",
            t!("Acceso a tus carpetas", "Access to your folders"),
            t!(
                "Lo que veríamos con Acceso total al disco.",
                "What we could see with Full Disk Access."
            ),
            Area::Files,
        ),
    ]
}

pub fn meta(_stress: bool) -> Vec<ScannerMeta> {
    scanners()
        .into_iter()
        .map(|(id, name, description, area)| ScannerMeta {
            id,
            name,
            description,
            area,
        })
        .collect()
}

fn cache(
    scanner: &str,
    area: Area,
    key: &str,
    title: &str,
    path: &str,
    size: u64,
    days: u64,
) -> Finding {
    Finding::new(scanner, area, key, title)
        .trash(format!("{HOME}/{path}"))
        .size(size)
        .items(size / 180_000 + 3)
        .last_used(days_ago(days))
        .risk(Risk::Safe)
        .recovery(Recovery::Automatic)
        .selected(true)
}

fn findings_for(scanner: &'static str) -> Vec<Finding> {
    match scanner {
        "sys.user_caches" => vec![
            cache(scanner, Area::System, "spotify", "Spotify", "Library/Caches/com.spotify.client", 7_400 * MB, 1)
                .explanation(t!(
                    "Música guardada para reproducir sin esperas. Spotify vuelve a descargar lo que necesite.",
                    "Music saved so it plays right away. Spotify downloads whatever it needs again."
                )),
            cache(scanner, Area::System, "chrome", "Google Chrome", "Library/Caches/Google/Chrome", 3_100 * MB, 0)
                .explanation(t!(
                    "Páginas e imágenes guardadas. Algunas webs cargarán un poco más lentas la primera vez.",
                    "Saved pages and images. Some sites will load a little slower the first time."
                ))
                .blocked_by(&["Google Chrome"]),
            cache(scanner, Area::System, "slack", "Slack", "Library/Caches/com.tinyspeck.slackmacgap", 920 * MB, 2)
                .explanation(t!(
                    "Archivos e imágenes de tus canales. Slack los recupera cuando hacen falta.",
                    "Files and images from your channels. Slack gets them again when it needs them."
                )),
            cache(scanner, Area::System, "figma", "Figma", "Library/Caches/com.figma.Desktop", 640 * MB, 9)
                .explanation(t!(
                    "Copias locales de archivos abiertos. Sigue todo en la nube.",
                    "Local copies of files you opened. Everything is still in the cloud."
                )),
            cache(scanner, Area::System, "webkit", "WebKit", "Library/Caches/com.apple.WebKit.Networking", 310 * MB, 3)
                .explanation(t!(
                    "Caché de red de las apps que usan WebKit.",
                    "Network cache for the apps that use WebKit."
                )),
        ],
        "sys.logs" => vec![
            cache(scanner, Area::System, "logs", t!("Registros de apps", "App logs"), "Library/Logs", 1_240 * MB, 1)
                .explanation(t!(
                    "Registros de actividad de tus apps. Solo sirven para diagnosticar problemas pasados.",
                    "Activity logs from your apps. They're only useful for diagnosing past problems."
                )),
            cache(scanner, Area::System, "crash", t!("Informes de fallos", "Crash reports"), "Library/Logs/DiagnosticReports", 180 * MB, 12)
                .explanation(t!(
                    "Informes de cuando alguna app se cerró sola.",
                    "Reports from the times an app quit unexpectedly."
                )),
        ],
        "sys.updates" => vec![
            Finding::new(scanner, Area::System, "macos", t!("Instalador de macOS Tahoe", "macOS Tahoe installer"))
                // macOS names the bundle in English on every system, so the English demo does too.
                .trash(t!("/Applications/Instalar macOS Tahoe.app", "/Applications/Install macOS Tahoe.app"))
                .size(16_800 * MB)
                .items(1)
                .last_used(days_ago(61))
                .risk(Risk::Moderate)
                .recovery(Recovery::Redownload)
                .selected(true)
                .explanation(t!(
                    "La actualización ya está instalada. Si algún día quieres otro USB de arranque, tendrás que volver a bajarlo.",
                    "The update is already installed. If you ever want another bootable USB drive, you'll have to download it again."
                )),
        ],
        "sys.trash" => vec![
            Finding::new(scanner, Area::System, "trash", t!("Papelera", "Trash"))
                .subtitle(format!("{HOME}/.Trash"))
                .size(5_200 * MB)
                .items(214)
                .last_used(days_ago(4))
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .action(Action::EmptyTrash)
                .explanation(t!(
                    "Vaciarla es definitivo: lo que haya dentro no se puede recuperar. Por eso no va marcada.",
                    "Emptying it is permanent: what's inside can't be recovered. That's why it isn't selected."
                )),
        ],
        "sys.time_machine" => vec![
            Finding::new(scanner, Area::System, "tm", t!("Instantáneas locales de Time Machine", "Local Time Machine snapshots"))
                .subtitle(t!("3 instantáneas de los últimos días", "3 snapshots from the last few days"))
                .size(21_000 * MB)
                .explanation(t!(
                    "macOS las borra solo cuando necesita espacio, así que no hace falta tocarlas. Te lo contamos para que sepas qué ocupa ese hueco.",
                    "macOS deletes them on its own when it needs space, so you don't have to touch them. We show them so you know what's taking up that space."
                )),
        ],
        "dev.xcode.derived_data" => vec![
            cache(scanner, Area::Developer, "dd", "DerivedData", "Library/Developer/Xcode/DerivedData", 38_400 * MB, 2)
                .recovery(Recovery::Rebuild)
                .blocked_by(&["Xcode"])
                .explanation(t!(
                    "Resultados intermedios de compilación. La próxima compilación tardará más, y ya.",
                    "Intermediate build results. Your next build will take longer, and that's all."
                )),
        ],
        "dev.xcode.device_support" => vec![
            cache(scanner, Area::Developer, "ios17", "iOS 17.5 (21F90)", "Library/Developer/Xcode/iOS DeviceSupport/iPhone15,3 17.5 (21F90)", 7_900 * MB, 240)
                .risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!(
                    "Símbolos de depuración. Se vuelven a copiar cuando conectas un dispositivo con esa versión.",
                    "Debug symbols. They're copied again when you connect a device running that version."
                )),
            cache(scanner, Area::Developer, "ios18", "iOS 18.1 (22B83)", "Library/Developer/Xcode/iOS DeviceSupport/iPhone15,3 18.1 (22B83)", 6_300 * MB, 120)
                .risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!(
                    "Símbolos de depuración. Se vuelven a copiar cuando conectas un dispositivo con esa versión.",
                    "Debug symbols. They're copied again when you connect a device running that version."
                )),
        ],
        "dev.xcode.simulators" => vec![
            Finding::new(scanner, Area::Developer, "unavailable", t!("Simuladores no disponibles", "Unavailable simulators"))
                .subtitle("xcrun simctl delete unavailable")
                .size(4_300 * MB)
                .items(11)
                .risk(Risk::Safe)
                .recovery(Recovery::Redownload)
                .selected(true)
                .action(Action::Command { program: "xcrun".into(), args: vec!["simctl".into(), "delete".into(), "unavailable".into()] })
                .explanation(t!(
                    "Dispositivos de versiones de iOS que ya no tienes. Los limpia el propio Xcode.",
                    "Devices for iOS versions you no longer have. Xcode itself cleans them up."
                )),
            cache(scanner, Area::Developer, "runtime16", t!("Runtime iOS 16.4", "iOS 16.4 runtime"), "Library/Developer/CoreSimulator/Profiles/Runtimes/iOS 16.4.simruntime", 7_100 * MB, 400)
                .risk(Risk::Moderate).recovery(Recovery::Redownload).selected(false)
                .explanation(t!(
                    "Un simulador completo de iOS 16. Se vuelve a descargar desde Xcode si te hace falta.",
                    "A full iOS 16 simulator. You can download it again from Xcode if you need it."
                )),
        ],
        "dev.node_modules" => vec![
            cache(scanner, Area::Developer, "shop", "node_modules", "Coding/tienda-web/node_modules", 2_140 * MB, 94)
                .group("tienda-web").risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!(
                    "Se recupera con npm install. Lleva 94 días sin moverse.",
                    "npm install brings it back. It hasn't been touched in 94 days."
                )),
            cache(scanner, Area::Developer, "shop-admin", "node_modules", "Coding/tienda-web/admin/node_modules", 1_380 * MB, 94)
                .group("tienda-web").risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!("Se recupera con npm install.", "npm install brings it back.")),
            cache(scanner, Area::Developer, "blog", "node_modules", "Coding/blog-astro/node_modules", 245 * MB, 41)
                .shared_bytes(365 * MB)
                .group("blog-astro").risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!("Se recupera con pnpm install.", "pnpm install brings it back.")),
            cache(scanner, Area::Developer, "scraper-venv", ".venv", "Coding/scraper-precios/.venv", 45 * MB, 180)
                .shared_bytes(916 * MB)
                .group("scraper-precios").risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!(
                    "Entorno de Python creado con uv. Se recupera con uv sync.",
                    "A Python environment made with uv. uv sync brings it back."
                )),
            cache(scanner, Area::Developer, "tesis", "node_modules", "Coding/viejo/tesis-visualizaciones/node_modules", 980 * MB, 420)
                .group("tesis-visualizaciones").risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!(
                    "Se recupera con npm install. Lleva más de un año sin moverse.",
                    "npm install brings it back. It hasn't been touched in over a year."
                )),
        ],
        "dev.rust_target" => vec![
            cache(scanner, Area::Developer, "healthy", "target", "Coding/motor-audio/target", 18_200 * MB, 33)
                .group("motor-audio").recovery(Recovery::Rebuild)
                .explanation(t!(
                    "Salida de cargo. Vuelve con cargo build, aunque tardará.",
                    "Build output from cargo. Running cargo build brings it back, though it takes a while."
                )),
            cache(scanner, Area::Developer, "cli", "target", "Coding/herramientas/rsync-mejor/target", 4_100 * MB, 72)
                .group("rsync-mejor").recovery(Recovery::Rebuild)
                .explanation(t!(
                    "Salida de cargo. Vuelve con cargo build.",
                    "Build output from cargo. Running cargo build brings it back."
                )),
        ],
        "dev.pkg_caches" => vec![
            cache(scanner, Area::Developer, "npm", t!("Caché de npm", "npm cache"), ".npm/_cacache", 5_800 * MB, 1)
                .recovery(Recovery::Redownload).risk(Risk::Safe)
                .explanation(t!(
                    "Paquetes ya descargados. npm los vuelve a bajar cuando hagan falta.",
                    "Packages npm already downloaded. It downloads them again when they're needed."
                )),
            cache(scanner, Area::Developer, "pnpm", t!("Almacén de pnpm", "pnpm store"), "Library/pnpm/store", 11_400 * MB, 1)
                .recovery(Recovery::Redownload).risk(Risk::Moderate).selected(false)
                .explanation(t!(
                    "Todos tus proyectos enlazan a este almacén. Borrarlo obliga a reinstalar la próxima vez.",
                    "All your projects link to this store. Deleting it means reinstalling everything next time."
                )),
            cache(scanner, Area::Developer, "cargo", t!("Registro de Cargo", "Cargo registry"), ".cargo/registry", 3_700 * MB, 2)
                .recovery(Recovery::Redownload)
                .explanation(t!(
                    "Crates descargados. Cargo los recupera al compilar.",
                    "Downloaded crates. Cargo gets them again when you build."
                )),
            Finding::new(scanner, Area::Developer, "brew", t!("Homebrew: descargas viejas", "Homebrew: old downloads"))
                .subtitle("brew cleanup --prune=all")
                .size(2_900 * MB)
                .items(87)
                .risk(Risk::Safe)
                .recovery(Recovery::Redownload)
                .selected(true)
                .action(Action::Command { program: "brew".into(), args: vec!["cleanup".into(), "--prune=all".into()] })
                .explanation(t!(
                    "Versiones antiguas e instaladores que Homebrew ya no necesita. Lo limpia Homebrew.",
                    "Old versions and installers that Homebrew no longer needs. Homebrew cleans them up itself."
                )),
        ],
        "dev.docker" => vec![
            Finding::new(scanner, Area::Developer, "build", t!("Caché de build de Docker", "Docker build cache"))
                .subtitle("docker builder prune -f")
                .size(12_600 * MB)
                .risk(Risk::Moderate)
                .recovery(Recovery::Rebuild)
                .selected(true)
                .action(Action::Command { program: "docker".into(), args: vec!["builder".into(), "prune".into(), "-f".into()] })
                .explanation(t!(
                    "Capas de builds anteriores. El siguiente docker build tardará más.",
                    "Layers from earlier builds. Your next docker build will take longer."
                )),
            Finding::new(scanner, Area::Developer, "images", t!("Imágenes de Docker sin usar", "Unused Docker images"))
                .subtitle("docker image prune -a -f")
                .size(9_300 * MB)
                .items(14)
                .risk(Risk::Moderate)
                .recovery(Recovery::Redownload)
                .action(Action::Command { program: "docker".into(), args: vec!["image".into(), "prune".into(), "-a".into(), "-f".into()] })
                .explanation(t!(
                    "Ningún contenedor las usa. Se vuelven a bajar con docker pull.",
                    "No container uses them. You can get them back with docker pull."
                )),
        ],
        "ai.hf_cache" => vec![
            cache(scanner, Area::LocalAi, "sdxl", "stabilityai/stable-diffusion-xl-base-1.0", ".cache/huggingface/hub/models--stabilityai--stable-diffusion-xl-base-1.0", 13_900 * MB, 88)
                .risk(Risk::Moderate).recovery(Recovery::Redownload).selected(false)
                .explanation(t!(
                    "Se volverá a descargar si algún script lo pide: son casi 14 GB.",
                    "It will download again if a script asks for it, and that's almost 14 GB."
                )),
            cache(scanner, Area::LocalAi, "whisper", "openai/whisper-large-v3", ".cache/huggingface/hub/models--openai--whisper-large-v3", 6_200 * MB, 40)
                .risk(Risk::Moderate).recovery(Recovery::Redownload).selected(false)
                .explanation(t!(
                    "Se volverá a descargar si algún script lo pide.",
                    "It will download again if a script asks for it."
                )),
        ],
        "ai.duplicates" => vec![
            Finding::new(scanner, Area::LocalAi, "dup-llama", t!("Deduplicar llama3.3:70b entre Ollama y LM Studio", "Deduplicate llama3.3:70b between Ollama and LM Studio"))
                .subtitle(format!("{HOME}/.lmstudio/models/lmstudio-community/Llama-3.3-70B-GGUF"))
                .size(42_100 * MB)
                .items(1)
                .risk(Risk::Safe)
                .recovery(Recovery::NothingLost)
                .selected(true)
                .action(Action::CloneDedupe {
                    keep: format!("{HOME}/.ollama/models/blobs/sha256-5e1f").into(),
                    duplicates: vec![format!("{HOME}/.lmstudio/models/lmstudio-community/Llama-3.3-70B-GGUF/model.gguf").into()],
                })
                .explanation(t!(
                    "Dos copias exactas del mismo archivo. Pasan a ser clones APFS: ambas rutas siguen funcionando y no se pierde nada.",
                    "Two exact copies of the same file. They become APFS clones: both paths keep working and nothing is lost."
                )),
        ],
        "ai.partial" => vec![
            cache(scanner, Area::LocalAi, "partial", t!("Descarga incompleta de gemma3:27b", "Incomplete download of gemma3:27b"), ".ollama/models/blobs/sha256-a1c9-partial", 3_400 * MB, 19)
                .recovery(Recovery::Redownload)
                .explanation(t!(
                    "Una descarga que no terminó. Ollama la reanuda o la empieza de nuevo.",
                    "A download that never finished. Ollama resumes it or starts over."
                )),
        ],
        "ai.comfy" => vec![
            Finding::new(scanner, Area::LocalAi, "outputs", t!("Imágenes generadas con ComfyUI", "Images generated with ComfyUI"))
                .trash(format!("{HOME}/ComfyUI/output"))
                .size(8_100 * MB)
                .items(3_412)
                .last_used(days_ago(26))
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .explanation(t!(
                    "Son imágenes tuyas: no se regeneran solas. Mira antes de quitarlas.",
                    "These are your images, and they don't come back on their own. Take a look before you remove them."
                )),
        ],
        "apps.leftovers" => vec![
            Finding::new(scanner, Area::Apps, "sketch", t!("Restos de Sketch", "Sketch leftovers"))
                .trash(format!("{HOME}/Library/Application Support/com.bohemiancoding.sketch3"))
                .size(2_300 * MB)
                .items(812)
                .last_used(days_ago(300))
                .risk(Risk::Moderate)
                .recovery(Recovery::None)
                .selected(true)
                .explanation(t!(
                    "La app ya no está instalada. Si vuelves a instalarla, empezará de cero.",
                    "The app is no longer installed. If you install it again, it will start from scratch."
                )),
            Finding::new(scanner, Area::Apps, "parallels", t!("Restos de Parallels", "Parallels leftovers"))
                .trash(format!("{HOME}/Library/Parallels"))
                .size(6_700 * MB)
                .items(40)
                .last_used(days_ago(510))
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .explanation(t!(
                    "Puede contener máquinas virtuales tuyas. Mira qué hay antes de quitarlo.",
                    "It may hold your virtual machines. Check what's in it before you remove it."
                )),
        ],
        "files.large" => vec![
            Finding::new(scanner, Area::Files, "video", "Grabación de pantalla - reunión.mov")
                .trash(format!("{HOME}/Movies/Grabación de pantalla - reunión.mov"))
                .size(24_800 * MB)
                .items(1)
                .last_used(days_ago(190))
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .explanation(t!(
                    "Es un archivo tuyo y no se regenera. Solo tú sabes si aún lo necesitas.",
                    "It's your file, and it can't be recreated. Only you know if you still need it."
                )),
            Finding::new(scanner, Area::Files, "iso", "ubuntu-24.04-desktop-arm64.iso")
                .trash(format!("{HOME}/Downloads/ubuntu-24.04-desktop-arm64.iso"))
                .size(6_100 * MB)
                .items(1)
                .last_used(days_ago(150))
                .risk(Risk::Review)
                .recovery(Recovery::Redownload)
                .explanation(t!(
                    "Una imagen de disco que se puede volver a descargar.",
                    "A disk image you can download again."
                )),
            Finding::new(scanner, Area::Files, "zip", "copia-fotos-2019.zip")
                .trash(format!("{HOME}/Desktop/Antiguo/Muy antiguo/Cosas de la mudanza/Copias de seguridad sin ordenar/copia-fotos-2019.zip"))
                .size(18_200 * MB)
                .items(1)
                .last_used(days_ago(800))
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .explanation(t!(
                    "Podría ser la única copia de esas fotos. Asegúrate antes de quitarla.",
                    "It could be the only copy of those photos. Make sure before you remove it."
                )),
        ],
        "files.installers" => vec![
            cache(scanner, Area::Files, "dmg1", "Docker.dmg", "Downloads/Docker.dmg", 1_400 * MB, 60)
                .risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!("Un instalador que ya usaste.", "An installer you already used.")),
            cache(scanner, Area::Files, "dmg2", "Figma-126.1.dmg", "Downloads/Figma-126.1.dmg", 310 * MB, 88)
                .risk(Risk::Moderate).recovery(Recovery::Redownload)
                .explanation(t!("Un instalador que ya usaste.", "An installer you already used.")),
        ],
        "files.fda" => vec![
            Finding::new(scanner, Area::Files, "fda", t!("Falta el Acceso total al disco", "Full Disk Access is missing"))
                .subtitle(t!(
                    "Documentos, Escritorio, Descargas y los datos de las apps",
                    "Documents, Desktop, Downloads and app data"
                ))
                .explanation(t!(
                    "Sin ese permiso, Documentos, Escritorio, Descargas y los datos de las apps se quedan fuera del escaneo. Puedes concederlo en Ajustes, y retirarlo cuando quieras.",
                    "Without that permission, Documents, Desktop, Downloads and app data are left out of the scan. You can grant it in Settings and take it back whenever you want."
                ))
                .tag(hm_core::TAG_NEEDS_FULL_DISK_ACCESS),
        ],
        _ => Vec::new(),
    }
}

fn stress_findings() -> Vec<Finding> {
    (0..6_000u64)
        .map(|index| {
            Finding::new(
                "dev.node_modules",
                Area::Developer,
                format!("stress-{index}"),
                "node_modules",
            )
            .trash(format!(
                "{HOME}/Coding/monorepo/packages/paquete-{index}/node_modules"
            ))
            .group(format!("paquete-{:03}", index / 40))
            .size(1_000_000 + index * 137_000)
            .items(index * 3)
            .last_used(hours_ago(index))
            .risk(if index % 7 == 0 {
                Risk::Review
            } else {
                Risk::Moderate
            })
            .recovery(Recovery::Redownload)
            .selected(index % 7 != 0)
            .explanation(t!(
                "Dato de prueba, para comprobar cómo se comporta la lista con miles de filas.",
                "Test data, to check how the list behaves with thousands of rows."
            ))
        })
        .collect()
}

pub fn run(
    cancel: CancelToken,
    stress: bool,
    on_event: impl Fn(ScanEvent),
    on_finish: impl FnOnce(Vec<Finding>),
) {
    let mut all = Vec::new();
    let mut pending: Vec<&'static str> = scanners().into_iter().map(|(id, ..)| id).collect();
    while !pending.is_empty() && !cancel.is_cancelled() {
        // A few at a time, like a real thread pool.
        let batch: Vec<&'static str> = pending.drain(..pending.len().min(4)).collect();
        for &id in &batch {
            on_event(ScanEvent::Started { scanner_id: id });
        }
        for &id in &batch {
            std::thread::sleep(Duration::from_millis(120));
            let mut findings = findings_for(id);
            if stress && id == "dev.node_modules" {
                findings.extend(stress_findings());
            }
            on_event(ScanEvent::Finished {
                scanner_id: id,
                findings: findings.clone(),
                elapsed: Duration::from_millis(120),
            });
            all.extend(findings);
        }
    }
    on_finish(all);
}
