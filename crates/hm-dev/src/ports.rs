//! Which process is listening on which port.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use hm_core::{exec, t, tf};

use crate::types::ListeningPort;

const LSOF: &str = "/usr/sbin/lsof";
const PS: &str = "/bin/ps";
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Listener {
    pid: u32,
    process_name: String,
    uid: Option<u32>,
    user: String,
    address: String,
    port: u16,
}

/// Undoes lsof's escaping of unusual characters, which some versions write as `\x20`.
fn unescape_lsof(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }
    let mut bytes = Vec::with_capacity(text.len());
    let raw = text.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'\\' && raw.get(index + 1) == Some(&b'x') {
            let hex = text
                .get(index + 2..index + 4)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = hex {
                bytes.push(byte);
                index += 4;
                continue;
            }
        }
        bytes.push(raw[index]);
        index += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Splits `*:3000`, `127.0.0.1:8080` or `[::1]:5432`.
fn split_address(name: &str) -> Option<(String, u16)> {
    let (address, port) = name.rsplit_once(':')?;
    Some((address.to_string(), port.parse().ok()?))
}

/// Parses `lsof -F pcuLPn`. Each process starts with a `p` line and sets the fields for the sockets
/// that follow it, one `n` line each.
fn parse_lsof(output: &str) -> Vec<Listener> {
    let mut listeners = Vec::new();
    let mut current = Listener::default();
    for line in output.lines() {
        let Some(field) = line.chars().next() else {
            continue;
        };
        let value = &line[field.len_utf8()..];
        match field {
            'p' => {
                current = Listener {
                    pid: value.parse().unwrap_or(0),
                    ..Listener::default()
                };
            }
            'c' => current.process_name = unescape_lsof(value),
            'u' => current.uid = value.parse().ok(),
            'L' => current.user = unescape_lsof(value),
            'n' => {
                if current.pid == 0 {
                    continue;
                }
                if let Some((address, port)) = split_address(value) {
                    listeners.push(Listener {
                        address,
                        port,
                        ..current.clone()
                    });
                }
            }
            _ => {}
        }
    }
    // A dual-stack socket appears once for IPv4 and once for IPv6, with the same `*:port`.
    listeners.sort_by(|a, b| (a.port, a.pid, &a.address).cmp(&(b.port, b.pid, &b.address)));
    listeners.dedup_by(|a, b| a.port == b.port && a.pid == b.pid && a.address == b.address);
    listeners
}

/// Parses `ps -o pid=,command=`, whose lines are the pid, spaces and the full command line.
fn parse_ps(output: &str) -> HashMap<u32, String> {
    output
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(char::is_whitespace)?;
            let command = command.trim();
            if command.is_empty() {
                return None;
            }
            Some((pid.parse().ok()?, command.to_string()))
        })
        .collect()
}

/// Parses `lsof -a -d cwd -Fn`, whose lines are `p<pid>`, `fcwd` and `n<path>` for each process.
fn parse_cwds(output: &str) -> HashMap<u32, PathBuf> {
    let mut cwds = HashMap::new();
    let mut pid = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
        } else if let (Some(value), Some(pid)) = (line.strip_prefix('n'), pid) {
            cwds.insert(pid, PathBuf::from(unescape_lsof(value)));
        }
    }
    cwds
}

/// Processes that say what they are regardless of the port. Built on each call rather than kept in a
/// `const`, so its text follows the current language.
fn process_hints() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ollama", "Ollama"),
        ("LM Studio", "LM Studio"),
        ("lms", "LM Studio"),
        ("llmster", "LM Studio"),
        ("postgres", "PostgreSQL"),
        ("redis-server", "Redis"),
        ("mysqld", "MySQL"),
        ("mariadbd", "MariaDB"),
        ("mongod", "MongoDB"),
        ("nginx", "nginx"),
        ("httpd", "Apache"),
        ("caddy", "Caddy"),
        ("adb", "Android Debug Bridge"),
        ("rapportd", t!("Continuidad de macOS", "macOS Continuity")),
    ]
}

/// What usually listens on a port, by convention. A function for the same reason as
/// [`process_hints`].
fn well_known_ports() -> Vec<(u16, &'static str)> {
    vec![
        (80, t!("Servidor web", "Web server")),
        (443, t!("Servidor web seguro", "Secure web server")),
        (1234, "LM Studio"),
        (1313, "Hugo"),
        (1337, "Strapi"),
        (1420, "Tauri"),
        (2181, "ZooKeeper"),
        (
            3000,
            t!(
                "Next.js u otro servidor de desarrollo",
                "Next.js or another dev server"
            ),
        ),
        (3001, t!("Servidor de desarrollo", "Dev server")),
        (3306, "MySQL"),
        (4000, t!("Servidor de desarrollo", "Dev server")),
        (4173, t!("Vista previa de Vite", "Vite preview")),
        (4200, "Angular"),
        (4321, "Astro"),
        (
            5000,
            t!(
                "Flask u otro servidor de desarrollo",
                "Flask or another dev server"
            ),
        ),
        (5173, "Vite"),
        (5174, "Vite"),
        (5432, "PostgreSQL"),
        (5672, "RabbitMQ"),
        (6006, "Storybook"),
        (6379, "Redis"),
        (
            8000,
            t!(
                "Django, FastAPI u otro servidor de desarrollo",
                "Django, FastAPI or another dev server"
            ),
        ),
        (8080, t!("Servidor de desarrollo", "Dev server")),
        (8081, t!("Metro de React Native", "React Native Metro")),
        (8888, "Jupyter"),
        (9000, t!("Servidor de desarrollo", "Dev server")),
        (9092, "Kafka"),
        (9200, "Elasticsearch"),
        (9229, t!("Depurador de Node", "Node debugger")),
        (11434, "Ollama"),
        (15672, t!("Panel de RabbitMQ", "RabbitMQ dashboard")),
        (19000, "Expo"),
        (19006, "Expo"),
        (27017, "MongoDB"),
    ]
}

/// A friendly guess at what is listening. What the process says about itself beats the port, which
/// is only a convention.
fn service_hint(port: u16, process_name: &str) -> Option<String> {
    let lower = process_name.to_lowercase();
    let by_port = well_known_ports()
        .into_iter()
        .find(|(known, _)| *known == port)
        .map(|(_, hint)| hint);

    // macOS's AirPlay Receiver takes these two ports, to the confusion of Flask developers.
    if lower == "controlcenter" && matches!(port, 5000 | 7000) {
        return Some(t!("Receptor de AirPlay de macOS", "macOS AirPlay Receiver").to_string());
    }
    // A container's published port shows up as Docker itself, so the port says more.
    if lower.starts_with("com.docker")
        || lower == "docker"
        || lower == "vpnkit"
        || lower.contains("orbstack")
    {
        return Some(match by_port {
            Some(hint) => tf!("{hint} (contenedor)", "{hint} (container)"),
            None => t!(
                "Puerto publicado por un contenedor",
                "Port published by a container"
            )
            .to_string(),
        });
    }
    for (needle, hint) in process_hints() {
        if process_name == needle
            || lower == needle.to_lowercase()
            || lower.starts_with(&format!("{}-", needle.to_lowercase()))
        {
            return Some(hint.to_string());
        }
    }
    by_port.map(str::to_string)
}

fn pid_list(pids: &[u32]) -> String {
    pids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Lists listening sockets for the current user's processes, sorted by port.
pub fn list_listening() -> Result<Vec<ListeningPort>> {
    let output = exec::run(
        Path::new(LSOF),
        &["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcuLPn", "+c", "0"],
        TIMEOUT,
    )
    .context(t!("no se pudo ejecutar lsof", "could not run lsof"))?;
    if output.timed_out {
        bail!(t!(
            "lsof no terminó a tiempo",
            "lsof did not finish in time"
        ));
    }
    // lsof exits with 1 when nothing matched, which is not an error. Only trust it to have failed
    // when it also complained and gave nothing back.
    if !output.success() && output.stdout.trim().is_empty() && !output.stderr.trim().is_empty() {
        bail!(tf!(
            "lsof falló: {}",
            "lsof failed: {}",
            output.stderr.trim()
        ));
    }

    // SAFETY: getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    let listeners: Vec<Listener> = parse_lsof(&output.stdout)
        .into_iter()
        .filter(|listener| listener.uid.is_none_or(|uid| uid == me))
        .collect();
    if listeners.is_empty() {
        return Ok(Vec::new());
    }

    let mut pids: Vec<u32> = listeners.iter().map(|listener| listener.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    let list = pid_list(&pids);

    // Details are a bonus: the list is still worth having if either command fails.
    let commands = exec::run(
        Path::new(PS),
        &["-ww", "-o", "pid=,command=", "-p", &list],
        TIMEOUT,
    )
    .map(|output| parse_ps(&output.stdout))
    .unwrap_or_default();
    let cwds = exec::run(
        Path::new(LSOF),
        &["-a", "-d", "cwd", "-p", &list, "-Fn"],
        TIMEOUT,
    )
    .map(|output| parse_cwds(&output.stdout))
    .unwrap_or_default();

    Ok(listeners
        .into_iter()
        .map(|listener| ListeningPort {
            port: listener.port,
            protocol: "TCP".to_string(),
            service_hint: service_hint(listener.port, &listener.process_name),
            address: listener.address,
            pid: listener.pid,
            process_name: listener.process_name,
            command: commands.get(&listener.pid).cloned(),
            cwd: cwds.get(&listener.pid).cloned(),
            user: listener.user,
        })
        .collect())
}

/// The real and effective owner of a process, or why the kernel would not say.
fn process_owner(pid: i32) -> std::io::Result<(u32, u32)> {
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: an all-zero `proc_bsdinfo` is valid, since it is plain integers and byte arrays.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is live for the call and its real size is passed alongside it.
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if written == size {
        Ok((info.pbi_ruid, info.pbi_uid))
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Why this pid may not be signalled, checked before anything else happens.
///
/// `kill` treats special values as groups: 0 is the caller's process group, and -1 is everything the
/// caller is allowed to signal. A careless cast from `u32` would reach those, and a large pid
/// wrapping around to -1 would take down the user's whole session.
fn refuse_pid(pid: u32) -> Result<i32> {
    if pid <= 1 {
        bail!(tf!(
            "el proceso {pid} está protegido",
            "process {pid} is protected"
        ));
    }
    let Ok(signed) = i32::try_from(pid) else {
        bail!(tf!(
            "{pid} no es un identificador de proceso válido",
            "{pid} is not a valid process ID"
        ));
    };
    if pid == std::process::id() {
        bail!(t!(
            "HealthyMac no puede terminarse a sí mismo",
            "HealthyMac can't terminate itself"
        ));
    }
    Ok(signed)
}

/// Sends SIGTERM to `pid`, or SIGKILL if `force`. Only ever affects the current user's processes.
pub fn terminate(pid: u32, force: bool) -> Result<()> {
    let target = refuse_pid(pid)?;
    // The kernel will not describe another user's process to a regular user, so a refusal here is
    // itself the answer. Anything unclear stops short of sending a signal.
    let (real, effective) = match process_owner(target) {
        Ok(owner) => owner,
        Err(error) => match error.raw_os_error() {
            Some(libc::ESRCH) => bail!(tf!(
                "el proceso {pid} ya no existe",
                "process {pid} no longer exists"
            )),
            Some(libc::EPERM) => bail!(tf!(
                "el proceso {pid} es de otro usuario",
                "process {pid} belongs to another user"
            )),
            _ => {
                return Err(error).with_context(|| {
                    tf!(
                        "no se pudo comprobar el proceso {pid}",
                        "could not check process {pid}"
                    )
                })
            }
        },
    };
    // SAFETY: getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    if real != me || effective != me {
        bail!(tf!(
            "el proceso {pid} es de otro usuario",
            "process {pid} belongs to another user"
        ));
    }
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: kill takes plain integers, and `target` has been checked to be one real process.
    if unsafe { libc::kill(target, signal) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(error).with_context(|| {
            tf!(
                "no se pudo enviar la señal al proceso {pid}",
                "could not send the signal to process {pid}"
            )
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    // Captured from `lsof -nP -iTCP -sTCP:LISTEN -F pcuLPn +c 0` on this project's development Mac.
    const CAPTURED_LSOF: &str = "\
p690
crapportd
u501
Lana
f11
PTCP
n*:59358
f12
PTCP
n*:59358
p691
cControlCenter
u501
Lana
f9
PTCP
n*:7000
f10
PTCP
n*:7000
f11
PTCP
n*:5000
f12
PTCP
n*:5000
p41200
cDiscord Helper (Renderer)
u501
Lana
f40
PTCP
n127.0.0.1:6463
p41300
cnode
u501
Lana
f22
PTCP
n[::1]:5173
p41400
cpostgres
u501
Lana
f7
PTCP
n127.0.0.1:5432
f8
PTCP
n[::1]:5432
";

    #[test]
    fn parses_captured_lsof() {
        let listeners = parse_lsof(CAPTURED_LSOF);
        let ports: Vec<u16> = listeners.iter().map(|listener| listener.port).collect();
        assert_eq!(ports, vec![5000, 5173, 5432, 5432, 6463, 7000, 59358]);

        let node = listeners
            .iter()
            .find(|listener| listener.port == 5173)
            .unwrap();
        assert_eq!(node.pid, 41300);
        assert_eq!(node.process_name, "node");
        assert_eq!(node.address, "[::1]");
        assert_eq!(node.uid, Some(501));
        assert_eq!(node.user, "ana");

        let discord = listeners
            .iter()
            .find(|listener| listener.port == 6463)
            .unwrap();
        assert_eq!(discord.process_name, "Discord Helper (Renderer)");
    }

    #[test]
    fn dual_stack_duplicates_collapse_but_distinct_addresses_stay() {
        let listeners = parse_lsof(CAPTURED_LSOF);
        assert_eq!(
            listeners
                .iter()
                .filter(|listener| listener.port == 7000)
                .count(),
            1
        );
        assert_eq!(
            listeners
                .iter()
                .filter(|listener| listener.port == 5432)
                .count(),
            2
        );
    }

    #[test]
    fn malformed_lsof_is_tolerated() {
        assert!(parse_lsof("").is_empty());
        assert!(parse_lsof("pnotapid\ncfoo\nn*:80\n").is_empty());
        assert!(parse_lsof("p1\ncfoo\nn*:notaport\n").is_empty());
        assert!(parse_lsof("\n\n?\n").is_empty());
        // No uid line at all, as for a process lsof could only half read.
        let listeners = parse_lsof("p7\ncfoo\nn*:80\n");
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].uid, None);
    }

    #[test]
    fn lsof_escapes_are_undone() {
        assert_eq!(unescape_lsof("Code\\x20Helper"), "Code Helper");
        assert_eq!(unescape_lsof("plain"), "plain");
        assert_eq!(unescape_lsof("broken\\x"), "broken\\x");
        assert_eq!(unescape_lsof("broken\\xZZ"), "broken\\xZZ");
    }

    #[test]
    fn parses_ps_and_cwd_output() {
        let commands = parse_ps("  690 /usr/libexec/rapportd\n41300 node /repo/node_modules/.bin/vite --port 5173\nbad\n");
        assert_eq!(commands[&690], "/usr/libexec/rapportd");
        assert_eq!(
            commands[&41300],
            "node /repo/node_modules/.bin/vite --port 5173"
        );
        assert_eq!(commands.len(), 2);

        let cwds = parse_cwds("p690\nfcwd\nn/\np41300\nfcwd\nn/Users/me/Coding/My App\n");
        assert_eq!(cwds[&41300], PathBuf::from("/Users/me/Coding/My App"));
        assert_eq!(cwds[&690], PathBuf::from("/"));
    }

    #[test]
    fn hints_prefer_the_process_over_the_port() {
        assert_eq!(service_hint(11434, "ollama").as_deref(), Some("Ollama"));
        assert_eq!(service_hint(9999, "ollama").as_deref(), Some("Ollama"));
        assert_eq!(
            service_hint(6000, "postgres").as_deref(),
            Some("PostgreSQL")
        );
        assert_eq!(service_hint(5173, "node").as_deref(), Some("Vite"));
        assert_eq!(service_hint(1234, "weird").as_deref(), Some("LM Studio"));
        assert_eq!(service_hint(5432, "node").as_deref(), Some("PostgreSQL"));
        assert_eq!(service_hint(47123, "node"), None);
    }

    #[test]
    fn hints_recognise_the_macos_airplay_ports() {
        assert_eq!(
            service_hint(5000, "ControlCenter").as_deref(),
            Some("Receptor de AirPlay de macOS")
        );
        assert_eq!(
            service_hint(5000, "python3").as_deref(),
            Some("Flask u otro servidor de desarrollo")
        );
    }

    #[test]
    fn hints_see_through_docker_published_ports() {
        assert_eq!(
            service_hint(5432, "com.docker.backend").as_deref(),
            Some("PostgreSQL (contenedor)")
        );
        assert_eq!(
            service_hint(48123, "OrbStack Helper").as_deref(),
            Some("Puerto publicado por un contenedor")
        );
    }

    #[test]
    fn special_pids_are_refused_before_any_signal() {
        for pid in [0u32, 1] {
            assert!(terminate(pid, false).is_err(), "{pid}");
        }
        // Each of these would become a negative pid if cast carelessly, and `kill(-1)` signals
        // every process the user owns.
        for pid in [u32::MAX, u32::MAX - 1, i32::MAX as u32 + 1] {
            let error = terminate(pid, true).unwrap_err().to_string();
            assert!(error.contains("válido"), "{pid}: {error}");
        }
    }

    #[test]
    fn it_refuses_to_terminate_itself() {
        assert!(terminate(std::process::id(), false).is_err());
    }

    #[test]
    fn another_users_process_is_refused() {
        // Any Mac has root processes other than launchd, which is refused for a different reason.
        let output = exec::run(Path::new(PS), &["-axo", "pid=,uid="], TIMEOUT).unwrap();
        let root_owned = output.stdout.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: u32 = parts.next()?.parse().ok()?;
            let uid: u32 = parts.next()?.parse().ok()?;
            (uid == 0 && pid > 1).then_some(pid)
        });
        let Some(pid) = root_owned else {
            return;
        };
        let error = terminate(pid, false).unwrap_err().to_string();
        assert!(error.contains("otro usuario"), "{error}");
    }

    #[test]
    fn a_process_that_does_not_exist_is_an_error_not_a_panic() {
        // Far above anything the kernel hands out by default, and still a valid pid.
        let error = terminate(i32::MAX as u32, false).unwrap_err().to_string();
        assert!(error.contains("ya no existe"), "{error}");
    }

    fn wait_for_exit(child: &mut std::process::Child) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn it_signals_a_process_of_our_own() {
        // Only ever a child this test started itself.
        for force in [false, true] {
            let mut child = Command::new("/bin/sleep")
                .arg("30")
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            terminate(child.id(), force).unwrap();
            assert!(wait_for_exit(&mut child), "force = {force}");
        }
    }

    #[test]
    fn listing_ports_on_this_machine_does_not_fail() {
        let ports = list_listening().unwrap();
        for pair in ports.windows(2) {
            assert!(pair[0].port <= pair[1].port, "sorted by port");
        }
        for port in &ports {
            assert_eq!(port.protocol, "TCP");
        }
    }
}
