//! Public data types. The UI codes against these: fields may be added, but never renamed or removed.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

// ---- Ports --------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct ListeningPort {
    pub port: u16,
    /// "TCP" or "UDP".
    pub protocol: String,
    pub address: String,
    pub pid: u32,
    pub process_name: String,
    pub command: Option<String>,
    /// The process's working directory, which usually tells you which project it belongs to.
    pub cwd: Option<PathBuf>,
    pub user: String,
    /// Friendly guess, in the current language, when the port is well known: "Ollama",
    /// "PostgreSQL", "Vite"...
    pub service_hint: Option<String>,
}

// ---- Projects -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Node,
    Rust,
    Python,
    Swift,
    Xcode,
    Go,
    Gradle,
    Maven,
    Flutter,
    Ruby,
    Php,
    DotNet,
    CMake,
    Other,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectArtifact {
    pub path: PathBuf,
    /// "node_modules", "target", ".venv"...
    pub label: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub dirty: bool,
    pub unpushed_commits: Option<u32>,
    pub last_commit: Option<SystemTime>,
    pub git_dir_bytes: u64,
    /// `false` if `git status` failed or timed out, so `branch`, `dirty` and `unpushed_commits` are
    /// unknown rather than clean.
    pub status_known: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectInfo {
    pub name: String,
    pub path: PathBuf,
    pub kinds: Vec<ProjectKind>,
    /// Latest of the last commit and the newest source file, ignoring artifacts.
    pub last_activity: Option<SystemTime>,
    pub idle_days: Option<u64>,
    pub artifacts: Vec<ProjectArtifact>,
    pub artifact_bytes: u64,
    pub git: Option<GitInfo>,
}

// ---- Docker -------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DockerUsage {
    pub count: u64,
    pub active: u64,
    pub bytes: u64,
    pub reclaimable: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DockerSummary {
    /// "Docker Desktop", "OrbStack", "Colima"...
    pub runtime: String,
    pub daemon_running: bool,
    pub version: Option<String>,
    pub images: DockerUsage,
    pub containers: DockerUsage,
    pub volumes: DockerUsage,
    pub build_cache: DockerUsage,
    pub disk_image_path: Option<PathBuf>,
    /// Space the VM disk image actually occupies, which is often far more than Docker reports.
    pub disk_image_bytes: u64,
    /// `true` if the disk image was found and measured. With `false`, a zero means unknown.
    pub disk_image_readable: bool,
    /// Why the disk image could not be read, in the current language, when that deserves saying.
    pub disk_image_note: Option<String>,
    /// `true` if `images`, `containers`, `volumes` and `build_cache` hold real figures. With
    /// `false`, the daemon is down or did not answer, and their zeros mean unknown.
    pub usage_known: bool,
}
