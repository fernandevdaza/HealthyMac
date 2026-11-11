//! Tidying the listening-port list for display.

use hm_dev::types::ListeningPort;

/// One row per process and port: a server listening on both 127.0.0.1 and [::1] shows once, with
/// both addresses. Keeps the scanner's order.
pub fn merge_addresses(ports: Vec<ListeningPort>) -> Vec<ListeningPort> {
    let mut merged: Vec<ListeningPort> = Vec::with_capacity(ports.len());
    for port in ports {
        match merged.iter_mut().find(|seen| {
            seen.pid == port.pid && seen.port == port.port && seen.protocol == port.protocol
        }) {
            Some(seen) => {
                if !seen
                    .address
                    .split(", ")
                    .any(|address| address == port.address)
                {
                    seen.address = format!("{}, {}", seen.address, port.address);
                }
            }
            None => merged.push(port),
        }
    }
    merged
}

/// Part of macOS itself, such as ControlCenter or rapportd. Quitting one does nothing useful: the
/// system starts it again, or a feature like AirPlay stops working until it does.
pub fn is_system_process(port: &ListeningPort) -> bool {
    const SYSTEM_DIRS: [&str; 5] = [
        "/System/",
        "/usr/libexec/",
        "/usr/sbin/",
        "/sbin/",
        "/usr/bin/",
    ];
    port.command
        .as_deref()
        .is_some_and(|command| SYSTEM_DIRS.iter().any(|dir| command.starts_with(dir)))
}

/// The name to show. Some tools rename their process to their version (Claude Code shows up as
/// "2.1.319"); then the command says more.
pub fn display_name(port: &ListeningPort) -> String {
    let looks_like_version = !port.process_name.is_empty()
        && port
            .process_name
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.');
    match (&port.command, looks_like_version) {
        (Some(command), true) => {
            let program = command.split_whitespace().next().unwrap_or(command);
            program.rsplit('/').next().unwrap_or(program).to_string()
        }
        _ => port.process_name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(port: u16, pid: u32, name: &str, command: &str, address: &str) -> ListeningPort {
        ListeningPort {
            port,
            protocol: "TCP".to_string(),
            address: address.to_string(),
            pid,
            process_name: name.to_string(),
            command: Some(command.to_string()),
            cwd: None,
            user: "u".to_string(),
            service_hint: None,
        }
    }

    #[test]
    fn same_process_and_port_shows_once() {
        let merged = merge_addresses(vec![
            listener(
                5432,
                7,
                "postgres",
                "/opt/homebrew/bin/postgres",
                "127.0.0.1",
            ),
            listener(5432, 7, "postgres", "/opt/homebrew/bin/postgres", "[::1]"),
            listener(
                5433,
                7,
                "postgres",
                "/opt/homebrew/bin/postgres",
                "127.0.0.1",
            ),
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].address, "127.0.0.1, [::1]");
    }

    #[test]
    fn system_processes_and_version_names() {
        let control = listener(
            5000,
            1,
            "ControlCenter",
            "/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter",
            "*",
        );
        assert!(is_system_process(&control));
        let claude = listener(50909, 2, "2.1.319", "claude", "127.0.0.1");
        assert!(!is_system_process(&claude));
        assert_eq!(display_name(&claude), "claude");
        let node = listener(
            3000,
            3,
            "node",
            "/usr/local/bin/node server.js",
            "127.0.0.1",
        );
        assert_eq!(display_name(&node), "node");
    }
}
