//! What may ever be removed.
//!
//! This is the last line of defence. A buggy scanner must not be able to harm the user's data, so
//! the policy assumes nothing about where a path came from: the cleaner re-validates every target
//! here at clean time, including through symlinks.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SafetyViolation {
    #[error(
        "{}: {}",
        crate::t!(
            "la ruta no es absoluta o contiene '..'",
            "the path is not absolute or contains '..'"
        ),
        .0.display()
    )]
    NotNormalized(PathBuf),
    #[error(
        "{}: {}",
        crate::t!("la ruta ya no existe", "the path no longer exists"),
        .0.display()
    )]
    Missing(PathBuf),
    #[error(
        "{}: {}",
        crate::t!("fuera de las zonas permitidas", "outside the allowed areas"),
        .0.display()
    )]
    OutsideAllowedZones(PathBuf),
    #[error(
        "{}: {}",
        crate::t!(
            "es una carpeta protegida o contiene una",
            "is a protected folder or contains one"
        ),
        .0.display()
    )]
    ProtectedFolder(PathBuf),
    #[error(
        "{}: {}",
        crate::t!(
            "está dentro de una zona con datos sensibles",
            "is inside an area with sensitive data"
        ),
        .0.display()
    )]
    SensitiveTree(PathBuf),
    #[error(
        "{}: {}",
        crate::t!("es un punto de montaje", "is a mount point"),
        .0.display()
    )]
    MountPoint(PathBuf),
    #[error(
        "{}: {}",
        crate::t!("excluida por el usuario", "excluded by you"),
        .0.display()
    )]
    UserExcluded(PathBuf),
    #[error(
        "{}: {}",
        crate::t!(
            "contiene el programa que se está ejecutando",
            "contains the program that is running"
        ),
        .0.display()
    )]
    RunningProgram(PathBuf),
    #[error(
        "{}: {}",
        crate::t!("programa no permitido", "program not allowed"),
        .0
    )]
    ProgramNotAllowed(String),
}

/// Home-relative folders that can never be removed, and that nothing containing them can remove.
/// Their contents are still fair game, subject to [`SENSITIVE_TREES`].
const PROTECTED_FOLDERS: &[&str] = &[
    "Applications",
    "Desktop",
    "Documents",
    "Downloads",
    "Library",
    "Library/Application Support",
    "Library/Caches",
    "Library/Containers",
    "Library/Developer",
    "Library/Group Containers",
    "Library/LaunchAgents",
    "Library/Logs",
    "Library/Preferences",
    "Movies",
    "Music",
    "Pictures",
    "Public",
    ".Trash",
    ".cache",
    ".config",
    ".local",
    ".local/share",
    ".cargo",
    ".rustup",
    ".docker",
    // Local model stores: tens of gigabytes that take hours to download. A model can be removed
    // individually, but a bug must never be able to take out a whole store.
    ".ollama",
    ".ollama/models",
    ".ollama/models/blobs",
    ".ollama/models/manifests",
    ".lmstudio",
    ".lmstudio/models",
    ".omlx",
    ".omlx/models",
    ".cache/huggingface",
    ".cache/huggingface/hub",
    ".cache/huggingface/hub/blobs",
];

/// Home-relative trees where nothing at any depth may be removed: credentials, personal data and
/// cloud-synced files, where a delete propagates to other devices.
const SENSITIVE_TREES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".password-store",
    ".config/gh",
    ".config/gcloud",
    ".netrc",
    ".npmrc",
    ".git-credentials",
    ".zshrc",
    ".zprofile",
    ".bashrc",
    ".bash_profile",
    "Library/Accounts",
    "Library/Calendars",
    "Library/CloudStorage",
    "Library/Cookies",
    "Library/Keychains",
    "Library/Mail",
    "Library/Messages",
    "Library/Mobile Documents",
    "Library/Passes",
    "Library/Safari",
    "Pictures/Photos Library.photoslibrary",
];

/// Absolute roots outside the home directory whose contents may be removed. `/Applications` is
/// handled separately and only ever allows a top-level `.app` bundle.
const ALLOWED_SYSTEM_ROOTS: &[&str] = &["/Library/Caches", "/Library/Logs"];

/// Tools whose own cleanup commands may run. Run directly, never through a shell, and never with
/// elevated privileges.
const ALLOWED_PROGRAMS: &[&str] = &[
    "brew", "bun", "cargo", "docker", "git", "go", "lms", "npm", "ollama", "orbctl", "pnpm", "pod",
    "rustup", "uv", "xcrun", "yarn",
];

#[derive(Clone, Debug)]
pub struct SafetyPolicy {
    home: PathBuf,
    excluded: Vec<PathBuf>,
    running_program: Option<PathBuf>,
}

impl SafetyPolicy {
    pub fn new(home: impl AsRef<Path>, excluded: &[PathBuf]) -> Self {
        Self {
            home: canonical_or_self(home.as_ref()),
            excluded: excluded
                .iter()
                .map(|path| canonical_or_self(path))
                .collect(),
            running_program: running_program(),
        }
    }

    /// Decides whether `path` may be removed.
    ///
    /// Checks the path as written and with its parent's symlinks resolved. The final component is
    /// not followed: removing a symlink only ever removes the link itself.
    pub fn check_removal(&self, path: &Path) -> Result<(), SafetyViolation> {
        let normalized = path.is_absolute()
            && path
                .components()
                .all(|component| !matches!(component, Component::ParentDir | Component::CurDir));
        if !normalized || path.file_name().is_none() {
            return Err(SafetyViolation::NotNormalized(path.to_path_buf()));
        }

        let meta =
            fs::symlink_metadata(path).map_err(|_| SafetyViolation::Missing(path.to_path_buf()))?;

        let parent = path
            .parent()
            .ok_or_else(|| SafetyViolation::NotNormalized(path.to_path_buf()))?;
        let resolved_parent = parent
            .canonicalize()
            .map_err(|_| SafetyViolation::Missing(path.to_path_buf()))?;
        let resolved = resolved_parent.join(path.file_name().unwrap_or_default());

        if meta.is_dir() && !meta.file_type().is_symlink() {
            let parent_meta = fs::metadata(&resolved_parent)
                .map_err(|_| SafetyViolation::Missing(path.to_path_buf()))?;
            if parent_meta.dev() != meta.dev() {
                return Err(SafetyViolation::MountPoint(path.to_path_buf()));
            }
        }

        if self
            .excluded
            .iter()
            .any(|excluded| resolved == *excluded || resolved.starts_with(excluded))
        {
            return Err(SafetyViolation::UserExcluded(path.to_path_buf()));
        }

        // An excluded path inside the target would be removed along with it.
        if self
            .excluded
            .iter()
            .any(|excluded| excluded.starts_with(&resolved))
        {
            return Err(SafetyViolation::UserExcluded(path.to_path_buf()));
        }

        // A build folder is a fair target, unless it is the one this program runs from: it would
        // pull its own bundle out from under itself.
        if self
            .running_program
            .as_ref()
            .is_some_and(|program| program.starts_with(&resolved))
        {
            return Err(SafetyViolation::RunningProgram(path.to_path_buf()));
        }

        if resolved.starts_with(&self.home) {
            return self.check_inside_home(path, &resolved);
        }
        check_outside_home(path, &resolved)
    }

    fn check_inside_home(&self, original: &Path, resolved: &Path) -> Result<(), SafetyViolation> {
        if resolved == self.home {
            return Err(SafetyViolation::ProtectedFolder(original.to_path_buf()));
        }
        for sensitive in SENSITIVE_TREES {
            let sensitive = self.home.join(sensitive);
            // Inside a sensitive tree, or containing one.
            if resolved.starts_with(&sensitive) || sensitive.starts_with(resolved) {
                return Err(SafetyViolation::SensitiveTree(original.to_path_buf()));
            }
        }
        for protected in PROTECTED_FOLDERS {
            // Equal to a protected folder, or an ancestor that would take it along.
            if self.home.join(protected).starts_with(resolved) {
                return Err(SafetyViolation::ProtectedFolder(original.to_path_buf()));
            }
        }
        Ok(())
    }

    /// Decides whether a tool's cleanup command may run. `program` must be a bare name on the
    /// allow-list, so a scanner cannot point this at an arbitrary binary.
    pub fn check_command(&self, program: &str) -> Result<(), SafetyViolation> {
        if ALLOWED_PROGRAMS.contains(&program) {
            Ok(())
        } else {
            Err(SafetyViolation::ProgramNotAllowed(program.to_string()))
        }
    }
}

/// Where this program's executable really is, or `None` if macOS won't say.
pub fn running_program() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
}

/// An `.app` bundle under `/Applications`, including inside a vendor's folder, but never something
/// nested in another bundle: that would break the app it belongs to.
fn is_installed_app(path: &Path) -> bool {
    let is_app = |candidate: &Path| candidate.extension().is_some_and(|ext| ext == "app");
    path.starts_with("/Applications") && is_app(path) && !path.ancestors().skip(1).any(is_app)
}

fn check_outside_home(original: &Path, resolved: &Path) -> Result<(), SafetyViolation> {
    if is_installed_app(resolved) {
        return Ok(());
    }
    for root in ALLOWED_SYSTEM_ROOTS {
        let root = Path::new(root);
        if resolved != root && resolved.starts_with(root) {
            return Ok(());
        }
    }
    Err(SafetyViolation::OutsideAllowedZones(original.to_path_buf()))
}

/// Resolves symlinks in the longest prefix of `path` that exists. A rule about a path that does not
/// exist yet must still match once it does: comparing against an unresolved `/var/...` when targets
/// resolve to `/private/var/...` would silently disable an exclusion.
fn canonical_or_self(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(resolved) = ancestor.canonicalize() {
            let rest = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return resolved.join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_whatever_contains_the_running_program() {
        let program = running_program().unwrap();
        let folder = program.parent().unwrap();
        let home = folder.parent().unwrap();
        let policy = SafetyPolicy::new(home, &[]);

        for path in [program.as_path(), folder] {
            assert!(
                matches!(
                    policy.check_removal(path),
                    Err(SafetyViolation::RunningProgram(_))
                ),
                "{path:?} should be refused"
            );
        }
    }
    use std::os::unix::fs::symlink;

    struct Home {
        dir: tempfile::TempDir,
        policy: SafetyPolicy,
    }

    impl Home {
        fn new() -> Self {
            Self::excluding(&[])
        }

        fn excluding(relative: &[&str]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let excluded: Vec<PathBuf> = relative.iter().map(|r| dir.path().join(r)).collect();
            let policy = SafetyPolicy::new(dir.path(), &excluded);
            Self { dir, policy }
        }

        fn mkdir(&self, relative: &str) -> PathBuf {
            let path = self.dir.path().join(relative);
            fs::create_dir_all(&path).unwrap();
            path.canonicalize().unwrap()
        }

        fn touch(&self, relative: &str) -> PathBuf {
            let path = self.dir.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"x").unwrap();
            path.canonicalize().unwrap()
        }
    }

    #[test]
    fn allows_ordinary_targets_inside_home() {
        let home = Home::new();
        let path = home.mkdir("Library/Caches/com.example.app");
        assert_eq!(home.policy.check_removal(&path), Ok(()));
        let path = home.mkdir("Coding/project/node_modules");
        assert_eq!(home.policy.check_removal(&path), Ok(()));
    }

    #[test]
    fn refuses_the_home_folder_itself() {
        let home = Home::new();
        let path = home.dir.path().canonicalize().unwrap();
        assert!(matches!(
            home.policy.check_removal(&path),
            Err(SafetyViolation::ProtectedFolder(_))
        ));
    }

    #[test]
    fn refuses_protected_folders_and_their_ancestors() {
        let home = Home::new();
        for relative in ["Documents", "Library/Caches", "Library/Application Support"] {
            let path = home.mkdir(relative);
            assert!(
                matches!(
                    home.policy.check_removal(&path),
                    Err(SafetyViolation::ProtectedFolder(_))
                ),
                "{relative} should be protected"
            );
        }
        // `Library` also contains sensitive trees, which is reported first. It must be refused
        // either way.
        let library = home.mkdir("Library");
        assert!(home.policy.check_removal(&library).is_err());
    }

    #[test]
    fn a_whole_model_store_can_never_go_but_a_single_model_can() {
        let home = Home::new();
        for store in [
            ".ollama/models",
            ".ollama/models/blobs",
            ".lmstudio/models",
            ".omlx/models",
            ".cache/huggingface",
            ".cache/huggingface/hub",
            ".cache/huggingface/hub/blobs",
        ] {
            let path = home.mkdir(store);
            assert!(
                matches!(
                    home.policy.check_removal(&path),
                    Err(SafetyViolation::ProtectedFolder(_))
                ),
                "{store} should be protected"
            );
        }
        for model in [
            ".lmstudio/models/org/model",
            ".omlx/models/model",
            ".cache/huggingface/hub/models--org--name",
        ] {
            let path = home.mkdir(model);
            assert_eq!(home.policy.check_removal(&path), Ok(()), "{model}");
        }
    }

    #[test]
    fn exclusion_of_a_path_that_does_not_exist_yet_still_applies_later() {
        let home = Home::excluding(&["Coding/not-created-yet"]);
        let later = home.mkdir("Coding/not-created-yet/node_modules");
        assert!(matches!(
            home.policy.check_removal(&later),
            Err(SafetyViolation::UserExcluded(_))
        ));
    }

    #[test]
    fn refuses_anything_in_sensitive_trees() {
        let home = Home::new();
        for relative in [
            ".ssh/id_ed25519",
            ".aws/credentials",
            "Library/Keychains/login.keychain-db",
            "Library/Mobile Documents/com~apple~CloudDocs/tesis.pdf",
            "Library/Mail/V10/x.emlx",
        ] {
            let path = home.touch(relative);
            assert!(
                matches!(
                    home.policy.check_removal(&path),
                    Err(SafetyViolation::SensitiveTree(_))
                ),
                "{relative} should be sensitive"
            );
        }
    }

    #[test]
    fn refuses_a_folder_that_contains_a_sensitive_tree() {
        let home = Home::new();
        let path = home.mkdir(".config/gh");
        let parent = path.parent().unwrap();
        // `.config` is both protected and an ancestor of a sensitive tree.
        assert!(home.policy.check_removal(parent).is_err());
    }

    #[test]
    fn refuses_parent_dir_tricks() {
        let home = Home::new();
        let cache = home.mkdir("Library/Caches/app");
        let sneaky = cache.join("../../../.ssh");
        assert!(matches!(
            home.policy.check_removal(&sneaky),
            Err(SafetyViolation::NotNormalized(_))
        ));
        assert!(matches!(
            home.policy.check_removal(Path::new("relative/path")),
            Err(SafetyViolation::NotNormalized(_))
        ));
        assert!(home.policy.check_removal(Path::new("/")).is_err());
    }

    #[test]
    fn resolves_symlinked_parents() {
        let home = Home::new();
        let secret = home.touch(".ssh/id_rsa");
        let caches = home.mkdir("Library/Caches");
        symlink(secret.parent().unwrap(), caches.join("innocent")).unwrap();
        let through_link = caches.join("innocent/id_rsa");
        assert!(matches!(
            home.policy.check_removal(&through_link),
            Err(SafetyViolation::SensitiveTree(_))
        ));
    }

    #[test]
    fn a_symlink_target_does_not_matter_since_only_the_link_goes() {
        let home = Home::new();
        let secret = home.mkdir(".ssh");
        let caches = home.mkdir("Library/Caches");
        let link = caches.join("link-to-ssh");
        symlink(&secret, &link).unwrap();
        assert_eq!(home.policy.check_removal(&link), Ok(()));
    }

    #[test]
    fn refuses_paths_outside_allowed_zones() {
        let home = Home::new();
        for path in [
            "/etc/hosts",
            "/usr/bin/ls",
            "/System/Library",
            "/Applications",
        ] {
            assert!(
                home.policy.check_removal(Path::new(path)).is_err(),
                "{path} should be refused"
            );
        }
    }

    #[test]
    fn only_whole_app_bundles_under_applications_qualify() {
        for path in ["/Applications/Foo.app", "/Applications/Vendor/Foo.app"] {
            assert!(is_installed_app(Path::new(path)), "{path}");
        }
        for path in [
            "/Applications",
            "/Applications/Vendor",
            "/Applications/Foo.app/Contents/Helper.app",
            "/Applications/Foo.app/Contents",
            "/Library/Foo.app",
        ] {
            assert!(!is_installed_app(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn missing_path_is_reported() {
        let home = Home::new();
        let path = home.dir.path().canonicalize().unwrap().join("gone");
        assert!(matches!(
            home.policy.check_removal(&path),
            Err(SafetyViolation::Missing(_))
        ));
    }

    #[test]
    fn honours_user_exclusions_in_both_directions() {
        let home = Home::excluding(&["Coding/keep"]);
        let inside = home.mkdir("Coding/keep/node_modules");
        assert!(matches!(
            home.policy.check_removal(&inside),
            Err(SafetyViolation::UserExcluded(_))
        ));
        let containing = home.dir.path().canonicalize().unwrap().join("Coding");
        assert!(matches!(
            home.policy.check_removal(&containing),
            Err(SafetyViolation::UserExcluded(_))
        ));
    }

    #[test]
    fn command_allow_list() {
        let home = Home::new();
        assert!(home.policy.check_command("brew").is_ok());
        for program in ["rm", "sudo", "sh", "/opt/homebrew/bin/brew", "diskutil"] {
            assert!(
                home.policy.check_command(program).is_err(),
                "{program} should be refused"
            );
        }
    }
}
