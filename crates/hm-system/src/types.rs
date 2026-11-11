//! Public data types. The UI codes against these: fields may be added, but never renamed or removed.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

// ---- Live monitor -------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct SystemSnapshot {
    pub taken_at: SystemTime,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    pub disks: Vec<DiskStats>,
    pub battery: Option<BatteryStats>,
    pub thermal: ThermalState,
    pub network: NetworkStats,
    pub uptime_secs: u64,
    pub top_by_cpu: Vec<ProcessStats>,
    pub top_by_memory: Vec<ProcessStats>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CpuStats {
    pub brand: String,
    pub usage_percent: f32,
    pub per_core_percent: Vec<f32>,
    pub performance_cores: u32,
    pub efficiency_cores: u32,
    pub load_average: [f64; 3],
    /// Each CPU cluster as macOS names it, fastest first. Prefer this for labels: on M5 Pro the
    /// second cluster is called "Performance", so "efficiency" would be wrong there.
    pub core_clusters: Vec<CoreCluster>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CoreCluster {
    /// `hw.perflevelN.name`, as macOS reports it: "Super", "Performance", "Efficiency"...
    pub name: String,
    pub logical_cpus: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    #[default]
    Normal,
    Warning,
    Critical,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MemoryStats {
    pub total: u64,
    pub used: u64,
    /// Memory held by apps, as Activity Monitor reports it.
    pub app: u64,
    pub wired: u64,
    pub compressed: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub pressure: MemoryPressure,
    /// How much unified memory the GPU may wire (`iogpu.wired_limit_mb`, or the macOS default of
    /// roughly 2/3 to 3/4 of RAM when unset). This caps how large a local model can run on the GPU.
    pub gpu_wired_limit: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DiskStats {
    pub name: String,
    pub mount_point: PathBuf,
    pub total: u64,
    pub available: u64,
    pub is_removable: bool,
    pub file_system: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BatteryStats {
    pub percent: u8,
    pub charging: bool,
    pub plugged_in: bool,
    pub cycle_count: Option<u32>,
    /// Current full charge as a percentage of design capacity.
    pub health_percent: Option<u8>,
    /// macOS's own verdict, e.g. "Normal" or "Service Recommended".
    pub condition: Option<String>,
    pub minutes_remaining: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalState {
    #[default]
    Unknown,
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct NetworkStats {
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ProcessStats {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

// ---- Health score -------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Good,
    Info,
    Warning,
    Critical,
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthFactor {
    pub title: String,
    pub detail: String,
    pub severity: Severity,
    /// Points taken off the score because of this factor.
    pub penalty: u8,
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthReport {
    /// 0 to 100.
    pub score: u8,
    /// "Excelente", "Buena", "Regular" or "Atención", or their English versions.
    pub label: String,
    /// Worst first.
    pub factors: Vec<HealthFactor>,
}

// ---- Disk map -----------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct DirNode {
    pub name: String,
    pub path: PathBuf,
    /// On-disk bytes of the whole subtree.
    pub size: u64,
    pub is_dir: bool,
    pub file_count: u64,
    pub last_modified: Option<SystemTime>,
    /// Largest first. Children below the size threshold are folded into `other_size`.
    pub children: Vec<DirNode>,
    pub other_size: u64,
    /// The subtree was only partly readable, so the size is a lower bound.
    pub partial: bool,
}

// ---- Apps ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppArch {
    AppleSilicon,
    Universal,
    /// Runs under Rosetta. Worth flagging, since Rosetta is being phased out.
    Intel,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct Leftover {
    pub path: PathBuf,
    pub size: u64,
    /// Where it was found, in the current language: "Caché", "Preferencias", "Soporte de
    /// aplicación"...
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub path: PathBuf,
    pub size: u64,
    pub leftovers: Vec<Leftover>,
    pub last_used: Option<SystemTime>,
    pub running: bool,
    /// Under `/System` or otherwise not removable. The UI must not offer to uninstall these.
    pub is_system: bool,
    pub arch: AppArch,
    pub icon_path: Option<PathBuf>,
    /// `CFBundleExecutable`: the process name to check before touching the app. It often differs
    /// from `name` (Steam runs as `steam_osx`).
    pub executable: Option<String>,
    /// Why HealthyMac cannot uninstall this app, in the current language, ready to show. The UI
    /// should disable the button rather than offer something that can only fail: besides system
    /// apps, this covers apps inside a vendor folder and bundles owned by root.
    ///
    /// `None` means nothing rules it out, not that it will work. macOS's App Management protection
    /// can still refuse a signed app, and that cannot be checked without asking TCC, which a scan
    /// must not do. So the cleaner can still report a failure.
    pub uninstall_blocked: Option<String>,
}

// ---- Startup items ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupKind {
    /// `~/Library/LaunchAgents`
    UserAgent,
    /// `/Library/LaunchAgents`
    GlobalAgent,
    /// `/Library/LaunchDaemons`
    GlobalDaemon,
}

#[derive(Clone, Debug, Serialize)]
pub struct StartupItem {
    pub label: String,
    pub kind: StartupKind,
    pub plist_path: PathBuf,
    pub program: Option<String>,
    pub run_at_load: bool,
    pub keep_alive: bool,
    pub disabled: bool,
    /// The program it points to no longer exists: usually left behind by an uninstalled app.
    pub orphaned: bool,
    /// The plist could not be parsed, so `label` comes from the file name and the other fields are
    /// unknown rather than false.
    pub malformed: bool,
}
