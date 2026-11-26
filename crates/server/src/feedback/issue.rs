//! The issue folder's writer: titles, slugs, file names, `issue.md`, the
//! intent's seed text and the GitHub issue body. Pure: no git, no I/O.
//!
//! Every string that comes from the user is hostile. Slugs and file names
//! are made only from lower-case ASCII letters, digits and hyphens (or fixed
//! words); anything shown in Markdown is cleaned to one line and escaped; the
//! front matter holds JSON-quoted strings only.

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use smithagents_core::feedback::{AttachmentKind, FeedbackAttachment, FeedbackKind, SubmissionId};
use smithagents_core::intent::slugify;

use crate::plugins::clean_message;

/// The stored format of a file the daemon writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Mp4,
}

impl FileKind {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Mp4 => "mp4",
        }
    }
}

pub const TITLE_MAX_CHARS: usize = 120;
pub const MAIN_IDEA_MAX_CHARS: usize = 20_000;
/// The longest issue body GitHub is sent.
const BODY_MAX_CHARS: usize = 60_000;
const NOT_DONE_MAX_CHARS: usize = 500;
const NAME_MAX_CHARS: usize = 120;
const FIELD_MAX_CHARS: usize = 64;
const SLUG_FALLBACK: &str = "feedback";

/// The first non-blank line, without leading `#` marks and spaces, cleaned
/// and cut to [`TITLE_MAX_CHARS`] characters. `None` when nothing is left.
pub fn title_of(main_idea: &str) -> Option<String> {
    let line = main_idea.lines().find(|l| !l.trim().is_empty())?;
    // Bound the work on an enormous line before cleaning.
    let line: String = line.chars().take(4 * TITLE_MAX_CHARS + 64).collect();
    let line = line.trim_start().trim_start_matches('#');
    let cleaned = clean_message(line);
    let title: String = cleaned.trim().chars().take(TITLE_MAX_CHARS).collect();
    let title = title.trim().to_string();
    (!title.is_empty()).then_some(title)
}

/// `slugify(title)`; `feedback` when the title has no ASCII letter or digit.
pub fn issue_slug(title: &str) -> String {
    if title.chars().any(|c| c.is_ascii_alphanumeric()) {
        slugify(title)
    } else {
        SLUG_FALLBACK.to_string()
    }
}

/// `slug` for n of 1 (or 0), else `<slug>-<n>`.
pub fn with_suffix(slug: &str, n: u32) -> String {
    if n <= 1 {
        slug.to_string()
    } else {
        format!("{slug}-{n}")
    }
}

/// A slug that is safe in a path and a branch name, whatever the caller
/// passed: the slug itself when it only holds lower-case ASCII letters,
/// digits and inner hyphens and is short, else made from it.
fn safe_slug(slug: &str) -> String {
    let plain = !slug.is_empty()
        && slug.len() <= 64
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if plain {
        slug.to_string()
    } else {
        issue_slug(slug)
    }
}

/// `docs/issues/<date>-<slug>`.
pub fn folder_path(date: NaiveDate, slug: &str) -> String {
    format!(
        "docs/issues/{}-{}",
        date.format("%Y-%m-%d"),
        safe_slug(slug)
    )
}

/// `feedback/<date>-<slug>`, or `idea/<slug>` for an idea.
pub fn branch_name(date: NaiveDate, slug: &str, idea: bool) -> String {
    let slug = safe_slug(slug);
    if idea {
        format!("idea/{slug}")
    } else {
        format!("feedback/{}-{slug}", date.format("%Y-%m-%d"))
    }
}

/// The names the daemon gives one attachment's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub file: String,
    pub annotated: Option<String>,
}

/// One entry per attachment, counting per prefix from 1 in order.
pub fn file_names(attachments: &[(AttachmentKind, FileKind)]) -> Vec<Names> {
    let (mut shots, mut images, mut clips) = (0u32, 0u32, 0u32);
    attachments
        .iter()
        .map(|(kind, file)| match kind {
            AttachmentKind::Screenshot => {
                shots += 1;
                Names {
                    file: format!("screenshot-{shots}.png"),
                    annotated: Some(format!("screenshot-{shots}.annotated.png")),
                }
            }
            AttachmentKind::Image => {
                images += 1;
                Names {
                    file: format!("image-{images}.{}", file.extension()),
                    annotated: None,
                }
            }
            AttachmentKind::Video => {
                clips += 1;
                Names {
                    file: format!("clip-{clips}.mp4"),
                    annotated: None,
                }
            }
        })
        .collect()
}

pub struct IssueInput<'a> {
    pub title: &'a str,
    pub main_idea: &'a str,
    pub kind: FeedbackKind,
    pub created: DateTime<Utc>,
    pub app_version: &'a str,
    pub platform: &'a str,
    pub attachments: &'a [FeedbackAttachment],
    pub names: &'a [Names],
}

/// A one-line, bounded version of user text: cleaned, with controls and
/// bidi marks as spaces.
fn one_line(text: &str, max_chars: usize) -> String {
    let head: String = text
        .chars()
        .take(max_chars.saturating_mul(4).max(64))
        .collect();
    let cleaned = clean_message(&head);
    cleaned
        .trim()
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Backslash-escapes what Markdown or GitHub would act on, so one line of
/// text stays text: no heading, list, quote, emphasis, link, image or HTML.
fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for (i, c) in text.chars().enumerate() {
        let leading_marker = i == 0 && matches!(c, '-' | '+' | '=');
        if leading_marker
            || matches!(
                c,
                '\\' | '`'
                    | '*'
                    | '_'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '#'
                    | '!'
                    | '|'
                    | '~'
                    | '&'
            )
        {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A name for a heading or a link text: cleaned, without `[ ] ( ) ` #`,
/// without its leading path, `attachment` when empty.
fn display_name(original: &str) -> String {
    let cleaned: String = one_line(original, 4 * NAME_MAX_CHARS)
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '(' | ')' | '`' | '#'))
        .collect();
    let cleaned = cleaned.trim();
    // A path comes first and has no spaces to speak of: drop it from the
    // first word, wherever its last separator is.
    let word_end = cleaned.find(char::is_whitespace).unwrap_or(cleaned.len());
    let start = cleaned[..word_end].rfind(['/', '\\']).map_or(0, |i| i + 1);
    let name: String = cleaned[start..]
        .trim()
        .chars()
        .take(NAME_MAX_CHARS)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "attachment".to_string()
    } else {
        escape_md(name)
    }
}

/// A front matter value: plain when it is a short run of safe characters,
/// else a JSON string (with the line separators YAML knows escaped).
fn yaml_scalar(text: &str) -> String {
    let short: String = text.chars().take(FIELD_MAX_CHARS).collect();
    let plain = !short.is_empty()
        && short
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        && !short.starts_with('-');
    if plain {
        short
    } else {
        json_string(&short)
    }
}

fn json_string(text: &str) -> String {
    let json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if c.is_control()
            || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Percent-encodes everything but the RFC 3986 unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// The main idea without its first non-blank line, trimmed, with control
/// characters other than line breaks and tabs removed.
fn idea_body(main_idea: &str) -> String {
    let bounded: String = main_idea.chars().take(MAIN_IDEA_MAX_CHARS).collect();
    let mut lines = bounded.lines().skip_while(|l| l.trim().is_empty());
    lines.next();
    let rest: Vec<&str> = lines.collect();
    let rest: String = rest
        .join("\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .filter(|c| !matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .collect();
    rest.trim().to_string()
}

/// The text of a comment, as one line.
fn comment_line(comment: &str) -> String {
    one_line(comment, 500)
}

/// `issue.md`.
pub fn render_issue(input: &IssueInput<'_>) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!(
        "title: {}\n",
        json_string(
            &input
                .title
                .chars()
                .take(TITLE_MAX_CHARS)
                .collect::<String>()
        )
    ));
    out.push_str(&format!("kind: {}\n", input.kind.intent_kind().wire_name()));
    out.push_str(&format!(
        "created: {}\n",
        input.created.to_rfc3339_opts(SecondsFormat::Secs, true)
    ));
    out.push_str(&format!(
        "app_version: {}\n",
        yaml_scalar(input.app_version)
    ));
    out.push_str(&format!("platform: {}\n", yaml_scalar(input.platform)));
    out.push_str("---\n\n");
    out.push_str(&format!(
        "# {}\n",
        escape_md(&one_line(input.title, TITLE_MAX_CHARS))
    ));

    let body = idea_body(input.main_idea);
    if !body.is_empty() {
        out.push_str(&format!("\n{body}\n"));
    }

    let mut shot = 0usize;
    for (i, a) in input.attachments.iter().enumerate() {
        if a.kind == AttachmentKind::Screenshot {
            shot += 1;
        }
        let Some(names) = input.names.get(i) else {
            continue;
        };
        match a.kind {
            AttachmentKind::Screenshot => {
                let shown = names.annotated.as_deref().unwrap_or(&names.file);
                out.push_str(&format!(
                    "\n## Screenshot {shot}\n\n![Screenshot {shot}]({})\n",
                    percent_encode(shown)
                ));
                if !a.boxes.is_empty() {
                    out.push('\n');
                    for (j, b) in a.boxes.iter().enumerate() {
                        let c = escape_md(&comment_line(&b.comment));
                        out.push_str(format!("{}. {c}", j + 1).trim_end());
                        out.push('\n');
                    }
                }
                let f = &names.file;
                out.push_str(&format!("\nOriginal: [{f}]({})\n", percent_encode(f)));
            }
            AttachmentKind::Image => {
                let n = display_name(&a.original_name);
                out.push_str(&format!(
                    "\n## {n}\n\n![{n}]({})\n",
                    percent_encode(&names.file)
                ));
            }
            AttachmentKind::Video => {
                let n = display_name(&a.original_name);
                out.push_str(&format!(
                    "\n## {n}\n\n[{n}]({})\n",
                    percent_encode(&names.file)
                ));
            }
        }
    }
    out
}

/// The text an intent is seeded from.
pub fn seed_text(
    input: &IssueInput<'_>,
    folder: &str,
    branch: &str,
    commit: &str,
    max_bytes: usize,
) -> String {
    const MORE: &str = "- (more comments in issue.md)";
    let idea: String = input.main_idea.chars().take(MAIN_IDEA_MAX_CHARS).collect();
    let mut head = format!(
        "{}\n\nKind: {}",
        idea.trim(),
        input.kind.intent_kind().wire_name()
    );

    let mut lines = Vec::new();
    let mut shot = 0usize;
    for a in input.attachments {
        if a.kind != AttachmentKind::Screenshot {
            continue;
        }
        shot += 1;
        for (j, b) in a.boxes.iter().enumerate() {
            lines.push(format!(
                "- Screenshot {shot}, box {}: {}",
                j + 1,
                comment_line(&b.comment)
            ));
        }
    }

    if !lines.is_empty() {
        head.push_str("\nComments on the screenshots:");
        let mut used = head.len();
        let mut kept = 0;
        for (i, line) in lines.iter().enumerate() {
            let reserve = if i + 1 < lines.len() {
                MORE.len() + 1
            } else {
                0
            };
            if used + 1 + line.len() + reserve > max_bytes {
                break;
            }
            used += 1 + line.len();
            kept += 1;
        }
        for line in &lines[..kept] {
            head.push('\n');
            head.push_str(line);
        }
        if kept < lines.len() {
            head.push('\n');
            head.push_str(MORE);
        }
    }
    format!("{head}\n\nFeedback files: `{folder}/` on branch `{branch}` (commit {commit}).")
}

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// `(owner, repository)` for a github.com remote URL. Credentials in the
/// URL are ignored and never returned.
pub fn github_repo(remote_url: &str) -> Option<(String, String)> {
    let url = remote_url.trim();
    let path = if let Some(rest) =
        strip_prefix_ci(url, "https://").or_else(|| strip_prefix_ci(url, "ssh://"))
    {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    } else if url.contains("://") {
        return None;
    } else {
        // scp-like: user@github.com:owner/repo
        let (host, path) = url.split_once(':')?;
        let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    (valid_part(owner) && valid_part(repo)).then(|| (owner.to_string(), repo.to_string()))
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// `https://github.com/<owner>/<repo>/blob/<commit>/<folder>/`.
pub fn permalink_base(owner: &str, repo: &str, commit: &str, folder: &str) -> String {
    format!(
        "https://github.com/{}/{}/blob/{}/{}/",
        percent_encode(owner),
        percent_encode(repo),
        percent_encode(commit),
        encode_path(folder)
    )
}

/// `issue.md` without its front matter, links to the folder's files turned
/// into permalinks, the marker last, cut to 60,000 characters.
pub fn issue_body(
    issue_md: &str,
    files: &[String],
    permalink_base: &str,
    marker: Option<&str>,
) -> String {
    let text = strip_front_matter(issue_md);
    let mut body = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        let (before, after) = rest.split_at(at + 2);
        body.push_str(before);
        match after.find(')') {
            Some(end) if files.iter().any(|f| f == &after[..end]) => {
                body.push_str(permalink_base);
                body.push_str(&percent_encode(&after[..end]));
                body.push_str("?raw=true");
                rest = &after[end..];
            }
            _ => rest = after,
        }
    }
    body.push_str(rest);

    let suffix = marker.map(|m| format!("\n\n{m}")).unwrap_or_default();
    let room = BODY_MAX_CHARS.saturating_sub(suffix.chars().count());
    if body.chars().count() > room {
        body = body.chars().take(room).collect();
    }
    body.push_str(&suffix);
    body
}

fn strip_front_matter(md: &str) -> &str {
    let Some(rest) = md.strip_prefix("---\n") else {
        return md;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            return rest[offset..].trim_start_matches('\n');
        }
    }
    md
}

/// `<!-- smithagents-feedback:<uuid> -->`.
pub fn marker(id: SubmissionId) -> String {
    format!("<!-- smithagents-feedback:{id} -->")
}

/// The text under a "Not done" heading, cleaned, at most 500 characters.
pub fn not_done_section(explainer_md: &str) -> Option<String> {
    let mut in_fence = false;
    let mut inside = false;
    let mut text = String::new();
    for line in explainer_md.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
        }
        let heading = !in_fence && t.starts_with('#');
        if heading {
            if inside {
                break;
            }
            let title = t.trim_start_matches('#').trim();
            if t.len() - t.trim_start_matches('#').len() <= 6
                && title.eq_ignore_ascii_case("not done")
            {
                inside = true;
            }
        } else if inside {
            text.push_str(line);
            text.push('\n');
            if text.len() > 8 * NOT_DONE_MAX_CHARS {
                break;
            }
        }
    }
    let cleaned: String = one_line(&text, NOT_DONE_MAX_CHARS);
    // Collapse the spaces the line breaks left.
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}
