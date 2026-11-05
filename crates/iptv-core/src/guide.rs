//! The offline User Guide (port of the Python app's `user_guide.py`).
//!
//! Guide Markdown (one file per language under `docs/help/`) renders
//! to the plain text a screen reader navigates: headings become
//! sections with stable `{#anchor}` topics, lists keep their items,
//! and inline Markdown markers are stripped while keeping the words.

use std::path::{Path, PathBuf};

/// Directory parts from the app root (port of `GUIDE_DIR_PARTS`).
pub const GUIDE_DIR_PARTS: [&str; 2] = ["docs", "help"];
/// The fallback language (port of `FALLBACK_LANGUAGE`).
pub const FALLBACK_LANGUAGE: &str = "en";
/// The guide's title section (port of `DEFAULT_TOPIC`).
pub const DEFAULT_TOPIC: &str = "user-guide";
/// How the help window itself works (port of `HELP_TOPIC`).
pub const HELP_TOPIC: &str = "using-help";

/// One heading of the guide and where it starts in the rendered text.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// The section's stable topic id.
    pub topic: String,
    /// The section's title text.
    pub title: String,
    /// Heading level (1-3).
    pub level: usize,
    /// Character offset into the rendered text.
    pub start: usize,
}

/// A rendered guide (port of `Guide`).
#[derive(Clone, Debug)]
pub struct Guide {
    /// The language this guide was rendered from.
    pub language: String,
    /// The full plain text shown to the user.
    pub text: String,
    /// Its sections, in document order.
    pub sections: Vec<Section>,
}

impl Guide {
    /// The section for `topic`, when present (port of `find`).
    #[must_use]
    pub fn find(&self, topic: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.topic == topic)
    }

    /// The section a character offset falls in (port of
    /// `section_at`).
    #[must_use]
    pub fn section_at(&self, position: usize) -> Option<&Section> {
        let mut current = self.sections.first();
        for section in &self.sections {
            if section.start > position {
                break;
            }
            current = Some(section);
        }
        current
    }

    /// All topic ids in order (port of `topics`).
    #[must_use]
    pub fn topics(&self) -> Vec<String> {
        self.sections
            .iter()
            .map(|section| section.topic.clone())
            .collect()
    }
}

/// Strip inline Markdown markers, keeping the words (port of
/// `_inline`): code spans lose their backticks, links keep their
/// label (plus the URL unless it is an in-page anchor), bold and
/// emphasis lose their asterisks.
#[must_use]
pub fn inline(text: &str) -> String {
    let text = strip_code_spans(text);
    let text = strip_links(&text);
    let text = strip_paired(&text, "**");
    strip_emphasis(&text)
}

fn strip_code_spans(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('`') {
            out.push_str(&after[..end]);
            rest = &after[end + 1..];
        } else {
            rest = after;
            break;
        }
    }
    out.push_str(rest);
    out
}

fn strip_links(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](") else {
            break;
        };
        let label_end = open + close;
        let url_start = label_end + 2;
        let Some(url_end_rel) = rest[url_start..].find([')', ' ', '\t']) else {
            break;
        };
        let url_end = url_start + url_end_rel;
        if rest.as_bytes().get(url_end) != Some(&b')') {
            break;
        }
        let label = &rest[open + 1..label_end];
        let url = &rest[url_start..url_end];
        out.push_str(&rest[..open]);
        if url.starts_with('#') {
            out.push_str(label);
        } else {
            let _ = write!(out, "{label} ({url})");
        }
        rest = &rest[url_end + 1..];
    }
    out.push_str(rest);
    out
}

fn strip_paired(text: &str, marker: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        match after.find(marker) {
            Some(end) if end > 0 => {
                out.push_str(&rest[..start]);
                out.push_str(&after[..end]);
                rest = &after[end + marker.len()..];
            }
            _ => break,
        }
    }
    out.push_str(rest);
    out
}

fn strip_emphasis(text: &str) -> String {
    // Approximates the reference's guarded emphasis regex: an opening
    // `*` not preceded by a word char or `*`, content not starting or
    // ending with a space, and a closing `*` not followed by a word
    // char or `*`.
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '*'
            && (index == 0 || !(chars[index - 1].is_alphanumeric() || chars[index - 1] == '_'))
            && index + 1 < chars.len()
            && !chars[index + 1].is_whitespace()
        {
            let mut close = index + 1;
            let mut found = None;
            while close < chars.len() {
                if chars[close] == '*'
                    && !chars[close - 1].is_whitespace()
                    && (close + 1 >= chars.len()
                        || !(chars[close + 1].is_alphanumeric()
                            || chars[close + 1] == '_'
                            || chars[close + 1] == '*'))
                {
                    found = Some(close);
                    break;
                }
                close += 1;
            }
            if let Some(close) = found {
                out.extend(&chars[index + 1..close]);
                index = close + 1;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// Remove `<!-- ... -->` comments (port of `_COMMENT_RE`).
fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start + 4..].find("-->") {
            rest = &rest[start + 4 + end + 3..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

struct TextBuilder {
    parts: Vec<String>,
    length: usize,
}

impl TextBuilder {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            length: 0,
        }
    }

    fn add_block(&mut self, lines: &[String]) -> usize {
        let separator = if self.parts.is_empty() { "" } else { "\n\n" };
        let offset = self.length + separator.chars().count();
        let chunk = format!("{}{}", separator, lines.join("\n"));
        self.length += chunk.chars().count();
        self.parts.push(chunk);
        offset
    }

    fn text(&self) -> String {
        self.parts.concat()
    }
}

fn heading(line: &str) -> Option<(usize, String)> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if (1..=3).contains(&level) && line.chars().nth(level).is_some_and(char::is_whitespace) {
        Some((level, line[level..].trim().to_string()))
    } else {
        None
    }
}

fn anchor_of(title_source: &str) -> (String, Option<String>) {
    let trimmed = title_source.trim_end();
    if let Some(start) = trimmed.rfind("{#") {
        if let Some(end_part) = trimmed[start + 2..].strip_suffix('}') {
            let anchor = end_part.trim();
            let valid = !anchor.is_empty()
                && anchor
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
                && anchor
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
            if valid {
                return (
                    trimmed[..start].trim().to_string(),
                    Some(anchor.to_string()),
                );
            }
        }
    }
    (title_source.to_string(), None)
}

fn bullet(line: &str) -> Option<String> {
    let stripped = line.trim_start();
    let indent = line.len() - stripped.len();
    if indent <= 4 {
        for marker in ["- ", "* "] {
            if let Some(rest) = stripped.strip_prefix(marker) {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

fn numbered(line: &str) -> Option<(String, String)> {
    let stripped = line.trim_start();
    let digits: String = stripped.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &stripped[digits.len()..];
    for closer in ['.', ')'] {
        if let Some(after) = rest.strip_prefix(closer) {
            if after.starts_with(char::is_whitespace) {
                return Some((format!("{digits}{closer}"), after.trim().to_string()));
            }
        }
    }
    None
}

/// Turn guide Markdown into the plain text shown in the help window
/// (port of `render`).
#[must_use]
pub fn render(markdown: &str, language: &str) -> Guide {
    let source = markdown.replace("\r\n", "\n").replace('\r', "\n");
    let source = strip_comments(&source);
    let mut builder = TextBuilder::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut block: Vec<String> = Vec::new();
    let mut block_is_list = false;

    macro_rules! flush {
        () => {
            if !block.is_empty() {
                let was_list = std::mem::replace(&mut block_is_list, false);
                if was_list {
                    let lines = std::mem::take(&mut block);
                    builder.add_block(&lines);
                } else {
                    let joined = block.join(" ");
                    block.clear();
                    builder.add_block(&[joined]);
                }
            }
        };
    }

    for raw in source.split('\n') {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            flush!();
            continue;
        }
        if let Some((level, title_source)) = heading(line) {
            flush!();
            let (title_text, anchor) = anchor_of(&title_source);
            let title = inline(&title_text).trim().to_string();
            let topic = anchor.unwrap_or_else(|| format!("section-{}", sections.len() + 1));
            let start = builder.add_block(std::slice::from_ref(&title));
            sections.push(Section {
                topic,
                title,
                level,
                start,
            });
            continue;
        }
        if let Some(text) = bullet(line) {
            if !block_is_list {
                flush!();
                block_is_list = true;
            }
            block.push(inline(&text));
            continue;
        }
        if let Some((marker, text)) = numbered(line) {
            if !block_is_list {
                flush!();
                block_is_list = true;
            }
            block.push(format!("{marker} {}", inline(&text)));
            continue;
        }
        if block_is_list && (raw.starts_with(' ') || raw.starts_with('\t')) {
            if let Some(last) = block.last_mut() {
                *last = format!("{} {}", last, inline(line.trim()));
            }
            continue;
        }
        if block_is_list {
            flush!();
        }
        block.push(inline(line.trim()));
    }
    flush!();
    Guide {
        language: language.to_string(),
        text: builder.text(),
        sections,
    }
}

/// The guide directory beside an executable: `<exe dir>/docs/help`
/// when that folder exists (port of `user_guide.guide_dir`, which
/// resolves against the installation/bundle directory, never the
/// working directory).
#[must_use]
pub fn exe_relative_guide_dir(exe: &Path) -> Option<PathBuf> {
    let parent = exe.parent()?;
    let mut path = parent.to_path_buf();
    for part in GUIDE_DIR_PARTS {
        path.push(part);
    }
    path.is_dir().then_some(path)
}

/// The guide directory: `IPTV_GUIDE_DIR`, else `docs/help` beside
/// the executable, else `docs/help` in the working directory as a
/// last resort for development checkouts.
#[must_use]
pub fn guide_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("IPTV_GUIDE_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe_relative_guide_dir(&exe) {
            return dir;
        }
    }
    let mut path = PathBuf::new();
    for part in GUIDE_DIR_PARTS {
        path.push(part);
    }
    path
}

fn language_candidates(language: Option<&str>) -> Vec<String> {
    let code = language
        .unwrap_or("")
        .trim()
        .to_lowercase()
        .replace('-', "_");
    let code = code.split('_').next().unwrap_or("").to_string();
    let mut candidates = Vec::new();
    if !code.is_empty() && code != FALLBACK_LANGUAGE {
        candidates.push(code);
    }
    candidates.push(FALLBACK_LANGUAGE.to_string());
    candidates
}

/// The guide file for exactly `language` (port of `guide_path`).
#[must_use]
pub fn guide_path(language: &str, directory: Option<&Path>) -> Option<PathBuf> {
    let path = directory
        .map_or_else(guide_dir, Path::to_path_buf)
        .join(format!("{language}.md"));
    path.is_file().then_some(path)
}

/// Language codes that have a guide file, English first (port of
/// `available_languages`).
#[must_use]
pub fn available_languages(directory: Option<&Path>) -> Vec<String> {
    let folder = directory.map_or_else(guide_dir, Path::to_path_buf);
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut codes: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".md"))
                .map(str::to_string)
        })
        .collect();
    codes.sort();
    if let Some(position) = codes.iter().position(|code| code == FALLBACK_LANGUAGE) {
        codes.remove(position);
        codes.insert(0, FALLBACK_LANGUAGE.to_string());
    }
    codes
}

/// The English guide compiled into the binary, so the help window
/// works before any packaging step places files on disk.
pub const EMBEDDED_ENGLISH: &str = include_str!("../../../docs/help/en.md");

/// The guide for `language`, or the English one when that language
/// has none (port of `load_guide`); the embedded English guide backs
/// the last resort instead of raising.
#[must_use]
pub fn load_guide(language: Option<&str>, directory: Option<&Path>) -> Guide {
    for code in language_candidates(language) {
        if let Some(path) = guide_path(&code, directory) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return render(&text, &code);
            }
        }
    }
    render(EMBEDDED_ENGLISH, FALLBACK_LANGUAGE)
}

/// The guide and the section to show for a context topic (port of
/// `open_topic`), including the English-section fallback for topics a
/// translation does not have yet; the manual's start never falls
/// back.
#[must_use]
pub fn open_topic(
    language: Option<&str>,
    topic: Option<&str>,
    directory: Option<&Path>,
) -> (Guide, Option<Section>) {
    let topic = topic.unwrap_or(DEFAULT_TOPIC);
    let guide = load_guide(language, directory);
    if let Some(section) = guide.find(topic) {
        return (guide.clone(), Some(section.clone()));
    }
    if topic != DEFAULT_TOPIC && guide.language != FALLBACK_LANGUAGE {
        let english = load_guide(Some(FALLBACK_LANGUAGE), directory);
        let found = english.find(topic).cloned();
        if let Some(section) = found {
            return (english, Some(section));
        }
    }
    let first = guide.sections.first().cloned();
    (guide, first)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# User Guide {#user-guide}\n\nWelcome to **Accessible** IPTV with `code` and a [link](https://example.com) plus [jump](#part).\n\n## First Steps\n\n- Open a playlist\n- Press Enter\n  continued line\n\n1. One\n2. Two\n\n<!-- hidden -->\nText after.\n";

    #[test]
    fn render_builds_sections_and_plain_text() {
        let guide = render(SAMPLE, "en");
        assert_eq!(guide.topics(), vec!["user-guide", "section-2"]);
        let first = guide.find("user-guide").expect("first");
        assert_eq!(first.title, "User Guide");
        assert_eq!(first.start, 0);
        assert!(guide.text.contains(
            "Welcome to Accessible IPTV with code and a link (https://example.com) plus jump."
        ));
        assert!(guide
            .text
            .contains("Open a playlist\nPress Enter continued line"));
        assert!(guide.text.contains("1. One\n2. Two"));
        assert!(!guide.text.contains("hidden"));
        let second = guide.find("section-2").expect("second");
        assert_eq!(
            guide.section_at(second.start).map(|s| &s.topic),
            guide.find("section-2").map(|s| &s.topic)
        );
    }

    #[test]
    fn guide_discovery_is_executable_relative_not_cwd() {
        // S08 regression: guides installed beside the executable
        // must be found whatever the working directory is.
        let root = std::env::temp_dir().join(format!("iptv-guide-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let help = root.join("docs").join("help");
        std::fs::create_dir_all(&help).expect("dirs");
        std::fs::write(help.join("es.md"), "# Guía {#user-guide}\n\nHola.\n").expect("guide");
        let exe = root.join("accessible-iptv-client");
        assert_eq!(exe_relative_guide_dir(&exe), Some(help.clone()));
        // A different folder without docs/help yields nothing.
        let bare = root.join("bare").join("app");
        assert_eq!(exe_relative_guide_dir(&bare), None);
        // And the installed guide actually loads through the path.
        let guide = load_guide(Some("es"), Some(&help));
        assert_eq!(guide.language, "es");
        assert!(guide.text.contains("Hola."));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bundled_guides_cover_pythons_languages() {
        // The repo's docs/help is what release staging ships
        // (validation 0091): the Python guide languages must all
        // be discoverable, English first, and each must render.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = root.join("docs").join("help");
        let languages = available_languages(Some(&dir));
        assert_eq!(languages.len(), 14);
        assert_eq!(
            languages.first().map(String::as_str),
            Some(FALLBACK_LANGUAGE)
        );
        for code in &languages {
            let guide = load_guide(Some(code), Some(&dir));
            assert_eq!(&guide.language, code);
            assert!(
                !guide.sections.is_empty(),
                "bundled guide for {code} rendered no sections"
            );
        }
    }

    #[test]
    fn embedded_english_guide_renders() {
        let guide = load_guide(Some("xx"), None);
        assert_eq!(guide.language, "en");
        assert!(!guide.sections.is_empty());
        assert!(guide.find(DEFAULT_TOPIC).is_some());
    }

    #[test]
    fn open_topic_falls_back_to_first_section() {
        let (guide, section) = open_topic(Some("en"), Some("missing-topic"), None);
        assert_eq!(
            section.map(|s| s.topic),
            guide.sections.first().map(|s| s.topic.clone())
        );
    }
}
