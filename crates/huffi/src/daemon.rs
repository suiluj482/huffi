use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;

use crate::provider::ProviderCollection;
use huffi_protocol::{read_message, write_message, ListEntry, QueryHit, Request, Response};

const BOOST_WEIGHT: f64 = 10.0;
const MAX_RESULTS: usize = 20;

pub struct Daemon {
    provider_collection: ProviderCollection,
}

impl Daemon {
    pub fn new(provider_collection: ProviderCollection) -> Self {
        Self {
            provider_collection,
        }
    }

    pub fn handle(&self, stream: UnixStream) -> anyhow::Result<()> {
        let peer = stream.peer_addr().map(|a| format!("{a:?}")).unwrap_or_default();
        stream.set_nonblocking(false)?;

        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);

        loop {
            let msg: Option<Request> = match read_message(&mut reader) {
                Ok(msg) => msg,
                Err(e) => {
                    eprintln!("[{peer}] read error: {e}");
                    break;
                }
            };

            let Some(req) = msg else {
                break;
            };

            let resp = self.dispatch(req);
            if let Err(e) = write_message(&mut writer, &resp) {
                eprintln!("[{peer}] write error: {e}");
                break;
            }
        }

        Ok(())
    }

    fn dispatch(&self, req: Request) -> Response {
        match req {
            Request::Query { query, offset, length } => self.handle_query(&query, offset, length),
            Request::Select { query, entry_id } => self.handle_select(&query, &entry_id),
            Request::Boost { query, history_key } => self.handle_boost(&query, &history_key),
            Request::Delete { query, history_key } => self.handle_delete(&query, &history_key),
            Request::List { prefix } => self.handle_list(&prefix),
        }
    }

    fn handle_query(&self, query: &str, offset: usize, length: usize) -> Response {
        let scored = self.provider_collection.query(query);
        let total = scored.len();
        let length = length.min(MAX_RESULTS);

        let results: Vec<QueryHit> = scored
            .into_iter()
            .skip(offset)
            .take(length)
            .map(|s| {
                let entry = s.entry;
                QueryHit {
                    entry_id: entry.id,
                    history_key: s.history_key,
                    base_score: s.base_score,
                    history_score: s.history_score,
                    score: s.combined,
                    title: entry.title,
                    subtitle: entry.subtitle,
                    comment: entry.comment,
                    icon: entry.icon,
                    extra: entry.extra,
                }
            })
            .collect();

        Response::QueryResult { results, total }
    }

    fn handle_select(&self, query: &str, entry_id: &str) -> Response {
        self.provider_collection.select(query, entry_id);
        Response::Ok
    }

    fn handle_boost(&self, query: &str, history_key: &str) -> Response {
        self.provider_collection.record_boost(query, history_key, BOOST_WEIGHT);
        Response::Ok
    }

    fn handle_delete(&self, query: &str, history_key: &str) -> Response {
        self.provider_collection.delete(query, history_key);
        Response::Ok
    }

    fn handle_list(&self, prefix: &str) -> Response {
        let raw = self.provider_collection.list_entries(prefix);

        let entries: Vec<ListEntry> = raw
            .into_iter()
            .map(|entry| ListEntry {
                entry_id: entry.key,
                score: entry.record.score,
                n: entry.record.n,
                last_update: entry.record.last_update,
            })
            .collect();

        Response::ListResult { entries }
    }
}
