//! The ticket tracker: the one the `tracker` setting names, or none with it
//! off; one per computer, never two at once. Everything that reads or
//! changes a ticket asks here, as `lib/tracker/index.js` was asked, and each
//! answer comes in Linear's GraphQL shape, so a caller never checks which
//! tracker answered. What only one tracker has (Linear's inbox) is asked of
//! that one alone, by the caller, while it is the one set.
//!
//! With the setting off nothing is asked, but in a program you run yourself
//! (`super::run_by_you`), which asks Linear, as it did before the setting
//! named a tracker.

use anyhow::Result;
use serde_json::Value;

use super::linear;
use crate::net::{ErrorKind, ServiceError};
use crate::settings;

pub use linear::{Person, Ticket};

/// A ticket tracker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracker {
    Linear,
}

impl Tracker {
    /// Its name, as it calls itself.
    pub fn name(self) -> &'static str {
        match self {
            Tracker::Linear => "Linear",
        }
    }
}

/// The tracker the setting names, or `None` with it off.
pub fn set() -> Option<Tracker> {
    match settings::cached("tracker").as_ref().and_then(Value::as_str) {
        Some("off") => None,
        _ => Some(Tracker::Linear),
    }
}

/// The name of the tracker asked (`asked`), for what is said to you, or
/// "the ticket tracker" when none is.
pub fn name() -> &'static str {
    asked().map_or("the ticket tracker", Tracker::name)
}

/// The tracker to ask, as the module's comment says, or `ErrorKind::Off`.
pub fn asked() -> Result<Tracker> {
    match set() {
        Some(tracker) => Ok(tracker),
        None if super::is_run_by_you() => Ok(Tracker::Linear),
        None => Err(off().into()),
    }
}

/// Fails with `ErrorKind::Off` unless `tracker` is the one to ask: what a
/// provider checks before every request of its own.
pub(crate) fn ensure(tracker: Tracker) -> Result<()> {
    if asked()? == tracker {
        return Ok(());
    }
    Err(ServiceError::new(
        ErrorKind::Off,
        tracker.name(),
        format!(
            "{} isn't the ticket tracker set in Settings (Integrations → Ticket tracker), so nothing asks it.",
            tracker.name()
        ),
    )
    .into())
}

fn off() -> ServiceError {
    ServiceError::new(
        ErrorKind::Off,
        "Ticket tracker",
        "The ticket tracker is turned off in Settings (Integrations → Ticket tracker), so nothing asks it.",
    )
}

/// The ticket `identifier` names (`ENG-123`), or `None` when the tracker
/// has no such ticket. `source` says what asked, for the request audit, as
/// in every question here.
pub async fn ticket(identifier: &str, source: &str) -> Result<Option<Ticket>> {
    match asked()? {
        Tracker::Linear => linear::ticket(identifier, source).await,
    }
}

/// The link to the project the ticket is in (a Linear project), or `None`
/// when it is in none; a ticket the tracker has no such one of is
/// `ErrorKind::NotFound`.
pub async fn project_url(identifier: &str, source: &str) -> Result<Option<String>> {
    match asked()? {
        Tracker::Linear => linear::project_url(identifier, source).await,
    }
}

/// The tickets assigned to you that are open, or were finished since
/// `since` (an ISO time), for the Tickets view.
pub async fn assigned(since: &str, source: &str) -> Result<Vec<Value>> {
    match asked()? {
        Tracker::Linear => linear::assigned(since, source).await,
    }
}

/// Everything the ticket view and image show of a ticket, or `None` when
/// the tracker has no such ticket.
pub async fn detail(identifier: &str, source: &str) -> Result<Option<Value>> {
    match asked()? {
        Tracker::Linear => linear::detail(identifier, source).await,
    }
}

/// A ticket's identifier, title, link, branch name and state, or `None`.
pub async fn issue(identifier: &str, source: &str) -> Result<Option<Value>> {
    match asked()? {
        Tracker::Linear => linear::issue(identifier, source).await,
    }
}

/// The tickets `identifiers` name that the tracker has, each with its title,
/// link and state, in no order; the ones it hasn't are left out.
pub async fn issues(identifiers: &[String], source: &str) -> Result<Vec<Value>> {
    match asked()? {
        Tracker::Linear => linear::issues(identifiers, source).await,
    }
}

/// The tracker's search for `term`, its first `limit`.
pub async fn search(term: &str, limit: usize, source: &str) -> Result<Vec<Value>> {
    match asked()? {
        Tracker::Linear => linear::search(term, limit, source).await,
    }
}

/// For the standup note: `changed`, the tickets assigned to you or made by
/// you that changed since `since`, with their state changes, and `started`,
/// the ones you have in progress.
pub async fn standup(since: &str, source: &str) -> Result<Value> {
    match asked()? {
        Tracker::Linear => linear::standup(since, source).await,
    }
}

/// What closing a ticket takes: `(issue, already done, state, state name)`,
/// where `issue` and `state` are what `close_ticket` is given; `None` when
/// the tracker has no such ticket or no way to close it.
pub async fn done_state_of(
    identifier: &str,
    source: &str,
) -> Result<Option<(String, bool, String, String)>> {
    match asked()? {
        Tracker::Linear => linear::done_state_of(identifier, source).await,
    }
}

/// Closes a ticket as `done_state_of` said; whether the tracker says it did.
pub async fn close_ticket(issue: &str, state: &str, source: &str) -> Result<bool> {
    match asked()? {
        Tracker::Linear => linear::close_ticket(issue, state, source).await,
    }
}

/// The ticket identifier a branch (or tab) name holds, in capitals.
pub fn ticket_in(branch: &str) -> Option<String> {
    linear::ticket_in(branch)
}
