mod app;
mod axiom;
mod cache;
mod chart;
mod cmdline_complete;
mod command;
mod completions;
mod config;
mod dashboard;
mod editor;
mod highlight;
mod history;
mod hover;
mod motion;
mod mpl;
mod params;
mod settings;
mod share;
mod term;
mod trace;
mod ui;
mod unit;
mod util;
mod viz;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::app::App;

fn main() -> Result<()> {
    // `parse()` writes help / errors to the right stream and exits
    // with the conventional codes (0 for `--help`/`--version`,
    // 2 for usage errors), so we don't need a manual `try_parse` ladder.
    let mut cli = CliArgs::parse();
    cli.build_params();

    install_panic_hook();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, runtime.handle().clone(), cli);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Install a panic hook that restores the terminal before printing the
/// `color-eyre` panic report. Without the restore step, a panic mid-frame
/// leaves the terminal in raw mode with the alt-screen active — the user
/// sees nothing and has to blindly type `reset`.
fn install_panic_hook() {
    use color_eyre::config::HookBuilder;
    let (panic_hook, eyre_hook) = HookBuilder::default().into_hooks();
    // Install eyre's report handler so any future `eyre::Report` get the
    // pretty rendering too. We can't return its error from main without
    // changing the result type, so swallow the install error — it only
    // ever fires when called twice, which we never do.
    let _ = eyre_hook.install();
    let panic_hook = panic_hook.into_panic_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restore. Errors here are swallowed because
        // we're already on the unwinding path — there's nowhere useful
        // for them to go.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        panic_hook(info);
    }));
}

/// Parsed command-line arguments.
///
/// With no subcommand, `ax [FILE.mpl]` opens an editor (the
/// historical default). The `trace` / `dashboard` subcommands open a
/// resource directly on startup. `infer_subcommands` lets them be
/// abbreviated to any unambiguous prefix (`ax tr <id>`,
/// `ax da <uid>`).
#[derive(Debug, Default, Parser)]
#[command(
    name = "ax",
    version,
    about = "Vim-style TUI editor and dashboard for the Axiom metrics service.",
    long_about = "With no subcommand, FILE.mpl is opened on startup (created on `:w` if missing).\n\
                  Subcommands open a resource directly:\n  \
                  ax trace <id>       open a trace\n  \
                  ax dashboard <uid>  open a dashboard\n\
                  Subcommand names may be abbreviated when unambiguous (e.g. `ax tr <id>`).",
    infer_subcommands = true
)]
pub struct CliArgs {
    /// MPL file to open on startup. Created on `:w` if missing.
    pub file: Option<PathBuf>,

    /// Provide a value for an MPL `param NAME: type;` declaration.
    /// May be repeated. `$NAME=value` and `NAME=value` are both accepted;
    /// values may contain `=`.
    #[arg(
        short = 'p', long = "param",
        value_name = "NAME=VALUE",
        value_parser = parse_param,
        global = true,
    )]
    pub params_kv: Vec<(String, String)>,

    /// Use a specific `[deployments.NAME]` entry from `~/.axiom.toml`.
    /// Overrides the `active_deployments` field for this launch only.
    /// Also supplies the deployment for the `trace` / `dashboard`
    /// subcommands. `global` so it can appear before or after the
    /// subcommand.
    #[arg(
        short = 'D', long = "deployment",
        value_name = "NAME",
        value_parser = parse_deployment_name,
        global = true,
    )]
    pub deployment: Option<String>,

    /// Subcommand to open a resource on startup. `None` runs the
    /// default editor flow over [`Self::file`].
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Holds the assembled `params_kv` as a `BTreeMap` for the rest of
    /// the codebase. Populated by [`CliArgs::params`].
    #[arg(skip)]
    pub params: BTreeMap<String, String>,
}

/// Startup subcommands. Each opens a resource directly instead of the
/// default editor flow. Deployment is taken from the global
/// `-D/--deployment` flag.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open a trace by id on startup (like `:trace <id>`).
    Trace {
        /// Trace id (hex).
        #[arg(value_name = "ID", value_parser = parse_nonempty_id)]
        id: String,
        /// Trace dataset to search. Defaults to the saved
        /// `:trace set dataset=…` value when omitted.
        #[arg(long, value_name = "NAME", value_parser = parse_nonempty_id)]
        dataset: Option<String>,
    },
    /// Open a dashboard by uid on startup (like `:open <uid>`).
    Dashboard {
        /// Dashboard uid.
        #[arg(value_name = "UID", value_parser = parse_nonempty_id)]
        id: String,
    },
}

impl CliArgs {
    /// Build a fresh [`BTreeMap`] from the accumulated `-p` flags.
    /// Later flags override earlier ones — matches the historical
    /// behaviour of the hand-rolled parser.
    pub fn build_params(&mut self) {
        self.params = self.params_kv.iter().cloned().collect();
    }
}

/// Parse a single `NAME=VALUE` (or `$NAME=VALUE`) pair. Returns the
/// canonical bare name (matching the MPL engine's `param.name`, which
/// excludes the leading `$`).
fn parse_param(raw: &str) -> std::result::Result<(String, String), String> {
    let (k, v) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=VALUE, got `{raw}`"))?;
    let k = k.trim();
    if k.is_empty() {
        return Err(format!("empty parameter name in `{raw}`"));
    }
    let name = k.strip_prefix('$').unwrap_or(k).to_string();
    Ok((name, v.to_string()))
}

/// Validate an id/uid argument — must be non-empty after trimming.
fn parse_nonempty_id(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err("empty id".to_string());
    }
    Ok(trimmed)
}

/// Validate a deployment name — must be non-empty after trimming.
/// Resolution against `~/.axiom.toml` happens later in `Config::select`,
/// which surfaces a clear error if the name doesn't exist there.
fn parse_deployment_name(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err("empty deployment name".to_string());
    }
    Ok(trimmed)
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runtime: tokio::runtime::Handle,
    cli: CliArgs,
) -> Result<()> {
    let mut app = App::new(runtime);
    app.params.cli = cli.params;
    app.deployment_override = cli.deployment.clone();

    // Subcommands open a resource directly; `None` runs the default
    // editor flow. The resource fetches kick off *before* bootstrap
    // so a cache-hit can seed state before any auto-run, and they
    // suppress the saved-query auto-run (the async response drives
    // the view instead).
    match cli.command {
        Some(Command::Trace { id, dataset }) => {
            // Deployment comes from the global `-D` flag; dataset from
            // `--dataset` or the saved `:trace` default.
            if let Err(e) = app.start_trace_fetch(id, dataset, cli.deployment.clone()) {
                app.set_error(format!("trace: {e}"));
            }
            app.bootstrap_skip_initial_query();
        }
        Some(Command::Dashboard { id }) => {
            app.fetch_dashboard_by_uid(id);
            app.bootstrap_skip_initial_query();
        }
        None => {
            // CLI file argument takes precedence over the session
            // cache. When the file doesn't exist yet, we still set it
            // as the current file so `:w` creates it.
            if let Some(path) = cli.file {
                if path.exists() {
                    if let Err(e) = app.open_file(path.clone()) {
                        app.set_error(format!("open failed: {e}"));
                    }
                } else {
                    app.current_file = Some(path.clone());
                    app.saved_buffer = String::new();
                    app.status = format!("new file: {}", path.display());
                }
            }
            app.bootstrap();
        }
    }

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        app.drain_events();

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            app.on_key(key);
        }
    }

    // Safety net: in case the loop exited without going through the `q` path.
    app.persist_query();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Run the real production parser against an argv slice (clap
    /// expects argv[0] to be the program name).
    fn parse(args: &[&str]) -> std::result::Result<CliArgs, String> {
        let mut argv = vec!["ax"];
        argv.extend_from_slice(args);
        let mut cli = CliArgs::try_parse_from(argv).map_err(|e| e.to_string())?;
        cli.build_params();
        Ok(cli)
    }

    #[test]
    fn file_only() {
        let cli = parse(&["q.mpl"]).unwrap();
        assert_eq!(
            cli.file.as_deref().map(|p| p.to_str().unwrap()),
            Some("q.mpl")
        );
        assert!(cli.params.is_empty());
    }

    #[test]
    fn one_param() {
        let cli = parse(&["-p", "host=db-01"]).unwrap();
        assert_eq!(cli.params.get("host").map(String::as_str), Some("db-01"));
    }

    #[test]
    fn multiple_params_with_file_in_any_order() {
        let cli = parse(&[
            "-p",
            "host=db-01",
            "q.mpl",
            "--param=region=us-east",
            "-p=window=1h",
        ])
        .unwrap();
        assert_eq!(
            cli.file.as_deref().map(|p| p.to_str().unwrap()),
            Some("q.mpl")
        );
        assert_eq!(cli.params.get("host").map(String::as_str), Some("db-01"));
        assert_eq!(
            cli.params.get("region").map(String::as_str),
            Some("us-east")
        );
        assert_eq!(cli.params.get("window").map(String::as_str), Some("1h"));
    }

    #[test]
    fn dollar_prefix_is_stripped() {
        let cli = parse(&["-p", "$host=db-01"]).unwrap();
        assert_eq!(cli.params.get("host").map(String::as_str), Some("db-01"));
    }

    #[test]
    fn value_with_equals_kept_intact() {
        // Only split on the FIRST `=`; values may contain `=`.
        let cli = parse(&["-p", "q=a=b=c"]).unwrap();
        assert_eq!(cli.params.get("q").map(String::as_str), Some("a=b=c"));
    }

    #[test]
    fn missing_equals_errors() {
        let err = parse(&["-p", "host"]).unwrap_err();
        assert!(err.contains("NAME=VALUE"), "got {err}");
    }

    #[test]
    fn empty_name_errors() {
        let err = parse(&["-p", "=val"]).unwrap_err();
        assert!(err.contains("empty parameter name"), "got {err}");
    }

    #[test]
    fn unknown_flag_errors() {
        // clap surfaces its own wording for unknown args; match the
        // canonical substring rather than the historical phrasing.
        let err = parse(&["--frobnicate"]).unwrap_err().to_lowercase();
        assert!(
            err.contains("unexpected") || err.contains("unknown"),
            "got {err}"
        );
    }

    #[test]
    fn second_positional_errors() {
        // The first token is the file positional; a second bare token
        // is rejected — clap now routes it through the subcommand
        // matcher, so the wording is "unrecognized subcommand".
        let err = parse(&["a.mpl", "b.mpl"]).unwrap_err().to_lowercase();
        assert!(
            err.contains("unexpected")
                || err.contains("argument")
                || err.contains("unrecognized subcommand"),
            "got {err}"
        );
    }

    /// Helper: extract the parsed subcommand.
    fn cmd(args: &[&str]) -> Command {
        parse(args).unwrap().command.expect("subcommand parsed")
    }

    #[test]
    fn trace_subcommand_basic() {
        match cmd(&["trace", "abc123"]) {
            Command::Trace { id, dataset } => {
                assert_eq!(id, "abc123");
                assert_eq!(dataset, None);
            }
            other => panic!("expected Trace, got {other:?}"),
        }
    }

    #[test]
    fn trace_subcommand_with_dataset() {
        match cmd(&["trace", "abc123", "--dataset", "axiom-traces-staging"]) {
            Command::Trace { id, dataset } => {
                assert_eq!(id, "abc123");
                assert_eq!(dataset.as_deref(), Some("axiom-traces-staging"));
            }
            other => panic!("expected Trace, got {other:?}"),
        }
    }

    #[test]
    fn dashboard_subcommand_basic() {
        match cmd(&["dashboard", "xyz"]) {
            Command::Dashboard { id } => assert_eq!(id, "xyz"),
            other => panic!("expected Dashboard, got {other:?}"),
        }
    }

    #[test]
    fn subcommands_match_unambiguous_prefixes() {
        // `infer_subcommands`: any unambiguous prefix resolves.
        assert!(matches!(cmd(&["tr", "id"]), Command::Trace { .. }));
        assert!(matches!(cmd(&["t", "id"]), Command::Trace { .. }));
        assert!(matches!(cmd(&["da", "id"]), Command::Dashboard { .. }));
        assert!(matches!(cmd(&["d", "id"]), Command::Dashboard { .. }));
    }

    #[test]
    fn deployment_flag_is_global_across_subcommands() {
        // `-D` works before or after the subcommand.
        let cli = parse(&["-D", "prod", "trace", "abc"]).unwrap();
        assert_eq!(cli.deployment.as_deref(), Some("prod"));
        assert!(matches!(cli.command, Some(Command::Trace { .. })));
        let cli = parse(&["trace", "abc", "-D", "prod"]).unwrap();
        assert_eq!(cli.deployment.as_deref(), Some("prod"));
    }

    #[test]
    fn trace_subcommand_missing_id_errors() {
        let err = parse(&["trace"]).unwrap_err().to_lowercase();
        assert!(
            err.contains("required") || err.contains("<id>") || err.contains("id"),
            "got {err}"
        );
    }

    #[test]
    fn trace_subcommand_empty_id_errors() {
        let err = parse(&["trace", "   "]).unwrap_err();
        assert!(err.contains("empty id"), "got {err}");
    }

    #[test]
    fn file_positional_still_works_with_subcommands_present() {
        // A non-subcommand first token is the file positional, not an
        // "unrecognized subcommand" error.
        let cli = parse(&["q.mpl"]).unwrap();
        assert_eq!(
            cli.file.as_deref().map(|p| p.to_str().unwrap()),
            Some("q.mpl")
        );
        assert!(cli.command.is_none());
    }

    #[test]
    fn deployment_flag_short_and_long() {
        let cli = parse(&["-D", "prod"]).unwrap();
        assert_eq!(cli.deployment.as_deref(), Some("prod"));
        let cli = parse(&["--deployment=staging"]).unwrap();
        assert_eq!(cli.deployment.as_deref(), Some("staging"));
    }

    #[test]
    fn deployment_flag_missing_value_errors() {
        let err = parse(&["-D"]).unwrap_err().to_lowercase();
        assert!(
            err.contains("requires") || err.contains("missing") || err.contains("value"),
            "got {err}"
        );
    }

    #[test]
    fn deployment_flag_empty_value_errors() {
        let err = parse(&["-D", "   "]).unwrap_err();
        assert!(err.contains("empty deployment name"), "got {err}");
    }

    #[test]
    fn deployment_flag_default_is_none() {
        // Backwards-compatible default: no flag, no override, falls
        // through to `active_deployments` in the config file.
        let cli = parse(&[]).unwrap();
        assert!(cli.deployment.is_none());
    }
}
