//! Selection actions and how they are carried out. Providers describe an
//! [`Action`] on each entry; the engine performs it in [`Action::perform`].

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::QuerySuggestion;
use crate::engine::config::ExternalConfig;

/// How an [`Action::Exec`] launches its args.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// Spawn the args directly, no wrapper.
    Direct,
    /// Append the args to the configured `terminal` wrapper; the terminal
    /// closes once the command exits.
    Terminal,
    /// Append the args to the configured `terminal_hold` wrapper; the
    /// terminal stays open after the command exits, so its output remains
    /// readable.
    TerminalHold,
}

impl ExecMode {
    /// The wrapper argv this mode prepends, `None` for [`Direct`](Self::Direct).
    fn wrapper(self, external: &ExternalConfig) -> Option<Vec<String>> {
        match self {
            ExecMode::Direct => None,
            ExecMode::Terminal => Some(external.terminal.clone()),
            ExecMode::TerminalHold => Some(external.terminal_hold.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run `args` as a program, wrapped according to `mode` — directly, in
    /// the configured terminal, or in a terminal that stays open. `cwd` is
    /// the directory to run in; when `None`, resolution falls back to the
    /// configured `working_dir`, then the user's home.
    Exec {
        args: Vec<String>,
        mode: ExecMode,
        cwd: Option<PathBuf>,
    },
    /// Copy `value` to the clipboard on selection. The clipboard binary is
    /// resolved from config when the action is performed.
    Clipboard { value: String },
    /// Run `args` on selection and pipe its stdout into the clipboard tool's
    /// stdin. The clipboard binary is resolved from config when the action is
    /// performed. Use this for content that is too large for argv or is not
    /// text (e.g. a decoded image).
    ClipboardCommand { args: Vec<String> },
    /// Replace the query with `suggestion` on selection and keep the
    /// launcher open — the Enter-triggered equivalent of tab-applying a
    /// suggestion. Nothing is spawned; the UI applies the suggestion, so
    /// `select` still records the launch and runs `Provider::handle` like
    /// any other selection.
    SetQuery { suggestion: QuerySuggestion },
    /// Do nothing; the default for entries that never set an action.
    NoOp,
}

impl Action {
    /// The argv to spawn for this action, resolving external binaries
    /// (terminal wrapper, clipboard tool) from `external`. `None` when
    /// there is nothing to run.
    fn argv(&self, external: &ExternalConfig) -> Option<Vec<String>> {
        let args: Vec<String> = match self {
            Action::Exec { args, mode, .. } => {
                let mut cmd = mode.wrapper(external).unwrap_or_default();
                cmd.extend(args.iter().cloned());
                cmd
            }
            Action::Clipboard { value } => {
                vec![external.clipboard.clone(), value.clone()]
            }
            Action::ClipboardCommand { .. } | Action::SetQuery { .. } | Action::NoOp => {
                return None;
            }
        };
        (!args.is_empty()).then_some(args)
    }

    /// The suggestion a [`SetQuery`](Self::SetQuery) action applies, or
    /// `None` for every other action.
    pub fn query_suggestion(&self) -> Option<&QuerySuggestion> {
        match self {
            Action::SetQuery { suggestion } => Some(suggestion),
            _ => None,
        }
    }

    /// The entry's own working directory, if it has one.
    fn cwd(&self) -> Option<&Path> {
        match self {
            Action::Exec { cwd, .. } => cwd.as_deref(),
            _ => None,
        }
    }

    /// Perform the action, resolving external binaries (terminal wrapper,
    /// clipboard tool) from `external`.
    pub fn perform(&self, external: &ExternalConfig) {
        if let Action::ClipboardCommand { args } = self {
            perform_clipboard_command(args, external);
            return;
        }
        let Some(args) = self.argv(external) else {
            return;
        };
        let Some(program) = args.first() else {
            return;
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut cmd = Command::new(program);
        cmd.args(&args[1..])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = resolve_cwd(self.cwd(), external, home.as_deref()) {
            cmd.current_dir(dir);
        }
        if let Err(e) = cmd.spawn() {
            eprintln!("failed to launch {program}: {e}");
        }
    }
}

/// Run `args` and stream its stdout into the configured clipboard tool.
///
/// Unlike [`Action::Clipboard`], this never puts the payload in argv: it
/// works for arbitrarily large text and for binary data such as images. The
/// clipboard tool reads the payload from stdin, which is the default when it
/// is spawned without arguments.
fn perform_clipboard_command(args: &[String], external: &ExternalConfig) {
    let Some((program, rest)) = args.split_first() else {
        return;
    };
    let mut source = Command::new(program);
    source
        .args(rest)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut source = match source.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("failed to launch {program}: {e}");
            return;
        }
    };

    let stdin = source.stdout.take().map_or_else(Stdio::null, Stdio::from);
    let mut clipboard = Command::new(&external.clipboard);
    clipboard
        .stdin(stdin)
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(e) = clipboard.spawn() {
        eprintln!("failed to launch {}: {e}", external.clipboard);
        let _ = source.kill();
        let _ = source.wait();
    }
}

/// Expand a leading `~/` against `home`, returning any other path as-is.
fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = home
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

/// The directory to spawn an action in: the entry's own cwd (e.g. a
/// desktop file's `Path=`), else the configured `working_dir`, else the
/// user's home — the first candidate that exists. `None` means huffi's own
/// cwd is inherited. Explicit candidates that don't exist are reported
/// rather than silently skipped.
fn resolve_cwd(
    entry_cwd: Option<&Path>,
    external: &ExternalConfig,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let entry = entry_cwd.map(|p| expand_tilde(p, home));
    let configured = external
        .working_dir
        .as_deref()
        .map(|p| expand_tilde(p, home));
    let home = home.map(Path::to_path_buf);

    for (candidate, explicit) in [(entry, true), (configured, true), (home, false)] {
        let Some(dir) = candidate else { continue };
        if dir.is_dir() {
            return Some(dir);
        }
        if explicit {
            eprintln!(
                "huffi: working directory {} does not exist, trying the next candidate",
                dir.display()
            );
        }
    }
    None
}
pub fn split_command(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in s.chars() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                in_quotes = false;
            }
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::config::ExternalConfig;
    use crate::engine::provider::entry;

    #[test]
    fn action_argv_resolves_the_configured_wrappers() {
        let external = ExternalConfig::default();
        let exec = Action::Exec {
            args: vec!["ls".into()],
            mode: ExecMode::Direct,
            cwd: None,
        };
        assert_eq!(exec.argv(&external), Some(vec!["ls".to_string()]));

        let terminal = Action::Exec {
            args: vec!["ls".into()],
            mode: ExecMode::Terminal,
            cwd: None,
        };
        assert_eq!(
            terminal.argv(&external),
            Some(vec![
                "kitty".to_string(),
                "--".to_string(),
                "ls".to_string()
            ])
        );

        let hold = Action::Exec {
            args: vec!["sh".into(), "-c".into(), "ls".into()],
            mode: ExecMode::TerminalHold,
            cwd: None,
        };
        assert_eq!(
            hold.argv(&external),
            Some(vec![
                "kitty".to_string(),
                "--hold".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "ls".to_string(),
            ])
        );

        let clipboard = Action::Clipboard { value: "x".into() };
        assert_eq!(
            clipboard.argv(&external),
            Some(vec!["wl-copy".to_string(), "x".to_string()])
        );

        assert_eq!(
            Action::ClipboardCommand {
                args: vec!["cat".into()]
            }
            .argv(&external),
            None,
            "a clipboard command is spawned as a pipeline, not as one argv"
        );

        assert_eq!(Action::NoOp.argv(&external), None);
        assert_eq!(
            Action::SetQuery {
                suggestion: QuerySuggestion::new("42")
            }
            .argv(&external),
            None,
            "a query suggestion spawns nothing"
        );
        assert_eq!(
            Action::Exec {
                args: vec![],
                mode: ExecMode::Direct,
                cwd: None,
            }
            .argv(&external),
            None
        );
    }

    #[test]
    fn clipboard_command_streams_stdout_into_the_clipboard_tool() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let dir =
            std::env::temp_dir().join(format!("huffi-clipboard-command-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("clipboard.txt");
        let tool = dir.join("clipboard-tool");
        std::fs::write(&tool, format!("#!/bin/sh\ncat > \"{}\"\n", out.display()))
            .expect("write clipboard tool");
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755))
            .expect("chmod clipboard tool");

        let external = ExternalConfig {
            clipboard: tool.to_string_lossy().into_owned(),
            ..ExternalConfig::default()
        };
        Action::ClipboardCommand {
            args: vec!["printf".into(), "hello from cliphist".into()],
        }
        .perform(&external);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if std::fs::read_to_string(&out).is_ok_and(|s| s == "hello from cliphist") {
                break;
            }
            assert!(Instant::now() < deadline, "clipboard tool received no data");
            std::thread::sleep(Duration::from_millis(10));
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    pub fn builders_pick_the_exec_mode() {
        let ls = || vec!["ls".to_string()];
        let direct = entry("id", "title").exec(ls()).score(1.0);
        assert_eq!(
            direct.action,
            Action::Exec {
                args: ls(),
                mode: ExecMode::Direct,
                cwd: None
            }
        );

        let terminal = entry("id", "title").terminal_exec(ls()).score(1.0);
        assert_eq!(
            terminal.action,
            Action::Exec {
                args: ls(),
                mode: ExecMode::Terminal,
                cwd: None
            }
        );

        let hold = entry("id", "title").terminal_hold(ls()).score(1.0);
        assert_eq!(
            hold.action,
            Action::Exec {
                args: ls(),
                mode: ExecMode::TerminalHold,
                cwd: None
            }
        );
    }

    /// `cwd()` is stored on the builder and merged into the exec action in
    /// `build()`, so providers may call it before or after picking a mode.
    #[test]
    fn cwd_merges_into_the_exec_action_in_either_chain_order() {
        let ls = || vec!["ls".to_string()];
        let after = entry("id", "title").exec(ls()).cwd("/tmp").score(1.0);
        let before = entry("id", "title").cwd("/tmp").exec(ls()).score(1.0);
        assert_eq!(after.action, before.action);
        match &after.action {
            Action::Exec { cwd, .. } => assert_eq!(cwd.as_deref(), Some(Path::new("/tmp"))),
            other => panic!("expected exec action, got {other:?}"),
        }
    }

    /// Entry cwd, then configured `working_dir`, then `$HOME`: the first
    /// candidate that exists on disk. Nothing usable means huffi's own cwd
    /// is inherited.
    #[test]
    fn resolve_cwd_walks_entry_config_then_home() {
        let root =
            std::env::temp_dir().join(format!("huffi-cwd-precedence-{}", std::process::id()));
        let entry_dir = root.join("entry");
        let config_dir = root.join("config");
        let home_dir = root.join("home");
        for dir in [&entry_dir, &config_dir, &home_dir] {
            std::fs::create_dir_all(dir).expect("temp cwd dir");
        }
        let missing = root.join("missing");
        let external = |dir: Option<&Path>| ExternalConfig {
            working_dir: dir.map(Path::to_path_buf),
            ..ExternalConfig::default()
        };

        assert_eq!(
            resolve_cwd(
                Some(entry_dir.as_path()),
                &external(Some(config_dir.as_path())),
                Some(home_dir.as_path())
            ),
            Some(entry_dir.clone()),
            "the entry's own cwd wins"
        );
        assert_eq!(
            resolve_cwd(
                Some(missing.as_path()),
                &external(Some(config_dir.as_path())),
                Some(home_dir.as_path())
            ),
            Some(config_dir.clone()),
            "a missing entry cwd falls through to the config"
        );
        assert_eq!(
            resolve_cwd(
                Some(missing.as_path()),
                &external(Some(missing.as_path())),
                Some(home_dir.as_path())
            ),
            Some(home_dir.clone()),
            "missing entry and config fall through to home"
        );
        assert_eq!(
            resolve_cwd(None, &external(None), Some(home_dir.as_path())),
            Some(home_dir.clone()),
            "home alone is enough"
        );
        assert_eq!(
            resolve_cwd(None, &external(None), None),
            None,
            "no candidates: inherit huffi's own cwd"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Both a provider-supplied cwd and the config key accept a leading
    /// `~/`, since neither is written as an absolute path in practice.
    #[test]
    fn resolve_cwd_expands_a_leading_tilde() {
        let home = std::env::temp_dir().join(format!("huffi-cwd-tilde-{}", std::process::id()));
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).expect("temp home dir");

        let external = ExternalConfig {
            working_dir: Some(PathBuf::from("~/projects")),
            ..ExternalConfig::default()
        };
        assert_eq!(
            resolve_cwd(None, &external, Some(home.as_path())),
            Some(projects.clone())
        );
        assert_eq!(
            resolve_cwd(
                Some(Path::new("~/projects")),
                &ExternalConfig::default(),
                Some(home.as_path())
            ),
            Some(projects),
            "entry cwd expands too"
        );

        std::fs::remove_dir_all(&home).ok();
    }
}
