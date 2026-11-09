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

use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::Value;

use super::{jira, linear};
use crate::net::{ErrorKind, ServiceError};
use crate::settings;

pub use linear::{Person, Ticket};

/// A ticket tracker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracker {
    Linear,
    Jira,
}

impl Tracker {
    /// Its name, as it calls itself.
    pub fn name(self) -> &'static str {
        match self {
            Tracker::Linear => "Linear",
            Tracker::Jira => "Jira",
        }
    }

    /// Its value in the setting.
    pub fn key(self) -> &'static str {
        match self {
            Tracker::Linear => "linear",
            Tracker::Jira => "jira",
        }
    }
}

/// The tracker the setting names, or `None` with it off.
pub fn set() -> Option<Tracker> {
    match settings::cached("tracker").as_ref().and_then(Value::as_str) {
        Some("off") => None,
        Some("jira") => Some(Tracker::Jira),
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
        Tracker::Jira => jira::ticket(identifier, source).await,
    }
}

/// The link to the project the ticket is in (a Linear project), or `None`
/// when it is in none; a ticket the tracker has no such one of is
/// `ErrorKind::NotFound`.
pub async fn project_url(identifier: &str, source: &str) -> Result<Option<String>> {
    match asked()? {
        Tracker::Linear => linear::project_url(identifier, source).await,
        Tracker::Jira => jira::project_url(identifier, source).await,
    }
}

/// The tickets assigned to you that are open, or were finished since
/// `since` (an ISO time), for the Tickets view.
pub async fn assigned(since: &str, source: &str) -> Result<Vec<Value>> {
    match asked()? {
        Tracker::Linear => linear::assigned(since, source).await,
        Tracker::Jira => jira::assigned(since, source).await,
    }
}

/// Everything the ticket view and image show of a ticket, or `None` when
/// the tracker has no such ticket.
pub async fn detail(identifier: &str, source: &str) -> Result<Option<Value>> {
    match asked()? {
        Tracker::Linear => linear::detail(identifier, source).await,
        Tracker::Jira => jira::detail(identifier, source).await,
    }
}

/// A ticket's identifier, title, link, branch name and state, or `None`.
pub async fn issue(identifier: &str, source: &str) -> Result<Option<Value>> {
    match asked()? {
        Tracker::Linear => linear::issue(identifier, source).await,
        Tracker::Jira => jira::issue(identifier, source).await,
    }
}

/// The tickets `identifiers` name that the tracker has, each with its title,
/// link and state, in no order; the ones it hasn't are left out.
pub async fn issues(identifiers: &[String], source: &str) -> Result<Vec<Value>> {
    match asked()? {
        Tracker::Linear => linear::issues(identifiers, source).await,
        Tracker::Jira => jira::issues(identifiers, source).await,
    }
}

/// The tracker's search for `term`, its first `limit`.
pub async fn search(term: &str, limit: usize, source: &str) -> Result<Vec<Value>> {
    match asked()? {
        Tracker::Linear => linear::search(term, limit, source).await,
        Tracker::Jira => jira::search(term, limit, source).await,
    }
}

/// For the standup note: `changed`, the tickets assigned to you or made by
/// you that changed since `since`, with their state changes, and `started`,
/// the ones you have in progress.
pub async fn standup(since: &str, source: &str) -> Result<Value> {
    match asked()? {
        Tracker::Linear => linear::standup(since, source).await,
        Tracker::Jira => jira::standup(since, source).await,
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
        Tracker::Jira => jira::done_state_of(identifier, source).await,
    }
}

/// Closes a ticket as `done_state_of` said; whether the tracker says it did.
pub async fn close_ticket(issue: &str, state: &str, source: &str) -> Result<bool> {
    match asked()? {
        Tracker::Linear => linear::close_ticket(issue, state, source).await,
        Tracker::Jira => jira::close_ticket(issue, state, source).await,
    }
}

/// The ticket identifier a branch (or tab) name holds, in capitals.
pub fn ticket_in(branch: &str) -> Option<String> {
    linear::ticket_in(branch)
}

/// A ticket link's identifier, as written: a Linear issue's
/// (`/issue/ABC-123/...`) or a Jira one's (`/browse/ABC-123`, or a board's
/// `?selectedIssue=ABC-123`). The pattern holds one group, the identifier,
/// so a caller can put it inside a longer one.
pub const LINK_ID: &str = r"(?:/issue/|/browse/|[?&]selectedIssue=)([A-Za-z][A-Za-z0-9]*-[0-9]+)";

/// The identifier a ticket link names, in capitals, from either tracker's
/// link (`LINK_ID`).
pub fn id_in_link(text: &str) -> Option<String> {
    static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(LINK_ID).unwrap());
    ID.captures(text).map(|m| m[1].to_ascii_uppercase())
}

/// The identifier when `url` is a ticket's page in the tracker set: a
/// Linear issue's on linear.app, or with Jira a page on its site (its
/// origin, `jira::site`), so a link elsewhere that only looks like one is
/// left to be read as a page.
pub fn own_link(url: &str) -> Option<String> {
    own_link_of(url, set(), jira::site().as_deref())
}

/// `own_link` with the tracker set and Jira's site given, reading nothing.
pub fn own_link_of(url: &str, set: Option<Tracker>, jira_site: Option<&str>) -> Option<String> {
    static LINEAR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^https://linear\.app/[^/]+/issue/([A-Za-z][A-Za-z0-9]*-[0-9]+)").unwrap()
    });
    match set? {
        Tracker::Linear => LINEAR.captures(url).map(|m| m[1].to_ascii_uppercase()),
        Tracker::Jira => {
            let rest = url.strip_prefix(jira_site?)?;
            rest.starts_with(['/', '?'])
                .then(|| id_in_link(rest))
                .flatten()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_link_of_either_tracker_names_its_ticket() {
        for (link, id) in [
            ("https://linear.app/acme/issue/eng-42/fix-it", "ENG-42"),
            ("https://acme.atlassian.net/browse/AB-7", "AB-7"),
            (
                "https://acme.atlassian.net/jira/software/projects/AB/boards/1?selectedIssue=AB-8",
                "AB-8",
            ),
            (
                "https://acme.atlassian.net/issues/?jql=x&selectedIssue=ab-9",
                "AB-9",
            ),
        ] {
            assert_eq!(id_in_link(link).as_deref(), Some(id), "{link}");
        }
        assert_eq!(
            id_in_link("https://acme.atlassian.net/jira/your-work"),
            None
        );
        assert_eq!(id_in_link("AB-7"), None, "an ID alone isn't a link");
    }

    #[test]
    fn only_the_set_tracker_s_own_links_are_its_tickets() {
        let linear = "https://linear.app/acme/issue/eng-4/x";
        let site = Some("https://acme.atlassian.net");
        let jira = "https://acme.atlassian.net/browse/AB-7";
        assert_eq!(
            own_link_of(linear, Some(Tracker::Linear), site).as_deref(),
            Some("ENG-4")
        );
        assert_eq!(own_link_of(jira, Some(Tracker::Linear), site), None);
        assert_eq!(
            own_link_of(jira, Some(Tracker::Jira), site).as_deref(),
            Some("AB-7")
        );
        assert_eq!(own_link_of(linear, Some(Tracker::Jira), site), None);
        assert_eq!(
            own_link_of(
                "https://acme.atlassian.net.evil.test/browse/AB-7",
                Some(Tracker::Jira),
                site
            ),
            None,
            "a host that only starts like the site's"
        );
        assert_eq!(own_link_of(jira, None, site), None, "the tracker off");
    }
}
