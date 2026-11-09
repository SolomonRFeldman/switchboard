//! Linear, over its GraphQL API: the API key, a query, a ticket by its
//! identifier as `tmux/ticket-title.sh` fetched its title, and the ticket a
//! branch names, as `tmux/path-facts.sh` matched it.
//!
//! The key is the one the ticket scripts used, first that answers:
//! `LINEAR_API_KEY`, the stored secret `linear-api-key` (the Keychain's, as
//! before), else `linearApiKey` in `~/.config/switchboard/linear.json`, the
//! file `worktree-from-ticket` kept it in. What the secret or the file gave
//! is kept ten minutes and forgotten once Linear refuses it. It is sent as
//! `Authorization: <key>`, as Linear takes a personal API key.
//!
//! Every call refuses unless Linear is the ticket tracker the `tracker`
//! setting names (but in a program you run yourself, `super::run_by_you`),
//! and is counted in the request audit under Linear, as "GraphQL". What the
//! tracker asks (`super::tracker`) is answered at the end, in Linear's own
//! shape, which every caller reads.

use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Credential, Remembered, env_value, stored_secret};
use crate::net::{self, Audit, ErrorKind, Response, ServiceError};
use crate::{paths, util};

const SERVICE: &str = "Linear";

/// The GraphQL endpoint (`SWITCHBOARD_LINEAR_API` moves it).
pub fn api_url() -> String {
    net::base_url("SWITCHBOARD_LINEAR_API", "https://api.linear.app/graphql")
}

// ── The key ─────────────────────────────────────────────────────────────────

static REMEMBERED: Remembered = Remembered::new();

/// The API key to send, as the module's comment says where it is looked for.
pub async fn api_key() -> Result<String> {
    Ok(credential().await?.value)
}

async fn credential() -> Result<Credential> {
    if let Some(value) = env_value("LINEAR_API_KEY") {
        return Ok(Credential {
            value,
            from: "LINEAR_API_KEY".to_string(),
        });
    }
    if let Some(known) = REMEMBERED.get() {
        return Ok(known);
    }
    let found = match stored_secret("linear-api-key").await {
        Some(value) => Some(Credential {
            value,
            from: "the stored secret linear-api-key".to_string(),
        }),
        None => util::read_json_object(paths::config_dir().join("linear.json"))
            .get("linearApiKey")
            .and_then(Value::as_str)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(|value| Credential {
                value,
                from: "linear.json".to_string(),
            }),
    };
    match found {
        Some(credential) => {
            REMEMBERED.set(&credential);
            Ok(credential)
        }
        None => Err(ServiceError::new(
            ErrorKind::NoCredentials,
            SERVICE,
            "No Linear API key: make one in Linear (Settings → Security & access → Personal API \
             keys), then store it with `switchboard platform secret-set linear-api-key` (it reads \
             the key from stdin), or set LINEAR_API_KEY.",
        )
        .into()),
    }
}

/// Forgets the key the secret or the file gave, so the next request looks
/// again.
pub fn forget_key() {
    REMEMBERED.forget();
}

// ── Requests ────────────────────────────────────────────────────────────────

/// A GraphQL query's `data`. Linear's errors in the answer are errors here,
/// with its message: a ticket it has no such one of is `ErrorKind::NotFound`,
/// a key it refuses `Unauthorized`, its rate limit spent `RateLimited`.
/// `source` says what asked, for the request audit.
pub async fn graphql(query: &str, variables: Value, source: &str) -> Result<Value> {
    super::tracker::ensure(super::tracker::Tracker::Linear)?;
    let credential = credential().await?;
    let body = json!({ "query": query, "variables": variables });
    let headers = [("Authorization", credential.value.as_str())];
    let audit = Audit {
        service: SERVICE,
        protocol: "GraphQL",
        source,
    };
    let response = net::post_json(&api_url(), &headers, &body, audit).await?;
    // Linear says what went wrong in `errors` whatever the status (often a
    // 400), so they are read first, and the status only when there are none.
    if let Some(errors) = response.body.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        return Err(linear_error(errors, &response, &credential).into());
    }
    if response.status == 401 {
        return Err(refused(&credential, "401").into());
    }
    if !response.ok() {
        return Err(ServiceError::new(
            ErrorKind::Failed,
            SERVICE,
            format!("Linear answered {}", response.status),
        )
        .into());
    }
    match response.body.get("data") {
        Some(data) if !data.is_null() => Ok(data.clone()),
        _ => Err(
            ServiceError::new(ErrorKind::Failed, SERVICE, "Linear's answer had no data.").into(),
        ),
    }
}

fn refused(credential: &Credential, why: &str) -> ServiceError {
    forget_key();
    ServiceError::new(
        ErrorKind::Unauthorized,
        SERVICE,
        format!(
            "Linear refused the API key from {} ({why}): make a new one and store it with \
             `switchboard platform secret-set linear-api-key`.",
            credential.from
        ),
    )
}

fn linear_error(errors: &[Value], response: &Response, credential: &Credential) -> ServiceError {
    // Linear's message for a person, where it gives one, else its own.
    let words = |e: &Value| {
        e.pointer("/extensions/userPresentableMessage")
            .and_then(Value::as_str)
            .or_else(|| e.get("message").and_then(Value::as_str))
            .map(str::to_string)
    };
    let messages: Vec<String> = errors.iter().filter_map(words).collect();
    let message = if messages.is_empty() {
        "an error it didn't explain".to_string()
    } else {
        messages.join("; ")
    };
    let code_of = |e: &Value| {
        e.pointer("/extensions/code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let codes: Vec<String> = errors.iter().map(code_of).collect();
    let has = |code: &str| codes.iter().any(|c| c == code);
    if has("AUTHENTICATION_ERROR") || response.status == 401 {
        return refused(credential, &message);
    }
    if has("RATELIMITED") {
        // The reset is in milliseconds since the epoch.
        let reset_at = response
            .header("x-ratelimit-requests-reset")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .map(|ms| ms / 1000);
        return ServiceError::new(
            ErrorKind::RateLimited { reset_at },
            SERVICE,
            format!("Linear's rate limit is spent: {message}"),
        );
    }
    // "Entity not found" is an INPUT_ERROR, as is a malformed argument; only
    // the first is something that isn't there.
    let missing = errors.iter().any(|e| {
        code_of(e) == "INPUT_ERROR"
            && e.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.to_lowercase().contains("not found"))
    });
    let kind = if missing {
        ErrorKind::NotFound
    } else {
        ErrorKind::Failed
    };
    ServiceError::new(kind, SERVICE, format!("Linear: {message}"))
}

// ── Tickets ─────────────────────────────────────────────────────────────────

/// A ticket, as the tab names and views need it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    /// `ENG-123`.
    pub identifier: String,
    pub title: String,
    pub url: String,
    /// Its state's name ("In Progress").
    pub state: String,
    pub assignee: Option<Person>,
}

/// Someone in Linear: their id, and the name Linear shows for them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub name: String,
}

const TICKET: &str = "query($id: String!) { issue(id: $id) { id identifier title url \
                      state { name } assignee { id name displayName } } }";

/// The ticket `identifier` names (`ENG-123`), or `None` when Linear has no
/// such ticket, which is the usual answer for a branch that only looks like
/// it names one.
pub async fn ticket(identifier: &str, source: &str) -> Result<Option<Ticket>> {
    let data = match graphql(TICKET, json!({ "id": identifier }), source).await {
        Ok(data) => data,
        Err(e) if net::kind_of(&e) == Some(&ErrorKind::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(ticket_from(data.get("issue").unwrap_or(&Value::Null)))
}

const PROJECT: &str = "query($id: String!) { issue(id: $id) { project { name url } } }";

/// The link to the Linear project the ticket `identifier` is in, or `None`
/// when it is in none; a ticket Linear has no such one of is
/// `ErrorKind::NotFound`.
pub async fn project_url(identifier: &str, source: &str) -> Result<Option<String>> {
    let data = graphql(PROJECT, json!({ "id": identifier }), source).await?;
    Ok(project_url_in(&data))
}

fn project_url_in(data: &Value) -> Option<String> {
    data.pointer("/issue/project/url")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
}

// A ticket from the `issue` Linear gave, or `None` when it gave none, or one
// without the fields every ticket has.
fn ticket_from(issue: &Value) -> Option<Ticket> {
    let text = |path: &str| {
        issue
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let assignee = issue
        .get("assignee")
        .filter(|a| a.is_object())
        .and_then(|a| {
            let id = a.get("id").and_then(Value::as_str)?.to_string();
            let name = a
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .or_else(|| a.get("name").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            Some(Person { id, name })
        });
    Some(Ticket {
        id: text("/id")?,
        identifier: text("/identifier")?,
        title: text("/title")?,
        url: text("/url").unwrap_or_default(),
        state: text("/state/name").unwrap_or_default(),
        assignee,
    })
}

// Any team key, as path-facts.sh and linear-ticket.sh matched it: letters
// and digits starting with a letter, a dash, a number. A branch that only
// looks like a ticket is harmless: Linear has no such ticket, and the tab
// keeps its directory's name.
static TICKET_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[a-z][a-z0-9]*-[0-9]+").unwrap());

/// The ticket identifier a branch (or tab) name holds, in capitals: the
/// first match, so `eng-42-fix-login` and `me/eng-42` give `ENG-42`.
pub fn ticket_in(branch: &str) -> Option<String> {
    TICKET_ID
        .find(branch)
        .map(|m| m.as_str().to_ascii_uppercase())
}

// ── The tracker's questions ─────────────────────────────────────────────────
// What `super::tracker` asks while Linear is the tracker set, each answered
// as Linear's GraphQL gives it, the shape every caller reads.

const ASSIGNED: &str = "query($since: DateTimeOrDuration!) {
  viewer {
    assignedIssues(first: 100, orderBy: updatedAt, filter: { or: [{ state: { type: { in: [\"unstarted\", \"started\"] } } }, { completedAt: { gte: $since } }] }) {
      nodes { identifier title description url priority priorityLabel estimate updatedAt completedAt canceledAt state { name type } project { name } }
    }
    canceled: assignedIssues(first: 50, orderBy: updatedAt, filter: { canceledAt: { gte: $since } }) {
      nodes { identifier title description url priority priorityLabel estimate updatedAt completedAt canceledAt state { name type } project { name } }
    }
  }
}";

/// The issues assigned to you that are open, or were finished or canceled
/// since `since`: the open and done ones, then the canceled ones, asked for
/// apart so they never take an open one's place in the first hundred.
pub async fn assigned(since: &str, source: &str) -> Result<Vec<Value>> {
    let data = graphql(ASSIGNED, json!({ "since": since }), source).await?;
    Ok(assigned_of(&data))
}

fn assigned_of(data: &Value) -> Vec<Value> {
    ["/viewer/assignedIssues/nodes", "/viewer/canceled/nodes"]
        .iter()
        .flat_map(|at| nodes(data, at))
        .collect()
}

const DETAIL: &str = "query($id: String!) { issue(id: $id) { \
    identifier title url description createdAt updatedAt priorityLabel estimate \
    state { name type } assignee { name } creator { id name } project { name } cycle { number name } \
    labels { nodes { name } } parent { identifier title } \
    children { nodes { identifier title url state { name type } } } \
    relations { nodes { type relatedIssue { identifier title url state { name type } } } } \
    attachments { nodes { title url metadata } } \
    comments(first: 100) { nodes { id body createdAt parent { id } user { id name } botActor { name } } } } }";

/// Everything the ticket view and image show of a ticket, or `None` when
/// Linear has no such ticket.
pub async fn detail(identifier: &str, source: &str) -> Result<Option<Value>> {
    issue_by(DETAIL, identifier, source).await
}

const BRIEF: &str = "query IssueByIdentifier($number: Float!, $key: String!) { \
     issues(filter: { number: { eq: $number }, team: { key: { eq: $key } } }) { \
     nodes { identifier title url branchName state { name type } } } }";

/// A ticket's identifier, title, link, branch name and state, or `None`
/// when Linear has no such ticket, found by its team's key and number.
pub async fn issue(identifier: &str, source: &str) -> Result<Option<Value>> {
    let Some((key, number)) = identifier
        .rsplit_once('-')
        .and_then(|(k, n)| Some((k.to_uppercase(), n.parse::<u64>().ok()?)))
    else {
        return Ok(None);
    };
    let data = graphql(BRIEF, json!({ "number": number, "key": key }), source).await?;
    Ok(data
        .pointer("/issues/nodes/0")
        .filter(|n| n.is_object())
        .cloned())
}

async fn issue_by(query: &str, identifier: &str, source: &str) -> Result<Option<Value>> {
    match graphql(query, json!({ "id": identifier }), source).await {
        Ok(data) => Ok(data.get("issue").filter(|i| i.is_object()).cloned()),
        Err(e) if net::kind_of(&e) == Some(&ErrorKind::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

const ISSUES: &str = "query Issues($filters: [IssueFilter!]) { \
     issues(filter: { or: $filters }, first: 150) { \
     nodes { identifier title url state { name type } } } }";

/// Comfortably under Linear's 250 page cap, so one query per chunk always
/// gives every match rather than silently paginating.
const CHUNK_SIZE: usize = 100;

/// The tickets `identifiers` name that Linear has, one request per chunk
/// rather than one per ticket: forty worktrees is forty round trips
/// otherwise. The filter is an unordered `or`, and tickets that no longer
/// exist (or the key can't see) don't come back at all.
pub async fn issues(identifiers: &[String], source: &str) -> Result<Vec<Value>> {
    let mut found = Vec::new();
    for batch in identifiers.chunks(CHUNK_SIZE) {
        let filters: Vec<Value> = batch
            .iter()
            .filter_map(|id| {
                let (key, number) = id.rsplit_once('-')?;
                let number: u64 = number.parse().ok()?;
                Some(json!({ "and": [{ "number": { "eq": number } }, { "team": { "key": { "eq": key.to_uppercase() } } }] }))
            })
            .collect();
        if filters.is_empty() {
            continue;
        }
        let data = graphql(ISSUES, json!({ "filters": filters }), source).await?;
        found.extend(nodes(&data, "/issues/nodes"));
    }
    Ok(found)
}

/// Linear's issue search, its first `limit`.
pub async fn search(term: &str, limit: usize, source: &str) -> Result<Vec<Value>> {
    let data = graphql(
        &format!(
            "query($q: String!) {{ searchIssues(term: $q, first: {limit}) {{ nodes {{ identifier title url state {{ name }} }} }} }}"
        ),
        json!({ "q": term }),
        source,
    )
    .await?;
    Ok(nodes(&data, "/searchIssues/nodes"))
}

const STANDUP: &str = "query($since: DateTimeOrDuration!) {
  changed: issues(first: 100, filter: { or: [{ assignee: { isMe: { eq: true } } }, { creator: { isMe: { eq: true } } }], updatedAt: { gte: $since } }) { nodes { identifier title url
    history(first: 50) { nodes { createdAt fromState { name } toState { name } } } } }
  started: issues(first: 50, filter: { assignee: { isMe: { eq: true } }, state: { type: { eq: \"started\" } } }) { nodes { identifier title url state { name } } } }";

/// For the standup note: the tickets assigned to you or made by you that
/// changed since `since`, with their state changes (`changed`), and the
/// ones you have in progress (`started`).
pub async fn standup(since: &str, source: &str) -> Result<Value> {
    graphql(STANDUP, json!({ "since": since }), source).await
}

/// The state closing a ticket moves it to: `(issue id, already done, state
/// id, state name)`, or `None` when Linear has no such ticket or its team no
/// completed state.
pub async fn done_state_of(
    identifier: &str,
    source: &str,
) -> Result<Option<(String, bool, String, String)>> {
    let data = graphql(
        "query($id: String!) { issue(id: $id) { id state { type } team { states(filter: { type: { eq: \"completed\" } }) { nodes { id name position } } } } }",
        json!({ "id": identifier }),
        source,
    )
    .await?;
    let Some(issue) = data.get("issue").filter(|i| i.is_object()) else {
        return Ok(None);
    };
    let text = |v: &Value, p: &str| {
        v.pointer(p)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut states: Vec<&Value> = issue
        .pointer("/team/states/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    states.sort_by(|a, b| {
        let p = |s: &Value| s.get("position").and_then(Value::as_f64).unwrap_or(0.0);
        p(a).total_cmp(&p(b))
    });
    Ok(states.first().map(|state| {
        (
            text(issue, "/id"),
            text(issue, "/state/type") == "completed",
            text(state, "/id"),
            text(state, "/name"),
        )
    }))
}

/// Moves a ticket to a state; whether Linear says it did.
pub async fn close_ticket(issue: &str, state: &str, source: &str) -> Result<bool> {
    let data = graphql(
        "mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: { stateId: $state }) { success } }",
        json!({ "id": issue, "state": state }),
        source,
    )
    .await?;
    Ok(data
        .pointer("/issueUpdate/success")
        .and_then(Value::as_bool)
        == Some(true))
}

/// Where the skills file a new ticket: the workspace's page, the team and
/// the labels from linear.json, never the key.
pub fn filing() -> Value {
    let mine = util::read_json_object(paths::config_dir().join("linear.json"));
    let text = |key: &str| {
        mine.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let labels = mine
        .get("linearLabels")
        .cloned()
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    json!({
        "site": text("linearWorkspace").map(|w| format!("https://linear.app/{w}")),
        "team": text("linearTeam"),
        "labels": labels,
    })
}

fn nodes(data: &Value, path: &str) -> Vec<Value> {
    data.pointer(path)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canceled_tickets_come_apart_from_the_open_ones() {
        let data = json!({ "viewer": {
            "assignedIssues": { "nodes": [{ "identifier": "AB-1" }] },
            "canceled": { "nodes": [{ "identifier": "AB-2" }] },
        } });
        let issues = assigned_of(&data);
        let ids: Vec<&str> = issues
            .iter()
            .filter_map(|i| i["identifier"].as_str())
            .collect();
        assert_eq!(ids, ["AB-1", "AB-2"]);
        assert!(assigned_of(&json!({})).is_empty());
    }

    #[test]
    fn a_ticket_s_project_is_its_link_or_none() {
        let data = json!({"issue": {"project": {"name": "Login", "url": "https://linear.app/a/project/login"}}});
        assert_eq!(
            project_url_in(&data).as_deref(),
            Some("https://linear.app/a/project/login")
        );
        assert_eq!(project_url_in(&json!({"issue": {"project": null}})), None);
    }

    #[test]
    fn tickets_in_branch_names() {
        assert_eq!(ticket_in("eng-42-fix-login"), Some("ENG-42".into()));
        assert_eq!(ticket_in("someone/ENG-1234-thing"), Some("ENG-1234".into()));
        assert_eq!(ticket_in("feature/abc2-7"), Some("ABC2-7".into()));
        assert_eq!(ticket_in("fix-auth-eng-42"), Some("ENG-42".into()));
        assert_eq!(ticket_in("main"), None);
        assert_eq!(ticket_in("2-fast"), None);
        assert_eq!(ticket_in(""), None);
    }

    #[test]
    fn a_ticket_from_linear_s_answer() {
        let issue = json!({
            "id": "abc", "identifier": "ENG-1", "title": "Fix it",
            "url": "https://linear.app/x/issue/ENG-1", "state": { "name": "Todo" },
            "assignee": { "id": "u1", "name": "jo", "displayName": "Jo" }
        });
        let ticket = ticket_from(&issue).unwrap();
        assert_eq!(ticket.state, "Todo");
        assert_eq!(ticket.assignee.unwrap().name, "Jo");
        let unassigned =
            json!({ "id": "a", "identifier": "ENG-2", "title": "t", "assignee": null });
        assert_eq!(ticket_from(&unassigned).unwrap().assignee, None);
        assert_eq!(ticket_from(&Value::Null), None);
        assert_eq!(ticket_from(&json!({ "id": 5 })), None);
    }
}
