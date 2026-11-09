//! Jira's rich text, the Atlassian Document Format (a tree of JSON nodes), as
//! the Markdown the ticket view and image draw, ported from the tmux
//! version's `lib/tracker/adf.js`. Every node Jira's editor makes has a rule
//! here; one it doesn't know is read for its text, so nothing is lost, only
//! plainer. Attachments have no address Markdown could show, so they read as
//! their name.
//!
//! <https://developer.atlassian.com/cloud/jira/platform/apis/document/structure/>

use serde_json::Value;

/// Markdown from a document, or from a string (a few fields, and Jira's
/// older API, still give plain text).
pub fn markdown_of(doc: &Value) -> String {
    match doc {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        _ => blocks(content(doc)).trim().to_string(),
    }
}

fn content(node: &Value) -> &[Value] {
    node.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn attr<'a>(node: &'a Value, key: &str) -> Option<&'a Value> {
    node.get("attrs").and_then(|a| a.get(key))
}

fn attr_text<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    attr(node, key)
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
}

fn kind(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}

// A text node with its marks, code first, since nothing inside backticks is
// read as Markdown. Spaces stay outside the marks, where Markdown can read
// them.
fn text(node: &Value) -> String {
    let raw = node.get("text").and_then(Value::as_str).unwrap_or("");
    let core = raw.trim();
    if core.is_empty() {
        return raw.to_string();
    }
    let lead = &raw[..raw.len() - raw.trim_start().len()];
    let trail = &raw[raw.trim_end().len()..];
    let mut marks: Vec<&Value> = node
        .get("marks")
        .and_then(Value::as_array)
        .map(|m| m.iter().collect())
        .unwrap_or_default();
    marks.sort_by_key(|m| kind(m) != "code");
    let mut out = core.to_string();
    for mark in marks {
        out = match kind(mark) {
            "strong" => format!("**{out}**"),
            "em" => format!("_{out}_"),
            "strike" => format!("~~{out}~~"),
            "code" => format!("`{out}`"),
            "link" => match attr_text(mark, "href") {
                Some(href) => format!("[{out}]({href})"),
                None => out,
            },
            _ => out,
        };
    }
    format!("{lead}{out}{trail}")
}

fn inline(nodes: &[Value]) -> String {
    nodes.iter().map(inline_node).collect()
}

fn inline_node(node: &Value) -> String {
    match kind(node) {
        "text" => text(node),
        "hardBreak" => "  \n".into(),
        "mention" => match attr_text(node, "text") {
            Some(name) if name.starts_with('@') => name.to_string(),
            Some(name) => format!("@{name}"),
            None => "@someone".into(),
        },
        "emoji" => attr_text(node, "text")
            .or_else(|| attr_text(node, "shortName"))
            .unwrap_or("")
            .to_string(),
        "inlineCard" => attr_text(node, "url")
            .map(|url| format!("<{url}>"))
            .unwrap_or_default(),
        "status" => attr_text(node, "text")
            .map(|t| format!("**[{t}]**"))
            .unwrap_or_default(),
        "date" => attr(node, "timestamp")
            .and_then(|t| {
                t.as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| t.as_i64())
            })
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
            .map(|t| t.strftime("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        "placeholder" => String::new(),
        _ if node.get("content").is_some() => inline(content(node)),
        _ => node
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

// Every line after the first moved in under the marker it follows, so a
// list item's later lines stay in the item.
fn indent(block: &str, prefix: &str) -> String {
    let pad = " ".repeat(prefix.chars().count());
    block
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            if i == 0 || line.is_empty() {
                line.to_string()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_list(node: &Value) -> bool {
    matches!(kind(node), "bulletList" | "orderedList" | "taskList")
}

// A list item's paragraphs, with a list inside it kept tight against the
// line it belongs under.
fn item_body(nodes: &[Value]) -> String {
    let mut out = String::new();
    for (i, (node, text)) in nodes
        .iter()
        .map(|n| (n, block(n)))
        .filter(|(_, t)| !t.is_empty())
        .enumerate()
    {
        if i > 0 {
            out.push_str(if is_list(node) { "\n" } else { "\n\n" });
        }
        out.push_str(&text);
    }
    out
}

fn list(node: &Value, ordered: bool) -> String {
    let start = attr(node, "order").and_then(Value::as_u64).unwrap_or(1);
    content(node)
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let prefix = if ordered {
                format!("{}. ", start + i as u64)
            } else {
                "- ".to_string()
            };
            format!(
                "{prefix}{}",
                indent(item_body(content(item)).trim(), &prefix)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tasks(node: &Value) -> String {
    content(node)
        .iter()
        .map(|item| {
            if kind(item) == "taskList" {
                indent(&format!("  {}", tasks(item)), "  ")
            } else {
                let done = attr(item, "state").and_then(Value::as_str) == Some("DONE");
                format!(
                    "- [{}] {}",
                    if done { "x" } else { " " },
                    inline(content(item))
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cell(node: &Value) -> String {
    blocks(content(node))
        .trim()
        .split('\n')
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn table(node: &Value) -> String {
    let rows: Vec<Vec<String>> = content(node)
        .iter()
        .map(|row| content(row).iter().map(cell).collect())
        .collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let line = |row: &Vec<String>| {
        let mut cells = row.clone();
        cells.resize(width, String::new());
        format!("| {} |", cells.join(" | "))
    };
    let mut out = vec![line(&rows[0]), format!("|{}", " --- |".repeat(width))];
    out.extend(rows[1..].iter().map(line));
    out.join("\n")
}

fn quote(block: &str) -> String {
    block
        .split('\n')
        .map(|l| {
            if l.is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn attachment(node: &Value) -> String {
    match attr_text(node, "alt") {
        Some(alt) => format!("_[attachment: {alt}]_"),
        None => "_[attachment]_".into(),
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn block(node: &Value) -> String {
    match kind(node) {
        "paragraph" => inline(content(node)),
        "heading" => {
            let level = attr(node, "level").and_then(Value::as_u64).unwrap_or(1);
            format!(
                "{} {}",
                "#".repeat(level.clamp(1, 6) as usize),
                inline(content(node))
            )
        }
        "bulletList" => list(node, false),
        "orderedList" => list(node, true),
        "taskList" => tasks(node),
        "decisionList" => content(node)
            .iter()
            .map(|item| format!("- ✓ {}", inline(content(item))))
            .collect::<Vec<_>>()
            .join("\n"),
        "codeBlock" => {
            let code: String = content(node)
                .iter()
                .map(|t| t.get("text").and_then(Value::as_str).unwrap_or(""))
                .collect();
            let language = attr(node, "language").and_then(Value::as_str).unwrap_or("");
            format!("```{language}\n{code}\n```")
        }
        "blockquote" => quote(&blocks(content(node))),
        "panel" => {
            let panel = attr_text(node, "panelType").unwrap_or("note");
            quote(&format!(
                "**{}:** {}",
                capitalized(panel),
                blocks(content(node)).trim()
            ))
        }
        "expand" | "nestedExpand" => [
            attr_text(node, "title")
                .map(|t| format!("**{t}**"))
                .unwrap_or_default(),
            blocks(content(node)),
        ]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"),
        "rule" => "---".into(),
        "table" => table(node),
        "blockCard" | "embedCard" => attr_text(node, "url")
            .map(|url| format!("<{url}>"))
            .unwrap_or_default(),
        "mediaSingle" | "mediaGroup" => content(node)
            .iter()
            .map(attachment)
            .collect::<Vec<_>>()
            .join(" "),
        "media" => attachment(node),
        "layoutSection" | "layoutColumn" | "bodiedExtension" => blocks(content(node)),
        "extension" | "inlineExtension" => String::new(),
        _ if node.get("content").is_some() => blocks(content(node)),
        _ => inline_node(node),
    }
}

fn blocks(nodes: &[Value]) -> String {
    nodes
        .iter()
        .map(block)
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(content: Value) -> Value {
        json!({ "type": "doc", "version": 1, "content": content })
    }

    fn t(text: &str) -> Value {
        json!({ "type": "text", "text": text })
    }

    fn p(content: Value) -> Value {
        json!({ "type": "paragraph", "content": content })
    }

    #[test]
    fn marks_wrap_the_words_and_leave_the_spaces_outside() {
        let marked = json!({ "type": "text", "text": " bold ", "marks": [{ "type": "strong" }] });
        let linked = json!({ "type": "text", "text": "the docs", "marks": [{ "type": "link", "attrs": { "href": "https://x.test/d" } }] });
        let code = json!({ "type": "text", "text": "a*b", "marks": [{ "type": "em" }, { "type": "code" }] });
        assert_eq!(
            markdown_of(&doc(json!([p(json!([
                t("a"),
                marked,
                t("and"),
                linked,
                t(" "),
                code
            ]))]))),
            "a **bold** and[the docs](https://x.test/d) _`a*b`_"
        );
        assert_eq!(
            text(&json!({ "type": "text", "text": "   ", "marks": [{ "type": "strong" }] })),
            "   "
        );
    }

    #[test]
    fn headings_lists_and_tasks() {
        let item = |words: &str| json!({ "type": "listItem", "content": [p(json!([t(words)]))] });
        let nested = json!({ "type": "listItem", "content": [
            p(json!([t("parent")])),
            { "type": "bulletList", "content": [item("child")] }
        ] });
        let tasks = json!({ "type": "taskList", "content": [
            { "type": "taskItem", "attrs": { "state": "DONE" }, "content": [t("shipped")] },
            { "type": "taskItem", "attrs": { "state": "TODO" }, "content": [t("next")] }
        ] });
        let out = markdown_of(&doc(json!([
            { "type": "heading", "attrs": { "level": 9 }, "content": [t("Plan")] },
            { "type": "orderedList", "attrs": { "order": 3 }, "content": [item("three"), nested] },
            tasks
        ])));
        assert_eq!(
            out,
            "###### Plan\n\n3. three\n4. parent\n   - child\n\n- [x] shipped\n- [ ] next"
        );
    }

    #[test]
    fn code_quotes_panels_tables_and_what_has_no_rule() {
        let out = markdown_of(&doc(json!([
            { "type": "codeBlock", "attrs": { "language": "rust" }, "content": [t("let a = 1;")] },
            { "type": "panel", "attrs": { "panelType": "warning" }, "content": [p(json!([t("Careful")]))] },
            { "type": "table", "content": [
                { "type": "tableRow", "content": [
                    { "type": "tableHeader", "content": [p(json!([t("Name")]))] },
                    { "type": "tableHeader", "content": [p(json!([t("Note")]))] }
                ] },
                { "type": "tableRow", "content": [
                    { "type": "tableCell", "content": [p(json!([t("a|b")]))] }
                ] }
            ] },
            { "type": "mediaSingle", "content": [{ "type": "media", "attrs": { "alt": "shot.png" } }] },
            { "type": "somethingNew", "content": [p(json!([t("still read")]))] },
            p(json!([
                { "type": "mention", "attrs": { "text": "Ada" } },
                t(" on "),
                { "type": "date", "attrs": { "timestamp": "1700000000000" } },
                t(" "),
                { "type": "status", "attrs": { "text": "BLOCKED" } }
            ]))
        ])));
        assert_eq!(
            out,
            "```rust\nlet a = 1;\n```\n\n> **Warning:** Careful\n\n| Name | Note |\n| --- | --- |\n| a\\|b |  |\n\n_[attachment: shot.png]_\n\nstill read\n\n@Ada on 2023-11-14 **[BLOCKED]**"
        );
    }

    #[test]
    fn plain_text_and_nothing() {
        assert_eq!(markdown_of(&json!("already text")), "already text");
        assert_eq!(markdown_of(&Value::Null), "");
    }
}
