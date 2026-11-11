//! The user's dictionary: names the speech model gets wrong ("Claude Code"
//! heard as "Cloud Code"). Every term biases ElevenLabs towards it (keyterms), is listed in
//! the post-processing prompt, and its misheard forms are replaced in the
//! pasted text, so the fix works without AI too.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    /// The correct spelling.
    pub term: String,
    /// What the speech model writes instead; replaced by `term`, case-insensitively.
    #[serde(default)]
    pub heard_as: Vec<String>,
}

impl DictionaryEntry {
    fn new(term: &str, heard_as: &[&str]) -> Self {
        Self { term: term.into(), heard_as: heard_as.iter().map(|s| s.to_string()).collect() }
    }
}

/// Starting entries, from what Scribe misheard in the dictation history
/// (October 2026) and post-processing left or made worse.
pub fn default_entries() -> Vec<DictionaryEntry> {
    vec![
        DictionaryEntry::new("Codex", &["kodeks"]),
        DictionaryEntry::new("Claude Code", &["Cloud Code", "CloudCode"]),
        DictionaryEntry::new("Claude Desktop", &["Cloud Desktop"]),
        DictionaryEntry::new("Claude", &["Cloude"]),
        DictionaryEntry::new("HyperRust", &["Hyper Rust", "Hyperrust"]),
        DictionaryEntry::new("ElevenLabs", &["Eleven Labs"]),
        DictionaryEntry::new("OpenRouter", &["Open Router"]),
        DictionaryEntry::new("ChatGPT", &["Chat GPT", "Chatu GPT", "ChataGPT"]),
        DictionaryEntry::new("eM Client", &["EM klient", "eM klient"]),
        DictionaryEntry::new("MCP", &["NCP"]),
        DictionaryEntry::new("Sentry", &["Sentre", "Sentri"]),
    ]
}

/// Trimmed entries without blank terms or forms.
pub fn normalized(entries: Vec<DictionaryEntry>) -> Vec<DictionaryEntry> {
    entries
        .into_iter()
        .map(|e| DictionaryEntry {
            term: e.term.trim().to_string(),
            heard_as: e.heard_as.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
        })
        .filter(|e| !e.term.is_empty())
        .collect()
}

/// ElevenLabs realtime takes at most 50 keyterms (batch 100), each under 50
/// characters and at most 5 words.
pub fn keyterms(entries: &[DictionaryEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| e.term.trim())
        .filter(|t| !t.is_empty() && t.chars().count() < 50 && t.split_whitespace().count() <= 5)
        .take(50)
        .map(str::to_string)
        .collect()
}

/// Appended to the post-processing system prompt; empty without entries.
pub fn prompt_hint(entries: &[DictionaryEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut hint = String::from(
        "\n\nSłownik użytkownika. Te nazwy zapisuj dokładnie w tej postaci, odmieniając je, gdy zdanie tego wymaga. \
         W nawiasie są formy, w których rozpoznawanie mowy je przekręca:",
    );
    for e in entries {
        hint.push_str("\n- ");
        hint.push_str(&e.term);
        if !e.heard_as.is_empty() {
            hint.push_str(&format!(" ({})", e.heard_as.join(", ")));
        }
    }
    hint
}

/// Longest Polish ending kept after a replaced form ("TechSoftem" → "TEKSOFT-em").
const MAX_SUFFIX: usize = 3;

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Joins a word into a path, domain or address ("example.pl", "a/kodeks").
fn joiner(c: char) -> bool {
    matches!(c, '.' | '_' | '/' | '\\' | '@')
}

/// Replace every misheard form with its term: whole words only, any case,
/// keeping a short ending (with a hyphen after an all-caps term, as Polish
/// spells it). Code in backticks and parts of paths or addresses stay as they are.
pub fn apply(text: &str, entries: &[DictionaryEntry]) -> String {
    let mut forms: Vec<(Vec<char>, &str)> = entries
        .iter()
        .flat_map(|e| e.heard_as.iter().map(move |f| (f.trim().chars().map(lower).collect(), e.term.trim())))
        .filter(|(f, t): &(Vec<char>, &str)| !f.is_empty() && !t.is_empty())
        .collect();
    if forms.is_empty() {
        return text.to_string();
    }
    forms.sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));

    let chars: Vec<char> = text.chars().collect();
    let lowered: Vec<char> = chars.iter().copied().map(lower).collect();
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            in_code = !in_code;
        }
        let starts_word = i == 0 || !(chars[i - 1].is_alphanumeric() || joiner(chars[i - 1]));
        let hit = (!in_code && starts_word)
            .then(|| {
                forms.iter().find_map(|(form, term)| {
                    let end = i + form.len();
                    if end > chars.len() || lowered[i..end] != form[..] {
                        return None;
                    }
                    let suffix = chars[end..].iter().take_while(|c| c.is_lowercase()).count();
                    let after = end + suffix;
                    let joined = chars.get(after).is_some_and(|&c| {
                        c.is_alphanumeric() || (joiner(c) && chars.get(after + 1).is_some_and(|n| n.is_alphanumeric()))
                    });
                    (suffix <= MAX_SUFFIX && !joined).then_some((form.len(), suffix, *term))
                })
            })
            .flatten();
        match hit {
            Some((len, suffix, term)) => {
                out.push_str(term);
                let all_caps = term.chars().any(char::is_alphabetic) && !term.chars().any(char::is_lowercase);
                if suffix > 0 && all_caps {
                    out.push('-');
                }
                out.extend(&chars[i + len..i + len + suffix]);
                i += len + suffix;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(text: &str) -> String {
        let mut entries = default_entries();
        entries.push(DictionaryEntry::new("TEKSOFT", &["TechSoft", "Tekzoft", "Tech Soft"]));
        apply(text, &entries)
    }

    #[test]
    fn replaces_misheard_forms_from_history() {
        assert_eq!(
            fix("Czy warto do firmy TechSoft napisać? Tekzoft zamierza zmienić hasła."),
            "Czy warto do firmy TEKSOFT napisać? TEKSOFT zamierza zmienić hasła."
        );
        assert_eq!(fix("zaksięgowane poza TechSoftem w Symfonii"), "zaksięgowane poza TEKSOFT-em w Symfonii");
        assert_eq!(fix("audyt przez Opusa i przez Kodeksa osobno"), "audyt przez Opusa i przez Codexa osobno");
        assert_eq!(fix("korzysta Cloud Code i kodeks"), "korzysta Claude Code i Codex");
        assert_eq!(fix("ikona Hyper Rust."), "ikona HyperRust.");
    }

    #[test]
    fn leaves_other_words_code_and_addresses_alone() {
        for text in [
            "Google Cloud i Cloudflare",
            "TechSoftware i Kodeksowego",
            "uruchom `cloud code` w terminalu",
            "pisz na biuro@techsoft.example albo techsoft.example",
            "kodeks_pracy.pdf",
            "TEKSOFT-em",
        ] {
            assert_eq!(fix(text), text);
        }
    }

    #[test]
    fn is_idempotent_and_case_insensitive() {
        let once = fix("tech soft i TECHSOFT, a potem kodeksu");
        assert_eq!(once, "TEKSOFT i TEKSOFT, a potem Codexu");
        assert_eq!(fix(&once), once);
    }

    #[test]
    fn keyterms_and_hint_follow_the_entries() {
        let entries = normalized(vec![
            DictionaryEntry::new(" TEKSOFT ", &["TechSoft", " "]),
            DictionaryEntry::new("  ", &["x"]),
            DictionaryEntry::new("jeden dwa trzy cztery pięć sześć", &[]),
        ]);
        assert_eq!(entries.len(), 2);
        assert_eq!(keyterms(&entries), vec!["TEKSOFT"]);
        let hint = prompt_hint(&entries);
        assert!(hint.contains("\n- TEKSOFT (TechSoft)\n- jeden dwa"), "{hint}");
        assert_eq!(prompt_hint(&[]), "");
    }
}
