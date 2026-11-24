//! Updates from the project's GitHub releases: is there a newer version, and swapping this app for it.
//!
//! The app is signed ad hoc, not notarized, so there is no Developer ID to check a download against.
//! What is checked instead: it comes over HTTPS from the project's releases, its SHA-256 matches the
//! digest GitHub publishes for the asset, its code signature is valid, and it is HealthyMac at the
//! version the release names. Downloads go through curl, which ships with macOS and, unlike a browser,
//! leaves no quarantine flag behind, so Gatekeeper does not block the new version.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use hm_core::exec::{run, CmdOutput};
use hm_core::{t, tf, CancelToken};
use serde::Deserialize;

pub const REPOSITORY: &str = "fernandevdaza/HealthyMac";
const BUNDLE_ID: &str = "dev.healthymac.app";
const CURL: &str = "/usr/bin/curl";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// `0.2.0` or `v0.2.0`. Anything else, a pre-release such as `0.3.0-beta` included, is `None`:
    /// only finished versions are offered.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let version = Self {
            major: parts.next()?.parse().ok()?,
            minor: parts.next()?.parse().ok()?,
            patch: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The version this binary was built as.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is a plain x.y.z")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// The release notes, as written on GitHub (Markdown).
    pub notes: String,
    /// The release's page, for reading the notes or downloading by hand.
    pub page: String,
    /// The disk image for this Mac, if the release has one.
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub url: String,
    pub size: u64,
    /// Lowercase hex. GitHub publishes it for every asset uploaded since mid-2025.
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// The disk image `scripts/package.sh` builds for this kind of Mac.
pub fn asset_name() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    format!("HealthyMac-mac-{arch}.dmg")
}

/// Reads one release as GitHub's API describes it.
pub fn parse_release(json: &[u8], asset_name: &str) -> Result<Release> {
    let api: ApiRelease = serde_json::from_slice(json).context(t!(
        "GitHub ha respondido algo que no es una versión",
        "GitHub answered with something that isn't a release"
    ))?;
    if api.draft || api.prerelease {
        bail!(t!(
            "la última versión publicada es un borrador o una versión de prueba",
            "the latest published release is a draft or a pre-release"
        ));
    }
    let version = Version::parse(&api.tag_name).with_context(|| {
        tf!(
            "no reconocemos la versión «{}»",
            "we don't recognize the version \"{}\"",
            api.tag_name
        )
    })?;
    let asset = api
        .assets
        .into_iter()
        .find(|asset| asset.name == asset_name)
        .map(|asset| Asset {
            url: asset.browser_download_url,
            size: asset.size,
            sha256: asset
                .digest
                .and_then(|digest| digest.strip_prefix("sha256:").map(str::to_lowercase)),
        });
    Ok(Release {
        version,
        notes: api.body.unwrap_or_default().trim().to_string(),
        page: api.html_url,
        asset,
    })
}

pub enum Check {
    UpToDate,
    Available(Release),
}

/// Where the latest release is described. `HEALTHYMAC_UPDATE_URL` points it elsewhere, so the whole
/// flow can be tested against a local server.
fn latest_url() -> String {
    std::env::var("HEALTHYMAC_UPDATE_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"))
}

/// Asks GitHub for the latest release. Blocks for up to about 20 seconds.
pub fn check() -> Result<Check> {
    let user_agent = format!("User-Agent: HealthyMac/{}", current_version());
    let url = latest_url();
    let output = run(
        Path::new(CURL),
        &[
            "-fsSL",
            "--max-time",
            "20",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            user_agent.as_str(),
            url.as_str(),
        ],
        Duration::from_secs(25),
    )?;
    ensure_success(
        &output,
        t!(
            "No hemos podido preguntar a GitHub",
            "We couldn't reach GitHub"
        ),
    )?;
    let release = parse_release(output.stdout.as_bytes(), &asset_name())?;
    Ok(if release.version > current_version() {
        Check::Available(release)
    } else {
        Check::UpToDate
    })
}

fn ensure_success(output: &CmdOutput, what: &str) -> Result<()> {
    if output.success() {
        return Ok(());
    }
    let reason = output.stderr.trim();
    if output.timed_out {
        bail!(tf!("{what}: tardaba demasiado", "{what}: it took too long"));
    }
    if reason.is_empty() {
        bail!("{what}");
    }
    bail!("{what}: {reason}")
}

/// A new version, copied out of its disk image and checked, waiting to take this one's place.
pub struct Staged {
    pub app: PathBuf,
    work: PathBuf,
}

/// How far an installation has got, written by the thread doing it and read by the interface.
#[derive(Debug, Default)]
pub struct Progress {
    downloaded: AtomicU64,
    verifying: AtomicBool,
}

impl Progress {
    /// Bytes of the disk image downloaded so far.
    pub fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Relaxed)
    }

    /// The download is complete and the new version is being checked and unpacked.
    pub fn verifying(&self) -> bool {
        self.verifying.load(Ordering::Relaxed)
    }
}

/// Downloads the release's disk image, checks it, and copies the app out of it. Blocks.
pub fn download_and_stage(release: &Release, progress: &Progress) -> Result<Staged> {
    let asset = release.asset.as_ref().context(t!(
        "Esta versión no trae un instalador para este Mac",
        "This release has no installer for this Mac"
    ))?;
    let work = std::env::temp_dir().join(format!("healthymac-update-{}", release.version));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;

    let image = work.join("HealthyMac.dmg");
    let image_arg = image.to_string_lossy().into_owned();
    download(asset, &image, progress)?;
    progress.verifying.store(true, Ordering::Relaxed);
    verify_download(&image, asset)?;

    let mount = work.join("mount");
    std::fs::create_dir_all(&mount)?;
    let mount_arg = mount.to_string_lossy().into_owned();
    let attached = run(
        Path::new("/usr/bin/hdiutil"),
        &[
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-quiet",
            "-mountpoint",
            mount_arg.as_str(),
            image_arg.as_str(),
        ],
        Duration::from_secs(60),
    )?;
    ensure_success(
        &attached,
        t!(
            "No se ha podido abrir el instalador",
            "The installer couldn't be opened"
        ),
    )?;
    let app = work.join("HealthyMac.app");
    let source = mount.join("HealthyMac.app").to_string_lossy().into_owned();
    let destination = app.to_string_lossy().into_owned();
    let copied = run(
        Path::new("/usr/bin/ditto"),
        &[source.as_str(), destination.as_str()],
        Duration::from_secs(120),
    );
    detach(&mount);
    ensure_success(
        &copied?,
        t!(
            "No se ha podido copiar la app del instalador",
            "The app couldn't be copied out of the installer"
        ),
    )?;

    verify_app(&app, release.version)?;
    Ok(Staged { app, work })
}

fn detach(mount: &Path) {
    let mount = mount.to_string_lossy().into_owned();
    let detached = run(
        Path::new("/usr/bin/hdiutil"),
        &["detach", "-quiet", mount.as_str()],
        Duration::from_secs(30),
    );
    if !detached.is_ok_and(|output| output.success()) {
        let _ = run(
            Path::new("/usr/bin/hdiutil"),
            &["detach", "-quiet", "-force", mount.as_str()],
            Duration::from_secs(30),
        );
    }
}

/// curl writes the file as it arrives, so its size on disk is how far the download has got.
fn download(asset: &Asset, destination: &Path, progress: &Progress) -> Result<()> {
    const LIMIT: Duration = Duration::from_secs(15 * 60);
    let what = t!("No se ha podido descargar", "The download failed");
    let mut child = Command::new(CURL)
        .args(["-fsSL", "--max-time", "900", "-o"])
        .arg(destination)
        .arg(&asset.url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context(what)?;
    let size_now = || std::fs::metadata(destination).map_or(0, |meta| meta.len());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            bail!(tf!("{what}: tardaba demasiado", "{what}: it took too long"));
        }
        progress.downloaded.store(size_now(), Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
    };
    progress.downloaded.store(size_now(), Ordering::Relaxed);
    if status.success() {
        return Ok(());
    }
    let mut reason = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut reason);
    }
    match reason.trim() {
        "" => bail!("{what}"),
        reason => bail!("{what}: {reason}"),
    }
}

fn verify_download(path: &Path, asset: &Asset) -> Result<()> {
    let size = std::fs::metadata(path)?.len();
    if size != asset.size {
        bail!(tf!(
            "La descarga llegó incompleta: {} de {} bytes",
            "The download came in incomplete: {} of {} bytes",
            size,
            asset.size
        ));
    }
    if let Some(expected) = &asset.sha256 {
        let actual = hm_core::fsutil::sha256_file(path, &CancelToken::new())?;
        if actual != *expected {
            bail!(t!(
                "La descarga no coincide con la que publicó GitHub, así que no la instalamos",
                "The download doesn't match the one GitHub published, so we won't install it"
            ));
        }
    }
    Ok(())
}

/// The new app must be HealthyMac, at the version the release names, with a valid signature.
fn verify_app(app: &Path, version: Version) -> Result<()> {
    let plist = app.join("Contents/Info.plist");
    let identifier = plist_value(&plist, "CFBundleIdentifier")?;
    if identifier != BUNDLE_ID {
        bail!(tf!(
            "El instalador no contiene HealthyMac, sino «{identifier}»",
            "The installer doesn't contain HealthyMac but \"{identifier}\""
        ));
    }
    let found = plist_value(&plist, "CFBundleShortVersionString")?;
    if Version::parse(&found) != Some(version) {
        bail!(tf!(
            "El instalador trae la versión {found}, no la {version}",
            "The installer has version {found}, not {version}"
        ));
    }
    let app = app.to_string_lossy().into_owned();
    let signature = run(
        Path::new("/usr/bin/codesign"),
        &["--verify", "--deep", "--strict", app.as_str()],
        Duration::from_secs(60),
    )?;
    ensure_success(
        &signature,
        t!(
            "La firma de la app nueva no es válida",
            "The new app's signature isn't valid"
        ),
    )
}

fn plist_value(plist: &Path, key: &str) -> Result<String> {
    let plist = plist.to_string_lossy().into_owned();
    let output = run(
        Path::new("/usr/bin/plutil"),
        &["-extract", key, "raw", "-o", "-", plist.as_str()],
        Duration::from_secs(10),
    )?;
    ensure_success(
        &output,
        t!(
            "El instalador no parece una app",
            "The installer doesn't look like an app"
        ),
    )?;
    Ok(output.stdout.trim().to_string())
}

/// Whether this copy can replace itself, and if not, why.
pub enum InstallTarget {
    Ready(PathBuf),
    /// Running from `cargo run` or a bare binary: there is no `.app` to replace.
    NotBundled,
    /// The `.app` or the folder it lives in can't be written, such as `/Applications` for a
    /// standard user.
    NotWritable(PathBuf),
}

pub fn install_target() -> InstallTarget {
    let Some(bundle) = hm_core::safety::running_program().and_then(|exe| bundle_of(&exe)) else {
        return InstallTarget::NotBundled;
    };
    let folder = bundle.parent().map(Path::to_path_buf).unwrap_or_default();
    if writable(&folder) && writable(&bundle) {
        InstallTarget::Ready(bundle)
    } else {
        InstallTarget::NotWritable(folder)
    }
}

/// `X.app` for an executable at `X.app/Contents/MacOS/<name>`.
fn bundle_of(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Waits for the app to quit, puts the new version in its place and opens it. The old one is kept
/// aside until the copy has worked, and goes back if it didn't. Arguments: the process to wait for,
/// the app to replace, the new app, the folder to clean up, and 1 to open the result.
const SWAP_SCRIPT: &str = r#"
pid=$1 target=$2 staged=$3 work=$4 relaunch=$5
waited=0
while kill -0 "$pid" 2>/dev/null; do
    waited=$((waited + 1))
    [ "$waited" -gt 600 ] && exit 1
    sleep 0.1
done
backup="$work/previous.app"
rm -rf "$backup"
if mv "$target" "$backup"; then
    if /usr/bin/ditto "$staged" "$target"; then
        rm -rf "$backup"
    else
        rm -rf "$target"
        mv "$backup" "$target"
    fi
fi
[ "$relaunch" = 1 ] && /usr/bin/open "$target"
[ -d "$target" ] && rm -rf "$work"
"#;

/// Hands the swap to a small script that outlives this process. Call [`gpui::App::quit`] right
/// after: the script starts once the app is gone.
pub fn replace_after_exit(staged: &Staged, target: &Path) -> Result<()> {
    spawn_swap(std::process::id(), target, staged, true)?;
    Ok(())
}

fn spawn_swap(pid: u32, target: &Path, staged: &Staged, relaunch: bool) -> Result<Child> {
    use std::os::unix::process::CommandExt;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(SWAP_SCRIPT)
        .arg("healthymac-update")
        .arg(pid.to_string())
        .arg(target)
        .arg(&staged.app)
        .arg(&staged.work)
        .arg(if relaunch { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so nothing aimed at the app reaches it.
        .process_group(0)
        .spawn()
        .context(t!(
            "No se ha podido preparar la instalación",
            "The installation couldn't be set up"
        ))
}

/// The first paragraph of the release notes, without Markdown headings or list markers, for a
/// one-glance summary. The full notes are on the release page.
pub fn summary(notes: &str, max_chars: usize) -> String {
    let paragraph: Vec<&str> = notes
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty() || line.starts_with('#'))
        .take_while(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.trim_start_matches(['-', '*', '>']).trim())
        .collect();
    let text = paragraph.join(" ").replace("**", "");
    if text.chars().count() <= max_chars {
        return text;
    }
    let cut: String = text.chars().take(max_chars).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", cut.trim_end_matches([',', '.', ';', ':']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn versions_compare_by_number_and_reject_pre_releases() {
        let v = |text| Version::parse(text).unwrap();
        assert!(v("v0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("v0.2.0"), v("0.2.0"));
        assert_eq!(v("v0.2.0").to_string(), "0.2.0");
        for bad in ["0.3.0-beta", "0.3", "0.3.0.1", "latest", ""] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        // The version in Cargo.toml has to be one the updater can compare.
        let _ = current_version();
    }

    const RELEASE: &str = r#"{
        "tag_name": "v0.3.0",
        "html_url": "https://github.com/fernandevdaza/HealthyMac/releases/tag/v0.3.0",
        "body": "Faster scans.\n",
        "draft": false,
        "prerelease": false,
        "assets": [
            {"name": "HealthyMac-mac-x86_64.dmg", "size": 10, "digest": null,
             "browser_download_url": "https://example.invalid/intel.dmg"},
            {"name": "HealthyMac-mac-arm64.dmg", "size": 6957767,
             "digest": "sha256:3C277FAA540B994385EF58EBF54FC5AD5C17E367F2437054EBD19924AB01DAFF",
             "browser_download_url": "https://example.invalid/arm.dmg"}
        ]
    }"#;

    #[test]
    fn reads_a_release_and_picks_the_image_for_this_mac() {
        let release = parse_release(RELEASE.as_bytes(), "HealthyMac-mac-arm64.dmg").unwrap();
        assert_eq!(release.version, Version::parse("0.3.0").unwrap());
        assert_eq!(release.notes, "Faster scans.");
        let asset = release.asset.unwrap();
        assert_eq!(asset.url, "https://example.invalid/arm.dmg");
        assert_eq!(asset.size, 6957767);
        assert_eq!(
            asset.sha256.as_deref(),
            Some("3c277faa540b994385ef58ebf54fc5ad5c17e367f2437054ebd19924ab01daff")
        );

        let other = parse_release(RELEASE.as_bytes(), "HealthyMac-mac-riscv.dmg").unwrap();
        assert_eq!(
            other.asset, None,
            "a release without our image is shown, not installed"
        );
    }

    #[test]
    fn drafts_pre_releases_and_odd_tags_are_not_offered() {
        let draft = RELEASE.replace(r#""draft": false"#, r#""draft": true"#);
        assert!(parse_release(draft.as_bytes(), "x").is_err());
        let beta = RELEASE.replace("v0.3.0\"", "v0.3.0-beta\"");
        assert!(parse_release(beta.as_bytes(), "x").is_err());
        assert!(parse_release(b"{\"message\": \"Not Found\"}", "x").is_err());
    }

    #[test]
    fn finds_the_bundle_only_for_an_executable_inside_one() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/HealthyMac.app/Contents/MacOS/HealthyMac"
            )),
            Some(PathBuf::from("/Applications/HealthyMac.app"))
        );
        assert_eq!(
            bundle_of(Path::new(
                "/Users/ana/HealthyMac/target/debug/healthymac-app"
            )),
            None
        );
        assert_eq!(bundle_of(Path::new("/tmp/Contents/MacOS/HealthyMac")), None);
    }

    #[test]
    fn a_download_that_does_not_match_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("HealthyMac.dmg");
        fs::write(&file, b"hola").unwrap();
        let right = "b221d9dbb083a7f33428d7c2a3c3198ae925614d70210e28716ccaa7cd4ddb79";
        let asset = |size, sha256: Option<&str>| Asset {
            url: String::new(),
            size,
            sha256: sha256.map(str::to_string),
        };
        assert!(verify_download(&file, &asset(4, Some(right))).is_ok());
        assert!(verify_download(&file, &asset(4, None)).is_ok());
        assert!(verify_download(&file, &asset(5, Some(right))).is_err());
        assert!(verify_download(&file, &asset(4, Some(&right.replace('b', "c")))).is_err());
    }

    #[test]
    fn downloads_with_curl_and_reports_how_much_has_arrived() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.dmg");
        fs::write(&source, vec![7u8; 300_000]).unwrap();
        let asset = Asset {
            url: format!("file://{}", source.display()),
            size: 300_000,
            sha256: None,
        };
        let destination = dir.path().join("HealthyMac.dmg");
        let progress = Progress::default();

        download(&asset, &destination, &progress).unwrap();

        assert_eq!(progress.downloaded(), 300_000);
        assert_eq!(fs::read(&destination).unwrap(), fs::read(&source).unwrap());
        assert!(!progress.verifying());

        let missing = Asset {
            url: format!("file://{}", dir.path().join("nope.dmg").display()),
            ..asset
        };
        let error = download(&missing, &dir.path().join("x.dmg"), &Progress::default())
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("No se ha podido descargar"), "{error}");
    }

    /// A process that has exited and been reaped, as the app is once it quits.
    fn finished_process() -> u32 {
        let mut child = Command::new("/bin/sleep").arg("0.2").spawn().unwrap();
        let pid = child.id();
        std::thread::spawn(move || child.wait());
        pid
    }

    fn fake_app(path: &Path, version: &str) {
        fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
        fs::write(path.join("Contents/version"), version).unwrap();
    }

    #[test]
    fn swaps_the_app_once_the_old_one_has_quit() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Applications/HealthyMac.app");
        let work = dir.path().join("work");
        fake_app(&target, "old");
        fake_app(&work.join("HealthyMac.app"), "new");
        let staged = Staged {
            app: work.join("HealthyMac.app"),
            work: work.clone(),
        };

        let status = spawn_swap(finished_process(), &target, &staged, false)
            .unwrap()
            .wait()
            .unwrap();

        assert!(status.success());
        assert_eq!(
            fs::read_to_string(target.join("Contents/version")).unwrap(),
            "new"
        );
        assert!(
            !work.exists(),
            "the downloaded copy and the old app are cleaned up"
        );
    }

    #[test]
    fn keeps_the_old_app_if_the_new_one_cannot_be_copied() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("HealthyMac.app");
        let work = dir.path().join("work");
        fake_app(&target, "old");
        fs::create_dir_all(&work).unwrap();
        let staged = Staged {
            app: work.join("missing.app"),
            work: work.clone(),
        };

        spawn_swap(finished_process(), &target, &staged, false)
            .unwrap()
            .wait()
            .unwrap();

        assert_eq!(
            fs::read_to_string(target.join("Contents/version")).unwrap(),
            "old"
        );
    }

    /// Asks the real GitHub API. Run with `cargo test -p hm-app -- --ignored`.
    #[test]
    #[ignore = "needs the network"]
    fn the_latest_release_on_github_can_be_read() {
        std::env::remove_var("HEALTHYMAC_UPDATE_URL");
        match check().unwrap() {
            Check::UpToDate => {}
            Check::Available(release) => {
                assert!(release.version > current_version());
                assert!(release.page.starts_with("https://github.com/"));
            }
        }
    }

    #[test]
    fn summarizes_the_first_paragraph_of_the_notes() {
        let notes = "## What's new\n\n- **Updates** from inside the app.\n- Light theme.\n\n## Fixes\n\nMore.";
        assert_eq!(
            summary(notes, 200),
            "Updates from inside the app. Light theme."
        );
        assert_eq!(summary(notes, 20), "Updates from inside…");
        assert_eq!(summary("", 20), "");
    }
}
