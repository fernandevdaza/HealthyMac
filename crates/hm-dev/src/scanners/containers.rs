//! What Docker could give back, and what it cannot be asked while it is stopped.

use anyhow::Result;
use hm_core::fmt::human_size;
use hm_core::{t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};

use super::full_disk_access_notice;
use crate::docker;
use crate::types::{DockerSummary, DockerUsage};
use crate::util;

const ID: &str = "dev.docker";

pub(super) struct DockerScanner;

impl Scanner for DockerScanner {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Docker"
    }

    fn description(&self) -> &'static str {
        t!(
            "Imágenes, contenedores detenidos, volúmenes y caché de compilación que Docker podría liberar.",
            "Images, stopped containers, volumes and build cache that Docker could free."
        )
    }

    fn area(&self) -> Area {
        Area::Developer
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let Some(summary) = docker::summary(ctx) else {
            return Ok(Vec::new());
        };
        let unused_volumes = if summary.daemon_running && summary.volumes.reclaimable > 0 {
            docker::unused_volume_names(ctx)
        } else {
            Vec::new()
        };
        Ok(util::finalize(
            ctx,
            findings_from(&summary, &unused_volumes),
        ))
    }
}

fn prune(args: &[&str]) -> Action {
    Action::Command {
        program: "docker".to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

fn reclaim_delay_note() -> &'static str {
    t!(
        "En Docker Desktop y similares, macOS puede tardar en recuperar el espacio, porque vive en una imagen de disco que se compacta poco a poco.",
        "With Docker Desktop and similar apps, macOS may take a while to get the space back, because Docker keeps it in a disk image that shrinks gradually."
    )
}

fn findings_from(summary: &DockerSummary, unused_volumes: &[String]) -> Vec<Finding> {
    let mut found = Vec::new();
    if !summary.daemon_running {
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                "daemon_stopped",
                tf!(
                    "{} no está en marcha",
                    "{} is not running",
                    summary.runtime
                ),
            )
            .explanation(tf!(
                "Sin {} en marcha no se puede saber cuánto ocupan sus imágenes, contenedores y volúmenes, ni limpiarlos. HealthyMac nunca lo arranca por su cuenta: ábrelo y vuelve a escanear para verlo.",
                "Without {} running there is no way to tell how much space its images, containers and volumes take, or to clean them. HealthyMac never starts it on its own: open it and scan again to see them.",
                summary.runtime
            )),
        );
    } else if !summary.usage_known {
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                "usage_unknown",
                t!(
                    "Docker no informó de su uso de disco",
                    "Docker didn't report its disk usage"
                ),
            )
            .explanation(tf!(
                "{} está en marcha, pero «docker system df» no dio una respuesta válida a tiempo. Vuelve a escanear más tarde.",
                "{} is running, but “docker system df” didn't give a valid answer in time. Scan again later.",
                summary.runtime
            )),
        );
    } else {
        found.extend(prune_findings(summary, unused_volumes));
    }
    found.extend(disk_image_finding(summary));
    found
}

fn inactive(usage: &DockerUsage) -> u64 {
    usage.count.saturating_sub(usage.active)
}

fn prune_findings(summary: &DockerSummary, unused_volumes: &[String]) -> Vec<Finding> {
    let mut found = Vec::new();
    let note = reclaim_delay_note();
    let images = &summary.images;
    if images.reclaimable > 0 {
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                "images",
                t!("Imágenes sin usar", "Unused images"),
            )
                .size(images.reclaimable)
                .items(inactive(images))
                .risk(Risk::Moderate)
                .recovery(Recovery::Redownload)
                .selected(false)
                .action(prune(&["image", "prune", "-a", "-f"]))
                .explanation(tf!(
                    "{} en total, {} en uso por algún contenedor. Se quitan las demás, y las que necesites se vuelven a descargar o construir. {note}",
                    "{} in total, {} in use by a container. The rest are removed, and the ones you need are downloaded or built again. {note}",
                    util::count(images.count, t!("imagen", "image"), t!("imágenes", "images")),
                    images.active
                )),
        );
    }
    let cache = &summary.build_cache;
    if cache.reclaimable > 0 {
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                "build_cache",
                t!("Caché de compilación", "Build cache"),
            )
                .size(cache.reclaimable)
                .items(inactive(cache))
                .risk(Risk::Moderate)
                .recovery(Recovery::Rebuild)
                .selected(false)
                .action(prune(&["builder", "prune", "-a", "-f"]))
                .explanation(tf!(
                    "Capas guardadas para acelerar «docker build». No se pierde nada, pero la próxima compilación de cada imagen tardará bastante más. {note}",
                    "Layers kept to speed up “docker build”. Nothing is lost, but the next build of each image will take quite a bit longer. {note}"
                )),
        );
    }
    let containers = &summary.containers;
    if containers.reclaimable > 0 {
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                "containers",
                t!("Contenedores detenidos", "Stopped containers"),
            )
                .size(containers.reclaimable)
                .items(inactive(containers))
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .selected(false)
                .action(prune(&["container", "prune", "-f"]))
                .explanation(tf!(
                    "{} detenidos. Lo que cada uno escribió fuera de un volumen se pierde al quitarlo, igual que sus registros. {note}",
                    "{} stopped. Whatever each one wrote outside a volume is lost when it is removed, and so are its logs. {note}",
                    util::count(
                        inactive(containers),
                        t!("contenedor", "container"),
                        t!("contenedores", "containers")
                    ),
                )),
        );
    }
    let volumes = &summary.volumes;
    if volumes.reclaimable > 0 {
        let mut names = unused_volumes
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if unused_volumes.len() > 8 {
            names = tf!(
                "{names} y {} más",
                "{names} and {} more",
                unused_volumes.len() - 8
            );
        }
        let which = if names.is_empty() {
            String::new()
        } else {
            tf!(" Son: {names}.", " They are: {names}.")
        };
        found.push(
            Finding::new(
                ID,
                Area::Developer,
                "volumes",
                t!("Volúmenes sin usar", "Unused volumes"),
            )
                .size(volumes.reclaimable)
                .items(inactive(volumes))
                .risk(Risk::Review)
                .recovery(Recovery::None)
                .selected(false)
                .action(prune(&["volume", "prune", "-a", "-f"]))
                .explanation(tf!(
                    "Volúmenes que ningún contenedor usa ahora.{which} Pueden guardar bases de datos y otros datos que no se pueden recuperar, aunque el proyecto esté parado solo por ahora. Revísalos antes de quitarlos.",
                    "Volumes no container is using right now.{which} They can hold databases and other data that can't be recovered, even if the project is only idle for now. Review them before removing them."
                )),
        );
    }
    found
}

fn disk_image_finding(summary: &DockerSummary) -> Option<Finding> {
    if let Some(note) = summary.disk_image_note.as_deref() {
        return Some(full_disk_access_notice(ID, "disk_image_unreadable", note));
    }
    let path = summary.disk_image_path.as_ref()?;
    Some(
        Finding::new(
            ID,
            Area::Developer,
            "disk_image",
            tf!(
                "Imagen de disco de {}: {}",
                "{} disk image: {}",
                summary.runtime,
                human_size(summary.disk_image_bytes)
            ),
        )
        .subtitle(path.display().to_string())
        .explanation(tf!(
            "Aquí viven las imágenes, los contenedores y los volúmenes, por eso no se puede borrar sin perderlos todos. Ocupa {} en disco, a menudo más de lo que informa Docker porque no siempre devuelve a macOS el espacio liberado.",
            "Images, containers and volumes live here, so it can't be deleted without losing them all. It takes up {} on disk, often more than Docker reports, because Docker doesn't always give freed space back to macOS.",
            human_size(summary.disk_image_bytes)
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn running() -> DockerSummary {
        DockerSummary {
            runtime: "Docker Desktop".to_string(),
            daemon_running: true,
            usage_known: true,
            version: Some("29.8.0".to_string()),
            images: DockerUsage {
                count: 82,
                active: 28,
                bytes: 60 * GB,
                reclaimable: 30 * GB,
            },
            containers: DockerUsage {
                count: 73,
                active: 0,
                bytes: GB,
                reclaimable: GB,
            },
            volumes: DockerUsage {
                count: 25,
                active: 15,
                bytes: 2 * GB,
                reclaimable: GB,
            },
            build_cache: DockerUsage {
                count: 509,
                active: 0,
                bytes: 46 * GB,
                reclaimable: 20 * GB,
            },
            ..DockerSummary::default()
        }
    }

    fn key<'a>(found: &'a [Finding], key: &str) -> &'a Finding {
        let id = format!("{ID}:{key}");
        found.iter().find(|finding| finding.id == id).unwrap()
    }

    #[test]
    fn nothing_docker_is_preselected_and_volumes_need_review() {
        let found = findings_from(&running(), &["pg_data".to_string()]);
        assert_eq!(found.len(), 4);
        assert!(found.iter().all(|finding| !finding.selected_by_default));
        let volumes = key(&found, "volumes");
        assert_eq!(volumes.risk, Risk::Review);
        assert_eq!(volumes.recovery, Recovery::None);
        assert!(volumes.explanation.contains("pg_data"));
        assert!(volumes.explanation.contains("bases de datos"));
    }

    #[test]
    fn prune_commands_match_what_the_reported_size_counts() {
        let found = findings_from(&running(), &[]);
        let args = |name: &str| match &key(&found, name).action {
            Action::Command { program, args } => {
                assert_eq!(program, "docker");
                args.join(" ")
            }
            other => panic!("{other:?}"),
        };
        // Without `-a`, these would only remove a fraction of what `docker system df` calls
        // reclaimable, and the user would be told it had freed far more than it did.
        assert_eq!(args("images"), "image prune -a -f");
        assert_eq!(args("build_cache"), "builder prune -a -f");
        assert_eq!(args("containers"), "container prune -f");
        assert_eq!(args("volumes"), "volume prune -a -f");
    }

    #[test]
    fn nothing_to_reclaim_means_no_finding_for_it() {
        let mut summary = running();
        summary.images.reclaimable = 0;
        summary.volumes.reclaimable = 0;
        summary.containers.reclaimable = 0;
        summary.build_cache.reclaimable = 0;
        assert!(findings_from(&summary, &[]).is_empty());
    }

    #[test]
    fn a_stopped_daemon_is_reported_not_treated_as_a_failure() {
        let summary = DockerSummary {
            runtime: "Docker Desktop".to_string(),
            ..DockerSummary::default()
        };
        let found = findings_from(&summary, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].action, Action::ReportOnly);
        assert!(found[0].title.contains("no está en marcha"));
        assert!(found[0].explanation.contains("nunca lo arranca"));
    }

    #[test]
    fn a_daemon_that_cannot_report_usage_offers_nothing() {
        let mut summary = running();
        summary.usage_known = false;
        let found = findings_from(&summary, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].action, Action::ReportOnly);
    }

    #[test]
    fn an_unreadable_disk_image_says_why_honestly() {
        let mut summary = running();
        summary.images.reclaimable = 0;
        summary.volumes.reclaimable = 0;
        summary.containers.reclaimable = 0;
        summary.build_cache.reclaimable = 0;
        summary.disk_image_note = Some("sin acceso".to_string());
        let found = findings_from(&summary, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].action, Action::ReportOnly);
        assert_eq!(found[0].explanation, "sin acceso");
    }

    #[test]
    fn a_readable_disk_image_is_shown_but_can_never_be_cleaned() {
        let mut summary = running();
        summary.images.reclaimable = 0;
        summary.volumes.reclaimable = 0;
        summary.containers.reclaimable = 0;
        summary.build_cache.reclaimable = 0;
        summary.disk_image_path = Some("/h/Docker.raw".into());
        summary.disk_image_bytes = 64 * GB;
        let found = findings_from(&summary, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].action, Action::ReportOnly);
        assert!(found[0].target_paths().is_empty());
        assert_eq!(
            found[0].size, 0,
            "not reclaimable, so it must not add to any total"
        );
    }
}
