use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub fn default_socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    runtime.join("huffi.sock")
}

pub fn get_or_spawn_daemon(socket_path: &Path) -> anyhow::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket_path) {
        return Ok(stream);
    }

    let daemon_name = "huffi-daemon";
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));

    let mut cmd = if let Some(dir) = exe_dir {
        Command::new(dir.join(daemon_name))
    } else {
        Command::new(daemon_name)
    };

    cmd.arg("--socket")
        .arg(socket_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        if let Ok(stream) = UnixStream::connect(socket_path) {
            return Ok(stream);
        }
    }

    anyhow::bail!("daemon didn't come up within 1s")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Request {
    Query { query: String, offset: usize, length: usize },
    Select { query: String, entry_id: String },
    Boost { query: String, history_key: String },
    Delete { query: String, history_key: String },
    History { prefix: String },
    Providers,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Response {
    QueryResult { prefix: Option<String>, results: Vec<QueryHit>, total: usize },
    Ok,
    Error { message: String },
    History { entries: Vec<HistoryEntry> },
    Providers { entries: Vec<ProviderEntry> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderEntry {
    pub id: String,
    pub prefixes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryHit {
    pub entry_id: String,
    pub history_key: Option<String>,
    pub base_score: f64,
    pub history_score: Option<f64>,
    pub score: f64,
    pub title: String,
    pub subtitle: Option<String>,
    pub comment: Option<String>,
    pub icon: Option<String>,
    pub extra: Option<serde_json::Value>,
    pub set_query: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub entry_id: String,
    pub score: f64,
    pub n: u32,
    pub last_update: f64,
}

pub fn write_message<W: Write>(writer: &mut W, msg: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn read_message<R: Read, T: for<'de> Deserialize<'de>>(reader: &mut BufReader<R>) -> io::Result<Option<T>> {
    let mut line = String::new();
    match reader.read_line(&mut line)? {
        0 => Ok(None),
        _ => {
            let line = line.trim_end();
            if line.is_empty() {
                return Ok(None);
            }
            serde_json::from_str(line)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_request(msg: &impl Serialize) -> Request {
        let mut buf = Vec::new();
        write_message(&mut buf, msg).unwrap();
        let mut cursor = BufReader::new(&buf[..]);
        read_message::<_, Request>(&mut cursor).unwrap().unwrap()
    }

    fn round_trip_response(msg: &Response) -> Response {
        let mut buf = Vec::new();
        write_message(&mut buf, msg).unwrap();
        let mut cursor = BufReader::new(&buf[..]);
        read_message::<_, Response>(&mut cursor).unwrap().unwrap()
    }

    #[test]
    fn query_round_trip_request() {
        let req = Request::Query { query: "fi".into(), offset: 0, length: 20 };
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn select_round_trip_request() {
        let req = Request::Select { query: "fi".into(), entry_id: "firefox.desktop".into() };
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn boost_round_trip_request() {
        let req = Request::Boost { query: "fi".into(), history_key: "finder.desktop".into() };
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn delete_round_trip_request() {
        let req = Request::Delete { query: "fi".into(), history_key: "firefox.desktop".into() };
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn history_round_trip_request() {
        let req = Request::History { prefix: "f".into() };
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn providers_round_trip_request() {
        let req = Request::Providers;
        assert_eq!(round_trip_request(&req), req);
    }

    #[test]
    fn query_result_round_trip_request() {
        let resp = Response::QueryResult {
            prefix: Some("=".into()),
            results: vec![
                QueryHit {
                    entry_id: "firefox.desktop".into(),
                    history_key: Some("firefox.desktop".into()),
                    base_score: 0.95,
                    history_score: Some(0.5),
                    score: 0.95,
                    title: "Firefox".into(),
                    subtitle: None,
                    comment: Some("Browse the World Wide Web".into()),
                    icon: Some("firefox".into()),
                    extra: None,
                    set_query: None,
                },
                QueryHit {
                    entry_id: "finder.desktop".into(),
                    history_key: None,
                    base_score: 0.6,
                    history_score: None,
                    score: 0.6,
                    title: "Files".into(),
                    subtitle: Some("File Manager".into()),
                    comment: None,
                    icon: Some("org.gnome.Nautilus".into()),
                    extra: None,
                    set_query: Some("= 2 + 2".into()),
                },
            ],
            total: 2,
        };
        assert_eq!(round_trip_response(&resp), resp);
    }

    #[test]
    fn ok_round_trip_request() {
        assert_eq!(round_trip_response(&Response::Ok), Response::Ok);
    }

    #[test]
    fn error_round_trip_request() {
        let resp = Response::Error { message: "something went wrong".into() };
        assert_eq!(round_trip_response(&resp), resp);
    }

    #[test]
    fn history_result_round_trip_request() {
        let resp = Response::History {
            entries: vec![
                HistoryEntry { entry_id: "firefox.desktop".into(), score: 12.4, n: 47, last_update: 1700000000.0 },
            ],
        };
        assert_eq!(round_trip_response(&resp), resp);
    }

    #[test]
    fn providers_result_round_trip_request() {
        let resp = Response::Providers {
            entries: vec![
                ProviderEntry { id: "desktop".into(), prefixes: vec![] },
                ProviderEntry { id: "calculator".into(), prefixes: vec!["=".into()] },
            ],
        };
        assert_eq!(round_trip_response(&resp), resp);
    }

    #[test]
    fn read_empty_stream() {
        let empty: &[u8] = b"";
        let mut reader = BufReader::new(empty);
        let msg: Option<Request> = read_message(&mut reader).unwrap();
        assert_eq!(msg, None);
    }

    #[test]
    fn read_multiple_messages() {
        let req1 = Request::Query { query: "f".into(), offset: 0, length: 20 };
        let req2 = Request::Select { query: "fi".into(), entry_id: "firefox.desktop".into() };
        let mut buf = Vec::new();
        write_message(&mut buf, &req1).unwrap();
        write_message(&mut buf, &req2).unwrap();

        let mut reader = BufReader::new(&buf[..]);
        let m1: Request = read_message(&mut reader).unwrap().unwrap();
        let m2: Request = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(m1, req1);
        assert_eq!(m2, req2);
    }
}
