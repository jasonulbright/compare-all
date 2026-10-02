//! `ca`: the console front end.
//!
//! The program runs a script or a quick comparison itself and opens no window.
//! A command line that asks for a view is read in full and reported, so the
//! desktop program can act on the same switches later.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use ca_cli::args::Invocation;
use ca_cli::{exit, help, quick, request::DesktopRequest, script};

#[derive(Parser)]
#[command(name = "ca", version = env!("CA_VERSION"))]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compare two text files line by line.
    Text { left: PathBuf, right: PathBuf },
    /// Compare two folders by name and size.
    Folder { left: PathBuf, right: PathBuf },
}

fn run(cli: Cli) -> anyhow::Result<bool> {
    match cli.cmd {
        Cmd::Text { left, right } => {
            let l = std::fs::read_to_string(&left)?;
            let r = std::fs::read_to_string(&right)?;
            let hunks = ca_diff::diff_lines(&l, &r);
            let mut differs = false;
            for h in &hunks {
                if h.kind != ca_diff::HunkKind::Same {
                    differs = true;
                    println!("{:?} L{:?} R{:?}", h.kind, h.left, h.right);
                }
            }
            Ok(!differs)
        }
        Cmd::Folder { left, right } => {
            let l = ca_fs::scan(&left)?;
            let r = ca_fs::scan(&right)?;
            let mut differs = false;
            for (p, e) in &l {
                match r.get(p) {
                    None => {
                        differs = true;
                        println!("left only  {}", p.display());
                    }
                    Some(o) if o.size != e.size => {
                        differs = true;
                        println!("size diff  {}", p.display());
                    }
                    Some(_) => {}
                }
            }
            for p in r.keys().filter(|p| !l.contains_key(*p)) {
                differs = true;
                println!("right only {}", p.display());
            }
            Ok(!differs)
        }
    }
}

/// The two subcommands that came before the switch based command line.
fn is_subcommand(first: &str) -> bool {
    matches!(first, "text" | "folder")
}

fn legacy() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::from(exit::SUCCESS),
        Ok(false) => ExitCode::from(exit::BINARY_DIFFERENT),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(exit::UNKNOWN_ERROR)
        }
    }
}

/// Report a command line that asks for a view.
fn report_desktop(request: &DesktopRequest) {
    if !request.silent {
        for name in &request.desktop_only {
            eprintln!("/{name} is accepted and left for the desktop program");
        }
        eprintln!("this program opens no window; the desktop program opens the view");
    }
    for path in &request.paths {
        println!("path {}", path.display());
    }
    if let Some(view) = &request.file_viewer {
        println!("view {view}");
    }
    if let Some(filters) = &request.filters {
        println!("filters {filters}");
    }
}

fn run_desktop(request: &DesktopRequest) -> u8 {
    if request.automerge.enabled {
        let Some(left) = request.paths.first() else {
            eprintln!("automatic merge needs left and right input files");
            return exit::UNKNOWN_ERROR;
        };
        let Some(right) = request.paths.get(1) else {
            eprintln!("automatic merge needs left and right input files");
            return exit::UNKNOWN_ERROR;
        };
        let output = request.merge_output.clone().or_else(|| {
            request
                .paths
                .get(3)
                .filter(|path| !path.as_os_str().is_empty())
                .cloned()
        });
        let center = request
            .paths
            .get(2)
            .filter(|path| !path.as_os_str().is_empty())
            .cloned();
        let paths = ca_view_merge::jobs::MergePaths {
            left: left.clone(),
            center,
            right: right.clone(),
            output,
        };
        let switches = ca_view_merge::automerge::Switches {
            favor_left: request.automerge.favor_left,
            favor_right: request.automerge.favor_right,
            force: request.automerge.force,
            ignore_unimportant: request.automerge.ignore_unimportant,
            review_conflicts: request.automerge.review_conflicts,
        };
        return match ca_view_merge::automerge::run(
            &paths,
            &ca_session::settings::TextMergeSettings::default(),
            switches,
        ) {
            ca_view_merge::automerge::Finish::Done { code, message } => {
                eprintln!("{message}");
                u8::try_from(code).unwrap_or(exit::UNKNOWN_ERROR)
            }
            ca_view_merge::automerge::Finish::Review => {
                eprintln!(
                    "merge conflicts need review in the desktop program; no output was written"
                );
                exit::CONFLICTS_NO_OUTPUT
            }
        };
    }

    report_desktop(request);
    eprintln!("the requested desktop operation was not run");
    exit::UNKNOWN_ERROR
}

fn main() -> ExitCode {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if arguments
        .first()
        .and_then(|first| first.to_str())
        .is_some_and(is_subcommand)
    {
        return legacy();
    }
    if arguments.is_empty() {
        print!("{}", help::text());
        return ExitCode::from(exit::SUCCESS);
    }
    match ca_cli::args::parse_os(arguments) {
        Ok(Invocation::Help) => {
            print!("{}", help::text());
            ExitCode::from(exit::SUCCESS)
        }
        Ok(Invocation::Version) => {
            println!("{}", env!("CA_VERSION"));
            ExitCode::from(exit::SUCCESS)
        }
        Ok(Invocation::Script(request)) => ExitCode::from(script::run(&request)),
        Ok(Invocation::Quick(request)) => ExitCode::from(quick::compare(&request)),
        Ok(Invocation::Desktop(request)) => ExitCode::from(run_desktop(&request)),
        Err(error) => {
            eprintln!("{error}");
            eprintln!("run ca --help for the switches this program knows");
            ExitCode::from(exit::UNKNOWN_ERROR)
        }
    }
}
