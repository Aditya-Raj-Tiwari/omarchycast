//! Omacast search daemon.
//!
//! Headless by design: the UI is a Quickshell overlay that runs inside the shell
//! process already on screen, so this process owns only the index, the matching
//! and the actions. That keeps the resident cost to a few megabytes.

mod clipboard;
mod config;
mod core;
mod hypr;
mod ipc;
mod launch;
mod providers;

use crate::config::Config;
use crate::core::store::Store;
use crate::core::{Action, Provider, Registry};
use crate::ipc::{Request, Response};
use crate::providers::apps::AppsProvider;
use crate::providers::calc::CalcProvider;
use crate::providers::date::DateProvider;
use crate::providers::notes::NotesProvider;
use notify_debouncer_full::new_debouncer;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Enough rows to scroll through without ever shipping a list nobody reads.
const RESULT_LIMIT: usize = 40;

const USAGE: &str = "\
omacastd — search daemon for the Omacast launcher overlay

USAGE:
    omacastd              run the daemon (the overlay connects over a unix socket)
    omacastd eval EXPR    evaluate EXPR with the calculator and date engines
    omacastd hotkey KEYS  install KEYS as the Hyprland binding, e.g. 'CTRL + SPACE'
    omacastd --help       show this message
";

struct State {
    registry: Registry,
    config: RwLock<Config>,
    notes: Arc<NotesProvider>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => run(),
        Some("eval") => {
            let expr = std::env::args().skip(2).collect::<Vec<_>>().join(" ");
            match providers::calc::eval_once(&expr).or_else(|| providers::date::eval_once(&expr)) {
                Some(result) => println!("{result}"),
                None => {
                    eprintln!("omacastd: no result for {expr:?}");
                    std::process::exit(1);
                }
            }
        }
        Some("hotkey") => {
            let keys = std::env::args().skip(2).collect::<Vec<_>>().join(" ");
            if let Err(e) = hypr::install_hotkey(&keys) {
                eprintln!("omacastd: {e}");
                std::process::exit(1);
            }
            println!("bound {keys} to the launcher");
        }
        Some("--help") | Some("-h") => print!("{USAGE}"),
        Some(other) => {
            eprintln!("omacastd: unknown command '{other}'\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn run() {
    let listener = match ipc::listen() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("omacastd: {e}");
            std::process::exit(1);
        }
    };

    let store = match Store::open() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("omacastd: could not open the frecency store: {e}");
            std::process::exit(1);
        }
    };

    let config = Config::load();
    let apps = AppsProvider::new(store.clone());
    let notes = NotesProvider::new(config.notes_directory());
    let state = Arc::new(State {
        registry: Registry::new(vec![
            CalcProvider::new(),
            DateProvider::new(),
            notes.clone(),
            apps.clone(),
        ]),
        config: RwLock::new(config),
        notes: notes.clone(),
    });

    watch_desktop_files(apps);
    watch_notes(notes);

    // Clean up the socket on Ctrl-C so a restart isn't blocked by a stale file.
    install_signal_handler();

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = state.clone();
        std::thread::spawn(move || {
            ipc::serve_connection(stream, |request| dispatch(&state, request));
        });
    }
}

fn dispatch(state: &State, request: Request) -> Response {
    match request {
        Request::Ping => Response::ok(),

        Request::Query { text } => {
            let Ok(config) = state.config.read() else {
                return Response::error("configuration is unavailable");
            };
            let items = state.registry.query(&text, &config, RESULT_LIMIT);
            Response { items: Some(items), ..Response::ok() }
        }

        Request::Activate { id, action } => {
            match state.registry.activate(&id, Action::parse(&action)) {
                Ok(()) => Response::ok(),
                Err(e) => Response::error(e),
            }
        }

        Request::Config => match state.config.read() {
            Ok(config) => Response { config: Some(config.clone()), ..Response::ok() },
            Err(_) => Response::error("configuration is unavailable"),
        },

        Request::SetConfig { config } => {
            let previous_hotkey = state.config.read().map(|c| c.hotkey.clone()).unwrap_or_default();

            // Rebinding touches the user's Hyprland config, so it only happens when
            // the value actually changed — and a failure there must not lose the
            // rest of the settings.
            let rebind = config.hotkey != previous_hotkey;
            if let Err(e) = config.save() {
                return Response::error(format!("could not save settings: {e}"));
            }
            if let Ok(mut current) = state.config.write() {
                *current = config.clone();
            }
            // A moved notes directory has to be re-indexed before the next query.
            state.notes.set_directory(config.notes_directory());
            if rebind {
                if let Err(e) = hypr::install_hotkey(&config.hotkey) {
                    return Response::error(format!("settings saved, but the hotkey failed: {e}"));
                }
            }
            Response::ok()
        }

        Request::Reindex => {
            for provider in state.registry.providers() {
                provider.reindex();
            }
            Response::ok()
        }
    }
}

/// Re-index when a .desktop file appears, changes or is removed, so a newly
/// installed app is searchable without restarting the daemon.
fn watch_desktop_files(apps: Arc<AppsProvider>) {
    std::thread::spawn(move || {
        let dirs: Vec<_> =
            AppsProvider::watch_paths().into_iter().filter(|p| p.exists()).collect();
        if dirs.is_empty() {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(Duration::from_millis(750), None, tx) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("omacastd: desktop-file watcher unavailable: {e}");
                return;
            }
        };
        for dir in &dirs {
            let _ =
                debouncer.watch(dir, notify_debouncer_full::notify::RecursiveMode::NonRecursive);
        }

        for result in rx {
            if result.is_ok() {
                apps.reindex();
            }
        }
    });
}

/// Watches the notes directory so a note written elsewhere shows up without a
/// restart. The directory may not exist yet, which is not an error.
fn watch_notes(notes: Arc<NotesProvider>) {
    std::thread::spawn(move || {
        let directory = notes.directory();
        if !directory.is_dir() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(Duration::from_millis(750), None, tx) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("omacastd: notes watcher unavailable: {e}");
                return;
            }
        };
        if debouncer
            .watch(&directory, notify_debouncer_full::notify::RecursiveMode::NonRecursive)
            .is_err()
        {
            return;
        }
        for result in rx {
            if result.is_ok() {
                notes.reindex();
            }
        }
    });
}

fn install_signal_handler() {
    extern "C" fn on_signal(_: libc::c_int) {
        crate::ipc::cleanup();
        std::process::exit(0);
    }
    // SAFETY: `signal` with a plain extern "C" handler; the handler only calls
    // async-signal-safe unlink plus _exit via process::exit.
    let handler = on_signal as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}
