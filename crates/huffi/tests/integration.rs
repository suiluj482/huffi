use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use huffi_protocol::{read_message, write_message, Request, Response};

const DAEMON_START_RETRIES: u32 = 80;
const DAEMON_START_DELAY_MS: u64 = 25;
const DAEMON_READ_TIMEOUT_SECS: u64 = 5;

static SOCK_COUNTER: AtomicU32 = AtomicU32::new(0);

struct DaemonProcess {
    child: Child,
    socket: PathBuf,
    data: PathBuf,
    stream: UnixStream,
}

impl DaemonProcess {
    fn start() -> Self {
        let id = SOCK_COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = PathBuf::from(format!(
            "/tmp/huffi-test-{}-{}.sock",
            std::process::id(),
            id
        ));
        let data = PathBuf::from(format!(
            "/tmp/huffi-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_dir_all(&data);

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_huffi-daemon"));
        cmd.arg("--socket").arg(&socket)
            .arg("--data").arg(data.join("history.json"))
            .arg("--dry-run");

        let child = cmd.spawn().expect("failed to start daemon");

        for _ in 0..DAEMON_START_RETRIES {
            thread::sleep(Duration::from_millis(DAEMON_START_DELAY_MS));
            if let Ok(stream) = UnixStream::connect(&socket) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(DAEMON_READ_TIMEOUT_SECS)))
                    .unwrap();
                return Self {
                    child,
                    socket,
                    data,
                    stream,
                };
            }
        }
        panic!("daemon didn't start");
    }

    fn send(&self, req: &Request) -> Response {
        let stream = self.stream.try_clone().expect("clone failed");
        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);

        write_message(&mut writer, req).expect("write failed");
        let resp: Option<Response> = read_message(&mut reader).expect("read failed");
        resp.expect("empty response")
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

#[test]
fn query_returns_results() {
    let daemon = DaemonProcess::start();
    let resp = daemon.send(&Request::Query {
        query: "fire".into(),
        offset: 0,
        length: 20,
    });

    match resp {
        Response::QueryResult { results, .. } => {
            assert!(!results.is_empty(), "expected some results for 'fire'");
            for hit in &results {
                assert!(hit.score > 0.0);
            }
        }
        other => panic!("expected QueryResult, got {other:?}"),
    }
}

#[test]
fn select_then_query_changes_ranking() {
    let daemon = DaemonProcess::start();

    let resp1 = daemon.send(&Request::Query {
        query: "calc".into(),
        offset: 0,
        length: 20,
    });
    let first_results = match resp1 {
        Response::QueryResult { results, .. } => results,
        other => panic!("expected QueryResult, got {other:?}"),
    };

    if let Some(top) = first_results.first() {
        for _ in 0..5 {
            daemon.send(&Request::Select {
                query: "calc".into(),
                entry_id: top.entry_id.clone(),
            });
        }
    }

    let resp2 = daemon.send(&Request::Query {
        query: "calc".into(),
        offset: 0,
        length: 20,
    });
    match resp2 {
        Response::QueryResult { results, .. } => {
            assert!(!results.is_empty());
            if let Some(top_before) = first_results.first() {
                assert_eq!(results[0].entry_id, top_before.entry_id);
            }
        }
        other => panic!("expected QueryResult, got {other:?}"),
    }
}

#[test]
fn query_reports_active_prefix() {
    let daemon = DaemonProcess::start();
    let resp = daemon.send(&Request::Query {
        query: "= 2 + 2".into(),
        offset: 0,
        length: 20,
    });

    match resp {
        Response::QueryResult { prefix, results, .. } => {
            assert_eq!(prefix.as_deref(), Some("="));
            assert!(!results.is_empty(), "expected calculator result for '= 2 + 2'");
        }
        other => panic!("expected QueryResult, got {other:?}"),
    }
}

#[test]
fn providers_lists_entries() {
    let daemon = DaemonProcess::start();
    let resp = daemon.send(&Request::Providers);

    match resp {
        Response::Providers { entries } => {
            assert!(!entries.is_empty());
            assert!(entries.iter().any(|e| e.id == "desktop"));
            assert!(entries.iter().any(|e| e.id == "calculator"));
        }
        other => panic!("expected Providers, got {other:?}"),
    }
}

#[test]
fn boost_moves_app_to_top() {
    let daemon = DaemonProcess::start();

    let resp1 = daemon.send(&Request::Query { query: "br".into(), offset: 0, length: 20 });
    let before = match resp1 {
        Response::QueryResult { results, .. } => results,
        other => panic!("expected QueryResult, got {other:?}"),
    };

    if before.len() >= 2 {
        let target = before[1].history_key.clone().unwrap_or(before[1].entry_id.clone());
        for _ in 0..10 {
            daemon.send(&Request::Boost {
                query: "br".into(),
                history_key: target.clone(),
            });
        }

        let resp2 = daemon.send(&Request::Query { query: "br".into(), offset: 0, length: 20 });
        match resp2 {
            Response::QueryResult { results, .. } => {
                let top_key = results[0].history_key.clone().unwrap_or(results[0].entry_id.clone());
                assert_eq!(top_key, target);
            }
            other => panic!("expected QueryResult, got {other:?}"),
        }
    }
}


