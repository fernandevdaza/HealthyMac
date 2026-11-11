//! The walk over the user's code folders.
//!
//! Three things need it (project artifacts, large git repositories and [`crate::projects`]), and it
//! is the most expensive read in the crate, so a single scan shares one walk. See [`shared_sweep`].
//!
//! The rules here protect real work, so they lean towards not offering something:
//!
//! - A folder only counts as an artifact with its manifest beside it. A `target` folder with no
//!   `Cargo.toml` next to it is just a folder someone named `target`.
//! - Ambiguous names (`build`, `dist`...) also need git to confirm they are ignored, because those
//!   names are often committed source. No repository means no confirmation.
//! - Anything git tracks is never an artifact, whatever it is called. Some projects commit
//!   `node_modules`, and `.gitignore` can list a folder that was committed before the rule existed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hm_core::{fmt::age_days, fsutil, Recovery, Risk, ScanContext, SizeInfo};
use rayon::prelude::*;

use crate::types::ProjectKind;
use crate::util::{self, Text};

/// Deep enough for real trees. A guard against pathological nesting, not a design limit.
const MAX_DEPTH: usize = 32;

/// A folder of this many levels below a project still says something about what it is made of.
/// Beyond that it is usually a test fixture or an example.
const KIND_DEPTH: usize = 2;

/// What removing an artifact costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactClass {
    /// Downloaded dependencies: needs the network to come back.
    Dependencies,
    /// Compiled output: needs a rebuild to come back.
    BuildOutput,
    /// A tool's own scratch space: the next run recreates it.
    ToolCache,
}

impl ArtifactClass {
    pub(crate) fn risk(self) -> Risk {
        match self {
            ArtifactClass::Dependencies | ArtifactClass::BuildOutput => Risk::Moderate,
            ArtifactClass::ToolCache => Risk::Safe,
        }
    }

    pub(crate) fn recovery(self) -> Recovery {
        match self {
            ArtifactClass::Dependencies => Recovery::Redownload,
            ArtifactClass::BuildOutput | ArtifactClass::ToolCache => Recovery::Rebuild,
        }
    }
}

/// How one artifact folder name is recognised.
#[derive(Debug)]
pub(crate) struct ArtifactRule {
    name: &'static str,
    /// `name` is a prefix (`cmake-build-debug`, `cmake-build-release`).
    prefix: bool,
    /// Any one of these beside the folder. `*.ext` matches by suffix, and folders count, since an
    /// Xcode project is a folder.
    manifests: &'static [&'static str],
    /// Counts with a Python manifest in any folder above it, since `__pycache__` appears beside
    /// every module and not just at the project root.
    python_scope: bool,
    /// Also needs `git check-ignore` to confirm it.
    pub(crate) ambiguous: bool,
    /// Never walked into, even when it does not qualify as an artifact. Without this, an orphan
    /// `node_modules` would yield every nested dependency as a "project".
    opaque: bool,
    /// A file that must exist inside. `venv` is only a virtualenv if it has a `pyvenv.cfg`.
    marker: Option<&'static str>,
    pub(crate) class: ArtifactClass,
    /// `false` if being ignored by git is not enough to assume it regenerates. These are listed
    /// but never preselected.
    pub(crate) preselect: bool,
    /// Many small folders per project, shown to the user as one line.
    pub(crate) aggregate: bool,
    /// What this is. A complete sentence.
    pub(crate) what: Text,
    /// How it comes back. A complete sentence.
    pub(crate) recover: Text,
}

impl ArtifactRule {
    const fn new(
        name: &'static str,
        manifests: &'static [&'static str],
        class: ArtifactClass,
        what: Text,
        recover: Text,
    ) -> Self {
        Self {
            name,
            prefix: false,
            manifests,
            python_scope: false,
            ambiguous: false,
            opaque: false,
            marker: None,
            class,
            preselect: true,
            aggregate: false,
            what,
            recover,
        }
    }

    const fn ambiguous(mut self) -> Self {
        self.ambiguous = true;
        self
    }

    const fn opaque(mut self) -> Self {
        self.opaque = true;
        self
    }

    const fn marker(mut self, marker: &'static str) -> Self {
        self.marker = Some(marker);
        self
    }

    const fn python_scope(mut self) -> Self {
        self.python_scope = true;
        self
    }

    const fn prefix(mut self) -> Self {
        self.prefix = true;
        self
    }

    const fn never_preselected(mut self) -> Self {
        self.preselect = false;
        self
    }

    const fn aggregate(mut self) -> Self {
        self.aggregate = true;
        self
    }

    fn matches(&self, name: &str) -> bool {
        if self.prefix {
            name.starts_with(self.name)
        } else {
            name == self.name
        }
    }
}

const NODE: &[&str] = &["package.json"];
const PYTHON: &[&str] = &[
    "pyproject.toml",
    "requirements.txt",
    "setup.py",
    "setup.cfg",
    "Pipfile",
    "poetry.lock",
    "uv.lock",
    "tox.ini",
    "noxfile.py",
];
const GRADLE: &[&str] = &[
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradlew",
];
const JVM_RUST: &[&str] = &["Cargo.toml", "pom.xml"];
const DOTNET: &[&str] = &["*.csproj", "*.fsproj", "*.vbproj", "*.sln"];
const XCODE: &[&str] = &["*.xcodeproj", "*.xcworkspace"];
/// Manifests of anything that can write to a generically named output folder.
const ANY_BUILD: &[&str] = &[
    "package.json",
    "pyproject.toml",
    "setup.py",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "CMakeLists.txt",
    "Makefile",
    "pubspec.yaml",
    "Package.swift",
    "build.zig",
    "mix.exs",
    "*.xcodeproj",
    "*.xcworkspace",
    "*.csproj",
    "*.fsproj",
    "*.vbproj",
    "*.sln",
];

const REINSTALL: Text = Text::new(
    "Se recupera al instalar de nuevo las dependencias del proyecto, con conexión.",
    "It comes back when you reinstall the project's dependencies, which needs a connection.",
);
const REBUILD: Text = Text::new(
    "Se regenera al compilar de nuevo el proyecto, lo que lleva su tiempo.",
    "It regenerates when you build the project again, which takes time.",
);
const NEXT_RUN: Text = Text::new(
    "La herramienta lo recrea la próxima vez que se ejecute.",
    "The tool recreates it the next time it runs.",
);

static RULES: &[ArtifactRule] = &[
    // Dependencies.
    ArtifactRule::new(
        "node_modules",
        NODE,
        ArtifactClass::Dependencies,
        Text::new("Dependencias instaladas del proyecto.", "The project's installed dependencies."),
        REINSTALL,
    )
    .opaque(),
    ArtifactRule::new(
        "bower_components",
        &["bower.json"],
        ArtifactClass::Dependencies,
        Text::new("Dependencias de Bower.", "Bower dependencies."),
        Text::new(
            "Se recupera con «bower install», con conexión.",
            "It comes back with “bower install”, which needs a connection.",
        ),
    )
    .opaque(),
    ArtifactRule::new(
        "Pods",
        &["Podfile"],
        ArtifactClass::Dependencies,
        Text::new(
            "Dependencias de CocoaPods del proyecto.",
            "The project's CocoaPods dependencies.",
        ),
        Text::new(
            "Se recupera con «pod install», con conexión.",
            "It comes back with “pod install”, which needs a connection.",
        ),
    ),
    ArtifactRule::new(
        ".dart_tool",
        &["pubspec.yaml"],
        ArtifactClass::Dependencies,
        Text::new(
            "Estado de las dependencias y compilaciones de Dart.",
            "State of Dart dependencies and builds.",
        ),
        Text::new(
            "Se recupera con «flutter pub get» o «dart pub get».",
            "It comes back with “flutter pub get” or “dart pub get”.",
        ),
    )
    .opaque(),
    ArtifactRule::new(
        ".venv",
        PYTHON,
        ArtifactClass::Dependencies,
        Text::new(
            "Entorno virtual de Python del proyecto.",
            "The project's Python virtual environment.",
        ),
        REINSTALL,
    )
    .marker("pyvenv.cfg")
    .opaque(),
    ArtifactRule::new(
        "venv",
        PYTHON,
        ArtifactClass::Dependencies,
        Text::new(
            "Entorno virtual de Python del proyecto.",
            "The project's Python virtual environment.",
        ),
        REINSTALL,
    )
    .marker("pyvenv.cfg")
    .opaque(),
    ArtifactRule::new(
        "env",
        PYTHON,
        ArtifactClass::Dependencies,
        Text::new(
            "Entorno virtual de Python del proyecto.",
            "The project's Python virtual environment.",
        ),
        REINSTALL,
    )
    .marker("pyvenv.cfg")
    .opaque(),
    ArtifactRule::new(
        ".tox",
        PYTHON,
        ArtifactClass::Dependencies,
        Text::new("Entornos de prueba de tox.", "tox test environments."),
        Text::new(
            "Tox los recrea en la próxima ejecución, con conexión.",
            "Tox recreates them on the next run, which needs a connection.",
        ),
    )
    .opaque(),
    ArtifactRule::new(
        ".nox",
        PYTHON,
        ArtifactClass::Dependencies,
        Text::new("Entornos de prueba de nox.", "nox test environments."),
        Text::new(
            "Nox los recrea en la próxima ejecución, con conexión.",
            "Nox recreates them on the next run, which needs a connection.",
        ),
    )
    .opaque(),
    ArtifactRule::new(
        ".terraform",
        &["*.tf"],
        ArtifactClass::Dependencies,
        Text::new(
            "Proveedores y módulos descargados por Terraform.",
            "Providers and modules downloaded by Terraform.",
        ),
        Text::new(
            "Se recupera con «terraform init». El estado de tu infraestructura no está aquí.",
            "It comes back with “terraform init”. Your infrastructure state is not here.",
        ),
    )
    .opaque(),
    ArtifactRule::new(
        "elm-stuff",
        &["elm.json"],
        ArtifactClass::Dependencies,
        Text::new("Dependencias y compilados de Elm.", "Elm dependencies and build output."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        "vendor",
        &["composer.json", "go.mod", "Gemfile"],
        ArtifactClass::Dependencies,
        Text::new(
            "Dependencias copiadas dentro del proyecto. Git las ignora.",
            "Dependencies copied into the project. Git ignores them.",
        ),
        REINSTALL,
    )
    .ambiguous()
    .never_preselected(),
    ArtifactRule::new(
        "deps",
        &["mix.exs", "rebar.config"],
        ArtifactClass::Dependencies,
        Text::new(
            "Dependencias de Elixir o Erlang. Git las ignora.",
            "Elixir or Erlang dependencies. Git ignores them.",
        ),
        REINSTALL,
    )
    .ambiguous()
    .never_preselected(),
    ArtifactRule::new(
        "Carthage",
        &["Cartfile"],
        ArtifactClass::Dependencies,
        Text::new(
            "Dependencias y compilados de Carthage. Git los ignora.",
            "Carthage dependencies and build output. Git ignores them.",
        ),
        Text::new(
            "Se recupera con «carthage bootstrap», con conexión.",
            "It comes back with “carthage bootstrap”, which needs a connection.",
        ),
    )
    .ambiguous()
    .never_preselected(),
    // Compiled output.
    ArtifactRule::new(
        "target",
        JVM_RUST,
        ArtifactClass::BuildOutput,
        Text::new("Compilados de Cargo o Maven.", "Cargo or Maven build output."),
        REBUILD,
    ),
    ArtifactRule::new(
        ".next",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new("Compilación y caché de Next.js.", "Next.js build and cache."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".nuxt",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new("Compilación de Nuxt.", "Nuxt build."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".svelte-kit",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new("Compilación de SvelteKit.", "SvelteKit build."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".angular",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new("Caché de compilación de Angular.", "Angular build cache."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".docusaurus",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new("Archivos generados por Docusaurus.", "Files generated by Docusaurus."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".astro",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new("Archivos generados por Astro.", "Files generated by Astro."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".gradle",
        GRADLE,
        ArtifactClass::BuildOutput,
        Text::new("Caché de Gradle de este proyecto.", "This project's Gradle cache."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".kotlin",
        GRADLE,
        ArtifactClass::BuildOutput,
        Text::new("Datos de compilación de Kotlin.", "Kotlin build data."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".cxx",
        GRADLE,
        ArtifactClass::BuildOutput,
        Text::new("Compilados nativos de Android.", "Android native build output."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        ".build",
        &["Package.swift"],
        ArtifactClass::BuildOutput,
        Text::new(
            "Compilados y dependencias de Swift Package Manager.",
            "Swift Package Manager build output and dependencies.",
        ),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        "DerivedData",
        XCODE,
        ArtifactClass::BuildOutput,
        Text::new(
            "Compilados de Xcode guardados dentro del proyecto.",
            "Xcode build output kept inside the project.",
        ),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        "cmake-build-",
        &["CMakeLists.txt"],
        ArtifactClass::BuildOutput,
        Text::new("Compilación de CMake creada por el IDE.", "CMake build created by the IDE."),
        REBUILD,
    )
    .prefix()
    .opaque(),
    ArtifactRule::new(
        ".stack-work",
        &["stack.yaml"],
        ArtifactClass::BuildOutput,
        Text::new("Compilados de Haskell Stack.", "Haskell Stack build output."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        "_build",
        &["mix.exs", "rebar.config"],
        ArtifactClass::BuildOutput,
        Text::new("Compilados de Elixir o Erlang.", "Elixir or Erlang build output."),
        REBUILD,
    ),
    ArtifactRule::new(
        ".zig-cache",
        &["build.zig"],
        ArtifactClass::BuildOutput,
        Text::new("Caché de compilación de Zig.", "Zig build cache."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        "zig-cache",
        &["build.zig"],
        ArtifactClass::BuildOutput,
        Text::new("Caché de compilación de Zig.", "Zig build cache."),
        REBUILD,
    )
    .opaque(),
    ArtifactRule::new(
        "build",
        ANY_BUILD,
        ArtifactClass::BuildOutput,
        Text::new(
            "Carpeta de compilación. Git confirma que está ignorada.",
            "Build folder. Git confirms it is ignored.",
        ),
        REBUILD,
    )
    .ambiguous(),
    ArtifactRule::new(
        "dist",
        ANY_BUILD,
        ArtifactClass::BuildOutput,
        Text::new(
            "Carpeta de distribución. Git confirma que está ignorada.",
            "Distribution folder. Git confirms it is ignored.",
        ),
        REBUILD,
    )
    .ambiguous(),
    ArtifactRule::new(
        "out",
        ANY_BUILD,
        ArtifactClass::BuildOutput,
        Text::new(
            "Carpeta de salida. Git confirma que está ignorada, pero conviene mirarla: algunas herramientas guardan aquí resultados que costó obtener.",
            "Output folder. Git confirms it is ignored, but take a look first: some tools keep results here that took effort to produce.",
        ),
        REBUILD,
    )
    .ambiguous()
    .never_preselected(),
    ArtifactRule::new(
        "bin",
        ANY_BUILD,
        ArtifactClass::BuildOutput,
        Text::new(
            "Binarios del proyecto. Git confirma que están ignorados, pero conviene mirarlos: pueden ser herramientas descargadas a mano.",
            "The project's binaries. Git confirms they are ignored, but take a look first: they may be tools downloaded by hand.",
        ),
        REBUILD,
    )
    .ambiguous()
    .never_preselected(),
    ArtifactRule::new(
        "obj",
        DOTNET,
        ArtifactClass::BuildOutput,
        Text::new(
            "Compilados intermedios de .NET. Git confirma que están ignorados.",
            "Intermediate .NET build output. Git confirms it is ignored.",
        ),
        REBUILD,
    )
    .ambiguous(),
    ArtifactRule::new(
        ".output",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new(
            "Compilación de Nuxt o Nitro. Git confirma que está ignorada.",
            "Nuxt or Nitro build. Git confirms it is ignored.",
        ),
        REBUILD,
    )
    .ambiguous(),
    ArtifactRule::new(
        "storybook-static",
        NODE,
        ArtifactClass::BuildOutput,
        Text::new(
            "Exportación de Storybook. Git confirma que está ignorada.",
            "Storybook export. Git confirms it is ignored.",
        ),
        REBUILD,
    )
    .ambiguous(),
    // A tool's own scratch space.
    ArtifactRule::new(
        ".turbo",
        NODE,
        ArtifactClass::ToolCache,
        Text::new("Caché de Turborepo.", "Turborepo cache."),
        NEXT_RUN,
    )
    .opaque(),
    ArtifactRule::new(
        ".parcel-cache",
        NODE,
        ArtifactClass::ToolCache,
        Text::new("Caché de Parcel.", "Parcel cache."),
        NEXT_RUN,
    )
    .opaque(),
    ArtifactRule::new(
        ".nyc_output",
        NODE,
        ArtifactClass::ToolCache,
        Text::new("Datos intermedios de cobertura de nyc.", "Intermediate nyc coverage data."),
        NEXT_RUN,
    )
    .opaque(),
    ArtifactRule::new(
        ".cache",
        NODE,
        ArtifactClass::ToolCache,
        Text::new(
            "Caché de herramientas del proyecto. Git confirma que está ignorada.",
            "The project's tool cache. Git confirms it is ignored.",
        ),
        NEXT_RUN,
    )
    .ambiguous(),
    ArtifactRule::new(
        "coverage",
        ANY_BUILD,
        ArtifactClass::ToolCache,
        Text::new(
            "Informe de cobertura de tests. Git confirma que está ignorado.",
            "Test coverage report. Git confirms it is ignored.",
        ),
        Text::new(
            "Se regenera al ejecutar los tests con cobertura.",
            "It regenerates when you run the tests with coverage.",
        ),
    )
    .ambiguous(),
    ArtifactRule::new(
        "htmlcov",
        PYTHON,
        ArtifactClass::ToolCache,
        Text::new(
            "Informe de cobertura de tests. Git confirma que está ignorado.",
            "Test coverage report. Git confirms it is ignored.",
        ),
        Text::new(
            "Se regenera al ejecutar los tests con cobertura.",
            "It regenerates when you run the tests with coverage.",
        ),
    )
    .ambiguous(),
    ArtifactRule::new(
        "__pycache__",
        PYTHON,
        ArtifactClass::ToolCache,
        Text::new("Bytecode compilado por Python.", "Bytecode compiled by Python."),
        NEXT_RUN,
    )
    .python_scope()
    .opaque()
    .aggregate(),
    ArtifactRule::new(
        ".pytest_cache",
        PYTHON,
        ArtifactClass::ToolCache,
        Text::new("Caché de pytest.", "pytest cache."),
        NEXT_RUN,
    )
    .python_scope()
    .opaque(),
    ArtifactRule::new(
        ".mypy_cache",
        PYTHON,
        ArtifactClass::ToolCache,
        Text::new("Caché de mypy.", "mypy cache."),
        NEXT_RUN,
    )
    .python_scope()
    .opaque(),
    ArtifactRule::new(
        ".ruff_cache",
        PYTHON,
        ArtifactClass::ToolCache,
        Text::new("Caché de Ruff.", "Ruff cache."),
        NEXT_RUN,
    )
    .python_scope()
    .opaque(),
];

fn rule_for(name: &str) -> Option<&'static ArtifactRule> {
    RULES.iter().find(|rule| rule.matches(name))
}

/// Which manifests mean which kind of project.
const KIND_MANIFESTS: &[(&str, ProjectKind)] = &[
    ("package.json", ProjectKind::Node),
    ("Cargo.toml", ProjectKind::Rust),
    ("pyproject.toml", ProjectKind::Python),
    ("requirements.txt", ProjectKind::Python),
    ("setup.py", ProjectKind::Python),
    ("setup.cfg", ProjectKind::Python),
    ("Pipfile", ProjectKind::Python),
    ("poetry.lock", ProjectKind::Python),
    ("uv.lock", ProjectKind::Python),
    ("tox.ini", ProjectKind::Python),
    ("noxfile.py", ProjectKind::Python),
    ("Package.swift", ProjectKind::Swift),
    ("*.xcodeproj", ProjectKind::Xcode),
    ("*.xcworkspace", ProjectKind::Xcode),
    ("Podfile", ProjectKind::Xcode),
    ("go.mod", ProjectKind::Go),
    ("build.gradle", ProjectKind::Gradle),
    ("build.gradle.kts", ProjectKind::Gradle),
    ("settings.gradle", ProjectKind::Gradle),
    ("settings.gradle.kts", ProjectKind::Gradle),
    ("gradlew", ProjectKind::Gradle),
    ("pom.xml", ProjectKind::Maven),
    ("pubspec.yaml", ProjectKind::Flutter),
    ("Gemfile", ProjectKind::Ruby),
    ("composer.json", ProjectKind::Php),
    ("*.csproj", ProjectKind::DotNet),
    ("*.fsproj", ProjectKind::DotNet),
    ("*.vbproj", ProjectKind::DotNet),
    ("*.sln", ProjectKind::DotNet),
    ("CMakeLists.txt", ProjectKind::CMake),
    ("mix.exs", ProjectKind::Other),
    ("rebar.config", ProjectKind::Other),
    ("stack.yaml", ProjectKind::Other),
    ("elm.json", ProjectKind::Other),
    ("build.zig", ProjectKind::Other),
    ("bower.json", ProjectKind::Other),
    ("Cartfile", ProjectKind::Other),
    ("*.tf", ProjectKind::Other),
];

/// Folder suffixes that are single documents on macOS: nothing useful inside, often huge.
const BUNDLE_SUFFIXES: &[&str] = &[
    ".app",
    ".framework",
    ".xcframework",
    ".xcarchive",
    ".xcodeproj",
    ".xcworkspace",
    ".xcassets",
    ".photoslibrary",
    ".dSYM",
    ".bundle",
];

/// Never walked into, wherever they appear.
const NEVER_ENTER: &[&str] = &[".git", ".hg", ".svn", ".Trash"];

/// Direct children of the home folder that hold no projects, or that must not be touched. Only
/// reached if the user configured the home folder itself as a root. Prompting folders are handled
/// separately because Full Disk Access unlocks those.
const HOME_NEVER_ENTER: &[&str] = &[
    "Library",
    "Applications",
    "Movies",
    "Music",
    "Pictures",
    "Public",
];

/// Home folders that raise a permission prompt.
const HOME_PROMPTING: &[&str] = &["Documents", "Desktop", "Downloads"];

fn matches_manifest(pattern: &str, name: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => name.len() > suffix.len() && name.ends_with(suffix),
        None => pattern == name,
    }
}

fn has_manifest(names: &[String], manifests: &[&str]) -> bool {
    manifests.iter().any(|pattern| {
        names
            .iter()
            .any(|name| matches_manifest(pattern, name.as_str()))
    })
}

fn kinds_in(names: &[String]) -> Vec<ProjectKind> {
    let mut kinds = Vec::new();
    for (pattern, kind) in KIND_MANIFESTS {
        if !kinds.contains(kind) && has_manifest(names, &[pattern]) {
            kinds.push(*kind);
        }
    }
    kinds
}

/// A version a project asks for, read from its pin file. Toolchain scanners use this to warn before
/// offering to remove something a project depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PinKind {
    Node,
    Python,
    Rust,
}

#[derive(Clone, Debug)]
pub(crate) struct Pin {
    pub(crate) kind: PinKind,
    pub(crate) value: String,
    pub(crate) project: PathBuf,
}

/// Pin files are one line in practice. This stops a misnamed big file being read whole.
const PIN_MAX_BYTES: u64 = 16 * 1024;

fn read_small(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > PIN_MAX_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// The channel from a `rust-toolchain` file, in either its plain or TOML form.
pub(crate) fn parse_rust_toolchain_file(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("channel") {
            let value = rest.trim_start().strip_prefix('=')?.trim();
            return Some(value.trim_matches(|c| c == '"' || c == '\'').to_string());
        }
    }
    let first = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    (!first.starts_with('[') && !first.contains('=')).then(|| first.to_string())
}

fn read_pins(dir: &Path, names: &[String]) -> Vec<Pin> {
    let mut pins = Vec::new();
    for name in names {
        let path = || dir.join(name);
        match name.as_str() {
            ".nvmrc" | ".node-version" => {
                if let Some(text) = read_small(&path()) {
                    let value = text.trim();
                    if !value.is_empty() {
                        pins.push(Pin {
                            kind: PinKind::Node,
                            value: value.to_string(),
                            project: dir.to_path_buf(),
                        });
                    }
                }
            }
            ".python-version" => {
                if let Some(text) = read_small(&path()) {
                    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                        pins.push(Pin {
                            kind: PinKind::Python,
                            value: line.to_string(),
                            project: dir.to_path_buf(),
                        });
                    }
                }
            }
            "rust-toolchain" | "rust-toolchain.toml" => {
                if let Some(value) = read_small(&path())
                    .as_deref()
                    .and_then(parse_rust_toolchain_file)
                {
                    pins.push(Pin {
                        kind: PinKind::Rust,
                        value,
                        project: dir.to_path_buf(),
                    });
                }
            }
            _ => {}
        }
    }
    pins
}

/// A virtualenv depends on the interpreter it was created from, so it pins that pyenv version even
/// when the project has no `.python-version`.
pub(crate) fn pyenv_version_of_venv(cfg: &str) -> Option<String> {
    let home = cfg.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "home").then(|| value.trim())
    })?;
    let (_, rest) = home.split_once(".pyenv/versions/")?;
    let version = rest.split('/').next()?;
    (!version.is_empty()).then(|| version.to_string())
}

#[derive(Clone, Debug)]
pub(crate) struct SweptArtifact {
    pub(crate) path: PathBuf,
    /// The folder's actual name: "node_modules", "cmake-build-debug"...
    pub(crate) label: String,
    pub(crate) rule: &'static ArtifactRule,
    /// How to get it back, when a lockfile beside it says exactly how.
    pub(crate) hint: Option<&'static str>,
    pub(crate) info: SizeInfo,
}

#[derive(Clone, Debug)]
pub(crate) struct SweptProject {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    /// Path relative to the swept root: unique, and how projects are grouped in the UI.
    pub(crate) display: String,
    pub(crate) kinds: Vec<ProjectKind>,
    pub(crate) artifacts: Vec<SweptArtifact>,
    /// Newest file in the project, ignoring artifacts, version control and Finder's `.DS_Store`.
    pub(crate) newest_source: Option<SystemTime>,
    pub(crate) last_commit: Option<SystemTime>,
    /// The working tree root when this project is a git repository.
    pub(crate) repo: Option<PathBuf>,
    pub(crate) git_dir: Option<PathBuf>,
    pub(crate) git_dir_bytes: u64,
}

impl SweptProject {
    pub(crate) fn last_activity(&self) -> Option<SystemTime> {
        util::newest(self.newest_source, self.last_commit)
    }

    /// `None` when there is no activity to go on, or it is in the future. Neither may ever read as
    /// idle, since idle is what makes an artifact preselected.
    pub(crate) fn idle_days(&self, now: SystemTime) -> Option<u64> {
        age_days(self.last_activity()?, now)
    }

    pub(crate) fn artifact_bytes(&self) -> u64 {
        self.artifacts
            .iter()
            .map(|artifact| artifact.info.exclusive())
            .sum()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Sweep {
    pub(crate) projects: Vec<SweptProject>,
    pub(crate) pins: Vec<Pin>,
    /// The home folder is a root and a prompting folder inside it was skipped.
    pub(crate) skipped_protected: bool,
    pub(crate) cancelled: bool,
}

struct Env<'a> {
    ctx: &'a ScanContext,
    git: Option<PathBuf>,
    home: PathBuf,
    excluded: Vec<PathBuf>,
    full_disk_access: bool,
    skipped_protected: AtomicBool,
}

impl Env<'_> {
    fn is_excluded(&self, path: &Path) -> bool {
        self.excluded
            .iter()
            .any(|excluded| path.starts_with(excluded))
    }

    /// Whether a child of `dir` is off limits because of where it is, not what it is called.
    fn off_limits(&self, dir: &Path, name: &str) -> bool {
        if dir != self.home {
            return false;
        }
        if HOME_NEVER_ENTER.contains(&name) || name.starts_with('.') {
            return true;
        }
        if HOME_PROMPTING.contains(&name) && !self.full_disk_access {
            self.skipped_protected.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }
}

#[derive(Debug)]
struct RawArtifact {
    path: PathBuf,
    label: String,
    rule: &'static ArtifactRule,
    hint: Option<&'static str>,
}

#[derive(Debug)]
struct RawProject {
    path: PathBuf,
    is_repo: bool,
    kinds: Vec<ProjectKind>,
    newest: Option<SystemTime>,
    artifacts: Vec<RawArtifact>,
}

impl RawProject {
    fn absorb(&mut self, inner: RawProject) {
        self.newest = util::newest(self.newest, inner.newest);
        self.artifacts.extend(inner.artifacts);
        let depth = inner
            .path
            .strip_prefix(&self.path)
            .map(|rest| rest.components().count())
            .unwrap_or(usize::MAX);
        if depth <= KIND_DEPTH {
            for kind in inner.kinds {
                if !self.kinds.contains(&kind) {
                    self.kinds.push(kind);
                }
            }
        }
    }
}

/// `true` for the build folder this program runs from. A developer's own `target` is otherwise a
/// perfect candidate, and cleaning it would pull the running app out from under itself.
fn holds_running_program(path: &Path) -> bool {
    static PROGRAM: OnceLock<Option<PathBuf>> = OnceLock::new();
    PROGRAM
        .get_or_init(hm_core::safety::running_program)
        .as_ref()
        .is_some_and(|program| program.starts_with(path))
}

/// What a subtree reports to its parent. Built bottom-up so that a project's activity and artifacts
/// are known by the time its boundary is reached, with no second pass over the tree.
#[derive(Debug, Default)]
struct DirResult {
    /// Newest source file not yet claimed by a project.
    newest: Option<SystemTime>,
    /// Artifacts waiting for the project folder above them.
    artifacts: Vec<RawArtifact>,
    /// Manifest folders with no repository of their own. A manifest further up absorbs them.
    pending: Vec<RawProject>,
    /// Git repositories, which always stand alone.
    closed: Vec<RawProject>,
    pins: Vec<Pin>,
}

impl DirResult {
    fn merge(mut self, other: DirResult) -> DirResult {
        self.newest = util::newest(self.newest, other.newest);
        self.artifacts.extend(other.artifacts);
        self.pending.extend(other.pending);
        self.closed.extend(other.closed);
        self.pins.extend(other.pins);
        self
    }
}

/// How to reinstall, read from the lockfile beside the artifact. Only for rules where that is what
/// the recovery depends on.
fn install_hint(rule: &ArtifactRule, names: &[String]) -> Option<&'static str> {
    let has = |file: &str| names.iter().any(|name| name == file);
    match rule.name {
        "node_modules" => {
            if has("pnpm-lock.yaml") {
                Some("pnpm install")
            } else if has("bun.lock") || has("bun.lockb") {
                Some("bun install")
            } else if has("yarn.lock") {
                Some("yarn install")
            } else if has("package-lock.json") {
                Some("npm install")
            } else {
                None
            }
        }
        ".venv" | "venv" | "env" => {
            if has("uv.lock") {
                Some("uv sync")
            } else if has("poetry.lock") {
                Some("poetry install")
            } else if has("requirements.txt") {
                Some("pip install -r requirements.txt")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Of the folders passed, which does git say are ignored? Empty on any doubt.
fn ignored_among(git: &Path, repo: &Path, candidates: &[PathBuf]) -> HashSet<PathBuf> {
    let relatives: Vec<&str> = candidates
        .iter()
        .filter_map(|path| path.strip_prefix(repo).ok()?.to_str())
        .collect();
    if relatives.is_empty() {
        return HashSet::new();
    }
    // `-z` is only accepted with `--stdin`, and the runner closes stdin, so this reads lines. Turning
    // off path quoting keeps accented names literal. The few names git still quotes will not match
    // below, which counts as not confirmed.
    let mut args = vec!["-c", "core.quotePath=false", "check-ignore", "--"];
    args.extend(relatives);
    let Some(output) = util::run_git(git, repo, &args) else {
        return HashSet::new();
    };
    // 0: some are ignored. 1: none are. Anything else: git could not answer.
    if output.code != Some(0) {
        return HashSet::new();
    }
    output
        .stdout
        .lines()
        .filter(|path| !path.is_empty())
        .map(|path| repo.join(path))
        .collect()
}

/// Of the repo-relative folders passed, which contain tracked files? `None` if git could not answer.
fn tracked_among(git: &Path, repo: &Path, relatives: &[&str]) -> Option<HashSet<String>> {
    let mut tracked = HashSet::new();
    // Monorepos can have hundreds of artifacts, and the argument list has a limit.
    for chunk in relatives.chunks(200) {
        let mut args = vec!["ls-files", "-z", "--"];
        args.extend_from_slice(chunk);
        let output = util::run_git(git, repo, &args)?;
        if !output.success() {
            return None;
        }
        for file in output.stdout.split('\0').filter(|file| !file.is_empty()) {
            for relative in chunk {
                let inside = file
                    .strip_prefix(relative)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
                if inside {
                    tracked.insert((*relative).to_string());
                }
            }
        }
    }
    Some(tracked)
}

fn walk(
    env: &Env,
    dir: &Path,
    device: u64,
    depth: usize,
    repo: Option<&Path>,
    python: bool,
) -> DirResult {
    let mut result = DirResult::default();
    if depth > MAX_DEPTH || env.ctx.is_cancelled() {
        return result;
    }
    // A folder that cannot be read counts as empty, never as an error.
    let Ok(entries) = fs::read_dir(dir) else {
        return result;
    };

    let mut names = Vec::new();
    let mut subdirs = Vec::new();
    let mut has_git = false;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        // A `.git` in the home folder is a dotfiles repository, whose `.gitignore` often says
        // `*`. Believing it would turn every folder in the home into a confirmed artifact.
        if name == ".git" {
            has_git = dir != env.home;
            continue;
        }
        if file_type.is_dir() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.dev() == device {
                subdirs.push((name.clone(), entry.path()));
            }
        } else if file_type.is_file() && name != ".DS_Store" {
            if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
                result.newest = util::newest(result.newest, Some(modified));
            }
        }
        // Symlinks are never followed, but a symlinked manifest is still a manifest.
        names.push(name);
    }

    let python_here = python || has_manifest(&names, PYTHON);
    let repo_here = if has_git { Some(dir) } else { repo };
    result.pins = read_pins(dir, &names);

    let mut descend = Vec::new();
    let mut maybe: Vec<(PathBuf, String, &'static ArtifactRule)> = Vec::new();
    let mut own_artifacts = Vec::new();
    for (name, path) in subdirs {
        if NEVER_ENTER.contains(&name.as_str())
            || BUNDLE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
            || env.off_limits(dir, &name)
            || env.is_excluded(&path)
        {
            continue;
        }
        let Some(rule) = rule_for(&name) else {
            descend.push(path);
            continue;
        };
        if holds_running_program(&path) {
            continue;
        }
        let marker_ok = rule.marker.is_none_or(|marker| path.join(marker).is_file());
        let manifest_ok = if rule.python_scope {
            python_here
        } else {
            has_manifest(&names, rule.manifests)
        };
        if marker_ok && manifest_ok {
            if rule.ambiguous {
                maybe.push((path, name, rule));
            } else {
                if matches!(rule.name, ".venv" | "venv" | "env") {
                    if let Some(version) = read_small(&path.join("pyvenv.cfg"))
                        .as_deref()
                        .and_then(pyenv_version_of_venv)
                    {
                        result.pins.push(Pin {
                            kind: PinKind::Python,
                            value: version,
                            project: dir.to_path_buf(),
                        });
                    }
                }
                own_artifacts.push(RawArtifact {
                    path,
                    label: name,
                    rule,
                    hint: install_hint(rule, &names),
                });
            }
        } else if !(rule.opaque && marker_ok) {
            descend.push(path);
        }
    }

    // Git is only asked when a name is ambiguous, and once for all the candidates in a folder.
    if !maybe.is_empty() {
        let ignored = match (env.git.as_deref(), repo_here) {
            (Some(git), Some(repo)) => {
                let paths: Vec<PathBuf> = maybe.iter().map(|(path, _, _)| path.clone()).collect();
                ignored_among(git, repo, &paths)
            }
            _ => HashSet::new(),
        };
        for (path, name, rule) in maybe {
            if ignored.contains(&path) {
                own_artifacts.push(RawArtifact {
                    path,
                    label: name,
                    rule,
                    hint: install_hint(rule, &names),
                });
            } else {
                // Not confirmed, so it is real source: walk it like any other folder.
                descend.push(path);
            }
        }
    }

    let children = descend
        .par_iter()
        .map(|path| walk(env, path, device, depth + 1, repo_here, python_here))
        .reduce(DirResult::default, DirResult::merge);
    let mut merged = result.merge(children);
    let has_own_artifacts = !own_artifacts.is_empty();
    merged.artifacts.extend(own_artifacts);

    let kinds = kinds_in(&names);
    if has_git || !kinds.is_empty() || has_own_artifacts {
        let mut project = RawProject {
            path: dir.to_path_buf(),
            is_repo: has_git,
            kinds,
            newest: merged.newest.take(),
            artifacts: std::mem::take(&mut merged.artifacts),
        };
        for inner in std::mem::take(&mut merged.pending) {
            project.absorb(inner);
        }
        if has_git {
            merged.closed.push(project);
        } else {
            merged.pending.push(project);
        }
    }
    merged
}

/// The repository a root sits inside, if the root is not itself one. A `.git` in the home folder or
/// above it does not count: see the note in [`walk`].
fn enclosing_repo(root: &Path, home: &Path) -> Option<PathBuf> {
    for ancestor in root.ancestors().skip(1) {
        if ancestor == home || !ancestor.starts_with(home) {
            return None;
        }
        if fs::symlink_metadata(ancestor.join(".git")).is_ok() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

/// The real git directory: `.git` itself, or where a worktree or submodule's `.git` file points.
fn resolve_git_dir(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    let meta = fs::symlink_metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    let text = read_small(&dot_git)?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        repo.join(target)
    })
}

pub(crate) fn parse_commit_time(stdout: &str) -> Option<SystemTime> {
    let seconds = stdout.trim().parse::<u64>().ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn finish_project(env: &Env, raw: RawProject, root: &Path) -> Option<SweptProject> {
    let RawProject {
        path,
        is_repo,
        mut kinds,
        newest,
        mut artifacts,
    } = raw;
    if env.ctx.is_cancelled() {
        return None;
    }
    if kinds.is_empty() {
        kinds.push(ProjectKind::Other);
    }
    let repo = is_repo.then(|| path.clone());

    // Tracked means real source, whatever the folder is called.
    if let (Some(git), Some(repo)) = (env.git.as_deref(), repo.as_deref()) {
        if !artifacts.is_empty() {
            let relatives: Vec<String> = artifacts
                .iter()
                .filter_map(|artifact| {
                    artifact
                        .path
                        .strip_prefix(repo)
                        .ok()?
                        .to_str()
                        .map(str::to_string)
                })
                .collect();
            let borrowed: Vec<&str> = relatives.iter().map(String::as_str).collect();
            let tracked = tracked_among(git, repo, &borrowed);
            artifacts.retain(|artifact| {
                let relative = artifact
                    .path
                    .strip_prefix(repo)
                    .ok()
                    .and_then(|path| path.to_str());
                match (&tracked, relative) {
                    (Some(tracked), Some(relative)) => !tracked.contains(relative),
                    // Git could not answer. Without that answer an ambiguous name is not confirmed.
                    _ => !artifact.rule.ambiguous,
                }
            });
        }
    }

    let measured: Vec<SweptArtifact> = artifacts
        .into_par_iter()
        .map(|artifact| SweptArtifact {
            info: env.ctx.measure(&artifact.path),
            path: artifact.path,
            label: artifact.label,
            rule: artifact.rule,
            hint: artifact.hint,
        })
        // Nothing to give back: empty, or only symlinks, as in a pnpm workspace's `node_modules`.
        .filter(|artifact| artifact.info.files > 0 && artifact.info.bytes > 0)
        .collect();

    let git_dir = repo.as_deref().and_then(resolve_git_dir);
    let git_dir_bytes = git_dir
        .as_deref()
        .map(|dir| env.ctx.measure(dir).bytes)
        .unwrap_or(0);
    let last_commit = match (env.git.as_deref(), repo.as_deref()) {
        (Some(git), Some(repo)) => util::run_git(git, repo, &["log", "-1", "--format=%ct"])
            .filter(|output| output.success())
            .and_then(|output| parse_commit_time(&output.stdout)),
        _ => None,
    };

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let display = match path.strip_prefix(root) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => name.clone(),
    };
    Some(SweptProject {
        path,
        name,
        display,
        kinds,
        artifacts: measured,
        newest_source: newest,
        last_commit,
        repo,
        git_dir,
        git_dir_bytes,
    })
}

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Existing roots, each once, with any root inside another dropped.
fn normalized_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut canonical: Vec<PathBuf> = roots
        .iter()
        .filter(|root| root.is_dir())
        .map(|root| canonical_or_self(root))
        .collect();
    canonical.sort();
    canonical.dedup();
    let all = canonical.clone();
    canonical.retain(|root| {
        !all.iter()
            .any(|other| other != root && root.starts_with(other))
    });
    canonical
}

/// Walks every project root. Call [`shared_sweep`] instead.
fn run_sweep(ctx: &ScanContext) -> Sweep {
    let env = Env {
        ctx,
        git: util::tool(ctx, "git"),
        home: canonical_or_self(&ctx.home),
        excluded: ctx
            .config
            .excluded_paths
            .iter()
            .map(|path| canonical_or_self(path))
            .collect(),
        full_disk_access: fsutil::has_full_disk_access(&ctx.home),
        skipped_protected: AtomicBool::new(false),
    };
    let roots = normalized_roots(ctx.config.effective_project_roots(&ctx.home));

    let mut pins = Vec::new();
    let mut raw: Vec<(RawProject, PathBuf)> = Vec::new();
    for root in &roots {
        if env.is_excluded(root) {
            continue;
        }
        let Ok(meta) = fs::metadata(root) else {
            continue;
        };
        let outer = enclosing_repo(root, &env.home);
        let walked = walk(&env, root, meta.dev(), 0, outer.as_deref(), false);
        pins.extend(walked.pins);

        // Projects with no repository of their own may still belong to one above the root.
        let mut under_outer: Option<RawProject> = None;
        for project in walked.pending {
            match &outer {
                Some(repo) => {
                    let merged = under_outer.get_or_insert_with(|| RawProject {
                        path: repo.clone(),
                        is_repo: true,
                        kinds: Vec::new(),
                        newest: None,
                        artifacts: Vec::new(),
                    });
                    merged.absorb(project);
                }
                None => raw.push((project, root.clone())),
            }
        }
        if let Some(project) = under_outer {
            raw.push((project, root.clone()));
        }
        raw.extend(
            walked
                .closed
                .into_iter()
                .map(|project| (project, root.clone())),
        );
    }

    let mut projects: Vec<SweptProject> = raw
        .into_par_iter()
        .filter_map(|(project, root)| finish_project(&env, project, &root))
        .collect();
    projects.sort_by(|a, b| a.path.cmp(&b.path));

    Sweep {
        projects,
        pins,
        skipped_protected: env.skipped_protected.load(Ordering::Relaxed),
        cancelled: ctx.is_cancelled(),
    }
}

/// A running scan shares one sweep. `now` is fixed when a [`ScanContext`] is created, so it
/// identifies the scan, and the age limit covers a context kept around and used again later.
const SHARE_FOR: Duration = Duration::from_secs(120);

/// More threads than this only make the walk fight itself over APFS's metadata locks. Measured on a
/// 15-core Mac sweeping 800 artifacts: 8 threads are as fast as 15, with 40% less kernel time, which
/// leaves more of the machine for everything else while a scan runs.
const MAX_SWEEP_THREADS: usize = 8;

#[derive(PartialEq, Eq)]
struct SweepKey {
    home: PathBuf,
    now: SystemTime,
    roots: Vec<PathBuf>,
    excluded: Vec<PathBuf>,
}

struct SharedSlot {
    key: SweepKey,
    born: Instant,
    sweep: Arc<OnceLock<Arc<Sweep>>>,
}

static SLOT: Mutex<Option<SharedSlot>> = Mutex::new(None);

/// The sweep for this scan, running it if nobody has yet.
///
/// The sweep runs on its own thread with its own rayon pool, and callers block on it. Doing the
/// walk on the caller's thread would deadlock under `run_scan`: while waiting for its children, a
/// rayon worker steals other jobs, so it could pick up another scanner that blocks on this same
/// sweep, sitting on top of a stack frame the sweep needs to finish. With a separate pool, nothing
/// the sweep waits for is ever held by a thread that is waiting for the sweep.
pub(crate) fn shared_sweep(ctx: &ScanContext) -> Arc<Sweep> {
    let key = SweepKey {
        home: ctx.home.clone(),
        now: ctx.now,
        roots: ctx.config.project_roots.clone(),
        excluded: ctx.config.excluded_paths.clone(),
    };
    let cell = {
        let mut slot = SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let reusable = slot.as_ref().filter(|slot| {
            slot.key == key
                && slot.born.elapsed() < SHARE_FOR
                && !slot.sweep.get().is_some_and(|sweep| sweep.cancelled)
        });
        match reusable {
            Some(slot) => slot.sweep.clone(),
            None => {
                let sweep = Arc::new(OnceLock::new());
                *slot = Some(SharedSlot {
                    key,
                    born: Instant::now(),
                    sweep: sweep.clone(),
                });
                sweep
            }
        }
    };
    cell.get_or_init(|| run_on_own_pool(ctx)).clone()
}

fn run_on_own_pool(ctx: &ScanContext) -> Arc<Sweep> {
    let owned = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("hm-dev-sweep".to_string())
        .spawn(move || {
            let threads = std::thread::available_parallelism()
                .map(|cores| cores.get().min(MAX_SWEEP_THREADS))
                .unwrap_or(MAX_SWEEP_THREADS);
            let pool = rayon::ThreadPoolBuilder::new()
                .start_handler(|_| hm_core::threads::lower_current_thread_priority())
                .num_threads(threads)
                .thread_name(|index| format!("hm-dev-sweep-{index}"))
                // Recursion through rayon's bridge is stack-hungry.
                .stack_size(8 * 1024 * 1024)
                .build();
            match pool {
                Ok(pool) => pool.install(|| run_sweep(&owned)),
                Err(_) => run_sweep(&owned),
            }
        });
    let swept = spawned.ok().and_then(|handle| handle.join().ok());
    // A sweep that crashed reports as cancelled, so that it is not shared with the next caller.
    Arc::new(swept.unwrap_or_else(|| Sweep {
        cancelled: true,
        ..Sweep::default()
    }))
}

/// Pins grouped by what they ask for, for the toolchain scanners.
pub(crate) fn pins_of(sweep: &Sweep, kind: PinKind) -> HashMap<String, Vec<PathBuf>> {
    let mut grouped: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for pin in sweep.pins.iter().filter(|pin| pin.kind == kind) {
        grouped
            .entry(pin.value.clone())
            .or_default()
            .push(pin.project.clone());
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn the_folder_this_program_runs_from_is_not_an_artifact() {
        let program = hm_core::safety::running_program().unwrap();
        assert!(holds_running_program(program.parent().unwrap()));
        assert!(!holds_running_program(Path::new("/nonexistent/target")));
    }

    fn artifact_labels(sweep: &Sweep) -> Vec<String> {
        let mut labels: Vec<String> = sweep
            .projects
            .iter()
            .flat_map(|project| &project.artifacts)
            .map(|artifact| artifact.path.display().to_string())
            .collect();
        labels.sort();
        labels
    }

    fn has_artifact(sweep: &Sweep, relative_suffix: &str) -> bool {
        artifact_labels(sweep)
            .iter()
            .any(|path| path.ends_with(relative_suffix))
    }

    #[test]
    fn an_artifact_needs_its_manifest_beside_it() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/node_modules/left-pad/index.js", b"x");
        fx.file("Coding/loose/node_modules/left-pad/index.js", b"x");
        fx.file("Coding/rs/Cargo.toml", b"");
        fx.file("Coding/rs/target/debug/app", b"x");
        fx.file("Coding/notes/target/plan.md", b"my plan");
        let sweep = run_sweep(&fx.ctx());
        assert!(has_artifact(&sweep, "web/node_modules"));
        assert!(has_artifact(&sweep, "rs/target"));
        assert!(!has_artifact(&sweep, "loose/node_modules"));
        assert!(!has_artifact(&sweep, "notes/target"));
    }

    #[test]
    fn an_orphan_node_modules_is_not_walked_for_nested_projects() {
        let fx = Fixture::new();
        fx.file("Coding/loose/node_modules/a/package.json", b"{}");
        fx.file("Coding/loose/node_modules/a/node_modules/b/index.js", b"x");
        let sweep = run_sweep(&fx.ctx());
        assert!(artifact_labels(&sweep).is_empty());
        assert!(sweep.projects.is_empty());
    }

    #[test]
    fn nested_node_modules_are_counted_once() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/node_modules/a/package.json", b"{}");
        fx.file(
            "Coding/web/node_modules/a/node_modules/b/index.js",
            &[1u8; 8192],
        );
        let sweep = run_sweep(&fx.ctx());
        let labels = artifact_labels(&sweep);
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert!(labels[0].ends_with("web/node_modules"));
        // Both files, inside the single outer measurement.
        let project = &sweep.projects[0];
        assert_eq!(project.artifacts[0].info.files, 2);
    }

    #[test]
    fn ambiguous_names_count_only_when_git_ignores_them_and_tracks_nothing() {
        let fx = Fixture::new();
        fx.file("Coding/app/package.json", b"{}");
        fx.file("Coding/app/.gitignore", b"dist/\nsrc-old/\nout\n");
        fx.file("Coding/app/dist/bundle.js", b"x");
        fx.file("Coding/app/build/real-source.c", b"int main;");
        fx.file("Coding/app/index.js", b"x");
        // A folder committed before the ignore rule was added.
        fx.file("Coding/app/out/committed.txt", b"x");
        fx.git("Coding/app", &["init", "-q"]);
        fx.git(
            "Coding/app",
            &["add", "-f", "package.json", ".gitignore", "build", "out"],
        );
        fx.git("Coding/app", &["commit", "-q", "-m", "x"]);

        let sweep = run_sweep(&fx.ctx());
        assert!(
            has_artifact(&sweep, "app/dist"),
            "{:?}",
            artifact_labels(&sweep)
        );
        assert!(!has_artifact(&sweep, "app/build"), "committed source");
        assert!(!has_artifact(&sweep, "app/out"), "ignored but tracked");
    }

    #[test]
    fn ambiguous_names_do_not_count_outside_a_repository_or_without_a_manifest() {
        let fx = Fixture::new();
        fx.file("Coding/plain/package.json", b"{}");
        fx.file("Coding/plain/dist/bundle.js", b"x");
        fx.file("Coding/repo/.gitignore", b"dist/\n");
        fx.file("Coding/repo/dist/bundle.js", b"x");
        fx.git("Coding/repo", &["init", "-q"]);
        let sweep = run_sweep(&fx.ctx());
        assert!(artifact_labels(&sweep).is_empty());
    }

    #[test]
    fn a_dotfiles_repository_in_the_home_folder_confirms_nothing() {
        let fx = Fixture::new();
        fx.file(".gitignore", b"*\n");
        fx.git("", &["init", "-q"]);
        fx.file("Coding/app/package.json", b"{}");
        fx.file("Coding/app/dist/precious.txt", b"x");
        let sweep = run_sweep(&fx.ctx());
        assert!(!has_artifact(&sweep, "app/dist"));
    }

    #[test]
    fn a_committed_node_modules_is_not_an_artifact() {
        let fx = Fixture::new();
        fx.file("Coding/action/package.json", b"{}");
        fx.file("Coding/action/node_modules/dep/index.js", b"x");
        fx.git("Coding/action", &["init", "-q"]);
        fx.git("Coding/action", &["add", "-f", "."]);
        fx.git("Coding/action", &["commit", "-q", "-m", "x"]);
        assert!(artifact_labels(&run_sweep(&fx.ctx())).is_empty());
    }

    #[test]
    fn git_confirmation_works_with_accents_and_spaces_in_the_path() {
        let fx = Fixture::new();
        fx.file("Coding/Automatización SIAT/package.json", b"{}");
        fx.file("Coding/Automatización SIAT/.gitignore", b"dist\n");
        fx.file("Coding/Automatización SIAT/pàgina web/package.json", b"{}");
        fx.file("Coding/Automatización SIAT/pàgina web/dist/x.js", b"x");
        fx.file("Coding/Automatización SIAT/dist/x.js", b"x");
        fx.git("Coding/Automatización SIAT", &["init", "-q"]);
        let sweep = run_sweep(&fx.ctx());
        assert_eq!(
            artifact_labels(&sweep).len(),
            2,
            "{:?}",
            artifact_labels(&sweep)
        );
    }

    #[test]
    fn git_ignore_works_from_a_nested_package_in_a_monorepo() {
        let fx = Fixture::new();
        fx.file("Coding/mono/.gitignore", b"dist\nnode_modules\n");
        fx.file("Coding/mono/package.json", b"{}");
        fx.file("Coding/mono/packages/a/package.json", b"{}");
        fx.file("Coding/mono/packages/a/dist/x.js", b"x");
        fx.file("Coding/mono/packages/a/node_modules/d/x.js", b"x");
        fx.git("Coding/mono", &["init", "-q"]);
        let sweep = run_sweep(&fx.ctx());
        assert_eq!(sweep.projects.len(), 1, "a monorepo is one project");
        assert_eq!(sweep.projects[0].name, "mono");
        assert!(has_artifact(&sweep, "packages/a/dist"));
        assert!(has_artifact(&sweep, "packages/a/node_modules"));
    }

    #[test]
    fn symlinks_are_never_followed_or_offered() {
        let fx = Fixture::new();
        fx.file("elsewhere/real/index.js", b"x");
        fx.file("elsewhere/proj/package.json", b"{}");
        fx.file("elsewhere/proj/node_modules/x.js", b"x");
        fx.file("Coding/web/package.json", b"{}");
        fx.symlink("elsewhere/real", "Coding/web/node_modules");
        fx.symlink("elsewhere/proj", "Coding/linked");
        let sweep = run_sweep(&fx.ctx());
        assert!(
            artifact_labels(&sweep).is_empty(),
            "{:?}",
            artifact_labels(&sweep)
        );
    }

    #[test]
    fn a_folder_of_nothing_but_symlinks_has_nothing_to_give_back() {
        let fx = Fixture::new();
        fx.file("Coding/mono/shared/index.js", b"x");
        fx.file("Coding/mono/packages/a/package.json", b"{}");
        fx.dir("Coding/mono/packages/a/node_modules/@scope");
        fx.symlink(
            "Coding/mono/shared",
            "Coding/mono/packages/a/node_modules/@scope/shared",
        );
        assert!(artifact_labels(&run_sweep(&fx.ctx())).is_empty());
    }

    #[test]
    fn dot_git_is_never_entered() {
        let fx = Fixture::new();
        fx.file("Coding/app/package.json", b"{}");
        fx.git("Coding/app", &["init", "-q"]);
        fx.file("Coding/app/.git/odd/package.json", b"{}");
        fx.file("Coding/app/.git/odd/node_modules/x.js", b"x");
        assert!(artifact_labels(&run_sweep(&fx.ctx())).is_empty());
    }

    #[test]
    fn a_venv_needs_its_marker() {
        let fx = Fixture::new();
        fx.file("Coding/py/pyproject.toml", b"");
        fx.file("Coding/py/.venv/pyvenv.cfg", b"home = /usr/bin\n");
        fx.file("Coding/py/venv/my_module.py", b"real code");
        let sweep = run_sweep(&fx.ctx());
        assert!(has_artifact(&sweep, "py/.venv"));
        assert!(!has_artifact(&sweep, "py/venv"));
    }

    #[test]
    fn pycache_counts_under_a_python_project_at_any_depth_but_not_alone() {
        let fx = Fixture::new();
        fx.file("Coding/py/pyproject.toml", b"");
        fx.file("Coding/py/src/pkg/__pycache__/m.pyc", b"x");
        fx.file("Coding/scripts/__pycache__/s.pyc", b"x");
        let sweep = run_sweep(&fx.ctx());
        assert!(has_artifact(&sweep, "pkg/__pycache__"));
        assert!(!has_artifact(&sweep, "scripts/__pycache__"));
    }

    #[test]
    fn activity_ignores_artifacts_and_finder_files() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/node_modules/d/x.js", b"x");
        fx.file("Coding/web/.DS_Store", b"x");
        fx.set_age("Coding/web/package.json", 100);
        let sweep = run_sweep(&fx.ctx());
        let project = &sweep.projects[0];
        let idle = project.idle_days(SystemTime::now()).unwrap();
        assert!((99..=101).contains(&idle), "idle was {idle}");
    }

    #[test]
    fn unreadable_folders_count_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        let locked = fx.file("Coding/web/locked/x.txt", b"x");
        let locked = locked.parent().unwrap().to_path_buf();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let sweep = run_sweep(&fx.ctx());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(sweep.projects.len(), 1);
    }

    #[test]
    fn cancelling_stops_the_sweep() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/node_modules/d/x.js", b"x");
        let ctx = fx.ctx();
        ctx.cancel.cancel();
        let sweep = run_sweep(&ctx);
        assert!(sweep.cancelled);
        assert!(sweep.projects.is_empty());
    }

    #[test]
    fn excluded_paths_are_not_walked() {
        let fx = Fixture::new();
        fx.file("Coding/keep/package.json", b"{}");
        fx.file("Coding/keep/node_modules/x.js", b"x");
        let mut ctx = fx.ctx();
        ctx.config.excluded_paths = vec![fx.home.join("Coding/keep")];
        assert!(artifact_labels(&run_sweep(&ctx)).is_empty());
    }

    #[test]
    fn the_home_folder_as_a_root_skips_what_must_not_be_touched() {
        let fx = Fixture::new();
        fx.file("Documents/secret/package.json", b"{}");
        fx.file("Documents/secret/node_modules/x.js", b"x");
        fx.file("Library/Caches/p/package.json", b"{}");
        fx.file("Library/Caches/p/node_modules/x.js", b"x");
        fx.file(".cache/p/package.json", b"{}");
        fx.file(".cache/p/node_modules/x.js", b"x");
        fx.file("work/app/package.json", b"{}");
        fx.file("work/app/node_modules/x.js", b"x");
        let mut ctx = fx.ctx();
        ctx.config.project_roots = vec![fx.home.clone()];
        let sweep = run_sweep(&ctx);
        let labels = artifact_labels(&sweep);
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert!(labels[0].ends_with("work/app/node_modules"));
        assert!(sweep.skipped_protected);
    }

    #[test]
    fn a_prompting_folder_configured_as_a_root_is_swept() {
        let fx = Fixture::new();
        fx.file("Documents/code/app/package.json", b"{}");
        fx.file("Documents/code/app/node_modules/x.js", b"x");
        let mut ctx = fx.ctx();
        ctx.config.project_roots = vec![fx.home.join("Documents/code")];
        assert!(has_artifact(&run_sweep(&ctx), "app/node_modules"));
    }

    #[test]
    fn full_disk_access_unlocks_prompting_folders_under_a_home_root() {
        let fx = Fixture::new();
        fx.file("Library/Safari/History.db", b"x");
        fx.file("Documents/app/package.json", b"{}");
        fx.file("Documents/app/node_modules/x.js", b"x");
        let mut ctx = fx.ctx();
        ctx.config.project_roots = vec![fx.home.clone()];
        assert!(has_artifact(&run_sweep(&ctx), "Documents/app/node_modules"));
    }

    #[test]
    fn overlapping_roots_do_not_double_count() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/node_modules/x.js", b"x");
        let mut ctx = fx.ctx();
        ctx.config.project_roots = vec![fx.home.join("Coding"), fx.home.join("Coding/web")];
        let sweep = run_sweep(&ctx);
        assert_eq!(artifact_labels(&sweep).len(), 1);
    }

    #[test]
    fn a_root_inside_a_repository_belongs_to_that_repository() {
        let fx = Fixture::new();
        fx.file("Coding/mono/.gitignore", b"dist\n");
        fx.file("Coding/mono/packages/a/package.json", b"{}");
        fx.file("Coding/mono/packages/a/dist/x.js", b"x");
        fx.git("Coding/mono", &["init", "-q"]);
        let mut ctx = fx.ctx();
        ctx.config.project_roots = vec![fx.home.join("Coding/mono/packages")];
        let sweep = run_sweep(&ctx);
        assert_eq!(sweep.projects.len(), 1);
        assert_eq!(sweep.projects[0].name, "mono");
        assert!(has_artifact(&sweep, "a/dist"));
    }

    #[test]
    fn scanners_sharing_a_sweep_inside_the_global_pool_do_not_deadlock() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/node_modules/x.js", b"x");
        let ctx = fx.ctx();
        let sizes: Vec<usize> = (0..32)
            .into_par_iter()
            .map(|_| shared_sweep(&ctx).projects.len())
            .collect();
        assert!(sizes.iter().all(|size| *size == 1));
    }

    #[test]
    fn pins_are_read_from_files() {
        let fx = Fixture::new();
        fx.file("Coding/a/package.json", b"{}");
        fx.file("Coding/a/.nvmrc", b"20\n");
        fx.file("Coding/b/pyproject.toml", b"");
        fx.file("Coding/b/.python-version", b"3.13.3\n");
        fx.file("Coding/c/Cargo.toml", b"");
        fx.file(
            "Coding/c/rust-toolchain.toml",
            b"[toolchain]\nchannel = \"1.75.0\"\n",
        );
        fx.file("Coding/d/pyproject.toml", b"");
        fx.file(
            "Coding/d/.venv/pyvenv.cfg",
            b"home = /Users/x/.pyenv/versions/3.12.7/bin\n",
        );
        let sweep = run_sweep(&fx.ctx());
        assert!(pins_of(&sweep, PinKind::Node).contains_key("20"));
        let python = pins_of(&sweep, PinKind::Python);
        assert!(python.contains_key("3.13.3"));
        assert!(python.contains_key("3.12.7"));
        assert!(pins_of(&sweep, PinKind::Rust).contains_key("1.75.0"));
    }

    #[test]
    fn rust_toolchain_files_parse_in_both_forms() {
        assert_eq!(
            parse_rust_toolchain_file("nightly-2024-01-01\n").as_deref(),
            Some("nightly-2024-01-01")
        );
        assert_eq!(
            parse_rust_toolchain_file("[toolchain]\nchannel = \"1.75\"\n").as_deref(),
            Some("1.75")
        );
        assert_eq!(
            parse_rust_toolchain_file("[toolchain]\ncomponents = []\n"),
            None
        );
        assert_eq!(parse_rust_toolchain_file(""), None);
    }

    #[test]
    fn every_rule_with_python_scope_is_backed_by_a_project_kind() {
        // A leftover artifact with no project above it would silently vanish.
        for manifest in PYTHON {
            assert!(
                KIND_MANIFESTS
                    .iter()
                    .any(|(pattern, _)| pattern == manifest),
                "{manifest} has no kind"
            );
        }
    }
}
