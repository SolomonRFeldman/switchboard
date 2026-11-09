//! `switchboard tracker name|config|show <ID>`: the ticket tracker, for the
//! skills that file and read tickets (`/shape` and the rest, through
//! `assets/skills/shape/tracker.md`), as the tmux version's `tracker.sh`
//! answered them. It only reads, so Switchboard's permissions let Claude
//! run it without asking, and it never prints a key or token.
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
//! It exits 2 with the tracker setting off, 3 when the tracker has no such
//! ticket, and 1 with what went wrong on stderr.

use std::process::ExitCode;

use serde_json::json;

use crate::integrations::tracker;
use crate::util;

const USAGE: &str = "Usage: switchboard tracker name | config | show <ID>";

/// `switchboard tracker`.
pub fn main(args: Vec<String>) -> ExitCode {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(set) = tracker::set() else {
        if matches!(words.as_slice(), ["name"] | ["config"] | ["show", _]) {
            eprintln!("The ticket tracker is off in Switchboard's Settings (Ticket tracker).");
            return ExitCode::from(2);
        }
        eprintln!("{USAGE}");
        return ExitCode::from(1);
    };
    match words.as_slice() {
        ["name"] => println!("{}", set.name()),
        ["config"] => println!(
            "{}",
            util::json_compact(&tracker::filing().unwrap_or_default())
        ),
        ["show", id] => {
            let Some(id) = crate::views::ticket::id_of(id) else {
                eprintln!("Not a ticket ID: {id}");
                return ExitCode::from(1);
            };
            match util::block_on(tracker::detail(&id, "switchboard tracker show")) {
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
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}
