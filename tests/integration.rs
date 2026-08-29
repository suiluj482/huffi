use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use huffi::engine::Engine;
use huffi::engine::provider::{Entry, MetaProvider, TestProvider, entry};
use huffi::engine::scoring::MatchField;

static DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

const CONTROL_SOCKET: &str = "/tmp/huffi-int.sock";

struct TestEngine {
    engine: Engine,
    data_file: PathBuf,
}

impl TestEngine {
    fn new() -> Self {
        let id = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/huffi-int-{}-{}", std::process::id(), id));
        let data_file = dir.join("history.json");

        let mut engine = Engine::new(&data_file, true).expect("engine failed to open");
        engine.add_provider(Box::new(MetaProvider::new(
            CONTROL_SOCKET,
            &data_file,
            true,
        )));
        engine.add_provider(Box::new(TestProvider::with_prefixes(
            "test",
            vec!["~~"],
            test_entries(),
        )));
        Self { engine, data_file }
    }

    fn query(&mut self, query: &str) -> (Option<String>, Vec<huffi::engine::QueryHit>, usize) {
        self.engine.query_hits(query, 0, 20)
    }
}

impl Drop for TestEngine {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.data_file);
        if let Some(dir) = self.data_file.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn test_entries() -> Vec<Entry> {
    vec![
        entry("firefox.desktop", "Firefox")
            .comment("Browse the World Wide Web")
            .icon("firefox")
            .history_key("firefox.desktop")
            .exec(vec!["firefox".into()])
            .match_fields(vec![MatchField {
                text: "Firefox".into(),
                weight: 1.0,
            }]),
        entry("org.gnome.Calculator.desktop", "Calculator")
            .icon("accessories-calculator")
            .history_key("org.gnome.Calculator.desktop")
            .exec(vec!["gnome-calculator".into()])
            .match_fields(vec![MatchField {
                text: "Calculator".into(),
                weight: 1.0,
            }]),
        entry("brave-browser.desktop", "Brave Browser")
            .history_key("brave-browser.desktop")
            .exec(vec!["brave-browser".into()])
            .match_fields(vec![MatchField {
                text: "Brave Browser".into(),
                weight: 1.0,
            }]),
        entry("breeze.desktop", "Breeze")
            .history_key("breeze.desktop")
            .exec(vec!["breeze".into()])
            .match_fields(vec![MatchField {
                text: "Breeze".into(),
                weight: 1.0,
            }]),
    ]
}

#[test]
fn query_returns_results() {
    let mut engine = TestEngine::new();
    let (_prefix, results, total) = engine.query("~~fire");
    assert!(!results.is_empty(), "expected some results for 'fire'");
    assert!(total >= results.len());
    for hit in &results {
        assert!(hit.score > 0.0);
    }
}

#[test]
fn select_then_query_changes_ranking() {
    let mut engine = TestEngine::new();

    let (_, first_results, _) = engine.query("~~calc");
    assert!(!first_results.is_empty(), "expected results for 'calc'");

    if let Some(top) = first_results.first() {
        for _ in 0..5 {
            engine.engine.select("~~calc", &top.entry_id);
        }
    }

    let (_, second_results, _) = engine.query("~~calc");
    assert!(!second_results.is_empty());
    if let Some(top_before) = first_results.first() {
        assert_eq!(second_results[0].entry_id, top_before.entry_id);
    }
}

#[test]
fn query_reports_active_prefix() {
    let mut engine = TestEngine::new();
    let (prefix, results, _total) = engine.query("= 2 + 2");
    assert_eq!(prefix.as_deref(), Some("="));
    assert!(
        !results.is_empty(),
        "expected calculator result for '= 2 + 2'"
    );
}

#[test]
fn meta_provider_answers_at_prefix() {
    let mut engine = TestEngine::new();
    let (prefix, results, _total) = engine.query("@socket");
    assert_eq!(prefix.as_deref(), Some("@"));
    let socket = results
        .iter()
        .find(|r| r.entry_id == "meta-socket")
        .expect("meta-socket hit");
    assert_eq!(socket.subtitle.as_deref(), Some(CONTROL_SOCKET));
}

#[test]
fn providers_lists_entries() {
    let engine = TestEngine::new();
    let providers = engine.engine.providers();
    assert!(!providers.is_empty());
    assert!(providers.iter().any(|e| e.id == "desktop"));
    assert!(providers.iter().any(|e| e.id == "calculator"));
    assert!(providers.iter().any(|e| e.id == "meta"));
    assert!(providers.iter().any(|e| e.id == "test"));
}

#[test]
fn boost_moves_app_to_top() {
    let mut engine = TestEngine::new();

    let (_, before, _) = engine.query("~~br");
    assert!(before.len() >= 2, "expected at least two results for 'br'");

    let target = before[1]
        .history_key
        .clone()
        .unwrap_or(before[1].entry_id.clone());
    for _ in 0..10 {
        engine.engine.boost("~~br", &target);
    }

    let (_, after, _) = engine.query("~~br");
    let top_key = after[0]
        .history_key
        .clone()
        .unwrap_or(after[0].entry_id.clone());
    assert_eq!(top_key, target);
}
