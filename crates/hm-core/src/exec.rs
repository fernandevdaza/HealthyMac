//! Running external tools without a shell, with a timeout.
//!
//! An app launched from Finder inherits a minimal `PATH` with no Homebrew, Cargo or Docker, so
//! [`which`] searches the usual tool locations explicitly rather than relying on the environment.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

const SYSTEM_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/Applications/Docker.app/Contents/Resources/bin",
    "/Applications/OrbStack.app/Contents/MacOS/xbin",
    "/Applications/LM Studio.app/Contents/Resources/app/.webpack",
    "/Applications/Ollama.app/Contents/Resources",
];

const HOME_DIRS: &[&str] = &[
    ".cargo/bin",
    ".local/bin",
    ".docker/bin",
    ".orbstack/bin",
    ".lmstudio/bin",
    ".bun/bin",
    "Library/pnpm",
    "Library/pnpm/bin",
    ".volta/bin",
    ".fnm",
];

/// Where tools live, most trustworthy first: fixed system locations, then the user's, then whatever
/// `PATH` this process inherited.
fn search_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = SYSTEM_DIRS.iter().map(PathBuf::from).collect();
    dirs.extend(HOME_DIRS.iter().map(|dir| home.join(dir)));
    // nvm has one bin folder per Node version. Newest first, so `npm` finds a current `node`.
    let mut nvm: Vec<PathBuf> = fs::read_dir(home.join(".nvm/versions/node"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path().join("bin"))
                .collect()
        })
        .unwrap_or_default();
    nvm.sort();
    nvm.reverse();
    dirs.extend(nvm);
    if let Some(path) = env::var_os("PATH") {
        dirs.extend(env::split_paths(&path));
    }
    dirs
}

/// Finds an executable by name. Names containing a `/` are returned as they are if they exist.
pub fn which(program: &str, home: &Path) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    search_dirs(home)
        .into_iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// The `PATH` handed to child processes. Finding a tool isn't enough: `npm` starts with
/// `#!/usr/bin/env node`, so from a Finder launch it would fail unless its child can find `node`.
fn child_path() -> Option<std::ffi::OsString> {
    let home = env::var_os("HOME").map(PathBuf::from)?;
    env::join_paths(search_dirs(&home)).ok()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[derive(Clone, Debug, Default)]
pub struct CmdOutput {
    /// `None` if the process was killed by a signal, including on timeout.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs `program` directly with `args`, killing it after `timeout`. Stdin is closed so a tool that
/// wants to prompt fails instead of hanging.
pub fn run(program: &Path, args: &[&str], timeout: Duration) -> Result<CmdOutput> {
    let mut command = Command::new(program);
    if let Some(path) = child_path() {
        command.env("PATH", path);
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            crate::tf!(
                "no se pudo ejecutar {}",
                "could not run {}",
                program.display()
            )
        })?;

    let mut stdout_pipe = child
        .stdout
        .take()
        .context(crate::t!("sin stdout", "no stdout"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .context(crate::t!("sin stderr", "no stderr"))?;
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut bytes);
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut bytes);
        bytes
    });

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(25));
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok(CmdOutput {
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    })
}

/// `true` if a process with exactly this name is running.
pub fn is_process_running(name: &str) -> bool {
    // `-a` includes our own ancestors, which pgrep skips by default. Without it, running from VS
    // Code's terminal would report that VS Code ("Code") is not open.
    run(
        Path::new("/usr/bin/pgrep"),
        &["-a", "-x", name],
        Duration::from_secs(5),
    )
    .map(|output| output.success())
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output() {
        let out = run(Path::new("/bin/echo"), &["hola"], Duration::from_secs(5)).unwrap();
        assert!(out.success());
        assert_eq!(out.stdout.trim(), "hola");
    }

    #[test]
    fn kills_on_timeout() {
        let started = Instant::now();
        let out = run(Path::new("/bin/sleep"), &["30"], Duration::from_millis(100)).unwrap();
        assert!(out.timed_out);
        assert!(!out.success());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_program_is_an_error() {
        assert!(run(Path::new("/no/such/tool"), &[], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn children_can_find_tools_even_with_a_minimal_inherited_path() {
        // This is the PATH a Finder-launched app inherits. It lacks every user tool folder, yet a
        // child like `npm` still has to resolve `node` through `/usr/bin/env`.
        let out = run(
            Path::new("/usr/bin/printenv"),
            &["PATH"],
            Duration::from_secs(5),
        )
        .unwrap();
        let dirs: Vec<PathBuf> = env::split_paths(out.stdout.trim()).collect();
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        assert!(dirs.contains(&home.join(".local/bin")));
    }

    #[test]
    fn which_finds_a_tool_in_a_user_folder_of_that_home() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".nvm/versions/node/v24.0.0/bin");
        fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("hm-fake-tool");
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(which("hm-fake-tool", home.path()), Some(tool));
    }

    #[test]
    fn which_finds_system_tools() {
        assert!(which("ls", Path::new("/nonexistent")).is_some());
        assert!(which("definitely-not-a-tool", Path::new("/nonexistent")).is_none());
    }
}
