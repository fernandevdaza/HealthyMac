//! Invented projects, ports and a Docker summary.

use std::path::PathBuf;

use hm_dev::types::*;

use super::{days_ago, GB, HOME, MB};

#[allow(clippy::too_many_arguments)]
fn project(
    name: &str,
    relative: &str,
    kinds: &[ProjectKind],
    idle: u64,
    artifacts: &[(&str, u64)],
    branch: &str,
    dirty: bool,
    unpushed: u32,
) -> ProjectInfo {
    let path = PathBuf::from(format!("{HOME}/Coding/{relative}"));
    let artifacts: Vec<ProjectArtifact> = artifacts
        .iter()
        .map(|(label, size)| ProjectArtifact {
            path: path.join(label),
            label: label.to_string(),
            size: *size,
        })
        .collect();
    ProjectInfo {
        name: name.to_string(),
        path,
        kinds: kinds.to_vec(),
        last_activity: days_ago(idle),
        idle_days: Some(idle),
        artifact_bytes: artifacts.iter().map(|artifact| artifact.size).sum(),
        artifacts,
        git: Some(GitInfo {
            branch: Some(branch.to_string()),
            dirty,
            unpushed_commits: Some(unpushed),
            last_commit: days_ago(idle),
            git_dir_bytes: 180 * MB,
            status_known: true,
        }),
    }
}

pub fn projects() -> Vec<ProjectInfo> {
    use ProjectKind::*;
    vec![
        project(
            "motor-audio",
            "motor-audio",
            &[Rust],
            33,
            &[("target", 18_200 * MB)],
            "main",
            false,
            0,
        ),
        project(
            "tienda-web",
            "tienda-web",
            &[Node],
            94,
            &[
                ("node_modules", 2_140 * MB),
                ("admin/node_modules", 1_380 * MB),
                (".next", 460 * MB),
            ],
            "feat/checkout",
            true,
            3,
        ),
        project(
            "rsync-mejor",
            "herramientas/rsync-mejor",
            &[Rust],
            72,
            &[("target", 4_100 * MB)],
            "main",
            false,
            0,
        ),
        project(
            "app-recetas",
            "app-recetas",
            &[Swift, Xcode],
            2,
            &[(".build", 1_800 * MB)],
            "develop",
            true,
            1,
        ),
        project(
            "scraper-precios",
            "scraper-precios",
            &[Python],
            180,
            &[(".venv", 1_240 * MB), ("__pycache__", 18 * MB)],
            "main",
            false,
            0,
        ),
        project(
            "blog-astro",
            "blog-astro",
            &[Node],
            41,
            &[("node_modules", 610 * MB), ("dist", 44 * MB)],
            "main",
            false,
            2,
        ),
        project(
            "tesis-visualizaciones",
            "viejo/tesis-visualizaciones",
            &[Node, Python],
            420,
            &[("node_modules", 980 * MB), (".venv", 720 * MB)],
            "master",
            false,
            0,
        ),
        project(
            "cliente-android",
            "cliente-android",
            &[Gradle],
            120,
            &[("build", 760 * MB), (".gradle", 310 * MB)],
            "main",
            false,
            0,
        ),
        project("dotfiles", "dotfiles", &[Other], 1, &[], "main", true, 0),
    ]
}

pub fn docker() -> DockerSummary {
    DockerSummary {
        runtime: "Docker Desktop".to_string(),
        daemon_running: true,
        version: Some("27.4.0".to_string()),
        images: DockerUsage {
            count: 31,
            active: 6,
            bytes: 26 * GB,
            reclaimable: 9_300 * MB,
        },
        containers: DockerUsage {
            count: 9,
            active: 3,
            bytes: 1_200 * MB,
            reclaimable: 400 * MB,
        },
        volumes: DockerUsage {
            count: 12,
            active: 5,
            bytes: 7_800 * MB,
            reclaimable: 1_100 * MB,
        },
        build_cache: DockerUsage {
            count: 212,
            active: 0,
            bytes: 12_600 * MB,
            reclaimable: 12_600 * MB,
        },
        disk_image_path: Some(PathBuf::from(format!(
            "{HOME}/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"
        ))),
        disk_image_bytes: 58 * GB,
        disk_image_readable: true,
        disk_image_note: None,
        usage_known: true,
    }
}

pub fn ports() -> Vec<ListeningPort> {
    let port =
        |port: u16, name: &str, pid: u32, cwd: Option<&str>, hint: Option<&str>| ListeningPort {
            port,
            protocol: "TCP".to_string(),
            address: "127.0.0.1".to_string(),
            pid,
            process_name: name.to_string(),
            command: None,
            cwd: cwd.map(|path| PathBuf::from(format!("{HOME}/{path}"))),
            user: "ana".to_string(),
            service_hint: hint.map(str::to_string),
        };
    // Sorted by port, like the real scanner's output.
    let mut ports = vec![
        port(
            3000,
            "node",
            41222,
            Some("Coding/tienda-web"),
            Some("Next.js"),
        ),
        port(5173, "node", 41873, Some("Coding/blog-astro"), Some("Vite")),
        port(5432, "postgres", 702, None, Some("PostgreSQL")),
        port(6379, "redis-server", 731, None, Some("Redis")),
        port(
            8000,
            "python3.12",
            50911,
            Some("Coding/scraper-precios"),
            None,
        ),
        port(
            8080,
            "motor-audio",
            51200,
            Some("Coding/motor-audio/ejemplos/servidor-de-streaming-de-pruebas"),
            None,
        ),
        port(11434, "ollama", 812, Some(".ollama"), Some("Ollama")),
        port(1234, "LM Studio", 13012, None, Some("LM Studio")),
    ];
    ports.sort_by_key(|listener| listener.port);
    ports
}
