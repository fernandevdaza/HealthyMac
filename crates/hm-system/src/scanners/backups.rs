//! iOS device backups made by the Mac.

use std::path::Path;
use std::time::SystemTime;

use anyhow::Result;
use hm_core::{fsutil, t, tf, Area, Finding, Recovery, Risk, ScanContext, Scanner};
use plist::Value;
use rayon::prelude::*;

use crate::gate::{self, name_of};

const SCANNER_ID: &str = "files.ios_backups";
const BACKUP_ROOT: &str = "Library/Application Support/MobileSync/Backup";

pub(crate) struct IosBackups;

impl Scanner for IosBackups {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Copias de seguridad de iOS", "iOS backups")
    }
    fn description(&self) -> &'static str {
        t!(
            "Copias de tu iPhone o iPad guardadas en este Mac.",
            "Backups of your iPhone or iPad stored on this Mac."
        )
    }
    fn area(&self) -> Area {
        Area::Files
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        Ok(scan_backups(ctx))
    }
}

pub(crate) fn scan_backups(ctx: &ScanContext) -> Vec<Finding> {
    let root = ctx.home_path(BACKUP_ROOT);
    if !gate::has_access(ctx) {
        // The parent can be checked without touching the protected folder. Without it there are no
        // backups, so there is nothing to gain from asking.
        let sync_folder = root.parent().is_some_and(fsutil::is_real_dir);
        return if sync_folder {
            vec![gate::missing_access_finding(
                SCANNER_ID,
                Area::Files,
                t!(
                    "macOS no deja leer las copias de seguridad de iPhone y iPad sin el Acceso total al \
                     disco, y pueden ocupar decenas de GB. Con el permiso, HealthyMac mostrará cuánto \
                     ocupa cada una y de qué dispositivo es.",
                    "macOS won't let HealthyMac read iPhone and iPad backups without Full Disk \
                     Access, and they can take up tens of GB. With the permission, HealthyMac will \
                     show how much each one takes and which device it is from."
                ),
            )]
        } else {
            Vec::new()
        };
    }
    let mut findings: Vec<Finding> = fsutil::children(&root)
        .par_iter()
        .filter(|path| fsutil::is_real_dir(path) && !gate::is_excluded(ctx, path))
        .filter_map(|path| backup_finding(ctx, path))
        .collect();
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.size));
    findings
}

#[derive(Debug, Default, PartialEq)]
struct BackupInfo {
    device: Option<String>,
    last_backup: Option<SystemTime>,
}

/// A backup with a missing or broken `Info.plist` is still taking up space, so it stays listed.
fn read_info(backup: &Path) -> BackupInfo {
    let Ok(value) = Value::from_file(backup.join("Info.plist")) else {
        return BackupInfo::default();
    };
    let Some(dict) = value.as_dictionary() else {
        return BackupInfo::default();
    };
    BackupInfo {
        device: ["Display Name", "Device Name"]
            .iter()
            .find_map(|key| dict.get(key).and_then(Value::as_string))
            .map(str::to_string),
        last_backup: dict
            .get("Last Backup Date")
            .and_then(Value::as_date)
            .map(SystemTime::from),
    }
}

fn backup_finding(ctx: &ScanContext, path: &Path) -> Option<Finding> {
    let info = ctx.measure(path);
    if info.bytes == 0 {
        return None;
    }
    let details = read_info(path);
    let id = name_of(path);
    let title = match &details.device {
        Some(device) => tf!("Copia de {device}", "Backup of {device}"),
        None => t!("Copia de un dispositivo iOS", "Backup of an iOS device").to_string(),
    };
    Some(
        Finding::new(SCANNER_ID, Area::Files, &id, title)
            .trash(path)
            .measured(&info)
            .last_used(details.last_backup.or(info.newest_modified))
            .explanation(t!(
                "Copia de seguridad completa de un iPhone o iPad, con sus fotos, mensajes y datos \
                 de apps. Puede ser la única copia de todo eso si no usas Copia en iCloud, así que \
                 asegúrate antes de quitarla.",
                "A full backup of an iPhone or iPad, with its photos, messages and app data. It \
                 may be the only copy of all that if you don't use iCloud Backup, so make sure \
                 before you remove it."
            ))
            .risk(Risk::Review)
            .recovery(Recovery::None)
            .selected(false),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, targets, Fixture};

    const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Device Name</key><string>iPhone de Ana</string>
<key>Last Backup Date</key><date>2026-01-15T10:00:00Z</date>
</dict></plist>"#;

    #[test]
    fn gated_without_access_when_the_sync_folder_exists() {
        let home = Fixture::new();
        home.write(&format!("{BACKUP_ROOT}/abc/Manifest.db"), 10_000);
        let findings = scan_backups(&home.ctx);
        assert_eq!(findings.len(), 1);
        assert!(find(&findings, "sin_acceso").is_some());
    }

    #[test]
    fn no_sync_folder_means_nothing_to_ask_for() {
        assert!(scan_backups(&Fixture::new().ctx).is_empty());
    }

    #[test]
    fn lists_each_backup_with_its_device_and_date() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        home.write(&format!("{BACKUP_ROOT}/abc/Manifest.db"), 10_000);
        home.write_text(&format!("{BACKUP_ROOT}/abc/Info.plist"), INFO);
        let findings = scan_backups(&home.ctx);
        let finding = find(&findings, "abc").expect("finding");
        assert_eq!(finding.title, "Copia de iPhone de Ana");
        assert_eq!(finding.risk, Risk::Review);
        assert!(!finding.selected_by_default);
        assert_eq!(targets(finding), [home.path(&format!("{BACKUP_ROOT}/abc"))]);
        let expected = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_768_471_200);
        assert_eq!(finding.last_used, Some(expected));
    }

    #[test]
    fn malformed_info_plist_still_lists_the_backup() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        home.write(&format!("{BACKUP_ROOT}/abc/Manifest.db"), 10_000);
        home.write_text(&format!("{BACKUP_ROOT}/abc/Info.plist"), "<plist><broken");
        let findings = scan_backups(&home.ctx);
        let finding = find(&findings, "abc").expect("finding");
        assert_eq!(finding.title, "Copia de un dispositivo iOS");
        assert!(finding.size > 0);
    }

    #[test]
    fn missing_backup_folder_with_access_is_silent() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        assert!(scan_backups(&home.ctx).is_empty());
    }
}
