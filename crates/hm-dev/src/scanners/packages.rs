//! Package manager caches.
//!
//! The rule for this table is to list only what is really regenerable. Several tools keep things
//! that look like a cache next to things that are not, so entries name exact subfolders:
//!
//! - `~/.cargo/bin`, `~/go/bin`, `~/Library/pnpm/global`, `~/.pub-cache/bin` hold global installs.
//! - `~/Library/Caches/pypoetry/virtualenvs` holds the real virtualenvs of Poetry projects.
//! - `~/.m2/repository` can hold artifacts built with `mvn install` that exist nowhere else.
//! - `~/.gradle/gradle.properties` often holds signing credentials.

use anyhow::Result;
use hm_core::{t, Area, Finding, Recovery, Risk, ScanContext, Scanner};

use super::{table_findings, CacheSpec, Fallback, HasTool, Removal};
use crate::util;

const ID: &str = "dev.packages";

/// Built on every scan rather than kept in a `const`, so its text follows the current language.
pub(super) fn specs() -> Vec<CacheSpec> {
    let jvm = t!("Gradle y Maven", "Gradle and Maven");
    let apple = t!("Swift y CocoaPods", "Swift and CocoaPods");
    let others = t!("Otros", "Other");
    vec![
        // Node.
        CacheSpec {
            key: "npm",
            group: "Node",
            title: t!("Caché de npm", "npm cache"),
            paths: &[".npm/_cacache"],
            removal: Removal::Command {
                program: "npm",
                args: &["cache", "clean", "--force"],
                fallback: Fallback::TrashContents,
            },
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes que npm ha descargado. Los vuelve a descargar cuando hacen falta. No afecta a tus proyectos ni a los paquetes instalados con -g.",
                "Packages npm has downloaded. It downloads them again when needed. Your projects and the packages installed with -g are not affected."
            ),
        },
        CacheSpec {
            key: "npx",
            group: "Node",
            title: t!("Paquetes temporales de npx", "Temporary npx packages"),
            paths: &[".npm/_npx"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes descargados para ejecutarlos con npx. Se vuelven a descargar la próxima vez que los uses.",
                "Packages downloaded to run with npx. They are downloaded again the next time you use them."
            ),
        },
        CacheSpec {
            key: "npm_logs",
            group: "Node",
            title: t!("Registros de npm", "npm logs"),
            paths: &[".npm/_logs"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Registros de ejecuciones anteriores de npm. Solo sirven para investigar errores pasados.",
                "Logs from earlier npm runs. They are only useful for looking into past errors."
            ),
        },
        CacheSpec {
            key: "pnpm_store",
            group: "Node",
            title: t!("Almacén de pnpm", "pnpm store"),
            paths: &["Library/pnpm/store", ".local/share/pnpm/store", ".pnpm-store"],
            removal: Removal::Command {
                program: "pnpm",
                args: &["store", "prune"],
                fallback: Fallback::TrashContents,
            },
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            // The size shown is the whole store, and `prune` frees only what no project references.
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Almacén global de paquetes de pnpm. El tamaño mostrado es el de todo el almacén: «pnpm store prune» solo quita los paquetes que ya ningún proyecto usa, así que lo que libera suele ser bastante menos. Tus proyectos y los paquetes globales no se tocan.",
                "pnpm's global package store. The size shown is the whole store: “pnpm store prune” only removes packages no project uses anymore, so it usually frees quite a bit less. Your projects and global packages are left alone."
            ),
        },
        CacheSpec {
            key: "pnpm_cache",
            group: "Node",
            title: t!("Caché de metadatos de pnpm", "pnpm metadata cache"),
            paths: &["Library/Caches/pnpm"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Metadatos del registro y paquetes de «pnpm dlx». pnpm los regenera solo.",
                "Registry metadata and “pnpm dlx” packages. pnpm regenerates them on its own."
            ),
        },
        // Yarn's own command is not used: Yarn 2+ refuses `cache clean` outside a project, and the
        // version installed cannot be told without running a shim that may download one.
        CacheSpec {
            key: "yarn",
            group: "Node",
            title: t!("Caché de Yarn", "Yarn cache"),
            paths: &["Library/Caches/Yarn", ".yarn/berry/cache"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes que Yarn ha descargado. Los vuelve a descargar cuando hacen falta.",
                "Packages Yarn has downloaded. It downloads them again when needed."
            ),
        },
        // `bun pm cache rm` needs a package.json in the working directory, and the cleaner has none.
        CacheSpec {
            key: "bun",
            group: "Node",
            title: t!("Caché de Bun", "Bun cache"),
            paths: &[".bun/install/cache"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes que Bun ha descargado. Los vuelve a descargar cuando hacen falta. No toca los paquetes globales.",
                "Packages Bun has downloaded. It downloads them again when needed. Global packages are left alone."
            ),
        },
        CacheSpec {
            key: "bun_runtime",
            group: "Node",
            title: t!("Caché de transpilación de Bun", "Bun transpiler cache"),
            paths: &["Library/Caches/bun"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Archivos que Bun ya transpiló, guardados para arrancar más rápido. Los regenera solo.",
                "Files Bun has already transpiled, kept so it starts faster. It regenerates them on its own."
            ),
        },
        CacheSpec {
            key: "deno",
            group: "Node",
            title: t!("Caché de Deno", "Deno cache"),
            paths: &["Library/Caches/deno"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Módulos que Deno ha descargado y compilado. Los vuelve a descargar cuando hacen falta.",
                "Modules Deno has downloaded and compiled. It downloads them again when needed."
            ),
        },
        // Rust. `~/.cargo/bin` holds `cargo install` binaries and is deliberately absent.
        CacheSpec {
            key: "cargo_registry",
            group: "Rust",
            title: t!("Registro de Cargo", "Cargo registry"),
            paths: &[".cargo/registry/cache", ".cargo/registry/src", ".cargo/registry/index"],
            removal: Removal::Folders,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &["cargo"],
            explanation: t!(
                "Crates descargados y descomprimidos. Cargo los vuelve a descargar en la próxima compilación que los necesite, así que hasta entonces no podrás compilar sin conexión. Tus binarios de «cargo install» no se tocan.",
                "Downloaded and unpacked crates. Cargo downloads them again on the next build that needs them, so until then you can't build offline. Your “cargo install” binaries are left alone."
            ),
        },
        CacheSpec {
            key: "cargo_git",
            group: "Rust",
            title: t!("Dependencias Git de Cargo", "Cargo Git dependencies"),
            paths: &[".cargo/git/db", ".cargo/git/checkouts"],
            removal: Removal::Folders,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &["cargo"],
            explanation: t!(
                "Repositorios clonados por Cargo para dependencias Git. Los vuelve a clonar cuando hacen falta.",
                "Repositories Cargo cloned for Git dependencies. It clones them again when needed."
            ),
        },
        // Go. The module cache is written read-only on purpose, so only Go itself can remove it.
        CacheSpec {
            key: "go_modules",
            group: "Go",
            title: t!("Módulos de Go", "Go modules"),
            paths: &["go/pkg/mod"],
            removal: Removal::Command {
                program: "go",
                args: &["clean", "-modcache"],
                fallback: Fallback::ReportOnly(t!(
                    "Go guarda estos archivos como solo lectura y solo se pueden quitar con «go clean -modcache», pero Go no está instalado.",
                    "Go stores these files as read-only, so only “go clean -modcache” can remove them, and Go is not installed."
                )),
            },
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Módulos que Go ha descargado. Los vuelve a descargar en la próxima compilación que los necesite. Tus herramientas de «go install» no se tocan.",
                "Modules Go has downloaded. It downloads them again on the next build that needs them. Your “go install” tools are left alone."
            ),
        },
        CacheSpec {
            key: "go_build",
            group: "Go",
            title: t!("Caché de compilación de Go", "Go build cache"),
            paths: &["Library/Caches/go-build"],
            removal: Removal::Command {
                program: "go",
                args: &["clean", "-cache"],
                fallback: Fallback::TrashContents,
            },
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Resultados de compilaciones anteriores de Go. Se regeneran solos: la siguiente compilación solo tardará algo más.",
                "Results of earlier Go builds. They regenerate on their own: the next build just takes a little longer."
            ),
        },
        // JVM.
        CacheSpec {
            key: "gradle_caches",
            group: jvm,
            title: t!("Cachés de Gradle", "Gradle caches"),
            paths: &[".gradle/caches"],
            removal: Removal::Contents,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            // A Gradle daemon is a `java` process, and it holds these files open.
            blocked_by: &["java"],
            explanation: t!(
                "Dependencias descargadas y compilaciones en caché de Gradle. Vuelven en la siguiente compilación, que será bastante más lenta y necesitará conexión. Tu configuración de Gradle no se toca.",
                "Downloaded dependencies and Gradle's build cache. They come back on the next build, which will be much slower and need a connection. Your Gradle settings are left alone."
            ),
        },
        CacheSpec {
            key: "gradle_wrapper",
            group: jvm,
            title: t!("Distribuciones de Gradle", "Gradle distributions"),
            paths: &[".gradle/wrapper/dists"],
            removal: Removal::Contents,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &["java"],
            explanation: t!(
                "Versiones de Gradle descargadas por los wrappers de tus proyectos. Cada proyecto descarga la suya de nuevo cuando la necesita.",
                "Gradle versions downloaded by your projects' wrappers. Each project downloads its own again when it needs it."
            ),
        },
        CacheSpec {
            key: "gradle_daemon_logs",
            group: jvm,
            title: t!("Registros del daemon de Gradle", "Gradle daemon logs"),
            paths: &[".gradle/daemon"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Automatic,
            selected: true,
            blocked_by: &["java"],
            explanation: t!(
                "Registros de los daemons de Gradle. Solo sirven para investigar errores pasados.",
                "Logs from Gradle daemons. They are only useful for looking into past errors."
            ),
        },
        // Not a pure cache: `mvn install` puts artifacts here that may exist nowhere else.
        CacheSpec {
            key: "maven",
            group: jvm,
            title: t!("Repositorio local de Maven", "Maven local repository"),
            paths: &[".m2/repository"],
            removal: Removal::Contents,
            risk: Risk::Review,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &["java"],
            explanation: t!(
                "Dependencias descargadas por Maven, que se pueden volver a descargar. Pero también guarda lo que hayas publicado con «mvn install», que puede no existir en ningún otro sitio. Revísalo antes de limpiarlo.",
                "Dependencies Maven downloaded, which can be downloaded again. But it also holds anything you published with “mvn install”, which may not exist anywhere else. Review it before cleaning it."
            ),
        },
        // Python.
        CacheSpec {
            key: "pip",
            group: "Python",
            title: t!("Caché de pip", "pip cache"),
            paths: &["Library/Caches/pip"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes que pip ha descargado. Los vuelve a descargar cuando hacen falta. No afecta a lo que ya está instalado.",
                "Packages pip has downloaded. It downloads them again when needed. What is already installed is not affected."
            ),
        },
        CacheSpec {
            key: "pipx",
            group: "Python",
            title: t!("Caché de pipx", "pipx cache"),
            paths: &["Library/Caches/pipx"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Entornos temporales de «pipx run». Las herramientas instaladas con pipx no están aquí.",
                "Temporary environments from “pipx run”. Tools installed with pipx are not here."
            ),
        },
        CacheSpec {
            key: "uv",
            group: "Python",
            title: t!("Caché de uv", "uv cache"),
            paths: &[".cache/uv"],
            removal: Removal::Command {
                program: "uv",
                args: &["cache", "clean"],
                fallback: Fallback::TrashContents,
            },
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes que uv ha descargado. Tus entornos virtuales siguen funcionando, pero como comparten archivos con esta caché, lo que se libera puede ser menos de lo indicado. Tus Python y herramientas de uv no se tocan.",
                "Packages uv has downloaded. Your virtual environments keep working, but since they share files with this cache, it may free less than shown. Python versions and tools installed with uv are left alone."
            ),
        },
        CacheSpec {
            key: "poetry",
            group: "Python",
            title: t!("Caché de Poetry", "Poetry cache"),
            paths: &["Library/Caches/pypoetry/cache", "Library/Caches/pypoetry/artifacts"],
            removal: Removal::Folders,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes que Poetry ha descargado. Los vuelve a descargar cuando hacen falta. Los entornos virtuales de tus proyectos no se incluyen aquí.",
                "Packages Poetry has downloaded. It downloads them again when needed. Your projects' virtual environments are not included."
            ),
        },
        // Lives under Library/Caches but is not a cache: every Poetry project's environment is here.
        CacheSpec {
            key: "poetry_virtualenvs",
            group: "Python",
            title: t!("Entornos virtuales de Poetry", "Poetry virtual environments"),
            paths: &["Library/Caches/pypoetry/virtualenvs"],
            removal: Removal::Contents,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Los entornos de tus proyectos con Poetry, aunque estén en una carpeta de cachés. Cada proyecto dejará de funcionar hasta que vuelvas a ejecutar «poetry install».",
                "The environments of your Poetry projects, even though they sit in a caches folder. Each project stops working until you run “poetry install” again."
            ),
        },
        // Apple.
        CacheSpec {
            key: "cocoapods",
            group: apple,
            title: t!("Caché de CocoaPods", "CocoaPods cache"),
            paths: &["Library/Caches/CocoaPods"],
            removal: Removal::Command {
                program: "pod",
                args: &["cache", "clean", "--all"],
                fallback: Fallback::TrashContents,
            },
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Pods que CocoaPods ha descargado. Los vuelve a descargar en el próximo «pod install». Las carpetas Pods de tus proyectos no se tocan.",
                "Pods CocoaPods has downloaded. It downloads them again on the next “pod install”. The Pods folders in your projects are left alone."
            ),
        },
        CacheSpec {
            key: "cocoapods_repos",
            group: apple,
            title: t!(
                "Repositorios de especificaciones de CocoaPods",
                "CocoaPods spec repositories"
            ),
            paths: &[".cocoapods/repos"],
            removal: Removal::Contents,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Índices de pods. Se vuelven a descargar en el próximo «pod install». Si añadiste repositorios privados, tendrás que añadirlos de nuevo.",
                "Pod indexes. They are downloaded again on the next “pod install”. If you added private repositories, you'll have to add them again."
            ),
        },
        CacheSpec {
            key: "swiftpm",
            group: apple,
            title: t!("Caché de Swift Package Manager", "Swift Package Manager cache"),
            paths: &["Library/Caches/org.swift.swiftpm"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &["Xcode"],
            explanation: t!(
                "Paquetes de Swift descargados. Se vuelven a descargar cuando hacen falta.",
                "Downloaded Swift packages. They are downloaded again when needed."
            ),
        },
        // Others.
        CacheSpec {
            key: "homebrew",
            group: "Homebrew",
            title: t!("Caché de Homebrew", "Homebrew cache"),
            paths: &["Library/Caches/Homebrew"],
            removal: Removal::Command {
                program: "brew",
                args: &["cleanup", "-s"],
                fallback: Fallback::TrashContents,
            },
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Instaladores que Homebrew ha descargado. «brew cleanup -s» los quita y además borra versiones antiguas de lo que tengas instalado, lo que puede liberar más de lo indicado. Lo que usas sigue funcionando.",
                "Installers Homebrew has downloaded. “brew cleanup -s” removes them and also deletes old versions of what you have installed, so it may free more than shown. What you use keeps working."
            ),
        },
        CacheSpec {
            key: "composer",
            group: others,
            title: t!("Caché de Composer", "Composer cache"),
            paths: &[".composer/cache", "Library/Caches/composer"],
            removal: Removal::Contents,
            risk: Risk::Safe,
            recovery: Recovery::Redownload,
            selected: true,
            blocked_by: &[],
            explanation: t!(
                "Paquetes de PHP descargados. Se vuelven a descargar cuando hacen falta. Los paquetes globales no se tocan.",
                "Downloaded PHP packages. They are downloaded again when needed. Global packages are left alone."
            ),
        },
        CacheSpec {
            key: "pub",
            group: others,
            title: t!("Paquetes de Dart y Flutter", "Dart and Flutter packages"),
            paths: &[".pub-cache/hosted", ".pub-cache/hosted-hashes", ".pub-cache/_temp"],
            removal: Removal::Folders,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Paquetes descargados por pub. Vuelven con «flutter pub get» en cada proyecto, que no funcionará sin conexión hasta entonces. Las herramientas activadas con «pub global» no se tocan.",
                "Packages downloaded by pub. They come back with “flutter pub get” in each project, which won't work offline until then. Tools activated with “pub global” are left alone."
            ),
        },
        CacheSpec {
            key: "nuget",
            group: others,
            title: t!("Paquetes de NuGet", "NuGet packages"),
            paths: &[".nuget/packages"],
            removal: Removal::Contents,
            risk: Risk::Moderate,
            recovery: Recovery::Redownload,
            selected: false,
            blocked_by: &[],
            explanation: t!(
                "Paquetes de .NET descargados. Vuelven con «dotnet restore» en cada proyecto.",
                "Downloaded .NET packages. They come back with “dotnet restore” in each project."
            ),
        },
    ]
}

pub(super) struct PackageCacheScanner;

fn findings(ctx: &ScanContext, has_tool: HasTool) -> Vec<Finding> {
    util::finalize(ctx, table_findings(ctx, ID, &specs(), has_tool))
}

impl Scanner for PackageCacheScanner {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        t!("Cachés de paquetes", "Package caches")
    }

    fn description(&self) -> &'static str {
        t!(
            "Lo que npm, pnpm, Cargo, Go, Gradle, uv, Homebrew y compañía guardan para no descargarlo otra vez.",
            "What npm, pnpm, Cargo, Go, Gradle, uv, Homebrew and others keep so they don't have to download it again."
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
    use hm_core::Action;

    fn with_all_tools(ctx: &ScanContext) -> Vec<Finding> {
        findings(ctx, &|_| true)
    }

    fn no_tools(ctx: &ScanContext) -> Vec<Finding> {
        findings(ctx, &|_| false)
    }

    fn key<'a>(found: &'a [Finding], key: &str) -> Option<&'a Finding> {
        found
            .iter()
            .find(|finding| finding.id == format!("{ID}:{key}"))
    }

    fn all_targets(found: &[Finding]) -> Vec<String> {
        found
            .iter()
            .flat_map(|finding| finding.target_paths())
            .map(|path| path.display().to_string())
            .collect()
    }

    #[test]
    fn global_installs_are_never_offered() {
        let fx = Fixture::new();
        for global in [
            ".cargo/bin/ripgrep",
            "go/bin/gopls",
            "Library/pnpm/global/5/node_modules/x/index.js",
            "Library/pnpm/bin/tool",
            ".pub-cache/bin/fvm",
            ".composer/vendor/bin/x",
            ".local/share/uv/python/cpython/bin/python",
            ".local/share/uv/tools/ruff/bin/ruff",
        ] {
            fx.file(global, b"x");
        }
        let found = no_tools(&fx.ctx());
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn cargo_lists_only_downloads_and_leaves_binaries_alone() {
        let fx = Fixture::new();
        fx.file(".cargo/bin/ripgrep", b"x");
        fx.file(".cargo/registry/cache/index/serde.crate", b"x");
        fx.file(".cargo/registry/src/index/serde/lib.rs", b"x");
        let found = no_tools(&fx.ctx());
        let targets = all_targets(&found);
        assert!(targets.iter().all(|path| !path.contains(".cargo/bin")));
        let registry = key(&found, "cargo_registry").unwrap();
        assert_eq!(registry.risk, Risk::Moderate);
        assert!(!registry.selected_by_default);
    }

    #[test]
    fn poetry_keeps_virtualenvs_out_of_its_cache_finding() {
        let fx = Fixture::new();
        fx.file("Library/Caches/pypoetry/cache/a", b"x");
        fx.file("Library/Caches/pypoetry/artifacts/b", b"x");
        fx.file(
            "Library/Caches/pypoetry/virtualenvs/proj-py3.12/pyvenv.cfg",
            b"x",
        );
        let found = no_tools(&fx.ctx());
        let cache = key(&found, "poetry").unwrap();
        assert!(cache.selected_by_default);
        for target in cache.target_paths() {
            assert!(!target.ends_with("virtualenvs"));
        }
        let envs = key(&found, "poetry_virtualenvs").unwrap();
        assert!(!envs.selected_by_default);
        assert_eq!(envs.risk, Risk::Moderate);
    }

    #[test]
    fn maven_is_for_review_because_it_can_hold_local_only_artifacts() {
        let fx = Fixture::new();
        fx.file(".m2/repository/com/acme/lib/1.0/lib.jar", b"x");
        fx.file(".m2/settings.xml", b"<secrets/>");
        let found = no_tools(&fx.ctx());
        let maven = key(&found, "maven").unwrap();
        assert_eq!(maven.risk, Risk::Review);
        assert!(!maven.selected_by_default);
        assert!(all_targets(&found)
            .iter()
            .all(|path| !path.ends_with("settings.xml")));
    }

    #[test]
    fn gradle_never_touches_its_properties_file() {
        let fx = Fixture::new();
        fx.file(".gradle/gradle.properties", b"signing.password=hunter2");
        fx.file(".gradle/caches/modules-2/a.jar", b"x");
        let found = no_tools(&fx.ctx());
        let caches = key(&found, "gradle_caches").unwrap();
        assert_eq!(caches.blocked_by, vec!["java".to_string()]);
        assert!(all_targets(&found)
            .iter()
            .all(|path| path.ends_with(".gradle/caches")));
    }

    #[test]
    fn go_modules_need_go_because_they_are_read_only() {
        let fx = Fixture::new();
        fx.file("go/pkg/mod/github.com/x/y@v1/y.go", b"x");
        let with = with_all_tools(&fx.ctx());
        assert_eq!(
            key(&with, "go_modules").unwrap().action,
            Action::Command {
                program: "go".to_string(),
                args: vec!["clean".to_string(), "-modcache".to_string()],
            }
        );
        let without = no_tools(&fx.ctx());
        let report = key(&without, "go_modules").unwrap();
        assert_eq!(report.action, Action::ReportOnly);
        assert!(report.explanation.contains("no está instalado"));
    }

    #[test]
    fn tools_with_their_own_cleanup_use_it() {
        let fx = Fixture::new();
        fx.file(".npm/_cacache/content-v2/x", b"x");
        fx.file("Library/pnpm/store/v11/files/00/x", b"x");
        fx.file(".cache/uv/wheels/x", b"x");
        fx.file("Library/Caches/Homebrew/downloads/x", b"x");
        let found = with_all_tools(&fx.ctx());
        for (name, program) in [
            ("npm", "npm"),
            ("pnpm_store", "pnpm"),
            ("uv", "uv"),
            ("homebrew", "brew"),
        ] {
            match &key(&found, name).unwrap().action {
                Action::Command { program: used, .. } => assert_eq!(used, program),
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    #[test]
    fn missing_tools_fall_back_to_the_trash() {
        let fx = Fixture::new();
        fx.file(".npm/_cacache/content-v2/x", b"x");
        fx.file("Library/pnpm/store/v11/files/00/x", b"x");
        fx.file("Library/Caches/Homebrew/downloads/x", b"x");
        let found = no_tools(&fx.ctx());
        for name in ["npm", "pnpm_store", "homebrew"] {
            assert!(
                matches!(
                    key(&found, name).unwrap().action,
                    Action::TrashContents { .. }
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn the_pnpm_store_does_not_claim_to_free_its_whole_size() {
        let fx = Fixture::new();
        fx.file("Library/pnpm/store/v11/files/00/x", b"x");
        let found = with_all_tools(&fx.ctx());
        let store = key(&found, "pnpm_store").unwrap();
        assert!(!store.selected_by_default);
        assert!(store.explanation.contains("menos"));
    }

    #[test]
    fn every_trash_target_passes_the_cleaner_safety_policy() {
        let fx = Fixture::new();
        for file in [
            ".npm/_cacache/a",
            ".npm/_npx/a",
            ".cargo/registry/cache/a",
            ".cargo/git/db/a",
            ".gradle/caches/a",
            ".m2/repository/a",
            "Library/Caches/pip/a",
            ".cache/uv/a",
            "Library/Caches/Homebrew/a",
            ".bun/install/cache/a",
            ".pub-cache/hosted/a",
        ] {
            fx.file(file, b"x");
        }
        let ctx = fx.ctx();
        let found = no_tools(&ctx);
        assert!(found.len() >= 10);
        for finding in &found {
            assert_ne!(finding.action, Action::ReportOnly, "{}", finding.id);
        }
    }
}
