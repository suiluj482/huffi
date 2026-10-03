use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use huffi::engine::Engine;
use huffi::engine::provider::{Entry, EntryMeta, MetaProvider, TestProvider, entry};
use huffi::engine::scoring::{MatchField, Scored};

static DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

const CONTROL_SOCKET: &str = "/tmp/huffi-int.sock";

struct TestEngine {
    engine: Engine,
    data_dir: PathBuf,
}

impl TestEngine {
    fn new() -> Self {
        let id = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/huffi-int-{}-{}", std::process::id(), id));

        let mut engine = Engine::new(&dir, true).expect("engine failed to open");
        engine
            .add_provider(Box::new(MetaProvider::new(CONTROL_SOCKET, &dir, true)))
            .expect("meta provider");
        engine
            .add_provider(Box::new(TestProvider::with_prefixes(
                "test",
                vec!["~~"],
                test_entries(),
            )))
            .expect("test provider");
        Self {
            engine,
            data_dir: dir,
        }
    }

    fn query(&mut self, query: &str) -> (Option<String>, Vec<Scored<EntryMeta>>, usize) {
        let reply = self.engine.query(query);
        let total = reply.scored.len();
        (reply.pre.prefix.clone(), reply.scored.to_vec(), total)
    }
}

impl Drop for TestEngine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data_dir);
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
        assert!(hit.combined > 0.0);
    }
}

#[test]
fn select_then_query_changes_ranking() {
    let mut engine = TestEngine::new();

    let (_, first_results, _) = engine.query("~~calc");
    assert!(!first_results.is_empty(), "expected results for 'calc'");

    if let Some(top) = first_results.first() {
        for _ in 0..5 {
            engine.engine.select("~~calc", &top.entry.id);
        }
    }

    let (_, second_results, _) = engine.query("~~calc");
    assert!(!second_results.is_empty());
    if let Some(top_before) = first_results.first() {
        assert_eq!(second_results[0].entry.id, top_before.entry.id);
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
        .find(|r| r.entry.id == "meta-socket")
        .expect("meta-socket hit");
    assert_eq!(socket.entry.subtitle.as_deref(), Some(CONTROL_SOCKET));
}

#[test]
fn providers_lists_entries() {
    let engine = TestEngine::new();
    let providers = engine.engine.providers();
    assert!(!providers.is_empty());
    assert!(
        providers
            .iter()
            .any(|e| e.id == "desktop" && e.name == "desktop")
    );
    assert!(
        providers
            .iter()
            .any(|e| e.id == "calculator" && e.name == "calculator")
    );
    assert!(providers.iter().any(|e| e.id == "meta" && e.name == "meta"));
    assert!(providers.iter().any(|e| e.id == "test" && e.name == "test"));
    assert!(
        providers
            .iter()
            .any(|e| e.id == "unicode" && e.name == "unicode")
    );
}

#[test]
fn boost_moves_app_to_top() {
    let mut engine = TestEngine::new();

    let (_, before, _) = engine.query("~~br");
    assert!(before.len() >= 2, "expected at least two results for 'br'");

    let target = before[1]
        .history_key
        .clone()
        .unwrap_or(before[1].entry.id.clone());
    for _ in 0..10 {
        engine.engine.boost("~~br", &target);
    }

    let (_, after, _) = engine.query("~~br");
    let top_key = after[0]
        .history_key
        .clone()
        .unwrap_or(after[0].entry.id.clone());
    assert_eq!(top_key, target);
}

/// The prefix is what puts the Unicode provider in play, and what keeps it
/// out of the way otherwise: `:` on its own must not swallow a query.
#[test]
fn unicode_prefix_is_gated_on_the_colon() {
    let mut engine = TestEngine::new();

    let (prefix, results, _total) = engine.query(":snowman");
    assert_eq!(prefix.as_deref(), Some(":"));
    // The provider hands over every row and the engine ranks them: the row
    // whose CLDR name is exactly what was typed comes first.
    let top = results.first().expect("a row for ':snowman'");
    assert_eq!(top.entry.title, "☃️");
    assert_eq!(top.entry.subtitle.as_deref(), Some("snowman"));
    assert!(top.combined > 0.0, "a named row has to survive scoring");

    // Nothing after the prefix is a placeholder, not an empty list. Other
    // providers still answer an empty query, so this counts the rows that came
    // from the Unicode provider.
    let (_prefix, results, _total) = engine.query(":");
    let unicode: Vec<_> = results
        .iter()
        .filter(|r| r.entry.provider_id.as_deref() == Some("unicode"))
        .collect();
    assert_eq!(
        unicode.len(),
        1,
        "expected just the placeholder: {unicode:?}"
    );
    assert_eq!(unicode[0].entry.id, "unicode-placeholder");
    assert_eq!(unicode[0].entry.title, "Search unicode characters");

    // Without the prefix the provider stays out of it entirely.
    let (prefix, _results, _total) = engine.query("snowman");
    assert_ne!(prefix.as_deref(), Some(":"));
}

/// A code point names a character outright, which the engine cannot fuzzy
/// match, so the row has to reach the list by score instead of vanishing.
#[test]
fn unicode_code_point_query_reaches_the_results() {
    let mut engine = TestEngine::new();
    let (prefix, results, _total) = engine.query(":u+1f600");
    assert_eq!(prefix.as_deref(), Some(":"));
    let top = results.first().expect("a row for ':u+1f600'");
    assert_eq!(top.entry.title, "😀");
    assert!(top.combined > 0.0);
}

/// The provider must not bake its own prefix into a suggestion: a `prefixes`
/// override would otherwise leave every `Tab` pointing at a prefix that no
/// longer triggers anything. This is the regression test for that.
#[test]
fn unicode_suggestions_follow_a_prefix_override() {
    use huffi::engine::config::EngineConfig;
    use huffi::engine::provider::config::ProviderOverride;

    let dir = PathBuf::from("/tmp/huffi-unicode-prefix-override");
    let mut config = EngineConfig::default();
    config.provider.builtin.insert(
        "unicode".to_string(),
        ProviderOverride {
            prefixes: Some(vec!["~".into()]),
            ..Default::default()
        },
    );
    let mut engine = Engine::new_with_config(&dir, true, &config).expect("engine failed to open");

    // A name row suggests its code point, and the override's prefix is the one
    // that ends up in front of it.
    let reply = engine.query("~snowman");
    assert_eq!(reply.pre.prefix.as_deref(), Some("~"));
    let top = reply.scored.first().expect("a row for '~snowman'");
    let suggestion = top.entry.set_query.as_ref().expect("a suggestion");
    assert!(suggestion.keep_prefix, "the prefix must not be baked in");
    assert_eq!(suggestion.query, "u+2603", "the text alone, no prefix");
    assert_eq!(suggestion.resolve(reply.pre.prefix.as_deref()), "~u+2603");

    // And the other way: a code point row suggests the name, behind the same
    // prefix. Both rows have to exist for the round trip to be reachable.
    let reply = engine.query("~u+2603");
    assert_eq!(reply.pre.prefix.as_deref(), Some("~"));
    let top = reply.scored.first().expect("a row for '~u+2603'");
    let suggestion = top.entry.set_query.as_ref().expect("a suggestion");
    assert_eq!(suggestion.query, "snowman");
    assert_eq!(suggestion.resolve(reply.pre.prefix.as_deref()), "~snowman");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Selecting copies the character, and the copy is a launch like any other, so
/// the character drifts up in later queries.
#[test]
fn unicode_select_copies_and_then_ranks_first() {
    use huffi::engine::provider::Action;

    let mut engine = TestEngine::new();

    let (_prefix, before, _) = engine.query(":bee");
    assert!(!before.is_empty(), "expected a row for ':bee'");
    let target = before
        .iter()
        .find(|r| r.entry.title == "🐝")
        .expect("the honeybee emoji")
        .clone();

    // The test engine runs dry, so the action is asserted rather than carried
    // out: what matters is that selecting the row copies the character and
    // not its name or its code point.
    match &target.entry.action {
        Action::Clipboard { value } => assert_eq!(value, "🐝"),
        other => panic!("expected Clipboard action, got {other:?}"),
    }
    assert_eq!(target.history_key.as_deref(), Some("unicode-1f41d"));

    for _ in 0..5 {
        engine.engine.select(":bee", &target.entry.id);
    }

    let (_prefix, after, _) = engine.query(":bee");
    assert_eq!(after[0].entry.id, target.entry.id);
}
