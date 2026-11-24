//! The ticket trackers, Linear and Jira Cloud: what reads or changes a
//! ticket asks here, naming which, and each answer comes in Linear's
//! GraphQL shape, so a caller never reads two shapes. Which one keeps a
//! repository's tickets is its `tickets` in repos.json (`repos::TICKETS`'s
//! `linear` or `jira`), a link's tracker is its site's, and what isn't one
//! repository's (the Tickets view, the standup note, search) asks each that
//! is on in Settings. What only one has (Linear's inbox, its projects) is
//! asked of it alone.
//!
//! Each refuses while its setting (`linear`, `jira`) is off, but in a
//! program you run yourself (`super::run_by_you`), as `super::ensure_on`
//! decides.

use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::Value;

use super::{jira, linear};
use crate::settings;

pub use linear::{Person, Ticket};

/// A ticket tracker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tracker {
    Linear,
    Jira,
}

/// Both trackers, Linear first.
pub const ALL: [Tracker; 2] = [Tracker::Linear, Tracker::Jira];

impl Tracker {
    /// Its name, as it calls itself.
    pub fn name(self) -> &'static str {
        match self {
            Tracker::Linear => "Linear",
            Tracker::Jira => "Jira",
        }
    }

    /// Its setting, and its value of repos.json's `tickets`.
    pub fn key(self) -> &'static str {
        match self {
            Tracker::Linear => "linear",
            Tracker::Jira => "jira",
        }
    }

    /// The tracker a setting or `tickets` value names.
    pub fn of(key: &str) -> Option<Tracker> {
        ALL.into_iter().find(|t| t.key() == key)
    }

    /// Whether its setting is on.
    pub fn is_on(self) -> bool {
        settings::is_on(self.key())
    }
}

/// The trackers on in Settings, Linear first.
pub fn on() -> Vec<Tracker> {
    ALL.into_iter().filter(|t| t.is_on()).collect()
}

/// Asks each tracker on at once, with `ask`: each answer with its tracker,
/// Linear's first, those that failed left out; every failure, with its
/// tracker, only when every one asked failed (none on is no answer).
pub async fn each_on<T, F, Fut>(ask: F) -> Result<Vec<(Tracker, T)>, Vec<(Tracker, anyhow::Error)>>
where
    F: Fn(Tracker) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let asked = on();
    let answers = futures_util::future::join_all(asked.iter().map(|t| ask(*t))).await;
    let (mut got, mut failed) = (Vec::new(), Vec::new());
    for (t, answer) in asked.into_iter().zip(answers) {
        match answer {
            Ok(found) => got.push((t, found)),
            Err(e) => failed.push((t, e)),
        }
    }
    if got.is_empty() && !failed.is_empty() {
        return Err(failed);
    }
    Ok(got)
}

/// The tracker a card or ticket names as the one it was read from (its
/// `tracker`, as `mark` writes it), Linear for one that names none, read
/// before cards named theirs.
pub fn of_card(card: &Value) -> Tracker {
    named_by(card).unwrap_or(Tracker::Linear)
}

/// The tracker a card names, if any: an Inbox notification's names none.
pub fn named_by(card: &Value) -> Option<Tracker> {
    card.get("tracker")
        .and_then(Value::as_str)
        .and_then(Tracker::of)
}

/// Marks a card or ticket as read from `tracker`, for `of_card`.
pub fn mark(card: &mut Value, tracker: Tracker) {
    card["tracker"] = Value::from(tracker.key());
}

/// The tracker that keeps the tickets of the repository at `dir`, by its
/// `tickets` read from the files alone (`repos::tickets_known`), when that
/// is one of the two.
pub fn of_dir(dir: &Path) -> Option<Tracker> {
    crate::repos::tickets_known(dir).and_then(|t| Tracker::of(&t))
}

/// The tracker a bare ticket ID typed in `dir` names: Jira in a repository
/// whose tickets are in Jira, else Linear, as an ID meant before Jira.
pub fn for_id_in(dir: &Path) -> Tracker {
    of_dir(dir).unwrap_or(Tracker::Linear)
}

/// The ticket `identifier` names (`ENG-123`), or `None` when the tracker
/// has no such ticket. `source` says what asked, for the request audit, as
/// in every question here.
pub async fn ticket(tracker: Tracker, identifier: &str, source: &str) -> Result<Option<Ticket>> {
    match tracker {
        Tracker::Linear => linear::ticket(identifier, source).await,
        Tracker::Jira => jira::ticket(identifier, source).await,
    }
}

/// The link to the project the ticket is in (a Linear project, a Jira
/// epic), or `None` when it is in none; a ticket the tracker has no such
/// one of is `ErrorKind::NotFound`.
pub async fn project_url(
    tracker: Tracker,
    identifier: &str,
    source: &str,
) -> Result<Option<String>> {
    match tracker {
        Tracker::Linear => linear::project_url(identifier, source).await,
        Tracker::Jira => jira::project_url(identifier, source).await,
    }
}

/// The open tickets assigned to you, Linear's the newest first and Jira's
/// the latest updated first, each with its identifier, title and state.
pub async fn open_assigned(tracker: Tracker, source: &str) -> Result<Vec<Value>> {
    match tracker {
        Tracker::Linear => linear::open_assigned(source).await,
        Tracker::Jira => jira::open_assigned(source).await,
    }
}

/// The tickets assigned to you that are open, or were finished since
/// `since` (an ISO time), for the Tickets view.
pub async fn assigned(tracker: Tracker, since: &str, source: &str) -> Result<Vec<Value>> {
    match tracker {
        Tracker::Linear => linear::assigned(since, source).await,
        Tracker::Jira => jira::assigned(since, source).await,
    }
}

/// Everything the ticket view and image show of a ticket, or `None` when
/// the tracker has no such ticket.
pub async fn detail(tracker: Tracker, identifier: &str, source: &str) -> Result<Option<Value>> {
    match tracker {
        Tracker::Linear => linear::detail(identifier, source).await,
        Tracker::Jira => jira::detail(identifier, source).await,
    }
}

/// A ticket's identifier, title, link, branch name and state, or `None`.
pub async fn issue(tracker: Tracker, identifier: &str, source: &str) -> Result<Option<Value>> {
    match tracker {
        Tracker::Linear => linear::issue(identifier, source).await,
        Tracker::Jira => jira::issue(identifier, source).await,
    }
}

/// The tickets `identifiers` name that the tracker has, each with its title,
/// link and state, in no order; the ones it hasn't are left out.
pub async fn issues(tracker: Tracker, identifiers: &[String], source: &str) -> Result<Vec<Value>> {
    match tracker {
        Tracker::Linear => linear::issues(identifiers, source).await,
        Tracker::Jira => jira::issues(identifiers, source).await,
    }
}

/// The tracker's search for `term`, its first `limit`, each with its
/// description.
pub async fn search(
    tracker: Tracker,
    term: &str,
    limit: usize,
    source: &str,
) -> Result<Vec<Value>> {
    match tracker {
        Tracker::Linear => linear::search(term, limit, source).await,
        Tracker::Jira => jira::search(term, limit, source).await,
    }
}

/// For the standup note: `changed`, the tickets assigned to you or made by
/// you that changed since `since`, with their state changes, and `started`,
/// the ones you have in progress.
pub async fn standup(tracker: Tracker, since: &str, source: &str) -> Result<Value> {
    match tracker {
        Tracker::Linear => linear::standup(since, source).await,
        Tracker::Jira => jira::standup(since, source).await,
    }
}

/// What closing a ticket takes: `(issue, already done, state, state name)`,
/// where `issue` and `state` are what `close_ticket` is given; `None` when
/// the tracker has no such ticket or no way to close it.
pub async fn done_state_of(
    tracker: Tracker,
    identifier: &str,
    source: &str,
) -> Result<Option<(String, bool, String, String)>> {
    match tracker {
        Tracker::Linear => linear::done_state_of(identifier, source).await,
        Tracker::Jira => jira::done_state_of(identifier, source).await,
    }
}

/// Closes a ticket as `done_state_of` said; whether the tracker says it did.
pub async fn close_ticket(
    tracker: Tracker,
    issue: &str,
    state: &str,
    source: &str,
) -> Result<bool> {
    match tracker {
        Tracker::Linear => linear::close_ticket(issue, state, source).await,
        Tracker::Jira => jira::close_ticket(issue, state, source).await,
    }
}

/// Where the skills file a new ticket in the tracker: `tracker` (its name),
/// `site` (Jira's site, or the Linear workspace's page), `team` (the Linear
/// team or Jira project key) and `labels`, never a key or token.
pub fn filing(tracker: Tracker) -> Value {
    let mut out = match tracker {
        Tracker::Linear => linear::filing(),
        Tracker::Jira => jira::filing(),
    };
    out["tracker"] = Value::from(tracker.name());
    out
}

/// A ticket link's identifier, as written: a Linear issue's
/// (`/issue/ABC-123/...`) or a Jira one's (`/browse/ABC-123`, or a board's
/// `?selectedIssue=ABC-123`). The pattern holds one group, the identifier,
/// so a caller can put it inside a longer one.
pub const LINK_ID: &str = r"(?:/issue/|/browse/|[?&]selectedIssue=)([A-Za-z][A-Za-z0-9]*-[0-9]+)";

/// A ticket's link as written in text, either tracker's on any site:
/// `https://`, its host, path and query up to `LINK_ID`, whose group, the
/// identifier, is the pattern's one. For showing a link by its ticket (a
/// PR's text, a card, a meeting's); which tracker's a link is, and whether
/// it is a ticket's at all, is `link_ticket`'s to say, by the site.
pub fn link_pattern() -> String {
    format!(r#"https://[\w.-]+(?:/[^\s"<>()?]*?)?(?:\?[^\s"<>()]*?)?{LINK_ID}"#)
}

/// The identifier a ticket link names, in capitals, from either tracker's
/// link (`LINK_ID`).
pub fn id_in_link(text: &str) -> Option<String> {
    static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(LINK_ID).unwrap());
    ID.captures(text).map(|m| m[1].to_ascii_uppercase())
}

/// The tracker and identifier when `url` is a ticket's page: a Linear
/// issue's on linear.app, or a Jira one's on the site jira.json names (its
/// origin, `jira::site`), so a link elsewhere that only looks like one is
/// left to be read as a page.
pub fn link_ticket(url: &str) -> Option<(Tracker, String)> {
    // Jira's site is read only for what isn't Linear's, so a Linear link
    // (and a unit test) reads no jira.json for it.
    link_ticket_of(url, jira::site)
}

/// Refuses a ticket's link (`LINK_ID`) that neither tracker's site has
/// (`link_ticket`), since its ID would name another ticket, saying why
/// (`foreign_link_says`).
pub fn refuse_foreign(text: &str) -> Result<(), String> {
    match foreign_link(text) {
        true => Err(foreign_link_says(text)),
        false => Ok(()),
    }
}

/// What is said of a link `refuse_foreign` refuses: that Jira isn't set up
/// when it has no site to tell its links by, else that it is neither
/// tracker's.
pub fn foreign_link_says(text: &str) -> String {
    if jira::site().is_none() && !text.contains("linear.app") {
        return format!("{text} isn't a Linear link. {}", jira::SET_UP);
    }
    format!("{text} isn't a link to a ticket in Linear or on your Jira site")
}

fn foreign_link(text: &str) -> bool {
    text.starts_with("http") && id_in_link(text).is_some() && link_ticket(text).is_none()
}

/// `link_ticket` with Jira's site given, asked for only when needed.
pub fn link_ticket_of(
    url: &str,
    jira_site: impl FnOnce() -> Option<String>,
) -> Option<(Tracker, String)> {
    static LINEAR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^https://linear\.app/[^/]+/issue/([A-Za-z][A-Za-z0-9]*-[0-9]+)").unwrap()
    });
    if let Some(m) = LINEAR.captures(url) {
        return Some((Tracker::Linear, m[1].to_ascii_uppercase()));
    }
    if !url.starts_with("http") || id_in_link(url).is_none() {
        return None;
    }
    let site = jira_site()?;
    let rest = url.strip_prefix(site.as_str())?;
    rest.starts_with(['/', '?'])
        .then(|| id_in_link(rest))
        .flatten()
        .map(|id| (Tracker::Jira, id))
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
    fn a_ticket_link_in_text_is_found_on_any_site() {
        let pattern = Regex::new(&link_pattern()).unwrap();
        let id = |text: &str| pattern.captures(text).map(|m| m[1].to_string());
        for (text, want) in [
            ("see https://linear.app/acme/issue/ENG-4/fix-it.", "ENG-4"),
            ("https://acme.atlassian.net/browse/AB-7", "AB-7"),
            ("https://jira.acme.test/browse/AB-8", "AB-8"),
            (
                "https://acme.atlassian.net/jira/software/projects/AB/boards/1?selectedIssue=AB-9",
                "AB-9",
            ),
            (
                "https://acme.atlassian.net/issues/?jql=x&selectedIssue=AB-10",
                "AB-10",
            ),
        ] {
            assert_eq!(id(text).as_deref(), Some(want), "{text}");
        }
        assert_eq!(id("https://github.com/acme/widgets/issues/12"), None);
        assert_eq!(id("AB-7"), None, "an ID alone isn't a link");
    }

    #[test]
    fn a_link_s_tracker_is_its_site_s() {
        let site = || Some("https://acme.atlassian.net".to_string());
        assert_eq!(
            link_ticket_of("https://linear.app/acme/issue/eng-4/x", site),
            Some((Tracker::Linear, "ENG-4".into()))
        );
        assert_eq!(
            link_ticket_of("https://acme.atlassian.net/browse/AB-7", site),
            Some((Tracker::Jira, "AB-7".into()))
        );
        assert_eq!(
            link_ticket_of(
                "https://acme.atlassian.net/jira/software/projects/AB/boards/1?selectedIssue=AB-8",
                site
            ),
            Some((Tracker::Jira, "AB-8".into()))
        );
        assert_eq!(
            link_ticket_of("https://acme.atlassian.net.evil.test/browse/AB-7", site),
            None,
            "a host that only starts like the site's"
        );
        assert_eq!(
            link_ticket_of("https://other.atlassian.net/browse/AB-7", site),
            None,
            "another site's"
        );
        assert_eq!(
            link_ticket_of("https://acme.atlassian.net/browse/AB-7", || None),
            None,
            "no Jira site set"
        );
        assert_eq!(link_ticket_of("AB-7", site), None, "an ID is no link");
    }

    #[test]
    fn a_link_neither_tracker_has_is_foreign() {
        // The unit tests read no jira.json, so no Jira site is set.
        assert!(foreign_link("https://acme.atlassian.net/browse/AB-2"));
        assert!(!foreign_link("https://linear.app/acme/issue/AB-2/a-title"));
        assert!(!foreign_link("AB-2"), "an ID is no link");
        assert!(!foreign_link("https://example.com/a-page"));
        assert!(
            refuse_foreign("https://acme.atlassian.net/browse/AB-2")
                .unwrap_err()
                .contains("Jira isn't set up")
        );
        assert_eq!(refuse_foreign("AB-2"), Ok(()));
    }

    #[test]
    fn a_setting_or_tickets_value_names_its_tracker() {
        assert_eq!(Tracker::of("jira"), Some(Tracker::Jira));
        assert_eq!(Tracker::of("linear"), Some(Tracker::Linear));
        assert_eq!(Tracker::of("github"), None);
    }
}
