//! System errors, in words a person would use.

use hm_core::t;

/// Replaces the raw OS wording at the end of an error with plain words in the current language,
/// leaving the rest of the message (usually which file it was) intact.
pub fn friendly_error(error: &str) -> String {
    let wordings: [(&str, &str); 5] = [
        (
            "No such file or directory (os error 2)",
            t!("ya no existe", "no longer exists"),
        ),
        (
            "Permission denied (os error 13)",
            t!("macOS no nos deja leerlo", "macOS won't let us read it"),
        ),
        (
            "Operation not permitted (os error 1)",
            t!(
                "macOS no lo permite: puede que falte el Acceso total al disco",
                "macOS doesn't allow it: Full Disk Access may be missing"
            ),
        ),
        (
            "No space left on device (os error 28)",
            t!("no queda espacio en el disco", "the disk is out of space"),
        ),
        (
            "Device not configured (os error 6)",
            t!(
                "el disco ya no está conectado",
                "the disk is no longer connected"
            ),
        ),
    ];
    let mut out = error.to_string();
    for (raw, plain) in wordings {
        out = out.replace(raw, plain);
    }
    out
}

/// Where macOS keeps the "App Management" list.
pub const APP_MANAGEMENT_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AppBundles";

/// Whether a failed uninstall was stopped by macOS's App Management protection. There is no way to
/// ask ahead of time without raising a permission prompt, so it can only be recognised afterwards,
/// by the wording of the failure.
pub fn blocked_by_app_management(finding_id: &str, error: &str) -> bool {
    if !finding_id.starts_with("apps.uninstall:") {
        return false;
    }
    let error = error.to_lowercase();
    [
        "not permitted",
        "permission",
        "permiso",
        "os error 1)",
        "os error 13",
        "code=513",
        "code: 513",
    ]
    .iter()
    .any(|sign| error.contains(sign))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_errors_become_plain_words() {
        assert_eq!(
            friendly_error("no se pudo leer /x: No such file or directory (os error 2)"),
            "no se pudo leer /x: ya no existe"
        );
        assert_eq!(friendly_error("algo inesperado"), "algo inesperado");
    }

    #[test]
    fn app_management_is_recognised_only_on_uninstalls() {
        let refused = "/Applications/Foo.app: Operation not permitted";
        assert!(blocked_by_app_management("apps.uninstall:com.foo", refused));
        assert!(blocked_by_app_management(
            "apps.uninstall:com.foo",
            "“Foo” couldn’t be moved to the Trash because you don’t have permission to access it."
        ));
        assert!(!blocked_by_app_management("sys.caches:foo", refused));
        assert!(!blocked_by_app_management(
            "apps.uninstall:com.foo",
            "disco lleno"
        ));
    }
}
