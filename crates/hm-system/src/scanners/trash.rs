//! The user's Trash.

use anyhow::Result;
use hm_core::{t, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};

use crate::gate::{self, name_of, DirListing};

const SCANNER_ID: &str = "sys.trash";

pub(crate) struct Trash;

impl Scanner for Trash {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Papelera", "Trash")
    }
    fn description(&self) -> &'static str {
        t!(
            "Lo que ya tiraste sigue ocupando espacio hasta que vacías la Papelera.",
            "What you threw away still takes up space until you empty the Trash."
        )
    }
    fn area(&self) -> Area {
        Area::System
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        Ok(scan_trash(ctx))
    }
}

pub(crate) fn scan_trash(ctx: &ScanContext) -> Vec<Finding> {
    let dir = ctx.home_path(".Trash");
    let entries = match gate::list_dir(&dir) {
        DirListing::Missing => return Vec::new(),
        // macOS refuses `~/.Trash` without Full Disk Access. That is not the same as empty, and
        // showing "0 B" here would be a lie, so say what we do not know instead.
        DirListing::Unreadable => {
            return vec![gate::missing_access_finding(
                SCANNER_ID,
                Area::System,
                t!(
                    "macOS no deja leer la Papelera sin el Acceso total al disco, así que no se sabe \
                     cuánto ocupa: no significa que esté vacía. Con el permiso, HealthyMac puede \
                     medirla y vaciarla desde aquí.",
                    "macOS won't let HealthyMac read the Trash without Full Disk Access, so its \
                     size is unknown. That does not mean it is empty. With the permission, \
                     HealthyMac can measure it and empty it from here."
                ),
            )];
        }
        DirListing::Entries(entries) => entries,
    };
    // Finder leaves a `.DS_Store` behind, so a Trash holding only that is empty to the user.
    let items: Vec<_> = entries
        .iter()
        .filter(|path| name_of(path) != ".DS_Store")
        .collect();
    if items.is_empty() {
        return Vec::new();
    }

    let info = ctx.measure(&dir);
    let mut explanation = String::from(t!(
        "Lo que has tirado sigue ocupando espacio hasta que se vacía la Papelera. Vaciarla es \
         definitivo: después no se puede recuperar, así que revisa el contenido antes.",
        "What you threw away still takes up space until the Trash is emptied. Emptying it is \
         permanent: nothing can be recovered afterwards, so check what is in it first."
    ));
    if info.unreadable > 0 {
        explanation.push_str(t!(
            " Una parte no se pudo leer, así que puede ocupar más.",
            " Part of it could not be read, so it may take up more space."
        ));
    }
    vec![Finding::new(
        SCANNER_ID,
        Area::System,
        "papelera",
        t!("Vaciar la Papelera", "Empty the Trash"),
    )
    .subtitle(dir.display().to_string())
    .measured(&info)
    .explanation(explanation)
    .risk(Risk::Review)
    .recovery(Recovery::None)
    .selected(false)
    .action(Action::EmptyTrash)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, Fixture, LockedDir};

    #[test]
    fn missing_trash_is_silent() {
        assert!(scan_trash(&Fixture::new().ctx).is_empty());
    }

    #[test]
    fn empty_trash_is_silent_even_with_ds_store() {
        let home = Fixture::new();
        home.write(".Trash/.DS_Store", 4096);
        assert!(scan_trash(&home.ctx).is_empty());
    }

    #[test]
    fn full_trash_needs_an_explicit_choice() {
        let home = Fixture::new();
        home.write(".Trash/old.bin", 50_000);
        let findings = scan_trash(&home.ctx);
        let finding = find(&findings, "papelera").expect("finding");
        assert_eq!(finding.action, Action::EmptyTrash);
        assert_eq!(finding.risk, Risk::Review);
        assert!(!finding.selected_by_default);
        assert!(finding.size >= 50_000);
    }

    #[test]
    fn unreadable_trash_is_not_reported_as_empty() {
        let home = Fixture::new();
        let trash = home.mkdir(".Trash");
        let _guard = LockedDir::new(&trash);
        let findings = scan_trash(&home.ctx);
        assert_eq!(findings.len(), 1);
        let finding = find(&findings, "sin_acceso").expect("report");
        assert_eq!(finding.action, Action::ReportOnly);
        assert_eq!(finding.size, 0);
        assert!(finding.explanation.contains("no significa que esté vacía"));
    }
}
