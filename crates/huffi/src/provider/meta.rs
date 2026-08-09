use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::provider::{Entry, Provider, entry};

/// Provides entries that expose daemon-internal state: uptime, socket path,
/// data path, pid, version, and dry-run mode.
///
/// Triggered by the `@` prefix. Values are copied to the clipboard when
/// selected, so typing `@socket` and pressing enter puts the socket path on
/// your clipboard.
pub struct MetaProvider {
    socket_path: std::path::PathBuf,
    data_path: std::path::PathBuf,
    dry_run: bool,
    started: Instant,
    started_at: SystemTime,
}

impl MetaProvider {
    pub fn new(
        socket_path: impl AsRef<Path>,
        data_path: impl AsRef<Path>,
        dry_run: bool,
    ) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            data_path: data_path.as_ref().to_path_buf(),
            dry_run,
            started: Instant::now(),
            started_at: SystemTime::now(),
        }
    }

    fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    fn pid(&self) -> u32 {
        std::process::id()
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

impl Provider for MetaProvider {
    fn id(&self) -> &str {
        "meta"
    }

    fn prefixes(&self) -> &[&str] {
        &["@"]
    }

    fn init(&mut self) {}

    fn query(&self, prefix: Option<&str>, _query: &str) -> Vec<Entry> {
        let Some(_prefix) = prefix else {
            return vec![];
        };

        let uptime = format_uptime(self.uptime());
        let socket = self.socket_path.to_string_lossy().into_owned();
        let data = self.data_path.to_string_lossy().into_owned();
        let pid = self.pid().to_string();
        let version = self.version().to_string();
        let dry_run = if self.dry_run { "on" } else { "off" };
        let started_at = started_at_string(self.started_at).unwrap_or_default();

        let mut entries = vec![
            meta_entry("meta-uptime", "Uptime", uptime),
            meta_entry("meta-socket", "Socket path", socket),
            meta_entry("meta-data", "Data file", data.clone()),
            meta_entry("meta-pid", "PID", pid.clone()),
            meta_entry("meta-version", "Version", version),
            meta_entry("meta-dry-run", "Dry-run", dry_run.to_string()),
            meta_entry("meta-started-at", "Started at", started_at),
        ];

        if let Some(rss) = proc_status_field("VmRSS:") {
            entries.push(meta_entry("meta-memory", "Memory (RSS)", format!("{rss} KiB")));
        }
        if let Some(threads) = proc_status_field("Threads:") {
            entries.push(meta_entry("meta-threads", "Threads", threads.to_string()));
        }

        entries.push(
            entry("meta-kill", "Kill daemon")
                .subtitle(format!("terminate pid {pid}"))
                .exec(vec!["kill".into(), pid])
                .match_field("Kill daemon"),
        );
        entries.push(
            entry("meta-open-data", "Open data file")
                .subtitle(data.clone())
                .exec(vec!["xdg-open".into(), data])
                .match_field("Open data file"),
        );

        entries
    }
}

/// Build an entry that displays a label with the daemon value as subtitle and
/// copies that value to the clipboard when selected. Only the label is
/// fuzzy-matched, so `@uptime` finds the entry but arbitrary path fragments
/// do not.
fn meta_entry(id: &str, label: &str, value: String) -> Entry {
    entry(id, label)
        .subtitle(value.clone())
        .clipboard(value)
        .match_field(label)
}

/// Read a numeric field from `/proc/self/status` (e.g. `VmRSS:` in KiB or
/// `Threads:`). Returns `None` if the field is absent or unparsable.
fn proc_status_field(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    proc_status_parses_field(&status, field)
}

fn proc_status_parses_field(status: &str, field: &str) -> Option<u64> {
    let rest = status.lines().find_map(|line| line.strip_prefix(field))?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Format a duration as compact non-zero components, e.g. `1d 2h 3m 4s`.
fn format_uptime(d: Duration) -> String {
    let total = d.as_secs();
    let days = total / 86400;
    let hours = total % 86400 / 3600;
    let minutes = total % 3600 / 60;
    let seconds = total % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

/// Format a wall-clock time as UTC `YYYY-MM-DD HH:MM:SS`.
fn started_at_string(t: SystemTime) -> Option<String> {
    use std::fmt::Write;

    let unix = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let days = unix / 86400;
    let (y, m, d) = civil_from_days(days as i64);

    let mut s = String::new();
    write!(
        s,
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        unix % 86400 / 3600,
        unix % 3600 / 60,
        unix % 60
    )
    .ok()?;
    Some(s)
}

/// Convert days since 1970-01-01 to a (year, month, day) civil date.
/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_without_prefix() {
        let p = MetaProvider::new("/tmp/x.sock", "/tmp/data.json", false);
        assert!(p.query(None, "").is_empty());
        assert!(p.query(None, "uptime").is_empty());
    }

    #[test]
    fn returns_entries_with_prefix() {
        let p = MetaProvider::new("/tmp/x.sock", "/tmp/data.json", true);
        let entries = p.query(Some("@"), "");
        assert_eq!(entries.len(), 11);
        assert!(entries.iter().any(|e| e.entry.id == "meta-uptime"));
        assert!(entries.iter().any(|e| e.entry.id == "meta-socket"));
        assert!(entries.iter().any(|e| e.entry.id == "meta-dry-run"));
        assert!(entries.iter().any(|e| e.entry.id == "meta-started-at"));
        assert!(entries.iter().any(|e| e.entry.id == "meta-memory"));
        assert!(entries.iter().any(|e| e.entry.id == "meta-threads"));
        assert!(entries.iter().any(|e| e.entry.id == "meta-kill"));
        assert!(entries.iter().any(|e| e.entry.id == "meta-open-data"));
    }

    #[test]
    fn memory_and_threads_entries_carry_values() {
        let p = MetaProvider::new("/tmp/x.sock", "/tmp/data.json", false);
        let entries = p.query(Some("@"), "");
        let memory = entries
            .iter()
            .find(|e| e.entry.id == "meta-memory")
            .expect("meta-memory entry");
        assert!(memory.entry.subtitle.as_deref().unwrap().ends_with("KiB"));
        let threads = entries
            .iter()
            .find(|e| e.entry.id == "meta-threads")
            .expect("meta-threads entry");
        assert!(threads.entry.subtitle.as_deref().unwrap().parse::<u64>().is_ok());
    }

    #[test]
    fn proc_status_parses_numeric_fields() {
        let status = "Name:\thuffi-daemon\nThreads:\t7\nVmRSS:\t   12345 kB\n";
        assert_eq!(proc_status_parses_field(status, "Threads:"), Some(7));
        assert_eq!(proc_status_parses_field(status, "VmRSS:"), Some(12345));
        assert_eq!(proc_status_parses_field(status, "Nope:"), None);
    }

    #[test]
    fn kill_entry_runs_kill_on_pid() {
        let p = MetaProvider::new("/tmp/x.sock", "/tmp/data.json", false);
        let entries = p.query(Some("@"), "");
        let kill = entries
            .iter()
            .find(|e| e.entry.id == "meta-kill")
            .expect("meta-kill entry");
        assert_eq!(kill.entry.title, "Kill daemon");
        match &kill.entry.action {
            crate::provider::Action::Exec { args, terminal } => {
                assert!(!terminal);
                assert_eq!(args[0], "kill");
                assert_eq!(args[1], std::process::id().to_string());
            }
            _ => panic!("expected Exec action"),
        }
    }

    #[test]
    fn open_data_entry_runs_xdg_open() {
        let p = MetaProvider::new("/tmp/x.sock", "/tmp/data.json", false);
        let entries = p.query(Some("@"), "");
        let open = entries
            .iter()
            .find(|e| e.entry.id == "meta-open-data")
            .expect("meta-open-data entry");
        assert_eq!(open.entry.title, "Open data file");
        assert_eq!(open.entry.subtitle.as_deref(), Some("/tmp/data.json"));
        match &open.entry.action {
            crate::provider::Action::Exec { args, terminal } => {
                assert!(!terminal);
                assert_eq!(args, &vec!["xdg-open".to_string(), "/tmp/data.json".to_string()]);
            }
            _ => panic!("expected Exec action"),
        }
    }

    #[test]
    fn socket_entry_carries_value_and_copy_action() {
        let p = MetaProvider::new("/tmp/x.sock", "/tmp/data.json", false);
        let entries = p.query(Some("@"), "sock");
        let socket = entries
            .iter()
            .find(|e| e.entry.id == "meta-socket")
            .expect("meta-socket entry");
        assert_eq!(socket.entry.title, "Socket path");
        assert_eq!(socket.entry.subtitle.as_deref(), Some("/tmp/x.sock"));
        match &socket.entry.action {
            crate::provider::Action::Exec { args, terminal } => {
                assert!(!terminal);
                assert_eq!(args, &vec!["wl-copy".to_string(), "/tmp/x.sock".to_string()]);
            }
            _ => panic!("expected Exec action"),
        }
    }

    #[test]
    fn dry_run_reflected_in_entry() {
        let p = MetaProvider::new("/tmp/x.sock", "/tmp/data.json", true);
        let entries = p.query(Some("@"), "");
        let dry = entries
            .iter()
            .find(|e| e.entry.id == "meta-dry-run")
            .expect("meta-dry-run entry");
        assert_eq!(dry.entry.title, "Dry-run");
        assert_eq!(dry.entry.subtitle.as_deref(), Some("on"));
    }

    #[test]
    fn value_not_in_match_fields() {
        let p = MetaProvider::new("/tmp/unlikely-path.sock", "/tmp/data.json", false);
        let entries = p.query(Some("@"), "");
        let socket = entries
            .iter()
            .find(|e| e.entry.id == "meta-socket")
            .expect("meta-socket entry");
        match &socket.rank {
            crate::scoring::Rank::MatchFields(fields) => {
                assert!(
                    fields.iter().all(|f| f.text != "/tmp/unlikely-path.sock"),
                    "value should not be fuzzy-matched"
                );
            }
            other => panic!("expected MatchFields, got {other:?}"),
        }
    }

    #[test]
    fn uptime_formatting() {
        assert_eq!(format_uptime(Duration::from_secs(0)), "0s");
        assert_eq!(format_uptime(Duration::from_secs(5)), "5s");
        assert_eq!(format_uptime(Duration::from_secs(65)), "1m 5s");
        assert_eq!(format_uptime(Duration::from_secs(3661)), "1h 1m 1s");
        assert_eq!(format_uptime(Duration::from_secs(90061)), "1d 1h 1m 1s");
    }

    #[test]
    fn civil_date_known_value() {
        // 2026-08-09
        let days = 20674;
        assert_eq!(civil_from_days(days), (2026, 8, 9));
    }
}
