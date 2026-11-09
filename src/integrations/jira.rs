//! Jira Cloud, over its REST API (v3): the questions `super::tracker` asks,
//! answered in Linear's GraphQL shape, as the tmux version's
//! `lib/tracker/jira.js` answered them, so a caller never checks which
//! tracker it asked. Where the two differ:
//!
//! - A state's type comes from its status category: To Do is unstarted, In
//!   Progress started, Done completed, or canceled when its resolution or
//!   the status's own name says so (Won't Do, Duplicate, Declined,
//!   Cancelled), since a workflow may set no resolution.
//! - A Linear project is a Jira epic: an issue's project is its parent epic.
//! - Descriptions and comments are Jira's document format, made Markdown by
//!   `super::adf`.
//! - Priorities run Highest (1, urgent) to Lowest (4), as Linear's do.
//! - Jira has no inbox to read, so the Inbox view is Linear's alone.
//! - Closing a ticket is a transition into a Done status, not a state set.
//! - Jira answers a request it can't sign in as it does one for an issue
//!   that isn't there (a 404, an empty search), so before an answer is taken
//!   to be missing tickets the token is checked, again every
//!   `SIGNED_IN_MS`, since the server runs for days and a token can expire.
//!
//! It signs in with an API token and the email of the account that made it:
//! the token `JIRA_API_TOKEN`, else the stored secret `jira-api-token` (the
//! Keychain's, as the tmux version kept it), else `jiraApiToken` in
//! `~/.config/switchboard/jira.json`; the email `JIRA_EMAIL`, else
//! `jiraEmail` there; the site `JIRA_SITE`, else `jiraSite` there, as
//! `https://<yours>.atlassian.net`. What the secret or the file gave is kept
//! ten minutes and forgotten once Jira refuses it. Every call refuses unless
//! Jira is the tracker set (but in a program you run yourself), and is
//! counted in the request audit under Jira, as "REST".

use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::Result;
use regex::Regex;
use serde_json::{Value, json};

use super::adf::markdown_of;
use super::tracker::{self, Person, Ticket, Tracker};
use super::{Credential, Remembered, env_value, stored_secret};
use crate::net::{self, Audit, ErrorKind, Response, ServiceError};
use crate::{paths, util};

const SERVICE: &str = "Jira";

/// Where an API token is made.
pub const TOKENS: &str = "https://id.atlassian.com/manage-profile/security/api-tokens";

/// What to do when Jira isn't set up, for a message or a view.
pub const SET_UP: &str = "Jira isn't set up: make an API token at \
     https://id.atlassian.com/manage-profile/security/api-tokens, and store it with \
     `switchboard platform secret-set jira-api-token` (it reads the token from stdin); then add \"jiraSite\" \
     (https://<yours>.atlassian.net) and \"jiraEmail\" to ~/.config/switchboard/jira.json, \
     or in Settings under Jira.";

fn config(key: &str) -> Option<String> {
    util::read_json_object(paths::config_dir().join("jira.json"))
        .get(key)
        .and_then(Value::as_str)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ── The site ────────────────────────────────────────────────────────────────

/// The site every request goes to: its origin alone, so the address of a
/// board or ticket pasted in still works, and always https, since every
/// request carries the token; only a stand-in on this computer (localhost,
/// as the tests run) may be plain http. `None` when none is set, or what is
/// set isn't an address.
pub fn site() -> Option<String> {
    site_of(&env_value("JIRA_SITE").or_else(|| config("jiraSite"))?)
}

// The origin is built from the host read here, so the host checked is the
// one requests reach: a name and password before an @ are dropped, and a
// host of anything but letters, digits, dots and dashes (with a port) is
// refused rather than guessed at.
fn site_of(value: &str) -> Option<String> {
    let (scheme, rest) = match value.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("https".to_string(), value),
    };
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let had_userinfo = authority.contains('@');
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, Some(port))
        }
        Some((host, "")) => (host, None),
        _ => (host_port, None),
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return None;
    }
    let local = (host == "localhost" || host == "127.0.0.1") && !had_userinfo;
    let scheme = if local { scheme.as_str() } else { "https" };
    Some(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// A ticket's page on the site, worked out without asking.
pub fn url_of(key: &str) -> Option<String> {
    Some(format!("{}/browse/{key}", site()?))
}

// ── Signing in ──────────────────────────────────────────────────────────────

static REMEMBERED: Remembered = Remembered::new();

struct SignIn {
    site: String,
    email: String,
    token: Credential,
}

async fn token() -> Option<Credential> {
    if let Some(value) = env_value("JIRA_API_TOKEN") {
        return Some(Credential {
            value,
            from: "JIRA_API_TOKEN".to_string(),
        });
    }
    if let Some(known) = REMEMBERED.get() {
        return Some(known);
    }
    let stored = stored_secret("jira-api-token")
        .await
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let found = match stored {
        Some(value) => Credential {
            value,
            from: "the stored secret jira-api-token".to_string(),
        },
        None => Credential {
            value: config("jiraApiToken")?,
            from: "jira.json".to_string(),
        },
    };
    REMEMBERED.set(&found);
    Some(found)
}

async fn sign_in() -> Result<SignIn> {
    let missing = || ServiceError::new(ErrorKind::NoCredentials, SERVICE, SET_UP);
    let token = token().await.ok_or_else(missing)?;
    let email = env_value("JIRA_EMAIL")
        .or_else(|| config("jiraEmail"))
        .ok_or_else(missing)?;
    let site = site().ok_or_else(missing)?;
    Ok(SignIn { site, email, token })
}

/// Forgets the token the secret or the file gave, and that it worked, so
/// the next request looks again and checks it.
pub fn forget_token() {
    REMEMBERED.forget();
    SIGNED_IN_AT.store(0, Ordering::Relaxed);
}

/// Whether a token, email and site are all there to sign in with.
pub async fn has_credentials() -> bool {
    sign_in().await.is_ok()
}

// ── Requests ────────────────────────────────────────────────────────────────

/// Jira's answer to one request, or an error of the kind it is: a missing
/// issue `ErrorKind::NotFound`, a refused token `Unauthorized`, saying it may
/// have expired, since Atlassian's tokens do after at most a year.
async fn request(method: &str, path: &str, body: Option<&Value>, source: &str) -> Result<Value> {
    tracker::ensure(Tracker::Jira)?;
    let sign_in = sign_in().await?;
    let auth = format!(
        "Basic {}",
        base64(format!("{}:{}", sign_in.email, sign_in.token.value).as_bytes())
    );
    let headers = [("Authorization", auth.as_str())];
    let audit = Audit {
        service: SERVICE,
        protocol: "REST",
        source,
    };
    let url = format!("{}{path}", sign_in.site);
    let response = net::request_json(method, &url, &headers, body, audit).await?;
    if response.ok() {
        return Ok(if response.body.is_null() {
            json!({})
        } else {
            response.body
        });
    }
    Err(jira_error(&response, &sign_in.token).into())
}

fn jira_error(response: &Response, token: &Credential) -> ServiceError {
    if response.status == 401 {
        REMEMBERED.forget();
        return ServiceError::new(
            ErrorKind::Unauthorized,
            SERVICE,
            format!(
                "Jira refused the API token from {}: it may have expired. Make a new one at \
                 {TOKENS} and store it with `switchboard platform secret-set jira-api-token` (it reads \
                 the token from stdin).",
                token.from
            ),
        );
    }
    let said = response
        .body
        .pointer("/errorMessages/0")
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .body
                .get("errors")
                .and_then(Value::as_object)
                .and_then(|e| e.values().next())
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .unwrap_or_else(|| format!("Jira answered {}", response.status));
    let kind = match response.status {
        404 => ErrorKind::NotFound,
        429 => ErrorKind::RateLimited {
            reset_at: response
                .header("retry-after")
                .and_then(|v| v.trim().parse::<i64>().ok())
                .map(|secs| util::now_ms() / 1000 + secs),
        },
        _ => ErrorKind::Failed,
    };
    ServiceError::new(kind, SERVICE, format!("Jira: {said}"))
}

/// How long a check that the token works holds.
const SIGNED_IN_MS: i64 = 10 * 60 * 1000;
static SIGNED_IN_AT: AtomicI64 = AtomicI64::new(0);

// Checks the token works, as the module's comment says, so an answer that
// looks like missing tickets is taken as one only when it does.
async fn signed_in(source: &str) -> Result<()> {
    if util::now_ms() - SIGNED_IN_AT.load(Ordering::Relaxed) <= SIGNED_IN_MS {
        return Ok(());
    }
    request("GET", "/rest/api/3/myself", None, source).await?;
    SIGNED_IN_AT.store(util::now_ms(), Ordering::Relaxed);
    Ok(())
}

// An answer, or `None` for an issue that isn't there, once the token is
// known to work.
async fn or_none(answer: Result<Value>, source: &str) -> Result<Option<Value>> {
    match answer {
        Ok(value) => Ok(Some(value)),
        Err(e) if net::kind_of(&e) == Some(&ErrorKind::NotFound) => {
            signed_in(source).await?;
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

// A path segment as a URL carries it: anything but letters, digits and
// `-._~` percent-encoded, as encodeURIComponent did.
fn encoded(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ── Linear's shape ──────────────────────────────────────────────────────────

// A Done status or resolution that means the work was dropped, not done.
static CANCELED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)won.?t|cancel|duplicate|declin|reject|obsolete").unwrap());

fn text<'a>(v: &'a Value, path: &str) -> &'a str {
    v.pointer(path).and_then(Value::as_str).unwrap_or("")
}

fn state_of(fields: &Value) -> Value {
    let kind = match text(fields, "/status/statusCategory/key") {
        "done" => {
            let words = format!(
                "{} {}",
                text(fields, "/resolution/name"),
                text(fields, "/status/name")
            );
            if CANCELED.is_match(&words) {
                "canceled"
            } else {
                "completed"
            }
        }
        "indeterminate" => "started",
        _ => "unstarted",
    };
    let name = match text(fields, "/status/name") {
        "" => "Unknown",
        name => name,
    };
    json!({ "name": name, "type": kind })
}

fn priority_of(fields: &Value) -> (u64, String) {
    let name = text(fields, "/priority/name");
    let priority = match name.to_lowercase().as_str() {
        "highest" | "blocker" | "critical" => 1,
        "high" | "major" => 2,
        "medium" => 3,
        "low" | "minor" | "lowest" | "trivial" => 4,
        _ => 0,
    };
    let label = if name.is_empty() { "No priority" } else { name };
    (priority, label.to_string())
}

fn is_epic(issuetype: &Value) -> bool {
    issuetype
        .get("hierarchyLevel")
        .and_then(Value::as_i64)
        .is_some_and(|l| l >= 1)
        || text(issuetype, "/name").to_lowercase().contains("epic")
}

// The parent epic, as Linear's project: one level above a ticket, or named
// Epic. A sub-task's parent is a task, and an epic's own parent (an
// initiative, a level above) is its parent, not its project.
fn epic_of(fields: &Value) -> Value {
    let Some(parent) = fields.get("parent").filter(|p| p.is_object()) else {
        return Value::Null;
    };
    let kind = parent.pointer("/fields/issuetype").unwrap_or(&Value::Null);
    let level_one = kind.get("hierarchyLevel").and_then(Value::as_i64) == Some(1);
    if !level_one && !text(kind, "/name").to_lowercase().contains("epic") {
        return Value::Null;
    }
    let key = text(parent, "/key");
    let name = match text(parent, "/fields/summary") {
        "" => key,
        summary => summary,
    };
    json!({ "name": name, "url": url_of(key) })
}

const FIELDS: &[&str] = &[
    "summary",
    "status",
    "resolution",
    "parent",
    "priority",
    "updated",
    "resolutiondate",
    "statuscategorychangedate",
    "assignee",
];

fn brief(issue: &Value) -> Value {
    let empty = json!({});
    let fields = issue.get("fields").unwrap_or(&empty);
    let key = text(issue, "/key");
    let (priority, label) = priority_of(fields);
    let state = state_of(fields);
    let done = text(fields, "/status/statusCategory/key") == "done";
    let finished = if done {
        fields
            .get("resolutiondate")
            .filter(|v| v.is_string())
            .or_else(|| fields.get("statuscategorychangedate"))
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    // Linear keeps a canceled ticket's time apart from a completed one's.
    let (completed, canceled) = if text(&state, "/type") == "canceled" {
        (Value::Null, finished)
    } else {
        (finished, Value::Null)
    };
    json!({
        "identifier": key,
        "title": text(fields, "/summary"),
        "url": url_of(key),
        "branchName": null,
        "state": state,
        "project": epic_of(fields),
        "priority": priority,
        "priorityLabel": label,
        "updatedAt": fields.get("updated").cloned().unwrap_or(Value::Null),
        "completedAt": completed,
        "canceledAt": canceled,
    })
}

fn person(user: &Value) -> Value {
    if !user.is_object() {
        return Value::Null;
    }
    let name = [text(user, "/displayName"), text(user, "/emailAddress")]
        .into_iter()
        .find(|n| !n.is_empty())
        .unwrap_or("Someone");
    json!({ "name": name })
}

fn issue_path(key: &str, rest: &str) -> String {
    format!("/rest/api/3/issue/{}{rest}", encoded(key))
}

// ── The tracker's questions ─────────────────────────────────────────────────

async fn fetch_issue(key: &str, fields: &[&str], source: &str) -> Result<Option<Value>> {
    let path = issue_path(key, &format!("?fields={}", fields.join(",")));
    or_none(request("GET", &path, None, source).await, source).await
}

/// The ticket `key` names, or `None` when Jira has no such ticket.
pub async fn ticket(key: &str, source: &str) -> Result<Option<Ticket>> {
    let Some(found) = fetch_issue(key, FIELDS, source).await? else {
        return Ok(None);
    };
    let fields = found.get("fields").cloned().unwrap_or(json!({}));
    let identifier = text(&found, "/key").to_string();
    if identifier.is_empty() {
        return Ok(None);
    }
    let assignee = fields
        .get("assignee")
        .filter(|a| a.is_object())
        .map(|a| Person {
            id: text(a, "/accountId").to_string(),
            name: text(&person(a), "/name").to_string(),
        });
    Ok(Some(Ticket {
        id: text(&found, "/id").to_string(),
        url: url_of(&identifier).unwrap_or_default(),
        identifier,
        title: text(&fields, "/summary").to_string(),
        state: text(&fields, "/status/name").to_string(),
        assignee,
    }))
}

/// A ticket's identifier, title, link and state, as `tracker::issue`, or
/// `None`.
pub async fn issue(key: &str, source: &str) -> Result<Option<Value>> {
    Ok(fetch_issue(key, FIELDS, source).await?.map(|i| brief(&i)))
}

/// The link to the epic the ticket is in, or `None` when it is in none; a
/// ticket Jira has no such one of is `ErrorKind::NotFound`.
pub async fn project_url(key: &str, source: &str) -> Result<Option<String>> {
    match issue(key, source).await? {
        Some(found) => Ok(found
            .pointer("/project/url")
            .and_then(Value::as_str)
            .map(str::to_string)),
        None => Err(ServiceError::new(
            ErrorKind::NotFound,
            SERVICE,
            format!("Jira has no ticket {key}."),
        )
        .into()),
    }
}

/// Many at once, the ones Jira has, a hundred to a request.
pub async fn issues(keys: &[String], source: &str) -> Result<Vec<Value>> {
    let mut found = Vec::new();
    for batch in keys.chunks(100) {
        let page = request(
            "POST",
            "/rest/api/3/issue/bulkfetch",
            Some(&json!({ "issueIdsOrKeys": batch, "fields": FIELDS })),
            source,
        )
        .await?;
        found.extend(
            page.get("issues")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(brief),
        );
    }
    // A bulk fetch leaves out what it can't return rather than failing, so
    // one that left any out checks the token as a missing issue does.
    let mut asked: Vec<&String> = keys.iter().collect();
    asked.sort();
    asked.dedup();
    if found.len() < asked.len() {
        signed_in(source).await?;
    }
    Ok(found)
}

// JQL, every page of it up to `limit` issues.
async fn jql(
    query: &str,
    fields: &[&str],
    limit: usize,
    expand: Option<&str>,
    source: &str,
) -> Result<Vec<Value>> {
    let mut found: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut body = json!({
            "jql": query,
            "fields": fields,
            "maxResults": (limit - found.len()).min(100),
        });
        if let Some(expand) = expand {
            body["expand"] = json!(expand);
        }
        if let Some(token) = &next {
            body["nextPageToken"] = json!(token);
        }
        let page = request("POST", "/rest/api/3/search/jql", Some(&body), source).await?;
        found.extend(
            page.get("issues")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        next = (page.get("isLast") == Some(&Value::Bool(false)))
            .then(|| page.get("nextPageToken").and_then(Value::as_str))
            .flatten()
            .map(str::to_string);
        if next.is_none() || found.len() >= limit {
            break;
        }
    }
    // A search Jira can't sign in comes back as one that matched nothing.
    if found.is_empty() {
        signed_in(source).await?;
    }
    Ok(found)
}

/// A time as JQL reads it: minutes before now, which no time zone shifts,
/// since Jira reads a written date in the Jira profile's zone, not this
/// computer's.
fn jql_time(iso: &str, now_ms: i64) -> String {
    let at = iso
        .parse::<jiff::Timestamp>()
        .map(|t| t.as_millisecond())
        .unwrap_or(now_ms);
    let minutes = ((now_ms - at) as f64 / 60_000.0).ceil().max(1.0) as i64;
    format!("-{minutes}m")
}

/// Assigned to you, open or finished or canceled since `since`, by when
/// they reached Done, as Linear's, most recently changed first, each with
/// its description for the card's pane. The open ones and the finished ones
/// are asked for apart, as Linear's canceled are, so a week of closed
/// tickets never takes an open one's place in the first hundred: which
/// finished ones count as canceled is up to their resolution's name, which
/// JQL would have to know.
pub async fn assigned(since: &str, source: &str) -> Result<Vec<Value>> {
    let finished_query = format!(
        "assignee = currentUser() AND statusCategory = Done AND statusCategoryChangedDate >= {} ORDER BY updated DESC",
        jql_time(since, util::now_ms())
    );
    let fields = [FIELDS, &["description"]].concat();
    let (open, finished) = tokio::join!(
        jql(
            "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC",
            &fields,
            100,
            None,
            source
        ),
        jql(&finished_query, &fields, 50, None, source)
    );
    Ok(open?
        .iter()
        .chain(finished?.iter())
        .map(|issue| {
            let mut out = brief(issue);
            let description = issue.pointer("/fields/description").unwrap_or(&Value::Null);
            out["description"] = json!(markdown_of(description));
            out
        })
        .collect())
}

static KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9]*-\d+$").unwrap());

/// Jira's text search, which reads the summary, description and comments.
/// Its query language gives meaning to most punctuation, so only words are
/// kept. A ticket key searched for comes first, since text search may not
/// find it.
pub async fn search(term: &str, limit: usize, source: &str) -> Result<Vec<Value>> {
    let term = term.trim();
    let words = term
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let by_key = if KEY.is_match(term) {
        issue(&term.to_uppercase(), source).await.ok().flatten()
    } else {
        None
    };
    let mut found: Vec<Value> = by_key.into_iter().collect();
    if !words.is_empty() {
        let query = format!(
            "text ~ \"{}\" ORDER BY updated DESC",
            words.replace('"', "")
        );
        for hit in jql(&query, FIELDS, limit, None, source).await? {
            let hit = brief(&hit);
            if !found.iter().any(|f| f["identifier"] == hit["identifier"]) {
                found.push(hit);
            }
        }
    }
    found.truncate(limit);
    Ok(found)
}

/// For the standup note, as Linear's answers it: tickets assigned to you or
/// reported by you that changed since `since`, with their status moves from
/// the changelog, and the ones you have in progress.
pub async fn standup(since: &str, source: &str) -> Result<Value> {
    let changed_query = format!(
        "(assignee = currentUser() OR reporter = currentUser()) AND updated >= {} ORDER BY updated DESC",
        jql_time(since, util::now_ms())
    );
    let (changed, started) = tokio::join!(
        jql(&changed_query, FIELDS, 100, Some("changelog"), source),
        jql(
            "assignee = currentUser() AND statusCategory = \"In Progress\" ORDER BY updated DESC",
            FIELDS,
            50,
            None,
            source
        )
    );
    let changed: Vec<Value> = changed?
        .iter()
        .map(|issue| {
            let mut out = brief(issue);
            let moves: Vec<Value> = issue
                .pointer("/changelog/histories")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|h| {
                    h.get("items")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|item| text(item, "/field") == "status")
                        .map(|item| {
                            json!({
                                "createdAt": h.get("created").cloned().unwrap_or(Value::Null),
                                "fromState": { "name": text(item, "/fromString") },
                                "toState": { "name": text(item, "/toString") },
                            })
                        })
                })
                .collect();
            out["history"] = json!({ "nodes": moves });
            out
        })
        .collect();
    let started: Vec<Value> = started?.iter().map(brief).collect();
    Ok(json!({ "changed": { "nodes": changed }, "started": { "nodes": started } }))
}

const DETAIL_FIELDS: &[&str] = &[
    "summary",
    "description",
    "status",
    "resolution",
    "priority",
    "assignee",
    "reporter",
    "creator",
    "created",
    "updated",
    "labels",
    "parent",
    "subtasks",
    "issuelinks",
    "issuetype",
];

/// Everything the ticket view and image show, in the shape Linear's detail
/// gives, or `None`. An epic's sub-issues are the issues under it, which
/// Jira keeps as its children rather than its sub-tasks.
pub async fn detail(key: &str, source: &str) -> Result<Option<Value>> {
    let Some(found) = fetch_issue(key, DETAIL_FIELDS, source).await? else {
        return Ok(None);
    };
    let key = text(&found, "/key").to_string();
    let empty = json!({});
    let fields = found.get("fields").unwrap_or(&empty);
    let comments_path = issue_path(&key, "/comment?maxResults=100&orderBy=created");
    let links_path = issue_path(&key, "/remotelink");
    let (comments, links) = tokio::join!(
        request("GET", &comments_path, None, source),
        request("GET", &links_path, None, source)
    );
    let under = if is_epic(fields.get("issuetype").unwrap_or(&Value::Null)) {
        jql(
            &format!("parent = {key} ORDER BY rank"),
            &["summary", "status"],
            100,
            None,
            source,
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let small = |i: &Value| {
        json!({
            "identifier": text(i, "/key"),
            "title": text(i, "/fields/summary"),
            "state": match text(i, "/fields/status/name") {
                "" => Value::Null,
                name => json!({ "name": name }),
            },
        })
    };
    let epic = epic_of(fields);
    let parent = match fields.get("parent").filter(|p| p.is_object()) {
        Some(p) if epic.is_null() => {
            json!({ "identifier": text(p, "/key"), "title": text(p, "/fields/summary") })
        }
        _ => Value::Null,
    };
    let children: Vec<Value> = fields
        .get("subtasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(under.iter())
        .map(small)
        .collect();
    let relations: Vec<Value> = fields
        .get("issuelinks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|link| {
            let (other, phrase) = match link.get("outwardIssue") {
                Some(out) => (out, text(link, "/type/outward")),
                None => (
                    link.get("inwardIssue").unwrap_or(&Value::Null),
                    text(link, "/type/inward"),
                ),
            };
            let phrase = [phrase, text(link, "/type/name"), "related to"]
                .into_iter()
                .find(|p| !p.is_empty())
                .unwrap_or("related to");
            let mut chars = phrase.chars();
            let phrase: String = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            json!({ "type": phrase, "relatedIssue": small(other) })
        })
        .collect();
    let attachments: Vec<Value> = links
        .ok()
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let url = text(l, "/object/url");
            (!url.is_empty()).then(|| {
                let title = match text(l, "/object/title") {
                    "" => url,
                    title => title,
                };
                json!({ "title": title, "url": url })
            })
        })
        .collect();
    let comments: Vec<Value> = comments
        .ok()
        .as_ref()
        .and_then(|c| c.get("comments"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| {
            json!({
                "id": c.get("id").cloned().unwrap_or(Value::Null),
                "body": markdown_of(c.get("body").unwrap_or(&Value::Null)),
                "createdAt": c.get("created").cloned().unwrap_or(Value::Null),
                "parent": null,
                "user": person(c.get("author").unwrap_or(&Value::Null)),
                "botActor": null,
            })
        })
        .collect();
    let (priority, label) = priority_of(fields);
    let labels: Vec<Value> = fields
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|name| json!({ "name": name }))
        .collect();
    let reporter = fields
        .get("reporter")
        .filter(|r| r.is_object())
        .or_else(|| fields.get("creator"))
        .unwrap_or(&Value::Null);
    Ok(Some(json!({
        "identifier": key,
        "title": text(fields, "/summary"),
        "url": url_of(&key),
        "description": markdown_of(fields.get("description").unwrap_or(&Value::Null)),
        "createdAt": fields.get("created").cloned().unwrap_or(Value::Null),
        "updatedAt": fields.get("updated").cloned().unwrap_or(Value::Null),
        "priority": priority,
        "priorityLabel": label,
        "estimate": null,
        "state": state_of(fields),
        "assignee": person(fields.get("assignee").unwrap_or(&Value::Null)),
        "creator": person(reporter),
        "project": epic,
        "cycle": null,
        "labels": { "nodes": labels },
        "parent": parent,
        "children": { "nodes": children },
        "relations": { "nodes": relations },
        "attachments": { "nodes": attachments },
        "comments": { "nodes": comments },
    })))
}

/// Closing a ticket: the first transition its workflow offers into a Done
/// status that isn't a Won't Do or Cancelled, as `(key, already done,
/// transition id, status name)`; `None` when Jira has no such ticket or its
/// workflow no way into Done from where it is.
pub async fn done_state_of(
    key: &str,
    source: &str,
) -> Result<Option<(String, bool, String, String)>> {
    // The ticket first, whose not-found checks the token, so a refused one
    // isn't taken for a missing ticket by the transitions' own 404.
    let Some(found) = issue(key, source).await? else {
        return Ok(None);
    };
    let key = text(&found, "/identifier").to_string();
    // A ticket already done may offer no way into Done, only back out of it.
    if text(&found, "/state/type") == "completed" {
        let name = text(&found, "/state/name").to_string();
        return Ok(Some((key, true, String::new(), name)));
    }
    let moves = request("GET", &issue_path(&key, "/transitions"), None, source).await?;
    let into_done = moves
        .get("transitions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|t| {
            text(t, "/to/statusCategory/key") == "done"
                && !CANCELED.is_match(&format!("{} {}", text(t, "/to/name"), text(t, "/name")))
        });
    Ok(into_done.map(|t| {
        let id = match t.get("id") {
            Some(Value::String(id)) => id.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let name = match text(t, "/to/name") {
            "" => text(t, "/name"),
            name => name,
        };
        (key, false, id, name.to_string())
    }))
}

/// Makes the transition `done_state_of` found; Jira answers nothing on
/// success, so success is true.
pub async fn close_ticket(key: &str, transition: &str, source: &str) -> Result<bool> {
    request(
        "POST",
        &issue_path(key, "/transitions"),
        Some(&json!({ "transition": { "id": transition } })),
        source,
    )
    .await?;
    Ok(true)
}

/// Where the skills file a new ticket: the site, the project and the labels
/// from jira.json, never the email or token.
pub fn filing() -> Value {
    let labels = util::read_json_object(paths::config_dir().join("jira.json"))
        .get("jiraLabels")
        .cloned()
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    json!({ "site": site(), "team": config("jiraProject"), "labels": labels })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_site_is_its_origin_and_https_but_for_this_computer() {
        let site = |s: &str| site_of(s);
        assert_eq!(
            site("acme.atlassian.net").as_deref(),
            Some("https://acme.atlassian.net")
        );
        assert_eq!(
            site("http://acme.atlassian.net/").as_deref(),
            Some("https://acme.atlassian.net")
        );
        assert_eq!(
            site(
                "https://acme.atlassian.net/jira/software/projects/AB/boards/1?selectedIssue=AB-1"
            )
            .as_deref(),
            Some("https://acme.atlassian.net")
        );
        assert_eq!(
            site("http://127.0.0.1:8080/x").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            site("http://localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            site("http://localhost:@acme.atlassian.net").as_deref(),
            Some("https://acme.atlassian.net"),
            "a host that only looks like this computer's before an @"
        );
        assert_eq!(
            site("http://me@localhost:3000").as_deref(),
            Some("https://localhost:3000"),
            "a name before the @ is never sent in the clear"
        );
        assert_eq!(site("ftp://acme.atlassian.net"), None);
        assert_eq!(site("https://acme\\.atlassian.net"), None);
        assert_eq!(site(""), None);
    }

    #[test]
    fn base64_as_basic_auth_sends_it() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"me@acme.com:secret"), "bWVAYWNtZS5jb206c2VjcmV0");
    }

    #[test]
    fn states_priorities_and_epics_in_linear_s_words() {
        let fields = |category: &str, status: &str, resolution: Option<&str>| {
            json!({
                "status": { "name": status, "statusCategory": { "key": category } },
                "resolution": resolution.map(|r| json!({ "name": r })),
            })
        };
        assert_eq!(state_of(&fields("new", "To Do", None))["type"], "unstarted");
        assert_eq!(
            state_of(&fields("indeterminate", "In Review", None))["type"],
            "started"
        );
        assert_eq!(
            state_of(&fields("done", "Done", Some("Done")))["type"],
            "completed"
        );
        assert_eq!(
            state_of(&fields("done", "Done", Some("Won't Do")))["type"],
            "canceled"
        );
        assert_eq!(
            state_of(&fields("done", "Cancelled", None))["type"],
            "canceled"
        );
        assert_eq!(state_of(&json!({}))["name"], "Unknown");
        assert_eq!(
            priority_of(&json!({ "priority": { "name": "Highest" } })),
            (1, "Highest".into())
        );
        assert_eq!(priority_of(&json!({})), (0, "No priority".into()));
        let in_epic = json!({ "parent": { "key": "AB-1", "fields": { "summary": "Login", "issuetype": { "name": "Epic", "hierarchyLevel": 1 } } } });
        assert_eq!(epic_of(&in_epic)["name"], "Login");
        let under_task = json!({ "parent": { "key": "AB-2", "fields": { "summary": "Task", "issuetype": { "name": "Task", "hierarchyLevel": 0 } } } });
        assert_eq!(epic_of(&under_task), Value::Null);
        let under_initiative = json!({ "parent": { "key": "AB-2", "fields": { "summary": "Milestone", "issuetype": { "name": "Initiative", "hierarchyLevel": 2 } } } });
        assert_eq!(
            epic_of(&under_initiative),
            Value::Null,
            "an epic's parent isn't its project"
        );
    }

    #[test]
    fn jql_times_are_minutes_ago() {
        let now = 1_700_000_000_000;
        let hour_ago = jiff::Timestamp::from_millisecond(now - 3_600_000)
            .unwrap()
            .to_string();
        assert_eq!(jql_time(&hour_ago, now), "-60m");
        let just_now = jiff::Timestamp::from_millisecond(now - 10)
            .unwrap()
            .to_string();
        assert_eq!(jql_time(&just_now, now), "-1m");
    }
}
