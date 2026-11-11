//! Large files in the user's home.

use anyhow::Result;
use hm_core::{t, Area, Finding, Recovery, Risk, ScanContext, Scanner};

use super::walk;
use crate::gate;

const SCANNER_ID: &str = "files.large";

pub(crate) struct LargeFiles;

impl Scanner for LargeFiles {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Archivos grandes", "Large files")
    }
    fn description(&self) -> &'static str {
        t!(
            "Los archivos que más ocupan en tu carpeta de usuario.",
            "The files that take up the most space in your home folder."
        )
    }
    fn area(&self) -> Area {
        Area::Files
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        Ok(scan_large(ctx))
    }
}

pub(crate) fn scan_large(ctx: &ScanContext) -> Vec<Finding> {
    let outcome = walk::user_files(
        ctx,
        ctx.config.large_file_min_bytes,
        walk::Purpose::LargeFiles,
    );
    let mut findings: Vec<Finding> = outcome
        .files
        .iter()
        .map(|file| {
            let name = gate::name_of(&file.path);
            let group = file
                .path
                .strip_prefix(&ctx.home)
                .ok()
                .and_then(|relative| relative.components().next())
                .map(|top| top.as_os_str().to_string_lossy().into_owned());
            let shared = file.on_disk.saturating_sub(file.reclaimable);
            let mut explanation = String::from(t!(
                "Ocupa mucho, pero solo tú sabes si lo necesitas. Si ya no te hace falta, irá a la \
                 Papelera y podrás recuperarlo hasta que la vacíes.",
                "It takes up a lot of space, but only you know if you need it. If you don't, it \
                 goes to the Trash and you can get it back until you empty it."
            ));
            if shared > file.reclaimable {
                explanation.push_str(t!(
                    " Comparte casi todo su espacio con otra copia, así que quitarlo solo liberará \
                     lo que no comparte.",
                    " It shares almost all of its space with another copy, so removing it only \
                     frees what it does not share."
                ));
            }
            let mut finding = Finding::new(
                SCANNER_ID,
                Area::Files,
                file.path.display().to_string(),
                name,
            )
            .trash(&file.path)
            .size(file.reclaimable)
            .shared_bytes(shared)
            .items(1)
            .last_used(file.modified)
            .explanation(explanation)
            .risk(Risk::Review)
            .recovery(Recovery::None)
            .selected(false);
            if let Some(group) = group {
                finding = finding.group(group);
            }
            finding
        })
        .collect();
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.size));
    if outcome.skipped_gated {
        findings.push(gate::missing_access_finding(
            SCANNER_ID,
            Area::Files,
            t!(
                "Sin el Acceso total al disco no se revisan Documentos, Escritorio, Descargas ni Música, \
                 para no llenarte la pantalla de avisos de permisos. Con el permiso, HealthyMac también \
                 encontrará los archivos grandes que tengas ahí.",
                "Without Full Disk Access, Documents, Desktop, Downloads and Music are not checked, \
                 so your screen doesn't fill up with permission prompts. With the permission, \
                 HealthyMac will also find the large files you have there."
            ),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, targets, Fixture};

    fn home_with_threshold(min: u64) -> Fixture {
        let mut home = Fixture::new();
        home.ctx.config.large_file_min_bytes = min;
        home
    }

    #[test]
    fn empty_home_is_silent() {
        assert!(scan_large(&home_with_threshold(50_000).ctx).is_empty());
    }

    #[test]
    fn honours_the_configured_threshold_and_is_never_selected() {
        let home = home_with_threshold(50_000);
        let big = home.write("Movies/big.mov", 100_000);
        home.write("Movies/small.mov", 1_000);
        let findings = scan_large(&home.ctx);
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.risk, Risk::Review);
        assert!(!finding.selected_by_default);
        assert_eq!(targets(finding), [big]);
        assert_eq!(finding.group.as_deref(), Some("Movies"));
    }

    #[test]
    fn a_clone_is_listed_but_says_it_would_free_little() {
        let home = home_with_threshold(50_000);
        let original = home.write("Movies/original.mov", 100_000);
        let clone = home.path("Movies/copy.mov");
        std::fs::copy(&original, &clone).expect("copy");
        if hm_core::fsutil::clone_id(&original).is_none()
            || hm_core::fsutil::clone_id(&original) != hm_core::fsutil::clone_id(&clone)
        {
            return;
        }
        let findings = scan_large(&home.ctx);
        assert_eq!(findings.len(), 2);
        for finding in &findings {
            assert!(finding.size < finding.shared_bytes, "{}", finding.size);
            assert!(finding.explanation.contains("Comparte"));
        }
    }

    #[test]
    fn gated_folders_are_reported_once_without_access() {
        let home = home_with_threshold(50_000);
        home.write("Documents/a.bin", 100_000);
        home.write("Downloads/b.bin", 100_000);
        let findings = scan_large(&home.ctx);
        assert_eq!(findings.len(), 1);
        let finding = find(&findings, "sin_acceso").expect("report");
        assert_eq!(finding.action, hm_core::Action::ReportOnly);
        assert!(finding.tags.iter().any(|tag| tag == gate::NEEDS_ACCESS_TAG));
    }

    #[test]
    fn gated_folders_are_scanned_with_access() {
        let home = home_with_threshold(50_000);
        home.grant_full_disk_access();
        let path = home.write("Documents/a.bin", 100_000);
        let findings = scan_large(&home.ctx);
        assert_eq!(findings.len(), 1);
        assert_eq!(targets(&findings[0]), [path]);
    }
}
