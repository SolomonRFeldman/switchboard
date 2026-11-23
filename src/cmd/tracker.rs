//! `switchboard tracker name|config|show <ID>`: the ticket tracker, Linear
//! or Jira, of the repository here (repos.json's `tickets`, as `switchboard
//! tickets` prints it), for the skills that file and read tickets (`/shape`
//! and the rest, through `assets/skills/shape/tracker.md`), as the tmux
//! version's `tracker.sh` answered them. It only reads, so Switchboard's
//! permissions let Claude run it without asking, and it never prints a key
//! or token.
//!
//! ```text
//! switchboard tracker name        Linear or Jira
//! switchboard tracker config      where tickets are filed, as JSON: tracker,
//!                                 site, team (a Linear team or Jira project
//!                                 key) and labels
//! switchboard tracker show <ID>   the ticket as JSON, { data: { issue } },
//!                                 in Linear's shape
//! ```
//!
//! `--tracker linear|jira` first names the tracker in place of the
//! repository's, and `show` takes a ticket's link too, which names its own.
//! It exits 2 when the repository's tickets aren't kept in either tracker
//! or the tracker is off in Settings, 3 when the tracker has no such
//! ticket, and 1 with what went wrong on stderr. No tmux-version file is
//! behind it beyond `tracker.sh`.

use std::process::ExitCode;

use serde_json::json;

use crate::integrations::tracker::{self, Tracker};
use crate::util;

const USAGE: &str =
    "Usage: switchboard tracker [--tracker linear|jira] name | config | show <ID or link>";

/// `switchboard tracker`.
pub fn main(args: Vec<String>) -> ExitCode {
    let mut words: Vec<&str> = args.iter().map(String::as_str).collect();
    let named = match words.as_slice() {
        ["--tracker", t, ..] => match Tracker::of(t) {
            Some(t) => {
                words.drain(..2);
                Some(t)
            }
            None => {
                eprintln!("{USAGE}");
                return ExitCode::from(1);
            }
        },
        _ => None,
    };
    if !matches!(words.as_slice(), ["name"] | ["config"] | ["show", _]) {
        eprintln!("{USAGE}");
        return ExitCode::from(1);
    }
    // A link names its tracker; else the one named, else the repository's.
    let link = match words.as_slice() {
        ["show", text] => crate::worktree::ticket_and_tracker_from(text),
        _ => None,
    };
    let here = std::env::current_dir().unwrap_or_default();
    let Some(set) = link
        .as_ref()
        .and_then(|(t, _)| *t)
        .or(named)
        .or_else(|| tracker::of_dir(&here))
    else {
        eprintln!(
            "This repository's tickets aren't kept in Linear or Jira: `switchboard tickets` says where they are."
        );
        return ExitCode::from(2);
    };
    if !set.is_on() {
        eprintln!("{} is off in Switchboard's Settings.", set.name());
        return ExitCode::from(2);
    }
    match words.as_slice() {
        ["name"] => println!("{}", set.name()),
        ["config"] => println!("{}", util::json_compact(&tracker::filing(set))),
        ["show", text] => {
            let Some(id) = link.map(|(_, id)| id.to_ascii_uppercase()) else {
                eprintln!("Not a ticket ID or link: {text}");
                return ExitCode::from(1);
            };
            match util::block_on(tracker::detail(set, &id, "switchboard tracker show")) {
                Ok(Some(issue)) => println!(
                    "{}",
                    util::json_compact(&json!({ "data": { "issue": issue } }))
                ),
                Ok(None) => {
                    eprintln!("{} has no ticket {id}.", set.name());
                    return ExitCode::from(3);
                }
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::from(1);
                }
            }
        }
        _ => unreachable!("checked above"),
    }
    ExitCode::SUCCESS
}
