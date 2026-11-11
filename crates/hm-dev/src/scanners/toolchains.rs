//! Installed versions of Rust, Node and Python.
//!
//! Nothing here is ever preselected, and the version in use is never offered at all. "In use" is
//! decided from each tool's own default, plus anything on the current `PATH`. When the default
//! cannot be determined, the newest version is treated as the default, because guessing wrong about
//! keeping something costs far less than guessing wrong about removing it.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use hm_core::{fsutil, t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};
use rayon::prelude::*;

use crate::sweep::{pins_of, shared_sweep, PinKind};
use crate::util;

const ID: &str = "dev.toolchains";
const RUSTUP_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct ToolchainScanner;

impl Scanner for ToolchainScanner {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        t!("Versiones de lenguajes", "Language versions")
    }

    fn description(&self) -> &'static str {
        t!(
            "Versiones de Rust, Node y Python que tienes instaladas además de la que usas.",
            "Versions of Rust, Node and Python you have installed besides the one you use."
        )
    }

    fn area(&self) -> Area {
        Area::Developer
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        findings.extend(rust_findings(ctx, query_rustup(ctx).as_deref()));
        findings.extend(nvm_findings(ctx));
        findings.extend(fnm_findings(ctx));
        findings.extend(volta_findings(ctx));
        findings.extend(pyenv_findings(ctx));
        Ok(util::finalize(ctx, findings))
    }
}

/// Which projects ask for each version. Loaded only once there is a candidate to weigh it against,
/// so a Mac with nothing to offer here never pays for walking the code folders.
struct ProjectPins {
    by_value: HashMap<String, Vec<PathBuf>>,
}

impl ProjectPins {
    fn load(ctx: &ScanContext, kind: PinKind) -> Self {
        Self {
            by_value: pins_of(&shared_sweep(ctx), kind),
        }
    }

    fn users_of(&self, matches: impl Fn(&str) -> bool) -> Vec<&PathBuf> {
        let mut users: Vec<&PathBuf> = self
            .by_value
            .iter()
            .filter(|(value, _)| matches(value))
            .flat_map(|(_, projects)| projects)
            .collect();
        users.sort();
        users.dedup();
        users
    }
}

fn project_names(users: &[&PathBuf]) -> String {
    let mut names: Vec<String> = users
        .iter()
        .filter_map(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names.dedup();
    if names.len() > 4 {
        let rest = names.len() - 3;
        names.truncate(3);
        tf!("{} y {rest} más", "{} and {rest} more", names.join(", "))
    } else {
        names.join(", ")
    }
}

/// `true` if a project's pin (`20`, `20.11`, `v20.11.0`) is satisfied by this exact version.
fn version_matches_pin(version: &str, pin: &str) -> bool {
    let (Some(version), Some(pin)) = (util::parse_version(version), util::parse_version(pin))
    else {
        return false;
    };
    pin.len() <= version.len() && version[..pin.len()] == pin[..]
}

/// The version folders under `root`. Symlinks are skipped: they point at something else, and pyenv
/// uses them for virtualenv names.
fn installed_versions(root: &Path) -> Vec<(String, PathBuf)> {
    fsutil::children(root)
        .into_iter()
        .filter(|path| fsutil::is_real_dir(path))
        .filter_map(|path| Some((path.file_name()?.to_str()?.to_string(), path)))
        .collect()
}

/// The name that sorts highest as a version, for when the default cannot be determined.
fn newest_version<'a>(names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    names
        .filter_map(|name| Some((util::parse_version(name)?, name)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, name)| name)
}

/// The next path component after `root` for every `PATH` entry under it. A version on the `PATH`
/// right now is in use, whatever the configured default says.
fn versions_on_path(root: &Path, path_var: Option<&std::ffi::OsStr>) -> HashSet<String> {
    let Some(path_var) = path_var else {
        return HashSet::new();
    };
    env::split_paths(path_var)
        .filter_map(|entry| {
            let rest = entry.strip_prefix(root).ok()?;
            Some(rest.components().next()?.as_os_str().to_str()?.to_string())
        })
        .collect()
}

fn on_current_path(root: &Path) -> HashSet<String> {
    versions_on_path(root, env::var_os("PATH").as_deref())
}

// ---- Rust ---------------------------------------------------------------------------------------

/// Toolchains marked as in use in `rustup toolchain list`, whose lines look like
/// `stable-aarch64-apple-darwin (active, default)`.
fn parse_rustup_list(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter_map(|line| {
            let (name, flags) = line.trim().split_once(' ')?;
            (flags.contains("default") || flags.contains("active") || flags.contains("override"))
                .then(|| name.to_string())
        })
        .collect()
}

/// The toolchain names in rustup's `settings.toml`: the default and every directory override.
fn parse_rustup_settings(text: &str) -> HashSet<String> {
    let mut named = HashSet::new();
    let mut in_overrides = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_overrides = line == "[overrides]";
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if in_overrides || key.trim() == "default_toolchain" {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                named.insert(value.to_string());
            }
        }
    }
    named
}

/// `settings.toml` may say `stable` for the installed `stable-aarch64-apple-darwin`.
fn toolchain_is_named(installed: &str, name: &str) -> bool {
    installed == name
        || installed
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('-'))
}

/// Asks rustup itself, pointed at `ctx.home` so that a fixture home sees only its own toolchains.
fn query_rustup(ctx: &ScanContext) -> Option<String> {
    if !fsutil::is_real_dir(&ctx.home_path(".rustup/toolchains")) {
        return None;
    }
    let rustup = util::tool(ctx, "rustup")?;
    let rustup_home = format!("RUSTUP_HOME={}", ctx.home_path(".rustup").to_str()?);
    let cargo_home = format!("CARGO_HOME={}", ctx.home_path(".cargo").to_str()?);
    let output = util::run_tool(
        Path::new("/usr/bin/env"),
        &[
            &rustup_home,
            &cargo_home,
            rustup.to_str()?,
            "toolchain",
            "list",
        ],
        RUSTUP_TIMEOUT,
    )?;
    output.success().then_some(output.stdout)
}

fn rust_findings(ctx: &ScanContext, rustup_list: Option<&str>) -> Vec<Finding> {
    let toolchains = ctx.home_path(".rustup/toolchains");
    let installed = installed_versions(&toolchains);
    if installed.is_empty() {
        return Vec::new();
    }
    let settings = fs::read_to_string(ctx.home_path(".rustup/settings.toml")).ok();
    let mut in_use = settings
        .as_deref()
        .map(parse_rustup_settings)
        .unwrap_or_default();
    if let Some(list) = rustup_list {
        in_use.extend(parse_rustup_list(list));
    }
    // With no idea what the default is, nothing can safely be offered.
    if in_use.is_empty() {
        return Vec::new();
    }
    let on_path = on_current_path(&toolchains);
    let candidates: Vec<&(String, PathBuf)> = installed
        .iter()
        .filter(|(name, _)| {
            !on_path.contains(name) && !in_use.iter().any(|used| toolchain_is_named(name, used))
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let pins = ProjectPins::load(ctx, PinKind::Rust);
    candidates
        .par_iter()
        .filter_map(|(name, path)| {
            let info = ctx.measure(path);
            if info.files == 0 {
                return None;
            }
            let users = pins.users_of(|pin| toolchain_is_named(name, pin));
            let finding = Finding::new(ID, Area::Developer, format!("rust:{name}"), name.clone())
                .group("Rust")
                .subtitle(path.display().to_string())
                .measured(&info)
                .recovery(Recovery::Redownload)
                .selected(false)
                .blocked_by(&["cargo", "rustc"])
                .action(Action::Command {
                    program: "rustup".to_string(),
                    args: vec!["toolchain".to_string(), "uninstall".to_string(), name.clone()],
                });
            Some(if users.is_empty() {
                finding.risk(Risk::Moderate).explanation(t!(
                    "Una versión de Rust que no es la predeterminada. Ningún proyecto de tus carpetas de código la pide. rustup puede volver a instalarla.",
                    "A Rust version that is not the default. No project in your code folders asks for it. rustup can install it again."
                ))
            } else {
                finding.risk(Risk::Review).tag("fijada").explanation(tf!(
                    "Una versión de Rust que piden estos proyectos: {}. rustup la descargará de nuevo cuando vuelvas a compilarlos.",
                    "A Rust version these projects ask for: {}. rustup will download it again when you build them next.",
                    project_names(&users)
                ))
            })
        })
        .collect()
}

// ---- Node ---------------------------------------------------------------------------------------

/// Global packages installed into this Node version, which removing it takes along.
fn node_global_packages(version_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for path in fsutil::children(&version_dir.join("lib/node_modules")) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('@') {
            for scoped in fsutil::children(&path) {
                if let Some(inner) = scoped.file_name().and_then(|inner| inner.to_str()) {
                    names.push(format!("{name}/{inner}"));
                }
            }
        } else if !matches!(name, "npm" | "corepack") && !name.starts_with('.') {
            names.push(name.to_string());
        }
    }
    names.sort();
    names
}

fn node_finding(
    ctx: &ScanContext,
    manager: &str,
    version: &str,
    path: &Path,
    version_dir: &Path,
    pins: &ProjectPins,
    resolve_pin: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Option<Finding> {
    let info = ctx.measure(path);
    if info.files == 0 {
        return None;
    }
    let globals = node_global_packages(version_dir);
    let users = pins.users_of(|pin| {
        let resolved = resolve_pin(pin);
        version_matches_pin(version, resolved.as_deref().unwrap_or(pin))
    });
    let finding = Finding::new(
        ID,
        Area::Developer,
        format!("node:{manager}:{version}"),
        format!("Node {version}"),
    )
    .group(format!("Node ({manager})"))
    .trash(path)
    .measured(&info)
    .recovery(Recovery::Redownload)
    .selected(false)
    .blocked_by(&["node"]);

    let mut notes = Vec::new();
    if !users.is_empty() {
        notes.push(tf!(
            "La piden estos proyectos: {}.",
            "These projects ask for it: {}.",
            project_names(&users)
        ));
    }
    if !globals.is_empty() {
        notes.push(tf!(
            "Se perderían sus paquetes globales: {}.",
            "Its global packages would be lost: {}.",
            globals.join(", ")
        ));
    }
    let needs_review = !notes.is_empty();
    notes.push(tf!(
        "Se puede volver a instalar con {manager}, pero los paquetes globales habría que instalarlos de nuevo.",
        "{manager} can install it again, but global packages would have to be reinstalled."
    ));
    Some(
        finding
            .risk(if needs_review {
                Risk::Review
            } else {
                Risk::Moderate
            })
            .explanation(tf!(
                "Una versión de Node que no es la activa. {}",
                "A Node version that is not the active one. {}",
                notes.join(" ")
            )),
    )
}

/// Follows nvm's alias files until reaching a version number or giving up:
/// `default` holds `lts/*`, `lts/*` holds `lts/krypton`, `lts/krypton` holds `v24.15.0`.
fn resolve_nvm_alias(alias_dir: &Path, start: &str) -> Option<String> {
    let mut value = start.trim().to_string();
    for _ in 0..10 {
        if util::parse_version(&value).is_some()
            || matches!(value.as_str(), "system" | "node" | "stable")
        {
            return Some(value);
        }
        let text = fs::read_to_string(alias_dir.join(&value)).ok()?;
        value = text.lines().next()?.trim().to_string();
    }
    None
}

/// What nvm's default alias comes down to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NvmDefault {
    Version(String),
    /// The system's Node, which leaves every nvm version free to be offered.
    System,
    /// The alias says nothing usable.
    Unknown,
}

/// The installed version a resolved alias refers to.
fn nvm_default_version(installed: &[String], alias: Option<&str>) -> NvmDefault {
    let version_or_unknown = |found: Option<&str>| match found {
        Some(version) => NvmDefault::Version(version.to_string()),
        None => NvmDefault::Unknown,
    };
    match alias {
        None => NvmDefault::Unknown,
        Some("system") => NvmDefault::System,
        Some("node" | "stable") => {
            version_or_unknown(newest_version(installed.iter().map(String::as_str)))
        }
        Some(version) => {
            let matching: Vec<&str> = installed
                .iter()
                .map(String::as_str)
                .filter(|name| version_matches_pin(name, version))
                .collect();
            if let Some(best) = newest_version(matching.into_iter()) {
                return NvmDefault::Version(best.to_string());
            }
            // An LTS alias can name a patch release older than the one installed.
            let major = util::parse_version(version).and_then(|parts| parts.first().copied());
            let same_major: Vec<&str> = installed
                .iter()
                .map(String::as_str)
                .filter(|name| {
                    util::parse_version(name).and_then(|parts| parts.first().copied()) == major
                })
                .collect();
            version_or_unknown(newest_version(same_major.into_iter()))
        }
    }
}

fn nvm_findings(ctx: &ScanContext) -> Vec<Finding> {
    let root = ctx.home_path(".nvm/versions/node");
    let installed = installed_versions(&root);
    if installed.is_empty() {
        return Vec::new();
    }
    let names: Vec<String> = installed.iter().map(|(name, _)| name.clone()).collect();
    let alias_dir = ctx.home_path(".nvm/alias");
    let default_alias = fs::read_to_string(alias_dir.join("default"))
        .ok()
        .and_then(|text| resolve_nvm_alias(&alias_dir, &text));

    let mut active = on_current_path(&root);
    match nvm_default_version(&names, default_alias.as_deref()) {
        NvmDefault::Version(version) => {
            active.insert(version);
        }
        NvmDefault::System => {}
        NvmDefault::Unknown => {
            active.extend(newest_version(names.iter().map(String::as_str)).map(str::to_string));
        }
    }
    // With `NVM_SYMLINK_CURRENT`, this link says what is active.
    if let Ok(target) = fs::read_link(ctx.home_path(".nvm/current")) {
        if let Some(name) = target.file_name().and_then(|name| name.to_str()) {
            active.insert(name.to_string());
        }
    }

    let candidates: Vec<&(String, PathBuf)> = installed
        .iter()
        .filter(|(name, _)| !active.contains(name))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let pins = ProjectPins::load(ctx, PinKind::Node);
    let resolve = |pin: &str| resolve_nvm_alias(&alias_dir, pin);
    candidates
        .par_iter()
        .filter_map(|(name, path)| node_finding(ctx, "nvm", name, path, path, &pins, &resolve))
        .collect()
}

fn fnm_findings(ctx: &ScanContext) -> Vec<Finding> {
    const ROOTS: &[&str] = &[
        "Library/Application Support/fnm",
        ".local/share/fnm",
        ".fnm",
    ];
    let mut found = Vec::new();
    for relative in ROOTS {
        let fnm = ctx.home_path(relative);
        let versions = fnm.join("node-versions");
        let installed = installed_versions(&versions);
        if installed.is_empty() {
            continue;
        }
        let mut active = on_current_path(&versions);
        // Every alias points at a version the user chose to keep a name for.
        let mut has_default = false;
        for alias in fsutil::children(&fnm.join("aliases")) {
            let Ok(target) = fs::read_link(&alias) else {
                continue;
            };
            let version = target.components().rev().find_map(|part| {
                let part = part.as_os_str().to_str()?;
                util::parse_version(part).map(|_| part.to_string())
            });
            if let Some(version) = version {
                active.insert(version);
                if alias.file_name().is_some_and(|name| name == "default") {
                    has_default = true;
                }
            }
        }
        if !has_default {
            active.extend(
                newest_version(installed.iter().map(|(name, _)| name.as_str())).map(str::to_string),
            );
        }
        let candidates: Vec<&(String, PathBuf)> = installed
            .iter()
            .filter(|(name, _)| !active.contains(name))
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let pins = ProjectPins::load(ctx, PinKind::Node);
        found.extend(candidates.iter().filter_map(|(name, path)| {
            node_finding(
                ctx,
                "fnm",
                name,
                path,
                &path.join("installation"),
                &pins,
                &|_| None,
            )
        }));
    }
    found
}

/// The default Node in Volta's `platform.json`: `{"node": {"runtime": "24.1.0", "npm": null}}`.
fn parse_volta_platform(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value
        .get("node")?
        .get("runtime")?
        .as_str()
        .map(str::to_string)
}

fn volta_findings(ctx: &ScanContext) -> Vec<Finding> {
    let root = ctx.home_path(".volta/tools/image/node");
    let installed = installed_versions(&root);
    if installed.is_empty() {
        return Vec::new();
    }
    let default = fs::read_to_string(ctx.home_path(".volta/tools/user/platform.json"))
        .ok()
        .as_deref()
        .and_then(parse_volta_platform);
    let mut active = on_current_path(&root);
    match default {
        Some(version) => {
            active.insert(version);
        }
        None => active.extend(
            newest_version(installed.iter().map(|(name, _)| name.as_str())).map(str::to_string),
        ),
    }
    let candidates: Vec<&(String, PathBuf)> = installed
        .iter()
        .filter(|(name, _)| !active.contains(name))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let pins = ProjectPins::load(ctx, PinKind::Node);
    candidates
        .iter()
        .filter_map(|(name, path)| {
            // Volta keeps global packages outside the Node image, so removing one loses nothing.
            let no_globals = Path::new("/nonexistent");
            node_finding(ctx, "Volta", name, path, no_globals, &pins, &|_| None)
        })
        .collect()
}

// ---- Python -------------------------------------------------------------------------------------

/// Packages someone installed into this Python, as opposed to the ones it ships with.
fn python_installed_packages(version_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for lib in fsutil::children(&version_dir.join("lib")) {
        let is_python = lib
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("python3") || name.starts_with("python2"));
        if !is_python {
            continue;
        }
        for path in fsutil::children(&lib.join("site-packages")) {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".dist-info") else {
                continue;
            };
            let package = stem.split('-').next().unwrap_or(stem);
            if !matches!(package, "pip" | "setuptools" | "wheel") {
                names.push(package.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn pyenv_findings(ctx: &ScanContext) -> Vec<Finding> {
    let versions = ctx.home_path(".pyenv/versions");
    let installed = installed_versions(&versions);
    if installed.is_empty() {
        return Vec::new();
    }
    let global = fs::read_to_string(ctx.home_path(".pyenv/version")).ok();
    let session = env::var("PYENV_VERSION").ok();
    let mut active = on_current_path(&versions);
    match global {
        Some(text) => {
            let chosen = text
                .lines()
                .chain(session.as_deref().unwrap_or("").split(':'))
                .map(str::trim)
                .filter(|line| !line.is_empty() && *line != "system");
            for name in chosen {
                // A pyenv-virtualenv name is a link to `<version>/envs/<name>`.
                match fs::read_link(versions.join(name)) {
                    Ok(target) => {
                        let base = target
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy().into_owned())
                            .find(|part| util::parse_version(part).is_some());
                        active.extend(base);
                    }
                    Err(_) => {
                        active.insert(name.to_string());
                    }
                }
            }
        }
        None => active.extend(
            newest_version(installed.iter().map(|(name, _)| name.as_str())).map(str::to_string),
        ),
    }
    let candidates: Vec<&(String, PathBuf)> = installed
        .iter()
        .filter(|(name, _)| !active.contains(name))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let pins = ProjectPins::load(ctx, PinKind::Python);
    candidates
        .par_iter()
        .filter_map(|(name, path)| {
            let info = ctx.measure(path);
            if info.files == 0 {
                return None;
            }
            let users = pins.users_of(|pin| name == pin || version_matches_pin(name, pin));
            let packages = python_installed_packages(path);
            let has_envs = !fsutil::children(&path.join("envs")).is_empty();

            let mut notes = Vec::new();
            if !users.is_empty() {
                notes.push(tf!(
                    "La usan estos proyectos, directamente o con un entorno virtual: {}.",
                    "These projects use it, directly or through a virtual environment: {}.",
                    project_names(&users)
                ));
            }
            if has_envs {
                notes.push(
                    t!(
                        "Contiene entornos de pyenv-virtualenv, que se perderían.",
                        "It holds pyenv-virtualenv environments, which would be lost."
                    )
                    .to_string(),
                );
            }
            if !packages.is_empty() {
                notes.push(tf!(
                    "Tiene {} instalados que se perderían.",
                    "It has {} installed that would be lost.",
                    util::count(
                        packages.len() as u64,
                        t!("paquete", "package"),
                        t!("paquetes", "packages")
                    )
                ));
            }
            let needs_review = !notes.is_empty();
            notes.push(
                t!(
                    "Los entornos virtuales creados con ella dejarán de funcionar. Se puede volver a instalar con «pyenv install», que la compila de nuevo.",
                    "Virtual environments created with it will stop working. “pyenv install” can install it again, building it from source."
                )
                .to_string(),
            );
            Some(
                Finding::new(ID, Area::Developer, format!("python:{name}"), format!("Python {name}"))
                    .group("Python (pyenv)")
                    .trash(path)
                    .measured(&info)
                    .risk(if needs_review { Risk::Review } else { Risk::Moderate })
                    .recovery(Recovery::Redownload)
                    .selected(false)
                    .explanation(tf!(
                        "Una versión de Python que no es la activa. {}",
                        "A Python version that is not the active one. {}",
                        notes.join(" ")
                    )),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;
    use std::os::unix::fs::symlink;

    const RUSTUP_LIST: &str = "stable-aarch64-apple-darwin (active, default)\nnightly-aarch64-apple-darwin\n1.75.0-aarch64-apple-darwin\n";

    fn toolchain(fx: &Fixture, name: &str) {
        fx.file(
            &format!(".rustup/toolchains/{name}/bin/rustc"),
            &[1u8; 4096],
        );
    }

    fn titles(found: &[Finding]) -> Vec<String> {
        let mut titles: Vec<String> = found.iter().map(|finding| finding.title.clone()).collect();
        titles.sort();
        titles
    }

    #[test]
    fn rustup_list_parses_every_flag_form() {
        let used = parse_rustup_list(RUSTUP_LIST);
        assert_eq!(used.len(), 1);
        assert!(used.contains("stable-aarch64-apple-darwin"));
        let old = "stable-x86_64-apple-darwin (default)\n1.70-x86_64-apple-darwin (override)\nbeta-x86_64-apple-darwin\n";
        let used = parse_rustup_list(old);
        assert_eq!(used.len(), 2);
        assert!(parse_rustup_list("").is_empty());
        assert!(parse_rustup_list("no installed toolchains\n").is_empty());
    }

    #[test]
    fn rustup_settings_parse_default_and_overrides() {
        let text = "version = \"12\"\ndefault_toolchain = \"stable-aarch64-apple-darwin\"\nprofile = \"default\"\n\n[overrides]\n\"/Users/x/old\" = \"1.70-aarch64-apple-darwin\"\n";
        let named = parse_rustup_settings(text);
        assert_eq!(named.len(), 2);
        assert!(named.contains("stable-aarch64-apple-darwin"));
        assert!(named.contains("1.70-aarch64-apple-darwin"));
    }

    #[test]
    fn the_active_rust_toolchain_is_never_offered() {
        let fx = Fixture::new();
        toolchain(&fx, "stable-aarch64-apple-darwin");
        toolchain(&fx, "nightly-aarch64-apple-darwin");
        let found = rust_findings(&fx.ctx(), Some(RUSTUP_LIST));
        assert_eq!(titles(&found), vec!["nightly-aarch64-apple-darwin"]);
        assert!(!found[0].selected_by_default);
        assert_eq!(found[0].risk, Risk::Moderate);
        assert_eq!(
            found[0].action,
            Action::Command {
                program: "rustup".to_string(),
                args: vec![
                    "toolchain".into(),
                    "uninstall".into(),
                    "nightly-aarch64-apple-darwin".into(),
                ],
            }
        );
    }

    #[test]
    fn settings_protect_the_default_even_when_rustup_is_missing() {
        let fx = Fixture::new();
        toolchain(&fx, "stable-aarch64-apple-darwin");
        toolchain(&fx, "beta-aarch64-apple-darwin");
        fx.file(".rustup/settings.toml", b"default_toolchain = \"stable\"\n");
        let found = rust_findings(&fx.ctx(), None);
        assert_eq!(titles(&found), vec!["beta-aarch64-apple-darwin"]);
    }

    #[test]
    fn directory_overrides_are_protected_too() {
        let fx = Fixture::new();
        toolchain(&fx, "stable-aarch64-apple-darwin");
        toolchain(&fx, "1.70-aarch64-apple-darwin");
        fx.file(
            ".rustup/settings.toml",
            b"default_toolchain = \"stable-aarch64-apple-darwin\"\n[overrides]\n\"/x\" = \"1.70-aarch64-apple-darwin\"\n",
        );
        assert!(rust_findings(&fx.ctx(), None).is_empty());
    }

    #[test]
    fn no_way_to_tell_the_default_means_no_rust_offers() {
        let fx = Fixture::new();
        toolchain(&fx, "stable-aarch64-apple-darwin");
        toolchain(&fx, "nightly-aarch64-apple-darwin");
        // Not installed, and the settings file is missing or useless.
        assert!(rust_findings(&fx.ctx(), None).is_empty());
        fx.file(".rustup/settings.toml", b"\x00garbage");
        assert!(rust_findings(&fx.ctx(), None).is_empty());
    }

    #[test]
    fn a_toolchain_pinned_by_a_project_is_for_review() {
        let fx = Fixture::new();
        toolchain(&fx, "stable-aarch64-apple-darwin");
        toolchain(&fx, "1.75.0-aarch64-apple-darwin");
        fx.file(
            ".rustup/settings.toml",
            b"default_toolchain = \"stable-aarch64-apple-darwin\"\n",
        );
        fx.file("Coding/old/Cargo.toml", b"");
        fx.file(
            "Coding/old/rust-toolchain.toml",
            b"[toolchain]\nchannel = \"1.75.0\"\n",
        );
        let found = rust_findings(&fx.ctx(), None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].risk, Risk::Review);
        assert!(found[0].explanation.contains("old"));
    }

    fn node(fx: &Fixture, root: &str, version: &str) {
        fx.file(&format!("{root}/{version}/bin/node"), &[1u8; 4096]);
    }

    #[test]
    fn nvm_follows_the_real_alias_chain() {
        let fx = Fixture::new();
        let root = ".nvm/versions/node";
        node(&fx, root, "v24.15.0");
        node(&fx, root, "v20.11.0");
        // Exactly what is on disk on a real machine.
        fx.file(".nvm/alias/default", b"lts/*\n");
        fx.file(".nvm/alias/lts/*", b"lts/krypton\n");
        fx.file(".nvm/alias/lts/krypton", b"v24.15.0\n");
        let found = nvm_findings(&fx.ctx());
        assert_eq!(titles(&found), vec!["Node v20.11.0"]);
        assert!(!found[0].selected_by_default);
    }

    #[test]
    fn nvm_partial_default_means_the_newest_of_that_major() {
        let fx = Fixture::new();
        let root = ".nvm/versions/node";
        for version in ["v22.1.0", "v22.9.0", "v18.0.0"] {
            node(&fx, root, version);
        }
        fx.file(".nvm/alias/default", b"22\n");
        let found = nvm_findings(&fx.ctx());
        assert_eq!(titles(&found), vec!["Node v18.0.0", "Node v22.1.0"]);
    }

    #[test]
    fn nvm_with_no_usable_default_protects_the_newest() {
        let fx = Fixture::new();
        let root = ".nvm/versions/node";
        node(&fx, root, "v24.0.0");
        node(&fx, root, "v20.0.0");
        // Missing alias file.
        assert_eq!(titles(&nvm_findings(&fx.ctx())), vec!["Node v20.0.0"]);
        // An alias that resolves nowhere.
        fx.file(".nvm/alias/default", b"lts/nonexistent\n");
        assert_eq!(titles(&nvm_findings(&fx.ctx())), vec!["Node v20.0.0"]);
        // An alias naming a major that is not installed.
        fx.file(".nvm/alias/default", b"12\n");
        assert_eq!(titles(&nvm_findings(&fx.ctx())), vec!["Node v20.0.0"]);
    }

    #[test]
    fn nvm_system_default_means_all_versions_can_be_offered() {
        let fx = Fixture::new();
        let root = ".nvm/versions/node";
        node(&fx, root, "v24.0.0");
        node(&fx, root, "v20.0.0");
        fx.file(".nvm/alias/default", b"system\n");
        assert_eq!(nvm_findings(&fx.ctx()).len(), 2);
    }

    #[test]
    fn node_versions_with_global_packages_or_project_pins_are_for_review() {
        let fx = Fixture::new();
        let root = ".nvm/versions/node";
        node(&fx, root, "v24.0.0");
        node(&fx, root, "v22.0.0");
        node(&fx, root, "v20.0.0");
        node(&fx, root, "v18.0.0");
        fx.file(".nvm/alias/default", b"v24.0.0\n");
        fx.file(
            &format!("{root}/v22.0.0/lib/node_modules/npm/package.json"),
            b"{}",
        );
        fx.file(
            &format!("{root}/v22.0.0/lib/node_modules/typescript/package.json"),
            b"{}",
        );
        fx.file(
            &format!("{root}/v22.0.0/lib/node_modules/@scope/tool/package.json"),
            b"{}",
        );
        fx.file("Coding/app/package.json", b"{}");
        fx.file("Coding/app/.nvmrc", b"20\n");
        fx.file(
            &format!("{root}/v18.0.0/lib/node_modules/npm/package.json"),
            b"{}",
        );

        let found = nvm_findings(&fx.ctx());
        let by = |title: &str| found.iter().find(|finding| finding.title == title).unwrap();
        let v22 = by("Node v22.0.0");
        assert_eq!(v22.risk, Risk::Review);
        assert!(v22.explanation.contains("typescript"));
        assert!(v22.explanation.contains("@scope/tool"));
        assert!(!v22.explanation.contains("npm,"));
        let v20 = by("Node v20.0.0");
        assert_eq!(v20.risk, Risk::Review);
        assert!(v20.explanation.contains("app"));
        // npm itself ships with Node, so nothing extra is lost.
        assert_eq!(by("Node v18.0.0").risk, Risk::Moderate);
    }

    #[test]
    fn fnm_protects_the_version_its_default_alias_points_to() {
        let fx = Fixture::new();
        let root = "Library/Application Support/fnm/node-versions";
        node(&fx, root, "v24.0.0/installation");
        node(&fx, root, "v20.0.0/installation");
        fx.dir("Library/Application Support/fnm/aliases");
        symlink(
            fx.home.join(root).join("v20.0.0/installation"),
            fx.home
                .join("Library/Application Support/fnm/aliases/default"),
        )
        .unwrap();
        // The default is the older one: going by "newest" would offer the one in use.
        assert_eq!(titles(&fnm_findings(&fx.ctx())), vec!["Node v24.0.0"]);
    }

    #[test]
    fn fnm_with_no_default_protects_the_newest() {
        let fx = Fixture::new();
        let root = "Library/Application Support/fnm/node-versions";
        node(&fx, root, "v24.0.0/installation");
        node(&fx, root, "v20.0.0/installation");
        assert_eq!(titles(&fnm_findings(&fx.ctx())), vec!["Node v20.0.0"]);
    }

    #[test]
    fn volta_protects_the_platform_default() {
        let fx = Fixture::new();
        let root = ".volta/tools/image/node";
        node(&fx, root, "24.1.0");
        node(&fx, root, "20.0.0");
        fx.file(
            ".volta/tools/user/platform.json",
            br#"{"node":{"runtime":"20.0.0","npm":null},"pnpm":null,"yarn":null}"#,
        );
        assert_eq!(titles(&volta_findings(&fx.ctx())), vec!["Node 24.1.0"]);
    }

    #[test]
    fn volta_with_a_malformed_platform_file_protects_the_newest() {
        let fx = Fixture::new();
        let root = ".volta/tools/image/node";
        node(&fx, root, "24.1.0");
        node(&fx, root, "20.0.0");
        fx.file(".volta/tools/user/platform.json", b"{ not json");
        assert_eq!(titles(&volta_findings(&fx.ctx())), vec!["Node 20.0.0"]);
    }

    fn python(fx: &Fixture, version: &str) {
        fx.file(
            &format!(".pyenv/versions/{version}/bin/python"),
            &[1u8; 4096],
        );
    }

    #[test]
    fn pyenv_protects_every_version_in_the_global_file() {
        let fx = Fixture::new();
        for version in ["3.12.7", "3.13.13", "3.14.4"] {
            python(&fx, version);
        }
        fx.file(".pyenv/version", b"3.12.7\n3.13.13\n");
        let found = pyenv_findings(&fx.ctx());
        assert_eq!(titles(&found), vec!["Python 3.14.4"]);
        assert_eq!(found[0].risk, Risk::Moderate);
        assert!(!found[0].selected_by_default);
    }

    #[test]
    fn pyenv_with_no_version_file_protects_the_newest() {
        let fx = Fixture::new();
        python(&fx, "3.9.1");
        python(&fx, "3.13.0");
        // A numeric comparison, not an alphabetical one, where 3.9 would beat 3.13.
        assert_eq!(titles(&pyenv_findings(&fx.ctx())), vec!["Python 3.9.1"]);
    }

    #[test]
    fn a_pyenv_virtualenv_name_protects_its_base_version() {
        let fx = Fixture::new();
        python(&fx, "3.12.7");
        python(&fx, "3.14.4");
        fx.dir(".pyenv/versions/3.12.7/envs/work");
        symlink(
            fx.home.join(".pyenv/versions/3.12.7/envs/work"),
            fx.home.join(".pyenv/versions/work"),
        )
        .unwrap();
        fx.file(".pyenv/version", b"work\n");
        assert_eq!(titles(&pyenv_findings(&fx.ctx())), vec!["Python 3.14.4"]);
    }

    #[test]
    fn pyenv_versions_something_depends_on_are_for_review() {
        let fx = Fixture::new();
        python(&fx, "3.12.7");
        python(&fx, "3.13.3");
        python(&fx, "3.14.3");
        python(&fx, "3.14.4");
        fx.file(".pyenv/version", b"3.12.7\n");
        // A virtualenv in a project was created from this version.
        fx.file("Coding/api/pyproject.toml", b"");
        fx.file(
            "Coding/api/.venv/pyvenv.cfg",
            format!("home = {}/.pyenv/versions/3.13.3/bin\n", fx.home.display()).as_bytes(),
        );
        // A package someone installed into it.
        fx.dir(".pyenv/versions/3.14.3/lib/python3.14/site-packages/requests-2.32.0.dist-info");
        fx.dir(".pyenv/versions/3.14.4/lib/python3.14/site-packages/pip-24.0.dist-info");

        let found = pyenv_findings(&fx.ctx());
        let by = |title: &str| found.iter().find(|finding| finding.title == title).unwrap();
        assert_eq!(by("Python 3.13.3").risk, Risk::Review);
        assert!(by("Python 3.13.3").explanation.contains("api"));
        assert_eq!(by("Python 3.14.3").risk, Risk::Review);
        // pip ships with Python, so there is nothing extra to lose.
        assert_eq!(by("Python 3.14.4").risk, Risk::Moderate);
    }

    #[test]
    fn a_version_on_the_path_counts_as_active() {
        let root = Path::new("/h/.nvm/versions/node");
        let path = env::join_paths([
            "/h/.nvm/versions/node/v20.0.0/bin",
            "/opt/homebrew/bin",
            "/usr/bin",
        ])
        .unwrap();
        let versions = versions_on_path(root, Some(path.as_os_str()));
        assert_eq!(versions.len(), 1);
        assert!(versions.contains("v20.0.0"));
        assert!(versions_on_path(root, None).is_empty());
    }

    #[test]
    fn pins_match_by_prefix_of_the_version() {
        assert!(version_matches_pin("v20.11.0", "20"));
        assert!(version_matches_pin("v20.11.0", "20.11"));
        assert!(version_matches_pin("20.11.0", "v20.11.0"));
        assert!(!version_matches_pin("v20.11.0", "20.1"));
        assert!(!version_matches_pin("v20.11.0", "lts/iron"));
        assert!(!version_matches_pin("v2.0.0", "20"));
    }

    #[test]
    fn no_toolchain_managers_means_no_findings() {
        let fx = Fixture::new();
        let found = ToolchainScanner.scan(&fx.ctx()).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn query_rustup_never_reports_the_real_machine_for_a_fixture_home() {
        let fx = Fixture::new();
        toolchain(&fx, "nightly-aarch64-apple-darwin");
        fx.file(
            ".rustup/settings.toml",
            b"default_toolchain = \"nightly-aarch64-apple-darwin\"\n",
        );
        // rustup may or may not be on this machine. Either way the real default must not leak in.
        if let Some(list) = query_rustup(&fx.ctx()) {
            assert!(
                !list.contains("stable-aarch64-apple-darwin"),
                "saw the real machine: {list}"
            );
        }
    }
}
