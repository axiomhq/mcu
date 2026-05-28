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
mod share;
mod term;
mod ui;
mod unit;
mod util;
mod viz;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
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
#[derive(Debug, Default, Parser)]
#[command(
    name = "mcu",
    version,
    about = "Vim-style TUI editor and dashboard for the Axiom metrics service.",
    long_about = "FILE.mpl is opened on startup. It will be created on `:w` if missing.\n\
                  Use -d/--dashboard to load a dashboard by uid on startup."
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
    )]
    pub params_kv: Vec<(String, String)>,

    /// Fetch and load a dashboard by uid on startup (equivalent to `:open <uid>`).
    #[arg(
        short = 'd', long = "dashboard",
        value_name = "UID",
        value_parser = parse_dashboard_uid,
    )]
    pub dashboard: Option<String>,

    /// Use a specific `[deployments.NAME]` entry from `~/.axiom.toml`.
    /// Overrides the `active_deployments` field for this launch only.
    #[arg(
        short = 'D', long = "deployment",
        value_name = "NAME",
        value_parser = parse_deployment_name,
    )]
    pub deployment: Option<String>,

    /// Holds the assembled `params_kv` as a `BTreeMap` for the rest of
    /// the codebase. Populated by [`CliArgs::params`].
    #[arg(skip)]
    pub params: BTreeMap<String, String>,
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

/// Validate a dashboard uid — must be non-empty after trimming.
fn parse_dashboard_uid(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err("empty dashboard uid".to_string());
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
    app.deployment_override = cli.deployment;
    // CLI file argument takes precedence over the session cache. When the file
    // does not exist yet, we still set it as the current file so `:w` creates it.
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
    // Order matters: kick off the dashboard fetch *before* bootstrap so
    // the cache-hit path can seed the editor with the dashboard's MPL
    // before bootstrap's auto-run reads `query_text()`. On a cold
    // dashboard cache the fetch is async — we suppress the saved-query
    // auto-run so we don't push results for the wrong MPL into
    // `self.series` while waiting for `DashboardOpened`.
    if let Some(uid) = cli.dashboard {
        app.fetch_dashboard_by_uid(uid);
        app.bootstrap_skip_initial_query();
    } else {
        app.bootstrap();
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
        let mut argv = vec!["mcu"];
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
        let err = parse(&["a.mpl", "b.mpl"]).unwrap_err().to_lowercase();
        assert!(
            err.contains("unexpected") || err.contains("argument"),
            "got {err}"
        );
    }

    #[test]
    fn dashboard_flag_short_and_long() {
        let cli = parse(&["-d", "abc123"]).unwrap();
        assert_eq!(cli.dashboard.as_deref(), Some("abc123"));
        let cli = parse(&["--dashboard=xyz"]).unwrap();
        assert_eq!(cli.dashboard.as_deref(), Some("xyz"));
    }

    #[test]
    fn dashboard_flag_missing_value_errors() {
        let err = parse(&["-d"]).unwrap_err().to_lowercase();
        assert!(
            err.contains("requires") || err.contains("missing") || err.contains("value"),
            "got {err}"
        );
    }

    #[test]
    fn dashboard_flag_empty_value_errors() {
        let err = parse(&["-d", "   "]).unwrap_err();
        assert!(err.contains("empty dashboard uid"), "got {err}");
    }

    #[test]
    fn dashboard_flag_duplicated_errors() {
        // clap rejects a second occurrence of a single-value option;
        // the exact wording is its `the argument ... cannot be used
        // multiple times` text.
        let err = parse(&["-d", "one", "--dashboard", "two"])
            .unwrap_err()
            .to_lowercase();
        assert!(
            err.contains("multiple times") || err.contains("more than once"),
            "got {err}"
        );
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
