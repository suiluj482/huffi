use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Messages a `huffi` invocation sends to the resident instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlRequest {
    /// Show the window and type this query into the entry box.
    Show { query: String },
    /// Hide the window if it is visible.
    Hide,
    /// Show the window if hidden, hide it if visible.
    Toggle { query: Option<String> },
    /// Exit the main loop and quit the resident.
    Quit,
    /// Report the current state (answered with a `ControlResponse`).
    Status,
}

/// The reply the resident sends for a `ControlRequest::Status`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ControlResponse {
    pub visible: bool,
}

/// Connect to the resident instance, send `req`, and report whether a
/// resident answered.
pub fn send_request(path: &Path, req: &ControlRequest) -> anyhow::Result<bool> {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return Ok(false);
    };
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    write_message(&mut stream, req)?;
    Ok(true)
}

/// Ask the resident for its state. Returns `None` when no instance answers.
pub fn request_status(path: &Path) -> anyhow::Result<Option<ControlResponse>> {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return Ok(None);
    };
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    write_message(&mut stream, &ControlRequest::Status)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream);
    Ok(read_message(&mut reader)?)
}

/// Tell the resident to exit. Returns whether an instance answered.
pub fn quit_running_instance(path: &Path) -> anyhow::Result<bool> {
    send_request(path, &ControlRequest::Quit)
}

/// Bind the single-instance socket. Returns `None` if a live instance already
/// holds it.
pub fn bind(path: &Path) -> anyhow::Result<Option<UnixListener>> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(Some(listener)),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Ok(None);
            }
            fs::remove_file(path)?;
            Ok(Some(UnixListener::bind(path)?))
        }
        Err(e) => Err(e.into()),
    }
}

/// Read one `ControlRequest` from an incoming connection.
pub fn read_request(stream: &UnixStream) -> anyhow::Result<Option<ControlRequest>> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream);
    Ok(read_message(&mut reader)?)
}

/// Write a response back to the requesting client (used for `Status`).
pub fn write_response(stream: &mut UnixStream, resp: &ControlResponse) -> io::Result<()> {
    write_message(stream, resp)
}

fn write_message<W: Write>(writer: &mut W, msg: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_message<R: Read, T: for<'de> Deserialize<'de>>(
    reader: &mut BufReader<R>,
) -> io::Result<Option<T>> {
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
    use std::path::PathBuf;

    use super::*;

    fn temp_socket_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "huffi-control-{}-{tag}-{nanos}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn send_request_reaches_resident() {
        let path = temp_socket_path("send");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            read_request(&stream).unwrap().unwrap()
        });
        let sent = send_request(
            &path,
            &ControlRequest::Show {
                query: "fire".into(),
            },
        )
        .unwrap();
        assert!(sent);
        match server.join().unwrap() {
            ControlRequest::Show { query } => assert_eq!(query, "fire"),
            other => panic!("unexpected request: {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn request_status_returns_visibility() {
        let path = temp_socket_path("status");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let req = read_request(&stream).unwrap().unwrap();
            assert!(matches!(req, ControlRequest::Status));
            write_response(&mut stream, &ControlResponse { visible: true }).unwrap();
        });
        let resp = request_status(&path).unwrap().expect("status response");
        assert!(resp.visible);
        server.join().unwrap();
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn no_instance_reports_absent() {
        let path = temp_socket_path("absent");
        assert!(!send_request(&path, &ControlRequest::Quit).unwrap());
        assert!(request_status(&path).unwrap().is_none());
    }

    #[test]
    fn bind_reclaims_stale_socket() {
        let path = temp_socket_path("stale");
        let listener = UnixListener::bind(&path).unwrap();
        drop(listener);
        let bound = bind(&path).unwrap();
        assert!(bound.is_some());
        drop(bound);
        let _ = fs::remove_file(&path);
    }
}
