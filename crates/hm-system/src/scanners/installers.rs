//! Installers left in Downloads, and iOS update files.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use hm_core::fmt::{age_days, human_age};
use hm_core::{fsutil, t, tf, Area, Finding, Recovery, Risk, ScanContext, Scanner};

use crate::gate::{self, name_of};

const SCANNER_ID: &str = "files.installers";
/// A recent installer may well be about to be used. Past this it is just taking up room.
const MIN_AGE_DAYS: u64 = 14;
const MAX_DEPTH: usize = 3;
const IOS_UPDATE_FOLDERS: &[&str] = &[
    "Library/iTunes/iPhone Software Updates",
    "Library/iTunes/iPad Software Updates",
    "Library/iTunes/iPod Software Updates",
];

pub(crate) struct Installers;

impl Scanner for Installers {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Instaladores antiguos", "Old installers")
    }
    fn description(&self) -> &'static str {
        t!(
            "Imágenes de disco e instaladores viejos, y actualizaciones de iOS descargadas.",
            "Old disk images and installers, and downloaded iOS updates."
        )
    }
    fn area(&self) -> Area {
        Area::Files
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        Ok(scan_installers(ctx))
    }
}

/// What the extension says about what the file could be.
fn risk_for(extension: &str) -> Option<Risk> {
    match extension {
        "pkg" | "xip" | "ipsw" => Some(Risk::Moderate),
        // Plenty of people keep their own encrypted disk images, so these need a look.
        "dmg" | "iso" => Some(Risk::Review),
        _ => None,
    }
}

pub(crate) fn scan_installers(ctx: &ScanContext) -> Vec<Finding> {
    let downloads = ctx.home_path("Downloads");
    let mut roots = Vec::new();
    let mut gated = false;
    if gate::has_access(ctx) {
        roots.push(downloads.clone());
    } else {
        gated = fsutil::is_real_dir(&downloads);
        roots = gate::configured_roots_inside(ctx, &downloads);
    }

    let mut found = Vec::new();
    for root in &roots {
        collect(ctx, root, MAX_DEPTH, &mut found);
    }
    let mut findings: Vec<Finding> = found
        .iter()
        .filter_map(|path| installer_finding(ctx, path))
        .collect();
    findings.extend(ios_updates(ctx));
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.size));

    if gated {
        findings.push(gate::missing_access_finding(
            SCANNER_ID,
            Area::Files,
            t!(
                "Sin el Acceso total al disco no se revisa la carpeta Descargas, que es donde suelen \
                 quedarse los instaladores. Con el permiso, HealthyMac encontrará los .dmg, .pkg e \
                 imágenes de sistema que ya no uses.",
                "Without Full Disk Access, the Downloads folder is not checked, and that is where \
                 installers usually end up. With the permission, HealthyMac will find the .dmg and \
                 .pkg files and system images you no longer use."
            ),
        ));
    }
    findings
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn collect(ctx: &ScanContext, dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if ctx.is_cancelled() {
        return;
    }
    for path in fsutil::children(dir) {
        if name_of(&path).starts_with('.')
            || gate::is_symlink(&path)
            || gate::is_excluded(ctx, &path)
        {
            continue;
        }
        let extension = extension_of(&path);
        // A `.pkg` can be a folder. Either way it is one installer, not somewhere to look inside.
        if risk_for(&extension).is_some() {
            found.push(path);
        } else if depth > 0 && extension != "app" && fsutil::is_real_dir(&path) {
            collect(ctx, &path, depth - 1, found);
        }
    }
}

/// The later of creation and modification. Downloads can keep the server's old modification date,
/// which would make a file fetched a minute ago look ancient.
fn arrived_at(path: &Path) -> Option<SystemTime> {
    let meta = fs::symlink_metadata(path).ok()?;
    meta.modified().ok().max(meta.created().ok())
}

fn installer_finding(ctx: &ScanContext, path: &Path) -> Option<Finding> {
    let arrived = arrived_at(path)?;
    if age_days(arrived, ctx.now)? < MIN_AGE_DAYS {
        return None;
    }
    let extension = extension_of(path);
    let risk = risk_for(&extension)?;
    let info = ctx.measure(path);
    if info.bytes == 0 {
        return None;
    }
    let mut explanation = tf!(
        "Instalador descargado {}. Una vez instalada la app ya no hace falta, y se puede volver a \
         descargar.",
        "Installer downloaded {}. Once the app is installed it is no longer needed, and it can be \
         downloaded again.",
        human_age(arrived, ctx.now)
    );
    if risk == Risk::Review {
        explanation.push_str(t!(
            " Si es una imagen de disco tuya, con tus propios archivos dentro, consérvala: esa no \
             se puede volver a descargar.",
            " If it is a disk image of your own, with your own files inside, keep it: that one \
             cannot be downloaded again."
        ));
    }
    Some(
        Finding::new(
            SCANNER_ID,
            Area::Files,
            path.display().to_string(),
            name_of(path),
        )
        .trash(path)
        .measured(&info)
        .last_used(Some(arrived))
        .explanation(explanation)
        .risk(risk)
        .recovery(Recovery::Redownload)
        .selected(false),
    )
}

fn ios_updates(ctx: &ScanContext) -> Vec<Finding> {
    // macOS guards `~/Library/iTunes`: merely listing it puts a Media Library dialog on screen.
    if !gate::has_access(ctx) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for folder in IOS_UPDATE_FOLDERS {
        for path in fsutil::children(&ctx.home_path(folder)) {
            if extension_of(&path) != "ipsw"
                || gate::is_symlink(&path)
                || gate::is_excluded(ctx, &path)
            {
                continue;
            }
            let info = ctx.measure(&path);
            if info.bytes == 0 {
                continue;
            }
            findings.push(
                Finding::new(
                    SCANNER_ID,
                    Area::Files,
                    path.display().to_string(),
                    tf!(
                        "Actualización de iOS: {}",
                        "iOS update: {}",
                        name_of(&path)
                    ),
                )
                .trash(&path)
                .measured(&info)
                .explanation(t!(
                    "Copia de una actualización de iOS o iPadOS que el Mac descargó para instalarla \
                     en un dispositivo. Si la necesitas otra vez, se descarga de nuevo.",
                    "A copy of an iOS or iPadOS update the Mac downloaded to install on a device. \
                     If you need it again, it downloads again."
                ))
                .risk(Risk::Moderate)
                .recovery(Recovery::Redownload)
                .selected(false),
            );
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, Fixture};
    use std::time::Duration;

    fn aged(mut home: Fixture) -> Fixture {
        home.ctx.now = SystemTime::now() + Duration::from_secs(30 * 86_400);
        home
    }

    #[test]
    fn downloads_are_gated_without_access() {
        let home = aged(Fixture::new());
        home.write("Downloads/old.dmg", 10_000);
        let findings = scan_installers(&home.ctx);
        assert_eq!(findings.len(), 1);
        assert!(find(&findings, "sin_acceso").is_some());
    }

    #[test]
    fn no_downloads_folder_means_nothing_to_gain() {
        assert!(scan_installers(&Fixture::new().ctx).is_empty());
    }

    #[test]
    fn finds_old_installers_with_access() {
        let home = aged(Fixture::new());
        home.grant_full_disk_access();
        home.write("Downloads/a.pkg", 10_000);
        home.write("Downloads/sub/b.xip", 10_000);
        home.write("Downloads/c.DMG", 10_000);
        home.write("Downloads/disk.iso", 10_000);
        home.write("Downloads/notes.txt", 10_000);
        home.write("Downloads/Thing.app/inner.dmg", 10_000);
        let findings = scan_installers(&home.ctx);
        assert_eq!(findings.len(), 4);
        assert!(findings.iter().all(|finding| !finding.selected_by_default));
    }

    #[test]
    fn disk_images_need_review_but_packages_do_not() {
        let home = aged(Fixture::new());
        home.grant_full_disk_access();
        let dmg = home.write("Downloads/a.dmg", 10_000);
        let pkg = home.write("Downloads/b.pkg", 10_000);
        let findings = scan_installers(&home.ctx);
        let risk_of = |path: &Path| {
            findings
                .iter()
                .find(|finding| finding.subtitle.as_deref() == Some(&path.display().to_string()))
                .map(|finding| finding.risk)
        };
        assert_eq!(risk_of(&dmg), Some(Risk::Review));
        assert_eq!(risk_of(&pkg), Some(Risk::Moderate));
    }

    #[test]
    fn a_recent_installer_is_left_alone() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        home.write("Downloads/new.dmg", 10_000);
        assert!(scan_installers(&home.ctx).is_empty());
    }

    #[test]
    fn a_pkg_folder_counts_as_one_installer() {
        let home = aged(Fixture::new());
        home.grant_full_disk_access();
        home.write("Downloads/Suite.pkg/Contents/payload", 10_000);
        home.write("Downloads/Suite.pkg/Contents/other.dmg", 10_000);
        let findings = scan_installers(&home.ctx);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].size >= 20_000);
    }

    #[test]
    fn ios_updates_are_not_even_looked_for_without_access() {
        let home = Fixture::new();
        home.write(
            "Library/iTunes/iPhone Software Updates/iPhone_18.ipsw",
            10_000,
        );
        assert!(scan_installers(&home.ctx).is_empty());
    }

    #[test]
    fn ios_updates_are_found_at_any_age_with_access() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        home.write(
            "Library/iTunes/iPhone Software Updates/iPhone_18.ipsw",
            10_000,
        );
        home.write("Library/iTunes/iPhone Software Updates/notes.txt", 10_000);
        let findings = scan_installers(&home.ctx);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].recovery, Recovery::Redownload);
        assert!(!findings[0].selected_by_default);
    }
}
