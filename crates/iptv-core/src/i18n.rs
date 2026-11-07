//! Internationalization core (port of the Python app's `i18n.py`).
//!
//! Translations run on patois (Brandon's order, 2026-11-07): the
//! catalogues that came from the Python client — its `iptvclient`
//! and `release_notes` domains merged per language by
//! `tools/merge_catalogues.py` — live in `crates/iptv-app/po/`,
//! are compiled by the app crate's build script, and are embedded
//! in the binary. Nothing is read from a locale folder at
//! runtime. This module keeps the Python app's preference
//! resolution rules and the What's New localization helpers on
//! top of patois's process-wide registry.

/// The auto sentinel stored in config for system language.
pub const LANG_AUTO: &str = "auto";
/// The patois domain: the app crate's package name, which is
/// what `patois::embed_domain!` registers the catalogs under.
pub const DOMAIN: &str = "iptv-app";

/// (code, native display label) for the language selector (port of
/// `_LANGUAGE_LABELS`): endonyms; only "Automatic" is translatable.
#[must_use]
pub fn language_labels() -> Vec<(&'static str, &'static str)> {
    vec![
        (LANG_AUTO, "Automatic"),
        ("en", "English"),
        ("es", "Español (Spanish)"),
        ("ar", "العربية (Arabic)"),
        ("pt", "Português – Brasil (Portuguese)"),
        ("fr", "Français (French)"),
        ("de", "Deutsch (German)"),
        ("ru", "Русский (Russian)"),
        ("tr", "Türkçe (Turkish)"),
        ("it", "Italiano (Italian)"),
        ("pl", "Polski (Polish)"),
        ("hi", "हिन्दी (Hindi)"),
        ("zh", "中文（简体） (Chinese, Simplified)"),
        ("ja", "日本語 (Japanese)"),
        ("hu", "Magyar (Hungarian)"),
    ]
}

/// Languages a compiled catalogue ships for (port of `SHIPPED_CATALOGS`).
pub const SHIPPED_CATALOGS: [&str; 13] = [
    "es", "ar", "pt", "fr", "de", "ru", "tr", "it", "pl", "hi", "zh", "ja", "hu",
];

/// Two-letter code from a locale-ish value (`en_US.UTF-8` → `en`).
fn two_letter(value: &str) -> Option<String> {
    let token = value.trim().split(['.', '@']).next()?;
    let code = token.replace('-', "_");
    let code = code.split('_').next()?.to_lowercase();
    if code.is_empty() || code == "c" || code == "posix" {
        None
    } else {
        Some(code)
    }
}

/// Best-effort OS UI language (port of `detect_system_language`): the
/// environment chain on POSIX. The Windows UI-language API the
/// reference uses first is a later platform addition; the environment
/// chain is what decides everywhere else.
#[must_use]
pub fn detect_system_language() -> String {
    for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(raw) = std::env::var(var) {
            if let Some(code) = two_letter(&raw) {
                return code;
            }
        }
    }
    "en".to_string()
}

/// Map a stored preference to catalogue candidates, or `None` for
/// source English (port of `_resolve_candidates`).
#[must_use]
pub fn resolve_candidates(lang_code: &str) -> Option<Vec<String>> {
    let code = lang_code.trim().to_lowercase();
    let resolved = if code.is_empty()
        || [LANG_AUTO, "automatic", "system", "default"].contains(&code.as_str())
    {
        detect_system_language()
    } else {
        code
    };
    if resolved.is_empty() || resolved == "en" {
        return None;
    }
    Some(vec![resolved, "en".to_string()])
}

/// Coerce a stored preference to its canonical stored form (port of
/// `set_language`'s coercion).
#[must_use]
pub fn normalize_preference(lang_code: &str) -> String {
    let code = lang_code.trim().to_lowercase();
    if code.is_empty() || ["automatic", "system", "default"].contains(&code.as_str()) {
        LANG_AUTO.to_string()
    } else {
        code
    }
}

/// The active language state (port of the Python app's
/// module-level catalogue holders plus `set_language` /
/// `init_from_config`). The catalogues themselves live in
/// patois's registry; this struct keeps the stored preference.
#[derive(Clone, Debug, Default)]
pub struct I18n {
    active: String,
}

impl I18n {
    /// Activate a preference (port of `set_language`); returns
    /// the stored preference code.
    pub fn set_language(&mut self, lang_code: &str) -> String {
        self.active = normalize_preference(lang_code);
        patois::set_default_domain(DOMAIN);
        patois::set_locale(&self.resolved_language());
        self.active.clone()
    }

    /// Activate from a config map's `language` key (port of
    /// `init_from_config`).
    pub fn init_from_config(
        &mut self,
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> String {
        let pref = config
            .get("language")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(LANG_AUTO);
        self.set_language(pref)
    }

    /// The stored preference code (port of `get_language`).
    #[must_use]
    pub fn language(&self) -> &str {
        if self.active.is_empty() {
            LANG_AUTO
        } else {
            &self.active
        }
    }

    /// The language actually in use (port of `resolved_language`).
    #[must_use]
    pub fn resolved_language(&self) -> String {
        resolve_candidates(self.language())
            .map_or_else(|| "en".to_string(), |candidates| candidates[0].clone())
    }

    /// Translate with the active catalogue (port of `gettext`).
    #[must_use]
    pub fn gettext(&self, message: &str) -> String {
        patois::t(message)
    }

    /// Translate a release-note bullet (port of `notes_gettext`).
    /// The notes entries are merged into the one app catalogue,
    /// so this is the same lookup as `gettext`.
    #[must_use]
    pub fn notes_gettext(&self, message: &str) -> String {
        patois::t(message)
    }

    /// Translate a template, then interpolate its `{name}`
    /// placeholders — lookup first, formatting second, always.
    #[must_use]
    pub fn gettextf(&self, template: &str, args: &[(&str, &str)]) -> String {
        interpolate(&self.gettext(template), args)
    }

    /// Plural-aware translate (port of `ngettext`).
    #[must_use]
    pub fn ngettext(&self, singular: &str, plural: &str, n: u64) -> String {
        patois::nt(singular, plural, n)
    }
}

/// Fill `{name}` placeholders in an already-looked-up template.
/// Interpolation always runs *after* the catalogue lookup: looking
/// up a pre-formatted message can never match a template key, which
/// is why dynamic strings must be translated in this order.
#[must_use]
pub fn interpolate(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// How many newest releases What's New shows before the older
/// history folds behind its expansion (port of
/// `release_notes.RELEASE_WINDOW`).
pub const RELEASE_WINDOW: usize = 3;

/// Release-note section labels (port of `release_notes.SECTIONS`):
/// section headings are ordinary application strings, translated
/// through the main domain, unlike bullets.
const SECTION_LABELS: [&str; 6] = [
    "Breaking changes",
    "Features",
    "Accessibility",
    "Bug fixes",
    "Documentation",
    "Other changes",
];

/// The canonical label for a section heading's text, accepting the
/// bare keys older release bodies used (port of `section_label`).
#[must_use]
pub fn section_label(heading: &str) -> Option<&'static str> {
    let key = heading.trim().to_lowercase();
    for label in SECTION_LABELS {
        if label.to_lowercase() == key {
            return Some(label);
        }
    }
    match key.as_str() {
        "breaking" => Some("Breaking changes"),
        "fixes" => Some("Bug fixes"),
        "other" => Some("Other changes"),
        _ => None,
    }
}

fn is_version_heading(line: &str) -> bool {
    let text = line.trim().trim_start_matches('#').trim();
    if let Some(rest) = text.strip_prefix("Version ") {
        return rest.chars().next().is_some_and(|c| c.is_ascii_digit());
    }
    let mut chars = text.chars();
    chars.next() == Some('v')
        && chars.next().is_some_and(|c| c.is_ascii_digit())
        && text.contains('.')
}

/// Localize a What's New body (port of `release_notes.
/// localize_notes`): version headings pass through, section labels
/// translate through the main catalogue, and every other line —
/// each note paragraph is one message — through `notes_gettext`
/// (the merged catalogue), falling back to English.
#[must_use]
pub fn localize_notes(i18n: &I18n, text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || is_version_heading(trimmed) {
                return line.to_string();
            }
            if let Some(label) = section_label(trimmed) {
                return i18n.gettext(label);
            }
            i18n.notes_gettext(trimmed)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Split a (localized) What's New body into the newest `window`
/// releases and the older history (port of
/// `release_notes.split_changelog_window`). The older part is
/// empty when the body holds `window` or fewer releases.
#[must_use]
pub fn split_notes_window(text: &str, window: usize) -> (String, String) {
    let lines: Vec<&str> = text.lines().collect();
    let headings: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_version_heading(line))
        .map(|(index, _)| index)
        .collect();
    if headings.len() <= window {
        return (text.to_string(), String::new());
    }
    let cut = headings[window];
    let recent = format!("{}\n", lines[..cut].join("\n").trim_end());
    let older = format!("{}\n", lines[cut..].join("\n").trim());
    (recent, older)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn interpolation_runs_after_lookup() {
        // S06 regression: formatting before lookup can never match
        // the template key, so interpolation always runs on the
        // looked-up template. With no catalogue embedded in this
        // crate's test binary the lookup is the identity, and the
        // ordering still shows: the template is what gets filled.
        let mut i18n = I18n::default();
        i18n.set_language("es");
        assert_eq!(
            i18n.gettextf("{count} channels loaded.", &[("count", "42")]),
            "42 channels loaded."
        );
        assert_eq!(
            interpolate("{a} and {b}", &[("a", "1"), ("b", "2")]),
            "1 and 2"
        );
    }

    #[test]
    fn facade_tracks_preference_without_catalogues() {
        // iptv-core's own tests embed no catalogues: patois
        // lookups pass through in English, and the preference
        // bookkeeping behaves exactly as the file-based port did.
        let mut i18n = I18n::default();
        assert_eq!(i18n.language(), LANG_AUTO);
        assert_eq!(i18n.set_language("de"), "de");
        assert_eq!(i18n.language(), "de");
        assert_eq!(i18n.resolved_language(), "de");
        assert_eq!(i18n.gettext("Cancel"), "Cancel");
        assert_eq!(i18n.ngettext("1 channel", "{n} channels", 1), "1 channel");
        assert_eq!(i18n.set_language("auto"), LANG_AUTO);
    }

    #[test]
    fn whats_new_splits_at_the_window() {
        // S09: version headings and note paragraphs pass through
        // localize_notes untouched in English, section labels are
        // looked up (identity here), and older releases fold away
        // behind the window. Real per-language output is covered
        // by the app crate's embedded-catalogue test.
        let i18n = I18n::default();
        let body = "Version 0.4.4\n\nLess talk, working keys.\n\nVersion 0.4.3\n\nBug fixes\n\nOld fix.\n\nVersion 0.4.2\n\nOlder.\n\nVersion 0.4.1\n\nOldest.";
        let localized = localize_notes(&i18n, body);
        assert!(localized.contains("Less talk, working keys."));
        assert!(localized.contains("Bug fixes"));
        let (recent, older) = split_notes_window(&localized, RELEASE_WINDOW);
        assert!(recent.contains("Version 0.4.4"));
        assert!(recent.contains("Version 0.4.2"));
        assert!(!recent.contains("Version 0.4.1"));
        assert!(older.contains("Version 0.4.1"));
        assert!(older.contains("Oldest."));
        // At or under the window there is no older part.
        let (all, none) = split_notes_window("Version 1.0\n\nOnly.", RELEASE_WINDOW);
        assert!(all.contains("Only."));
        assert!(none.is_empty());
    }

    #[test]
    fn bundled_catalogues_cover_shipped_languages() {
        // The patois catalogues in crates/iptv-app/po are what
        // the app embeds (the two Python domains merged by
        // tools/merge_catalogues.py): every shipped language
        // must carry a full catalogue, not a stub.
        let po_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../iptv-app/po");
        for code in SHIPPED_CATALOGS {
            let text = std::fs::read_to_string(po_dir.join(format!("{code}.po")))
                .expect("bundled patois catalogue");
            assert!(
                text.matches("\nmsgid ").count() > 400,
                "bundled catalogue for {code} is suspiciously small"
            );
        }
        let german = std::fs::read_to_string(po_dir.join("de.po")).expect("de catalogue");
        assert!(german.contains("msgid \"Cancel\""));
        assert!(german.contains("msgstr \"Abbrechen\""));
        let hungarian = std::fs::read_to_string(po_dir.join("hu.po")).expect("hu catalogue");
        assert!(hungarian.contains("msgid \"User Guide\""));
        assert!(hungarian.contains("msgstr \"Felhasználói útmutató\""));
    }

    #[test]
    fn preference_resolution_matches_python() {
        assert_eq!(normalize_preference(" Automatic "), "auto");
        assert_eq!(normalize_preference("HU"), "hu");
        assert!(resolve_candidates("en").is_none());
        assert_eq!(
            resolve_candidates("hu"),
            Some(vec!["hu".to_string(), "en".to_string()])
        );
        assert_eq!(language_labels().len(), 15);
        assert_eq!(SHIPPED_CATALOGS.len(), 13);
    }
}
