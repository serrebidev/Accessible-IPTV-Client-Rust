//! Internationalization core (port of the Python app's `i18n.py`).
//!
//! The Python app uses stdlib gettext with compiled `.mo` catalogues
//! under `locale/<lang>/LC_MESSAGES/` for two domains (`iptvclient`
//! and `release_notes`). This port keeps the same resolution rules and
//! ships its own `.mo` reader, including the catalogue's `Plural-Forms`
//! expression, so the same catalogue files work unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The auto sentinel stored in config for system language.
pub const LANG_AUTO: &str = "auto";
/// The main catalogue domain.
pub const DOMAIN: &str = "iptvclient";
/// The release-notes catalogue domain.
pub const NOTES_DOMAIN: &str = "release_notes";

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

/// A parsed `.mo` catalogue.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    messages: BTreeMap<String, String>,
    plurals: BTreeMap<String, Vec<String>>,
    plural_forms: Option<String>,
}

impl Catalogue {
    /// Parse `.mo` bytes (either endianness), tolerating truncation
    /// the way Python's gettext reader does not need to: a malformed
    /// catalogue yields an empty one instead of an error.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let mut catalog = Self::default();
        if bytes.len() < 28 {
            return catalog;
        }
        let magic_le = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let little = match magic_le {
            0x9504_12de => true,
            0xde12_0495 => false,
            _ => return catalog,
        };
        let read_u32 = |offset: usize| -> Option<u32> {
            let slice = bytes.get(offset..offset + 4)?;
            let array: [u8; 4] = slice.try_into().ok()?;
            Some(if little {
                u32::from_le_bytes(array)
            } else {
                u32::from_be_bytes(array)
            })
        };
        let (Some(count), Some(origins), Some(translations)) =
            (read_u32(8), read_u32(12), read_u32(16))
        else {
            return catalog;
        };
        let read_string = |table: u32, index: u32| -> Option<String> {
            let entry = (table + index * 8) as usize;
            let length = read_u32(entry)? as usize;
            let offset = read_u32(entry + 4)? as usize;
            let raw = bytes.get(offset..offset + length)?;
            Some(String::from_utf8_lossy(raw).into_owned())
        };
        for index in 0..count {
            let (Some(msgid), Some(msgstr)) = (
                read_string(origins, index),
                read_string(translations, index),
            ) else {
                continue;
            };
            if msgid.is_empty() {
                for line in msgstr.lines() {
                    if let Some(forms) = line.trim().strip_prefix("Plural-Forms:") {
                        catalog.plural_forms = Some(forms.trim().to_string());
                    }
                }
                continue;
            }
            if let Some((singular, plural)) = msgid.split_once('\0') {
                let forms: Vec<String> = msgstr.split('\0').map(str::to_string).collect();
                catalog.messages.insert(
                    singular.to_string(),
                    forms.first().cloned().unwrap_or_default(),
                );
                catalog.plurals.insert(plural.to_string(), forms);
                catalog
                    .plurals
                    .insert(singular.to_string(), catalog.plurals[plural].clone());
            } else {
                catalog.messages.insert(msgid, msgstr);
            }
        }
        catalog
    }

    /// Load `<dir>/<lang>/LC_MESSAGES/<domain>.mo` for the first
    /// candidate that has one (port of gettext's chain lookup).
    #[must_use]
    pub fn load(dir: &Path, domain: &str, candidates: &[String]) -> Self {
        for code in candidates {
            let path = dir
                .join(code)
                .join("LC_MESSAGES")
                .join(format!("{domain}.mo"));
            if let Ok(bytes) = std::fs::read(&path) {
                let catalog = Self::parse(&bytes);
                if !catalog.messages.is_empty() || catalog.plural_forms.is_some() {
                    return catalog;
                }
            }
        }
        Self::default()
    }

    /// Translate (port of `gettext`): falls back to the source text.
    #[must_use]
    pub fn gettext(&self, message: &str) -> String {
        self.messages
            .get(message)
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| message.to_string())
    }

    /// Plural-aware translate (port of `ngettext`).
    #[must_use]
    pub fn ngettext(&self, singular: &str, plural: &str, n: u64) -> String {
        if let Some(forms) = self
            .plurals
            .get(plural)
            .or_else(|| self.plurals.get(singular))
        {
            let index = self.plural_index(n);
            if let Some(text) = forms.get(index).filter(|value| !value.is_empty()) {
                return text.clone();
            }
        }
        if n == 1 {
            singular.to_string()
        } else {
            plural.to_string()
        }
    }

    /// The plural form index for `n` from the catalogue's
    /// `Plural-Forms` expression (default: English `n != 1`).
    #[must_use]
    pub fn plural_index(&self, n: u64) -> usize {
        let Some(forms) = &self.plural_forms else {
            return usize::from(n != 1);
        };
        let Some((_, expression)) = forms.split_once("plural=") else {
            return usize::from(n != 1);
        };
        let expression = expression.trim().trim_end_matches(';').trim();
        let nplurals = forms
            .split(';')
            .next()
            .and_then(|head| head.trim().strip_prefix("nplurals="))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(2);
        let value = eval_plural(expression, n).unwrap_or_else(|| usize::from(n != 1));
        value.min(nplurals.saturating_sub(1))
    }

    /// Whether any translations are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Evaluate a gettext plural expression over `n`: integer arithmetic,
/// comparisons, boolean operators and the ternary conditional — the
/// full grammar real catalogues use.
fn eval_plural(expression: &str, n: u64) -> Option<usize> {
    let tokens = tokenize_plural(expression)?;
    let mut parser = PluralParser { tokens, pos: 0, n };
    let value = parser.ternary()?;
    if parser.pos != parser.tokens.len() {
        return None;
    }
    Some(usize::try_from(value).unwrap_or(0))
}

#[derive(Clone, Debug, PartialEq)]
enum PluralToken {
    Number(u64),
    Variable,
    Op(&'static str),
}

fn tokenize_plural(expression: &str) -> Option<Vec<PluralToken>> {
    let chars: Vec<char> = expression.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
        } else if ch.is_ascii_digit() {
            let mut value: u64 = 0;
            while index < chars.len() && chars[index].is_ascii_digit() {
                value = value * 10 + u64::from(chars[index].to_digit(10)?);
                index += 1;
            }
            tokens.push(PluralToken::Number(value));
        } else if ch == 'n' {
            tokens.push(PluralToken::Variable);
            index += 1;
        } else {
            let two: String = chars.iter().skip(index).take(2).collect();
            let op = match two.as_str() {
                "==" => Some("=="),
                "!=" => Some("!="),
                "<=" => Some("<="),
                ">=" => Some(">="),
                "&&" => Some("&&"),
                "||" => Some("||"),
                _ => None,
            };
            if let Some(op) = op {
                tokens.push(PluralToken::Op(op));
                index += 2;
            } else if "()?:%+-*/<>!".contains(ch) {
                let text: &'static str = match ch {
                    '(' => "(",
                    ')' => ")",
                    '?' => "?",
                    ':' => ":",
                    '%' => "%",
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    '<' => "<",
                    '>' => ">",
                    _ => "!",
                };
                tokens.push(PluralToken::Op(text));
                index += 1;
            } else {
                return None;
            }
        }
    }
    Some(tokens)
}

struct PluralParser {
    tokens: Vec<PluralToken>,
    pos: usize,
    n: u64,
}

impl PluralParser {
    fn peek(&self) -> Option<&PluralToken> {
        self.tokens.get(self.pos)
    }

    fn take_op(&mut self, op: &'static str) -> bool {
        if self.peek() == Some(&PluralToken::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ternary(&mut self) -> Option<u64> {
        let condition = self.or_expr()?;
        if self.take_op("?") {
            let yes = self.ternary()?;
            if !self.take_op(":") {
                return None;
            }
            let no = self.ternary()?;
            Some(if condition != 0 { yes } else { no })
        } else {
            Some(condition)
        }
    }

    fn or_expr(&mut self) -> Option<u64> {
        let mut value = self.and_expr()?;
        while self.take_op("||") {
            let rhs = self.and_expr()?;
            value = u64::from(value != 0 || rhs != 0);
        }
        Some(value)
    }

    fn and_expr(&mut self) -> Option<u64> {
        let mut value = self.equality()?;
        while self.take_op("&&") {
            let rhs = self.equality()?;
            value = u64::from(value != 0 && rhs != 0);
        }
        Some(value)
    }

    fn equality(&mut self) -> Option<u64> {
        let mut value = self.comparison()?;
        loop {
            if self.take_op("==") {
                value = u64::from(value == self.comparison()?);
            } else if self.take_op("!=") {
                value = u64::from(value != self.comparison()?);
            } else {
                return Some(value);
            }
        }
    }

    fn comparison(&mut self) -> Option<u64> {
        let mut value = self.additive()?;
        loop {
            if self.take_op("<=") {
                value = u64::from(value <= self.additive()?);
            } else if self.take_op(">=") {
                value = u64::from(value >= self.additive()?);
            } else if self.take_op("<") {
                value = u64::from(value < self.additive()?);
            } else if self.take_op(">") {
                value = u64::from(value > self.additive()?);
            } else {
                return Some(value);
            }
        }
    }

    fn additive(&mut self) -> Option<u64> {
        let mut value = self.term()?;
        loop {
            if self.take_op("+") {
                value += self.term()?;
            } else if self.take_op("-") {
                value = value.saturating_sub(self.term()?);
            } else {
                return Some(value);
            }
        }
    }

    fn term(&mut self) -> Option<u64> {
        let mut value = self.unary()?;
        loop {
            if self.take_op("*") {
                value *= self.unary()?;
            } else if self.take_op("/") {
                let rhs = self.unary()?;
                value = value.checked_div(rhs).unwrap_or(0);
            } else if self.take_op("%") {
                let rhs = self.unary()?;
                value = value.checked_rem(rhs).unwrap_or(0);
            } else {
                return Some(value);
            }
        }
    }

    fn unary(&mut self) -> Option<u64> {
        if self.take_op("!") {
            return Some(u64::from(self.unary()? == 0));
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<u64> {
        match self.peek()?.clone() {
            PluralToken::Number(value) => {
                self.pos += 1;
                Some(value)
            }
            PluralToken::Variable => {
                self.pos += 1;
                Some(self.n)
            }
            PluralToken::Op("(") => {
                self.pos += 1;
                let value = self.ternary()?;
                if !self.take_op(")") {
                    return None;
                }
                Some(value)
            }
            PluralToken::Op(_) => None,
        }
    }
}

/// The active language state (port of the module-level catalogue
/// holders plus `set_language` / `init_from_config`).
#[derive(Clone, Debug, Default)]
pub struct I18n {
    active: String,
    catalog: Catalogue,
    notes: Catalogue,
}

impl I18n {
    /// Activate a preference against a locale directory (port of
    /// `set_language`); returns the stored preference code.
    pub fn set_language(&mut self, locale_dir: &Path, lang_code: &str) -> String {
        self.active = normalize_preference(lang_code);
        match resolve_candidates(&self.active) {
            None => {
                self.catalog = Catalogue::default();
                self.notes = Catalogue::default();
            }
            Some(candidates) => {
                self.catalog = Catalogue::load(locale_dir, DOMAIN, &candidates);
                self.notes = Catalogue::load(locale_dir, NOTES_DOMAIN, &candidates);
            }
        }
        self.active.clone()
    }

    /// Activate from a config map's `language` key (port of
    /// `init_from_config`).
    pub fn init_from_config(
        &mut self,
        locale_dir: &Path,
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> String {
        let pref = config
            .get("language")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(LANG_AUTO);
        self.set_language(locale_dir, pref)
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
        self.catalog.gettext(message)
    }

    /// Translate a release-note bullet (port of `notes_gettext`).
    #[must_use]
    pub fn notes_gettext(&self, message: &str) -> String {
        self.notes.gettext(message)
    }

    /// Plural-aware translate (port of `ngettext`).
    #[must_use]
    pub fn ngettext(&self, singular: &str, plural: &str, n: u64) -> String {
        self.catalog.ngettext(singular, plural, n)
    }
}

/// Where catalogues live: `IPTV_LOCALE_DIR`, else `locale/` beside the
/// executable, else `locale/` in the working directory (the Python
/// app resolves against its own tree; the Rust layout equivalents).
#[must_use]
pub fn locale_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("IPTV_LOCALE_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let beside = parent.join("locale");
            if beside.is_dir() {
                return beside;
            }
        }
    }
    PathBuf::from("locale")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal little-endian `.mo` with the given entries.
    fn mo_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let count = u32::try_from(entries.len()).expect("test");
        let origins_offset = 28_u32;
        let translations_offset = origins_offset + count * 8;
        let mut data_offset = translations_offset + count * 8;
        let mut origins = Vec::new();
        let mut translations = Vec::new();
        let mut blobs = Vec::new();
        for (msgid, _) in entries {
            origins.push((u32::try_from(msgid.len()).expect("test"), data_offset));
            data_offset += u32::try_from(msgid.len()).expect("test") + 1;
            blobs.extend_from_slice(msgid.as_bytes());
            blobs.push(0);
        }
        for (_, msgstr) in entries {
            translations.push((u32::try_from(msgstr.len()).expect("test"), data_offset));
            data_offset += u32::try_from(msgstr.len()).expect("test") + 1;
            blobs.extend_from_slice(msgstr.as_bytes());
            blobs.push(0);
        }
        let mut out = Vec::new();
        out.extend_from_slice(&0x9504_12de_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&origins_offset.to_le_bytes());
        out.extend_from_slice(&translations_offset.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        for (length, offset) in origins.iter().chain(translations.iter()) {
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
        }
        out.extend_from_slice(&blobs);
        out
    }

    #[test]
    fn mo_catalogue_translates_and_pluralizes() {
        let header =
            "Content-Type: text/plain; charset=UTF-8\nPlural-Forms: nplurals=2; plural=(n != 1);\n";
        let bytes = mo_bytes(&[
            ("", header),
            ("Open playlist", "Abrir lista"),
            ("1 channel\0{n} channels", "1 canal\0{n} canales"),
        ]);
        let catalog = Catalogue::parse(&bytes);
        assert_eq!(catalog.gettext("Open playlist"), "Abrir lista");
        assert_eq!(catalog.gettext("Untranslated"), "Untranslated");
        assert_eq!(catalog.ngettext("1 channel", "{n} channels", 1), "1 canal");
        assert_eq!(
            catalog.ngettext("1 channel", "{n} channels", 5),
            "{n} canales"
        );
    }

    #[test]
    fn plural_expression_handles_russian_rule() {
        let header = "Plural-Forms: nplurals=3; plural=n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2;\n";
        let bytes = mo_bytes(&[("", header)]);
        let catalog = Catalogue::parse(&bytes);
        assert_eq!(catalog.plural_index(1), 0);
        assert_eq!(catalog.plural_index(2), 1);
        assert_eq!(catalog.plural_index(5), 2);
        assert_eq!(catalog.plural_index(21), 0);
        assert_eq!(catalog.plural_index(111), 2);
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
