use std::fs;
use std::io::{self, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use huffi_protocol::{read_message, write_message};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlRequest {
    Show { query: String },
}

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    runtime.join("huffi-ui-gtk.sock")
}

pub fn notify_running_instance(path: &Path, query: &str) -> anyhow::Result<bool> {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return Ok(false);
    };
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    write_message(
        &mut stream,
        &ControlRequest::Show {
            query: query.to_string(),
        },
    )?;
    Ok(true)
}

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

pub fn read_request(stream: &UnixStream) -> anyhow::Result<Option<ControlRequest>> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream);
    Ok(read_message(&mut reader)?)
}
