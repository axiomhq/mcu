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
mod hover;
mod motion;
mod mpl;
mod params;
mod share;
mod term;
mod ui;
mod viz;

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::app::App;

fn main() -> Result<()> {
    let cli = match parse_cli_args() {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("metrics-tui: {e}");
            print_usage(&mut io::stderr());
            std::process::exit(2);
        }
    };

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

/// Parsed command-line arguments.
#[derive(Debug, Default)]
pub struct CliArgs {
    pub file: Option<std::path::PathBuf>,
    /// User-supplied values for MPL `param $name: type;` declarations.
    /// Sent with each query so the server can resolve them.
    pub params: BTreeMap<String, String>,
    /// Optional dashboard uid to fetch on startup (equivalent to `:open <uid>`).
    pub dashboard: Option<String>,
}

fn parse_cli_args() -> std::result::Result<CliArgs, String> {
    let mut args = std::env::args().skip(1).peekable();
    let mut cli = CliArgs::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage(&mut io::stdout());
                std::process::exit(0);
            }
            "-p" | "--param" => {
                let pair = args
                    .next()
                    .ok_or_else(|| format!("missing argument to {arg}"))?;
                insert_param(&mut cli.params, &pair)?;
            }
            s if s.starts_with("-p=") => insert_param(&mut cli.params, &s[3..])?,
            s if s.starts_with("--param=") => insert_param(&mut cli.params, &s[8..])?,
            "-d" | "--dashboard" => {
                let uid = args
                    .next()
                    .ok_or_else(|| format!("missing argument to {arg}"))?;
                set_dashboard(&mut cli.dashboard, uid)?;
            }
            s if s.starts_with("-d=") => set_dashboard(&mut cli.dashboard, s[3..].to_string())?,
            s if s.starts_with("--dashboard=") => {
                set_dashboard(&mut cli.dashboard, s[12..].to_string())?
            }
            s if s.starts_with('-') => {
                return Err(format!("unknown flag: {s}"));
            }
            _ if cli.file.is_none() => {
                cli.file = Some(std::path::PathBuf::from(arg));
            }
            _ => return Err(format!("unexpected positional argument: {arg}")),
        }
    }
    Ok(cli)
}

fn set_dashboard(
    slot: &mut Option<String>,
    uid: String,
) -> std::result::Result<(), String> {
    let trimmed = uid.trim().to_string();
    if trimmed.is_empty() {
        return Err("empty dashboard uid".to_string());
    }
    if slot.is_some() {
        return Err("--dashboard specified more than once".to_string());
    }
    *slot = Some(trimmed);
    Ok(())
}

fn insert_param(
    params: &mut BTreeMap<String, String>,
    raw: &str,
) -> std::result::Result<(), String> {
    let (k, v) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=VALUE, got `{raw}`"))?;
    let k = k.trim();
    if k.is_empty() {
        return Err(format!("empty parameter name in `{raw}`"));
    }
    // Accept both `$foo` and `foo`; canonicalize to bare names so they
    // match the engine's `param.name` (which excludes the leading `$`).
    let name = k.strip_prefix('$').unwrap_or(k).to_string();
    params.insert(name, v.to_string());
    Ok(())
}

fn print_usage(out: &mut impl io::Write) {
    let _ = writeln!(
        out,
        "usage: metrics-tui [-p NAME=VALUE]... [-d UID] [FILE.mpl]\n\
         \n\
         Options:\n  \
           -p, --param NAME=VALUE   provide a value for an MPL `param` declaration.\n  \
           -d, --dashboard UID      fetch and load a dashboard on startup.\n  \
           -h, --help               show this message.\n\
         \n\
         FILE.mpl is opened on startup. It will be created on `:w` if missing."
    );
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runtime: tokio::runtime::Handle,
    cli: CliArgs,
) -> Result<()> {
    let mut app = App::new(runtime);
    app.cli_params = cli.params;
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
mod cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<CliArgs, String> {
        // Re-implement the body of `parse_cli_args` but read from a slice
        // instead of `std::env::args`. Keeps the test hermetic.
        let mut iter = args.iter().copied().peekable();
        let mut cli = CliArgs::default();
        while let Some(arg) = iter.next() {
            match arg {
                "-h" | "--help" => return Err("help".to_string()),
                "-p" | "--param" => {
                    let pair = iter
                        .next()
                        .ok_or_else(|| format!("missing argument to {arg}"))?;
                    insert_param(&mut cli.params, pair)?;
                }
                s if s.starts_with("-p=") => insert_param(&mut cli.params, &s[3..])?,
                s if s.starts_with("--param=") => insert_param(&mut cli.params, &s[8..])?,
                "-d" | "--dashboard" => {
                    let uid = iter
                        .next()
                        .ok_or_else(|| format!("missing argument to {arg}"))?;
                    set_dashboard(&mut cli.dashboard, uid.to_string())?;
                }
                s if s.starts_with("-d=") => {
                    set_dashboard(&mut cli.dashboard, s[3..].to_string())?
                }
                s if s.starts_with("--dashboard=") => {
                    set_dashboard(&mut cli.dashboard, s[12..].to_string())?
                }
                s if s.starts_with('-') => return Err(format!("unknown flag: {s}")),
                _ if cli.file.is_none() => {
                    cli.file = Some(std::path::PathBuf::from(arg));
                }
                _ => return Err(format!("unexpected positional: {arg}")),
            }
        }
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
        let err = parse(&["--frobnicate"]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn second_positional_errors() {
        let err = parse(&["a.mpl", "b.mpl"]).unwrap_err();
        assert!(err.contains("unexpected positional"));
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
        let err = parse(&["-d"]).unwrap_err();
        assert!(err.contains("missing argument"), "got {err}");
    }

    #[test]
    fn dashboard_flag_empty_value_errors() {
        let err = parse(&["-d", "   "]).unwrap_err();
        assert!(err.contains("empty dashboard uid"), "got {err}");
    }

    #[test]
    fn dashboard_flag_duplicated_errors() {
        let err = parse(&["-d", "one", "--dashboard", "two"]).unwrap_err();
        assert!(err.contains("more than once"), "got {err}");
    }
}
