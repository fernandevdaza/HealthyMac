//! Docker, OrbStack and Colima disk usage.
//!
//! Everything here is read-only and nothing starts a daemon: if the runtime is stopped, the answer
//! says so, and the disk image is the only thing left to report. That image lives in a folder guarded
//! by a permission prompt for Docker Desktop and OrbStack, so it is only read with Full Disk Access.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hm_core::{fsutil, tf, ScanContext};

use crate::types::{DockerSummary, DockerUsage};
use crate::util;

/// `docker system df` walks every image and volume, and can take a while on a busy machine.
const DF_TIMEOUT: Duration = Duration::from_secs(30);
const CLI_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Runtime {
    DockerDesktop,
    OrbStack,
    Colima,
    /// A `docker` CLI with no runtime recognised, such as one pointed at a remote host.
    Generic,
}

impl Runtime {
    fn label(self) -> &'static str {
        match self {
            Runtime::DockerDesktop => "Docker Desktop",
            Runtime::OrbStack => "OrbStack",
            Runtime::Colima => "Colima",
            Runtime::Generic => "Docker",
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Installed {
    desktop: bool,
    orbstack: bool,
    colima: bool,
    cli: Option<PathBuf>,
}

impl Installed {
    fn detect(ctx: &ScanContext) -> Self {
        let app = |name: &str| {
            Path::new("/Applications").join(name).is_dir()
                || ctx.home_path("Applications").join(name).is_dir()
        };
        Self {
            desktop: app("Docker.app"),
            orbstack: app("OrbStack.app"),
            colima: ctx.which("colima").is_some(),
            cli: ctx.which("docker"),
        }
    }

    fn any(&self) -> bool {
        self.desktop || self.orbstack || self.colima || self.cli.is_some()
    }
}

/// Which runtime the CLI is talking to, going by its context when it has one.
fn choose_runtime(installed: &Installed, context: Option<&str>) -> Runtime {
    match context {
        Some("orbstack") => return Runtime::OrbStack,
        Some(name) if name.starts_with("colima") => return Runtime::Colima,
        Some(name) if name.starts_with("desktop") => return Runtime::DockerDesktop,
        _ => {}
    }
    if installed.desktop {
        Runtime::DockerDesktop
    } else if installed.orbstack {
        Runtime::OrbStack
    } else if installed.colima {
        Runtime::Colima
    } else {
        Runtime::Generic
    }
}

/// Parses `1.224GB`, `30.46GB (50%)`, `512.3kB` or `0B`, the forms `docker system df` writes.
fn parse_docker_size(text: &str) -> Option<u64> {
    let size = text.split_whitespace().next()?;
    let split = size.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = size.split_at(split);
    let number: f64 = number.parse().ok()?;
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "pb" => 1e15,
        "kib" => 1024.0,
        "mib" => 1_048_576.0,
        "gib" => 1_073_741_824.0,
        "tib" => 1_099_511_627_776.0,
        _ => return None,
    };
    (number.is_finite() && number >= 0.0).then(|| (number * multiplier).round() as u64)
}

/// The four kinds of usage, each only if the output reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DiskUsage {
    images: Option<DockerUsage>,
    containers: Option<DockerUsage>,
    volumes: Option<DockerUsage>,
    build_cache: Option<DockerUsage>,
}

/// Parses `docker system df --format json`: one JSON object per line. `None` if nothing in it could
/// be understood, as opposed to the empty machine, which still has all four lines.
fn parse_system_df(output: &str) -> Option<DiskUsage> {
    let mut usage = DiskUsage::default();
    let mut any = false;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
    {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let text = |key: &str| row.get(key).and_then(|value| value.as_str());
        let number = |key: &str| text(key).and_then(|value| value.trim().parse::<u64>().ok());
        let Some(kind) = text("Type") else {
            continue;
        };
        let parsed = DockerUsage {
            count: number("TotalCount").unwrap_or(0),
            active: number("Active").unwrap_or(0),
            bytes: text("Size").and_then(parse_docker_size).unwrap_or(0),
            reclaimable: text("Reclaimable").and_then(parse_docker_size).unwrap_or(0),
        };
        match kind {
            "Images" => usage.images = Some(parsed),
            "Containers" => usage.containers = Some(parsed),
            "Local Volumes" => usage.volumes = Some(parsed),
            "Build Cache" => usage.build_cache = Some(parsed),
            _ => continue,
        }
        any = true;
    }
    any.then_some(usage)
}

fn first_line(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

fn current_context(docker: &Path) -> Option<String> {
    let output = util::run_tool(docker, &["context", "show"], CLI_TIMEOUT)?;
    output
        .success()
        .then(|| first_line(&output.stdout))
        .flatten()
}

/// The daemon's version, which doubles as the check that it is up. The CLI connects to what is
/// already running and never starts anything.
fn server_version(docker: &Path) -> Option<String> {
    let output = util::run_tool(
        docker,
        &["version", "--format", "{{.Server.Version}}"],
        CLI_TIMEOUT,
    )?;
    output
        .success()
        .then(|| first_line(&output.stdout))
        .flatten()
}

/// Names of volumes no container uses, so the user can see what a prune would take.
pub(crate) fn unused_volume_names(ctx: &ScanContext) -> Vec<String> {
    let Some(docker) = ctx.which("docker") else {
        return Vec::new();
    };
    let output = util::run_tool(
        &docker,
        &[
            "volume",
            "ls",
            "-f",
            "dangling=true",
            "--format",
            "{{.Name}}",
        ],
        CLI_TIMEOUT,
    );
    match output {
        Some(output) if output.success() => output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Where each runtime keeps its VM disk, relative to the home folder.
fn disk_image_candidates(runtime: Runtime) -> &'static [&'static str] {
    match runtime {
        Runtime::DockerDesktop => {
            &["Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"]
        }
        Runtime::OrbStack => &[
            "Library/Group Containers/HUAQ24HBR6.dev.orbstack/data/data.img",
            ".orbstack/data/data.img",
        ],
        Runtime::Colima => &[
            ".colima/_lima/_disks/colima/datadisk",
            ".colima/_lima/colima/diffdisk",
            ".lima/colima/diffdisk",
        ],
        Runtime::Generic => &[],
    }
}

/// Fills in the disk image, or says honestly why it could not be read.
fn inspect_disk_image(ctx: &ScanContext, runtime: Runtime, summary: &mut DockerSummary) {
    let candidates = disk_image_candidates(runtime);
    let mut blocked = false;
    for relative in candidates {
        let path = ctx.home_path(relative);
        if !util::may_touch(ctx, &path) {
            blocked = true;
            continue;
        }
        if path.is_file() {
            // Sparse files count for the blocks they really use, not their apparent size.
            summary.disk_image_bytes = fsutil::measure(&path, &ctx.cancel).bytes;
            summary.disk_image_path = Some(path);
            summary.disk_image_readable = true;
            return;
        }
    }
    if blocked {
        summary.disk_image_note = Some(tf!(
            "No se puede saber cuánto ocupa la imagen de disco de {} porque está en una carpeta protegida de macOS, que no se abre sin pedir permiso. Si concedes Acceso total al disco a HealthyMac, verás su tamaño real, que suele ser mayor de lo que informa Docker.",
            "There is no way to tell how much space the {} disk image takes, because it is in a protected macOS folder that can't be opened without asking for permission. If you give HealthyMac Full Disk Access, you'll see its real size, which is usually larger than Docker reports.",
            runtime.label()
        ));
    }
}

fn apply_usage(summary: &mut DockerSummary, usage: DiskUsage) {
    summary.images = usage.images.unwrap_or_default();
    summary.containers = usage.containers.unwrap_or_default();
    summary.volumes = usage.volumes.unwrap_or_default();
    summary.build_cache = usage.build_cache.unwrap_or_default();
    summary.usage_known = true;
}

/// `None` if no container runtime is installed. Never starts a daemon.
pub fn summary(ctx: &ScanContext) -> Option<DockerSummary> {
    let installed = Installed::detect(ctx);
    if !installed.any() {
        return None;
    }
    let docker = installed.cli.as_deref();
    let context = docker.and_then(current_context);
    let runtime = choose_runtime(&installed, context.as_deref());
    let mut summary = DockerSummary {
        runtime: runtime.label().to_string(),
        ..DockerSummary::default()
    };
    if let Some(docker) = docker {
        if let Some(version) = server_version(docker) {
            summary.daemon_running = true;
            summary.version = Some(version);
            let df = util::run_tool(docker, &["system", "df", "--format", "json"], DF_TIMEOUT)
                .filter(|output| output.success())
                .and_then(|output| parse_system_df(&output.stdout));
            if let Some(usage) = df {
                apply_usage(&mut summary, usage);
            }
        }
    }
    inspect_disk_image(ctx, runtime, &mut summary);
    Some(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    const CAPTURED_DF: &str = r#"{"Active":"28","Reclaimable":"30.46GB (50%)","Size":"60.89GB","TotalCount":"82","Type":"Images"}
{"Active":"0","Reclaimable":"1.224GB (100%)","Size":"1.224GB","TotalCount":"73","Type":"Containers"}
{"Active":"15","Reclaimable":"1.256GB (54%)","Size":"2.307GB","TotalCount":"25","Type":"Local Volumes"}
{"Active":"0","Reclaimable":"20.73GB","Size":"46.27GB","TotalCount":"509","Type":"Build Cache"}
"#;

    const EMPTY_DF: &str = r#"{"Active":"0","Reclaimable":"0B (0%)","Size":"0B","TotalCount":"0","Type":"Images"}
{"Active":"0","Reclaimable":"0B (0%)","Size":"0B","TotalCount":"0","Type":"Containers"}
{"Active":"0","Reclaimable":"0B","Size":"0B","TotalCount":"0","Type":"Local Volumes"}
{"Active":"0","Reclaimable":"0B","Size":"0B","TotalCount":"0","Type":"Build Cache"}
"#;

    #[test]
    fn parses_captured_system_df() {
        let usage = parse_system_df(CAPTURED_DF).unwrap();
        let images = usage.images.unwrap();
        assert_eq!(images.count, 82);
        assert_eq!(images.active, 28);
        assert_eq!(images.bytes, 60_890_000_000);
        assert_eq!(images.reclaimable, 30_460_000_000);
        let volumes = usage.volumes.unwrap();
        assert_eq!(volumes.count, 25);
        assert_eq!(volumes.reclaimable, 1_256_000_000);
        // Build cache writes its reclaimable figure with no percentage.
        assert_eq!(usage.build_cache.unwrap().reclaimable, 20_730_000_000);
        assert_eq!(usage.containers.unwrap().active, 0);
    }

    #[test]
    fn an_empty_machine_is_not_a_parse_failure() {
        let usage = parse_system_df(EMPTY_DF).unwrap();
        assert_eq!(usage.images.unwrap().reclaimable, 0);
    }

    #[test]
    fn malformed_or_error_output_is_recognised_as_such() {
        assert_eq!(parse_system_df(""), None);
        assert_eq!(
            parse_system_df("Cannot connect to the Docker daemon at unix:///var/run/docker.sock."),
            None
        );
        assert_eq!(
            parse_system_df("{not json}\n{\"also\":\"not ours\"}\n"),
            None
        );
    }

    #[test]
    fn a_bad_field_does_not_lose_the_rest_of_the_row() {
        let usage = parse_system_df(
            r#"{"Active":"x","Reclaimable":"lots","Size":"1GB","TotalCount":"2","Type":"Images"}"#,
        )
        .unwrap();
        let images = usage.images.unwrap();
        assert_eq!(images.count, 2);
        assert_eq!(images.active, 0);
        assert_eq!(images.bytes, 1_000_000_000);
        assert_eq!(images.reclaimable, 0);
    }

    #[test]
    fn docker_sizes_parse_in_every_unit_it_writes() {
        assert_eq!(parse_docker_size("0B"), Some(0));
        assert_eq!(parse_docker_size("512.3kB"), Some(512_300));
        assert_eq!(parse_docker_size("1.224GB"), Some(1_224_000_000));
        assert_eq!(parse_docker_size("30.46GB (50%)"), Some(30_460_000_000));
        assert_eq!(parse_docker_size("2TB"), Some(2_000_000_000_000));
        assert_eq!(parse_docker_size("1KiB"), Some(1024));
        assert_eq!(parse_docker_size("lots"), None);
        assert_eq!(parse_docker_size(""), None);
        assert_eq!(parse_docker_size("-1GB"), None);
    }

    #[test]
    fn the_context_decides_the_runtime_and_installs_are_the_fallback() {
        let both = Installed {
            desktop: true,
            orbstack: true,
            ..Installed::default()
        };
        assert_eq!(choose_runtime(&both, Some("orbstack")), Runtime::OrbStack);
        assert_eq!(
            choose_runtime(&both, Some("desktop-linux")),
            Runtime::DockerDesktop
        );
        assert_eq!(choose_runtime(&both, Some("colima-work")), Runtime::Colima);
        assert_eq!(
            choose_runtime(&both, Some("default")),
            Runtime::DockerDesktop
        );
        assert_eq!(
            choose_runtime(&Installed::default(), None),
            Runtime::Generic
        );
    }

    #[test]
    fn docker_desktop_disk_image_is_not_read_without_full_disk_access() {
        let fx = Fixture::new();
        let image = "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw";
        fx.file(image, &[1u8; 8192]);
        let mut summary = DockerSummary::default();
        inspect_disk_image(&fx.ctx(), Runtime::DockerDesktop, &mut summary);
        assert!(!summary.disk_image_readable);
        assert_eq!(summary.disk_image_path, None);
        assert_eq!(summary.disk_image_bytes, 0);
        assert!(summary
            .disk_image_note
            .unwrap()
            .contains("Acceso total al disco"));
    }

    #[test]
    fn docker_desktop_disk_image_is_measured_with_full_disk_access() {
        let fx = Fixture::new();
        fx.file("Library/Safari/History.db", b"x");
        let image = "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw";
        fx.file(image, &[1u8; 8192]);
        let mut summary = DockerSummary::default();
        inspect_disk_image(&fx.ctx(), Runtime::DockerDesktop, &mut summary);
        assert!(summary.disk_image_readable);
        assert!(summary.disk_image_bytes >= 8192);
        assert_eq!(summary.disk_image_note, None);
    }

    #[test]
    fn colima_disk_image_needs_no_permission() {
        let fx = Fixture::new();
        fx.file(".colima/_lima/colima/diffdisk", &[1u8; 8192]);
        let mut summary = DockerSummary::default();
        inspect_disk_image(&fx.ctx(), Runtime::Colima, &mut summary);
        assert!(summary.disk_image_readable);
    }

    #[test]
    fn no_disk_image_is_simply_nothing_to_report() {
        let fx = Fixture::new();
        let mut summary = DockerSummary::default();
        inspect_disk_image(&fx.ctx(), Runtime::Colima, &mut summary);
        assert!(!summary.disk_image_readable);
        assert_eq!(summary.disk_image_note, None);
    }
}
