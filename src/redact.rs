//! Secret redaction, applied before anything is written to disk.
//!
//! Written as small hand-made scanners instead of regular expressions, so the
//! binary has no third-party dependency. Each scanner documents the pattern it
//! implements. Behaviour is checked against the reference cases in the tests
//! and against the previous Python implementation.

use crate::json::{Map, Value};

pub const MASK: &str = "[REDACTED]";
pub const NOT_KEPT: &str = "[CONTENT NOT KEPT]";
/// Placeholder used while scanners run, so that a later scanner never
/// re-matches the text of an earlier mask. Replaced by MASK at the end.
const HOLD: char = '\u{E000}';
const MAX_STRING: usize = 8000;
const MAX_ITEMS: usize = 100;
const MAX_DEPTH: usize = 12;

type Chars = Vec<char>;

thread_local! {
    /// Labels of what was masked, collected while `redact_noting` runs.
    static FOUND: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Remember that a secret of this kind was masked. Never the value itself.
fn note(label: impl Into<String>) {
    FOUND.with(|f| {
        if let Some(list) = f.borrow_mut().as_mut() {
            let label = label.into();
            if !list.contains(&label) && list.len() < 20 {
                list.push(label);
            }
        }
    });
}

/// A name taken from the text (a key, a flag, a label), cleaned for display.
fn shown_name(s: &[char], a: usize, b: usize) -> String {
    let name: String = s[a..b].iter().collect();
    name.trim_matches(|c: char| "\"'\\-:= ".contains(c)).chars().take(40).collect()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn at(s: &[char], i: usize) -> Option<char> {
    s.get(i).copied()
}

/// Regex `\b` before position i, for a pattern starting with a word character.
fn word_start(s: &[char], i: usize) -> bool {
    i == 0 || !is_word(s[i - 1])
}

fn eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Case-insensitive literal at position i.
fn lit_ci(s: &[char], i: usize, lit: &str) -> Option<usize> {
    let mut j = i;
    for c in lit.chars() {
        if !eq_ci(at(s, j)?, c) {
            return None;
        }
        j += 1;
    }
    Some(j)
}

fn lit(s: &[char], i: usize, lit: &str) -> Option<usize> {
    let mut j = i;
    for c in lit.chars() {
        if at(s, j)? != c {
            return None;
        }
        j += 1;
    }
    Some(j)
}

/// Greedy run of characters matching `f`, at least `min` long. Returns the end.
fn run(s: &[char], i: usize, min: usize, f: impl Fn(char) -> bool) -> Option<usize> {
    let mut j = i;
    while j < s.len() && f(s[j]) {
        j += 1;
    }
    (j - i >= min).then_some(j)
}

fn skip_ws(s: &[char], i: usize) -> usize {
    run(s, i, 0, char::is_whitespace).unwrap_or(i)
}

/// `"[^"\n]*"` or `'[^'\n]*'` at i; returns the end after the closing quote.
fn quoted(s: &[char], i: usize) -> Option<usize> {
    let q = at(s, i)?;
    if q != '"' && q != '\'' {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() && s[j] != q && s[j] != '\n' {
        j += 1;
    }
    (at(s, j)? == q).then_some(j + 1)
}

/// `(?:"[^"\n]*"|'[^'\n]*'|[^\s"']+)`
fn quoted_or_word(s: &[char], i: usize) -> Option<usize> {
    quoted(s, i).or_else(|| run(s, i, 1, |c| !c.is_whitespace() && c != '"' && c != '\''))
}

/// Generic left-to-right substitution: at each position, `try_at` either
/// returns (end, replacement) or None; non-matching characters are copied.
fn substitute(s: &[char], mut try_at: impl FnMut(&[char], usize) -> Option<(usize, String)>) -> Chars {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match try_at(s, i) {
            Some((end, replacement)) if end > i => {
                out.extend(replacement.chars());
                i = end;
            }
            _ => {
                out.push(s[i]);
                i += 1;
            }
        }
    }
    out
}

fn text(s: &[char], a: usize, b: usize) -> String {
    s[a..b].iter().collect()
}

// ---------- Scanners ----------

/// Heredoc bodies: `cat <<'EOF' > file … EOF`. The body is file content, not a
/// command. Pattern: `(<<-?[ \t]*(['"]?)([A-Za-z_]\w*)\2[^\n]*\n)(.*?)(\n[ \t]*\3[ \t]*(?=\n|$))`
fn heredoc(s: &[char]) -> Chars {
    substitute(s, |s, i| {
        let mut j = lit(s, i, "<<")?;
        if at(s, j) == Some('-') {
            j += 1;
        }
        while matches!(at(s, j), Some(' ' | '\t')) {
            j += 1;
        }
        let quote = match at(s, j) {
            Some(q @ ('\'' | '"')) => {
                j += 1;
                Some(q)
            }
            _ => None,
        };
        let id_start = j;
        let first = at(s, j)?;
        if !(first.is_ascii_alphabetic() || first == '_') {
            return None;
        }
        let id_end_max = run(s, j + 1, 0, is_word)?;
        // A regex would backtrack over the delimiter length; try longest first.
        for id_end in (id_start + 1..=id_end_max).rev() {
            let mut k = id_end;
            if let Some(q) = quote {
                if at(s, k) != Some(q) {
                    continue;
                }
                k += 1;
            }
            let Some(nl) = (k..s.len()).find(|&x| s[x] == '\n') else { continue };
            let header_end = nl + 1;
            let ident = &s[id_start..id_end];
            // Lazy body: first "\n[ \t]*IDENT[ \t]*" followed by "\n" or end.
            let mut p = header_end;
            while p < s.len() {
                if s[p] == '\n' {
                    let mut q = p + 1;
                    while matches!(at(s, q), Some(' ' | '\t')) {
                        q += 1;
                    }
                    if s.len() >= q + ident.len() && &s[q..q + ident.len()] == ident {
                        let mut r = q + ident.len();
                        while matches!(at(s, r), Some(' ' | '\t')) {
                            r += 1;
                        }
                        if r == s.len() || s[r] == '\n' {
                            return Some((r, format!("{}{}{}", text(s, i, header_end), NOT_KEPT, text(s, p, r))));
                        }
                    }
                }
                p += 1;
            }
        }
        None
    })
}

/// Labels from the rules file ("mot de passe : x", "mdp=x", "clé API = x"),
/// followed by = or : and a value. The label must start a word.
fn labels(s: &[char]) -> Chars {
    let rules = crate::rules::get();
    substitute(s, |s, i| {
        if i > 0 && (is_word(s[i - 1]) || s[i - 1] == '-') {
            return None;
        }
        for label in &rules.labels {
            for end in label.ends(s, i) {
                let j = skip_ws(s, end);
                if !matches!(at(s, j), Some('=' | ':')) {
                    continue;
                }
                let k = skip_ws(s, j + 1);
                if at(s, k) == Some(HOLD) {
                    continue;
                }
                // A quoted value keeps its quotes: mdp="[REDACTED]".
                if let Some(close) = quoted(s, k).filter(|&close| close > k + 2) {
                    note(format!("Value after \"{}\"", shown_name(s, i, end)));
                    return Some((close, format!("{}{}{}", text(s, i, k + 1), HOLD, s[k])));
                }
                if let Some(v) = run(s, k, 1, |c| !c.is_whitespace() && !"'\",;&".contains(c)) {
                    note(format!("Value after \"{}\"", shown_name(s, i, end)));
                    return Some((v, format!("{}{}", text(s, i, k), HOLD)));
                }
            }
        }
        None
    })
}

/// `-----WORD [^-]*PRIVATE KEY-----` (also `PRIVATE KEY BLOCK`, for PGP).
/// Returns the end of the marker.
fn key_marker(s: &[char], i: usize, word: &str) -> Option<usize> {
    let j = lit(s, i, "-----")?;
    let j = lit(s, j, word)?;
    let j = lit(s, j, " ")?;
    let dash = (j..s.len()).find(|&x| s[x] == '-')?;
    let before: String = s[j..dash].iter().collect();
    if !before.ends_with("PRIVATE KEY") && !before.ends_with("PRIVATE KEY BLOCK") {
        return None;
    }
    lit(s, dash, "-----")
}

/// True when a private key starts in `s` and does not end there: the rest of
/// the key follows in the next strings (a key split into lines).
fn opens_private_key(s: &[char]) -> bool {
    (0..s.len()).rev().find_map(|i| key_marker(s, i, "BEGIN").map(|_| i)).is_some_and(|begin| !(begin..s.len()).any(|p| key_marker(s, p, "END").is_some()))
}

fn closes_private_key(s: &[char]) -> bool {
    (0..s.len()).any(|p| key_marker(s, p, "END").is_some())
}

/// `-----BEGIN … PRIVATE KEY----- … -----END … PRIVATE KEY-----`. Without an
/// END marker (a truncated key, or one cut into several strings), everything
/// after BEGIN is masked.
fn private_keys(s: &[char]) -> Chars {
    substitute(s, |s, i| {
        let body_start = key_marker(s, i, "BEGIN")?;
        let end = (body_start..s.len()).find_map(|p| key_marker(s, p, "END")).unwrap_or(s.len());
        note("Private key");
        Some((end, HOLD.to_string()))
    })
}

/// HTTP headers whose whole value is a credential (`header` rules):
/// `Cookie: a=1; b=2`, `Authorization: Token abc`. The value runs to the end
/// of the line or to its closing quote. A value already masked (`Bearer …`)
/// is left as it is; a quote with no closing quote is not a value
/// (`grep "Cookie:" src/`).
fn headers(s: &[char]) -> Chars {
    let rules = crate::rules::get();
    substitute(s, |s, i| {
        if !word_start(s, i) || !is_word(s[i]) {
            return None;
        }
        let name_end = rules.headers.iter().find_map(|h| lit_ci(s, i, h).filter(|&e| at(s, e).is_none_or(|c| !is_word(c) && c != '-')))?;
        let inline_ws = |s: &[char], mut j: usize| {
            while matches!(at(s, j), Some(' ' | '\t')) {
                j += 1;
            }
            j
        };
        let j = inline_ws(s, after_key_of(s, name_end));
        if at(s, j) != Some(':') {
            return None;
        }
        let j = inline_ws(s, j + 1);
        let quote: &[char] = match (at(s, j), at(s, j + 1)) {
            (Some('\\'), Some('"')) => &['\\', '"'],
            (Some(q @ ('"' | '\'')), _) => if q == '"' { &['"'] } else { &['\''] },
            _ => &[],
        };
        let start = j + quote.len();
        let mut e = start;
        let mut closed = quote.is_empty();
        while e < s.len() && s[e] != '\n' && s[e] != '\r' {
            let here = &s[e..];
            if quote.is_empty() {
                if matches!(s[e], '"' | '\'') || here.starts_with(&['\\', '"']) {
                    break;
                }
            } else if here.starts_with(quote) {
                closed = true;
                break;
            }
            e += 1;
        }
        while e > start && s[e - 1].is_whitespace() {
            e -= 1;
        }
        if !closed || e == start || s[start..e].contains(&HOLD) || text(s, start, e).contains(MASK) {
            return None;
        }
        note(format!("Header \"{}\"", text(s, i, name_end)));
        Some((e, format!("{}{}", text(s, i, start), HOLD)))
    })
}

/// `(?i)\b(Bearer|Basic)\s+[a-z0-9._~+/=-]+` → `\1 HOLD`
fn auth_headers(s: &[char]) -> Chars {
    substitute(s, |s, i| {
        if !word_start(s, i) {
            return None;
        }
        let end = lit_ci(s, i, "bearer").or_else(|| lit_ci(s, i, "basic"))?;
        let j = run(s, end, 1, char::is_whitespace)?;
        let k = run(s, j, 1, |c| c.is_ascii_alphanumeric() || "._~+/=-".contains(c))?;
        note("Authorization header");
        Some((k, format!("{} {}", text(s, i, end), HOLD)))
    })
}

/// Credential formats from the rules file (`token` rules), plus JSON Web
/// Tokens (`eyJ….eyJ….signature`), whose shape needs code.
fn known_tokens(s: &[char]) -> Chars {
    let rules = crate::rules::get();
    let alnum_dash_us = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    substitute(s, |s, i| {
        if !word_start(s, i) || !is_word(s[i]) {
            return None;
        }
        let (end, name) = rules.tokens.iter().find_map(|t| Some((t.end(s, i)?, t.name.as_str()))).or_else(|| {
            let j = run(s, lit(s, i, "eyJ")?, 8, alnum_dash_us)?;
            let j = run(s, lit(s, lit(s, j, ".")?, "eyJ")?, 8, alnum_dash_us)?;
            Some((run(s, lit(s, j, ".")?, 8, alnum_dash_us)?, "JSON Web Token"))
        })?;
        note(name);
        Some((end, HOLD.to_string()))
    })
}

/// `scheme` per `[a-z][a-z0-9+.-]*://` (case-insensitive); returns the end after "://".
fn url_scheme(s: &[char], i: usize) -> Option<usize> {
    if !at(s, i)?.is_ascii_alphabetic() {
        return None;
    }
    let j = run(s, i + 1, 0, |c| c.is_ascii_alphanumeric() || "+.-".contains(c))?;
    lit(s, j, "://")
}

/// Credentials in any URL: `(?i)\b([a-z][a-z0-9+.-]*://)[^/@\s:]+:[^/@\s]+@` → `\1HOLD@`
fn url_credentials(s: &[char]) -> Chars {
    substitute(s, |s, i| {
        if !word_start(s, i) {
            return None;
        }
        let scheme_end = url_scheme(s, i)?;
        let user_end = run(s, scheme_end, 1, |c| !"/@:".contains(c) && !c.is_whitespace())?;
        let colon = lit(s, user_end, ":")?;
        let pass_end = run(s, colon, 1, |c| !"/@".contains(c) && !c.is_whitespace())?;
        let end = lit(s, pass_end, "@")?;
        note("Password in a URL");
        Some((end, format!("{}{}@", text(s, i, scheme_end), HOLD)))
    })
}

/// curl -u user:pass / --user user:pass (a bare user name is kept).
/// `((?:^|\s)(?:-u|--user)(?:\s+|=))(?![a-z][a-z0-9+.-]*://)(?:"…:…"|'…:…'|[^\s:"']+:[^\s"']+)`
fn user_option(s: &[char]) -> Chars {
    substitute(s, |s, i| {
        let start = if i == 0 && at(s, 0) == Some('-') {
            0
        } else if s[i].is_whitespace() {
            i + 1
        } else {
            return None;
        };
        let j = lit(s, start, "-u").or_else(|| lit(s, start, "--user"))?;
        let j = if at(s, j) == Some('=') { j + 1 } else { run(s, j, 1, char::is_whitespace)? };
        // Case-sensitive lookahead, as in the original pattern.
        if at(s, j).is_some_and(|c| c.is_ascii_lowercase()) {
            let k = run(s, j + 1, 0, |c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+.-".contains(c)).unwrap_or(j + 1);
            if lit(s, k, "://").is_some() {
                return None;
            }
        }
        let end = match at(s, j)? {
            '"' | '\'' => {
                let end = quoted(s, j)?;
                s[j + 1..end - 1].contains(&':').then_some(end)?
            }
            _ => {
                let user = run(s, j, 1, |c| !c.is_whitespace() && !":\"'".contains(c))?;
                let colon = lit(s, user, ":")?;
                run(s, colon, 1, |c| !c.is_whitespace() && c != '"' && c != '\'')?
            }
        };
        note("Password (curl -u)");
        Some((end, format!("{}{}", text(s, i, j), HOLD)))
    })
}

/// `(\bsshpass\s+-p\s*)(?:"…"|'…'|[^\s"']+)` → `\1HOLD`
fn sshpass(s: &[char]) -> Chars {
    substitute(s, |s, i| {
        if !word_start(s, i) {
            return None;
        }
        let j = lit(s, i, "sshpass")?;
        let j = run(s, j, 1, char::is_whitespace)?;
        let j = lit(s, j, "-p")?;
        let j = skip_ws(s, j);
        let end = quoted_or_word(s, j)?;
        note("Password (sshpass -p)");
        Some((end, format!("{}{}", text(s, i, j), HOLD)))
    })
}

/// Signed or authenticated URLs: `[?&]NAME=value` → `NAME=HOLD`, for the
/// `url-param` names of the rules file.
/// The value runs to the next `&`, `#`, space, quote or backslash.
/// Also inside an encoded URL (`%3Ftoken%3D…%26`, a redirect address) and
/// after an HTML-escaped `&amp;`.
fn url_params(s: &[char]) -> Chars {
    substitute(s, |s, i| {
        let plain = i > 0 && matches!(s[i - 1], '?' | '&');
        let encoded = i >= 3 && (lit_ci(s, i - 3, "%3F").is_some() || lit_ci(s, i - 3, "%26").is_some());
        let html = i >= 5 && lit(s, i - 5, "&amp;").is_some();
        if !plain && !encoded && !html {
            return None;
        }
        let name_end = run(s, i, 1, |c| c.is_ascii_alphanumeric() || c == '-' || c == '_')?;
        let name: String = s[i..name_end].iter().flat_map(|c| c.to_lowercase()).collect();
        if !crate::rules::get().url_params.contains(&name) {
            return None;
        }
        let v = if encoded { lit(s, name_end, "=").or_else(|| lit_ci(s, name_end, "%3D"))? } else { lit(s, name_end, "=")? };
        if at(s, v) == Some(HOLD) {
            return None;
        }
        let mut end = v;
        while end < s.len() && !s[end].is_whitespace() && !"&#\"'\\".contains(s[end]) && !(encoded && lit_ci(s, end, "%26").is_some()) {
            end += 1;
        }
        if end == v {
            return None;
        }
        note(format!("URL parameter \"{}\"", text(s, i, name_end)));
        Some((end, format!("{}{}", text(s, i, v), HOLD)))
    })
}

/// A word that holds a `key` pattern of the rules file (TOKEN, db_password…).
fn has_secret_word(word: &[char]) -> bool {
    crate::rules::get().keys.iter().any(|p| p.found_in(word))
}

/// End of a key, past its closing quote when it is written as a JSON key
/// ("token", 'token' or \"token\").
fn after_key_of(s: &[char], run_end: usize) -> usize {
    match (at(s, run_end), at(s, run_end + 1)) {
        (Some('\\'), Some('"' | '\'')) => run_end + 2,
        (Some('"' | '\''), _) => run_end + 1,
        _ => run_end,
    }
}

/// KEY=value, token: value, --password value. Prose such as "fix secret
/// handling" is left alone, and values already masked are skipped.
/// `(?i)((?:[\w-]*KW[\w-]*\s*[=:]\s*|(?<!\S)--?[\w-]*KW[\w-]*\s+))(?!HOLD)(?:"…"|'…'|[^\s,;&]+)`
fn key_values(s: &[char]) -> Chars {
    let word_or_dash = |c: char| is_word(c) || c == '-';
    // `git config github.token VALUE`, `aws configure set aws_secret_access_key VALUE`
    let config = mentions_any(s, &["config", "configure"]);
    substitute(s, |s, i| {
        // Only at the start of a word: a match starting mid-word would fail
        // for the same reason, and skipping keeps the scan linear.
        if !word_or_dash(s[i]) || (i > 0 && word_or_dash(s[i - 1])) {
            return None;
        }
        let run_end = run(s, i, 1, word_or_dash)?;
        let word: String = s[i..run_end].iter().collect();
        if !has_secret_word(&s[i..run_end]) || is_counter(word.trim_start_matches('-')) {
            return None; // max_tokens=4096, "tokens_used": 12
        }
        let json_key = after_key_of(s, run_end) != run_end;
        let value = |s: &[char], k: usize| -> Option<usize> {
            if at(s, k) == Some(HOLD) || lit(s, k, MASK).is_some() {
                return None; // already masked
            }
            if json_key && matches!(at(s, k), Some('{' | '[')) {
                return None; // an object or a list: the keys inside are checked one by one
            }
            // \"value\" (escaped JSON inside a string): up to the next \"
            if at(s, k) == Some('\\') && at(s, k + 1) == Some('"') {
                let mut e = k + 2;
                while e + 1 < s.len() && !(s[e] == '\\' && s[e + 1] == '"') && s[e] != '\n' {
                    e += 1;
                }
                return (e + 1 < s.len() && s[e] == '\\').then_some(e + 2);
            }
            // In JSON, an unquoted value (number, null) ends at } or ].
            quoted(s, k).or_else(|| run(s, k, 1, |c| !c.is_whitespace() && !",;&\"'".contains(c) && !(json_key && "}]".contains(c))))
        };
        // Branch A: KEY = value / KEY: value, including a JSON key written
        // inside text: "token":"…" or, escaped once more, \"token\":\"…\"
        let after_key = after_key_of(s, run_end);
        let j = skip_ws(s, after_key);
        if matches!(at(s, j), Some('=' | ':')) {
            let k = skip_ws(s, j + 1);
            if let Some(end) = value(s, k) {
                // A quoted value keeps its quotes: "token":"[REDACTED]".
                let quote = match at(s, k) {
                    Some('\\') => "\\\"",
                    Some('"') => "\"",
                    Some('\'') => "'",
                    _ => "",
                };
                if !quote.is_empty() && lit(s, k + quote.chars().count(), MASK).is_some() {
                    return None;
                }
                note(format!("Value of \"{}\"", shown_name(s, i, run_end)));
                return Some((end, format!("{}{quote}{HOLD}{quote}", text(s, i, k))));
            }
        }
        // Branch B: --flag value (flag at the start of a word)
        if s[i] == '-' && (i == 0 || s[i - 1].is_whitespace()) {
            let k = run(s, run_end, 1, char::is_whitespace)?;
            if let Some(end) = value(s, k) {
                note(format!("Value of \"{}\"", shown_name(s, i, run_end)));
                return Some((end, format!("{}{}", text(s, i, k), HOLD)));
            }
        }
        // Branch C: a setting name then its value, in a `config` command.
        if config && s[i] != '-' && !json_key {
            let k = run(s, run_end, 1, |c| c == ' ' || c == '\t')?;
            if at(s, k).is_some_and(|c| c != '-' && c != '|' && c != '>') {
                if let Some(end) = value(s, k) {
                    note(format!("Value of \"{}\"", shown_name(s, i, run_end)));
                    return Some((end, format!("{}{}", text(s, i, k), HOLD)));
                }
            }
        }
        None
    })
}

// Programs where a short option carries a password: `password-option` rules
// in rules/redaction.rules (see crate::rules::PasswordOption).
use crate::rules::PasswordOption;

/// The first of `words` that `s` contains as a whole word.
fn first_word<'a>(s: &[char], words: &[&'a str]) -> Option<&'a str> {
    (0..s.len()).find_map(|i| {
        if !word_start(s, i) {
            return None;
        }
        words.iter().find(|name| lit(s, i, name).is_some_and(|end| end == s.len() || !(is_word(s[end]) || s[end] == '-'))).copied()
    })
}

fn mentions_any(s: &[char], words: &[&str]) -> bool {
    first_word(s, words).is_some()
}

/// The password options that apply to this command, with the program named.
fn password_options(s: &[char]) -> Vec<(&'static PasswordOption, &'static str)> {
    crate::rules::get()
        .password_options
        .iter()
        .filter(|o| o.with.as_deref().is_none_or(|w| mentions_any(s, &[w])))
        .filter_map(|o| first_word(s, &o.programs.iter().map(String::as_str).collect::<Vec<_>>()).map(|p| (o, p)))
        .collect()
}

/// `mysql -pSECRET`, `docker login -p SECRET`, `redis-cli -a SECRET`.
/// `((?:^|\s)-p)(?:"…"|'…'|[^\s"']+)` → `\1HOLD`
fn password_flags(s: &[char]) -> Chars {
    let options = password_options(s);
    if options.is_empty() {
        return s.to_vec();
    }
    substitute(s, |s, i| {
        let start = if i == 0 && at(s, 0) == Some('-') {
            0
        } else if s[i].is_whitespace() {
            i + 1
        } else {
            return None;
        };
        options.iter().find_map(|(o, program)| {
            let j = lit(s, start, &o.flag)?;
            let k = if o.glued { j } else { run(s, j, 1, |c| c == ' ' || c == '\t')? };
            if at(s, k) == Some(HOLD) {
                return None;
            }
            let end = quoted_or_word(s, k)?;
            note(format!("Password ({program} {})", o.flag));
            Some((end, format!("{}{}", text(s, i, k), HOLD)))
        })
    })
}

/// Redact secrets and file contents from a free-text string (usually a command).
/// Only the beginning of very long strings is scanned (and kept): the result
/// is cut to MAX_STRING characters anyway, and this bounds the work.
const SCAN_LIMIT: usize = 200_000;

pub fn redact_string(value: &str) -> String {
    let mut s: Chars = value.chars().filter(|&c| c != HOLD).take(SCAN_LIMIT).collect();
    for step in [heredoc, labels, private_keys, auth_headers, headers, known_tokens, url_credentials, url_params, user_option, sshpass, key_values, password_flags] {
        s = step(&s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s {
        if c == HOLD {
            out.push_str(MASK);
        } else {
            out.push(c);
        }
    }
    out
}

fn truncate(s: String, suffix: bool) -> String {
    if s.chars().count() <= MAX_STRING {
        return s;
    }
    let mut cut: String = s.chars().take(MAX_STRING).collect();
    if suffix {
        cut.push_str("… [TRUNCATED]");
    }
    cut
}

/// Field names that hold credentials (`field` rules; `max_tokens` and other
/// counters excluded).
pub fn is_secret_key(key: &str) -> bool {
    let k: Vec<char> = key.chars().collect();
    crate::rules::get().fields.iter().any(|p| p.found_in(&k)) && !is_counter(key)
}

fn is_counter(k: &str) -> bool {
    let k = k.to_lowercase();
    let token_word = |t: &str| t == "token" || t == "tokens";
    for prefix in ["max", "num", "total", "input", "output", "prompt", "completion", "reasoning", "cached"] {
        if let Some(rest) = k.strip_prefix(prefix) {
            let rest = rest.strip_prefix('_').unwrap_or(rest);
            if token_word(rest) {
                return true;
            }
        }
    }
    for t in ["tokens_", "token_"] {
        if let Some(rest) = k.strip_prefix(t) {
            if ["count", "used", "limit", "usage"].contains(&rest) {
                return true;
            }
        }
    }
    false
}

/// Field names whose values are contents, never kept (`omit` rules).
fn is_omitted_key(key: &str) -> bool {
    crate::rules::get().omit.contains(&key.to_lowercase())
}

/// Redact an argv-style list, where a flag and its value are separate items.
pub fn redact_argv(argv: &[String]) -> Vec<String> {
    let command: Chars = argv.join(" ").chars().collect();
    let options = password_options(&command);
    let sshpass = mentions_any(&command, &["sshpass"]);
    let mut in_key = false; // inside a private key cut into several strings
    let mut out = Vec::with_capacity(argv.len());
    for (i, arg) in argv.iter().enumerate() {
        let previous = if i > 0 { argv[i - 1].as_str() } else { "" };
        let chars: Chars = arg.chars().collect();
        if in_key {
            in_key = !closes_private_key(&chars);
            out.push(MASK.to_string());
            continue;
        }
        if opens_private_key(&chars) {
            in_key = true;
            out.push(truncate(redact_string(arg), false));
            continue;
        }
        let glued = options.iter().find(|(o, _)| o.glued && arg.starts_with(o.flag.as_str()) && arg.chars().count() > o.flag.len());
        let separate = options.iter().find(|(o, _)| !o.glued && previous == o.flag);
        if (previous == "-u" || previous == "--user") && arg.contains(':') {
            note("Password (curl -u)");
            out.push(MASK.to_string());
        } else if previous.starts_with('-') && !previous.contains(['=', ' ']) && is_secret_key(previous.trim_start_matches('-')) {
            note(format!("Value of \"{}\"", previous.trim_start_matches('-')));
            out.push(MASK.to_string()); // --password VALUE, --token VALUE
        } else if previous == "-p" && sshpass {
            note("Password (sshpass -p)");
            out.push(MASK.to_string());
        } else if let Some((o, program)) = separate {
            note(format!("Password ({program} {})", o.flag));
            out.push(MASK.to_string());
        } else if let Some((o, program)) = glued {
            note(format!("Password ({program} {})", o.flag));
            out.push(format!("{}{MASK}", o.flag));
        } else {
            out.push(truncate(redact_string(arg), false));
        }
    }
    out
}

/// Redact a JSON value and list the kinds of secrets that were masked
/// ("GitHub token", "URL parameter \"sig\""…). The values are never listed.
pub fn redact_noting(value: &Value) -> (Value, Vec<String>) {
    FOUND.with(|f| *f.borrow_mut() = Some(Vec::new()));
    let out = redact(value);
    let found = FOUND.with(|f| f.borrow_mut().take()).unwrap_or_default();
    (out, found)
}

/// Redact a JSON value recursively: secret fields masked, content fields
/// omitted, strings scanned, sizes bounded.
pub fn redact(value: &Value) -> Value {
    redact_at(value, 0)
}

fn redact_at(value: &Value, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::str("[DEPTH LIMITED]");
    }
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map.iter().take(MAX_ITEMS) {
                let v = if is_secret_key(k) {
                    if !matches!(v, Value::Null) && v.as_str() != Some("") && v.as_str() != Some(MASK) {
                        note(format!("Field \"{k}\""));
                    }
                    Value::str(MASK)
                } else if is_omitted_key(k) {
                    Value::str(NOT_KEPT)
                } else {
                    redact_at(v, depth + 1)
                };
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let items = &items[..items.len().min(MAX_ITEMS)];
            if !items.is_empty() && items.iter().all(|v| matches!(v, Value::String(_))) {
                let argv: Vec<String> = items.iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect();
                return Value::Array(redact_argv(&argv).into_iter().map(Value::String).collect());
            }
            Value::Array(items.iter().map(|v| redact_at(v, depth + 1)).collect())
        }
        Value::String(s) => Value::String(truncate(redact_string(s), true)),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unescape(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match (c, it.peek()) {
                ('\\', Some('n')) => { out.push('\n'); it.next(); }
                ('\\', Some('t')) => { out.push('\t'); it.next(); }
                ('\\', Some('\\')) => { out.push('\\'); it.next(); }
                _ => out.push(c),
            }
        }
        out
    }

    /// Runs rules/cases.txt and reports every failing case with its line.
    #[test]
    fn rules_cases() {
        let source = include_str!("../rules/cases.txt");
        let mut failures = Vec::new();
        let mut current: Option<(usize, bool, String)> = None; // (line, is_json, redacted)
        let mut kinds: Vec<String> = Vec::new();
        let mut cases = 0;
        for (n, raw) in source.lines().enumerate() {
            let line = n + 1;
            if raw.trim().is_empty() || raw.starts_with('#') {
                continue;
            }
            let (kind, rest) = raw.split_once(':').unwrap_or_else(|| panic!("cases.txt line {line}: expected kind: value"));
            let value = rest.strip_prefix(' ').unwrap_or(rest).trim_start_matches(' ');
            let is_json = current.as_ref().is_some_and(|c| c.1);
            let text = if kind == "json" || (is_json && kind == "out") { value.to_string() } else { unescape(value) };
            match kind {
                "in" => {
                    let (out, found) = redact_noting(&Value::String(text.clone()));
                    current = Some((line, false, out.as_str().unwrap_or_default().to_string()));
                    kinds = found;
                }
                "json" => {
                    let v = crate::json::parse(&text).unwrap_or_else(|e| panic!("cases.txt line {line}: invalid JSON: {e:?}"));
                    let (out, found) = redact_noting(&v);
                    current = Some((line, true, out.to_compact()));
                    kinds = found;
                }
                "kind" => {
                    cases += 1;
                    let start = current.as_ref().map(|c| c.0).unwrap_or_else(|| panic!("cases.txt line {line}: kind: without in: or json:"));
                    if !kinds.contains(&text) {
                        failures.push(format!("line {start}: kind {text}\n    got:  {kinds:?}"));
                    }
                }
                "keep" => {
                    cases += 1;
                    current = None;
                    let got = redact_string(&text);
                    if got != text {
                        failures.push(format!("line {line}: should stay unchanged\n    got:  {got}"));
                    }
                }
                "out" | "hide" => {
                    cases += 1;
                    let (start, json, got) = current.clone().unwrap_or_else(|| panic!("cases.txt line {line}: {kind}: without in: or json:"));
                    let ok = match (kind, json) {
                        ("out", true) => crate::json::parse(&got).ok() == crate::json::parse(&text).ok() && crate::json::parse(&text).is_ok(),
                        ("out", false) => got == text,
                        _ => !got.contains(&text),
                    };
                    if !ok {
                        failures.push(format!("line {start}: {kind} {text}\n    got:  {got}"));
                    }
                    // Redacting twice must not change the result.
                    if !json && redact_string(&got) != got {
                        failures.push(format!("line {start}: a second redaction changes the result: {}", redact_string(&got)));
                    }
                }
                other => panic!("cases.txt line {line}: unknown line type {other:?}"),
            }
        }
        assert!(cases >= 40, "only {cases} cases: is the file truncated?");
        assert!(failures.is_empty(), "{} failing case(s) in rules/cases.txt:\n{}", failures.len(), failures.join("\n"));
    }

    #[test]
    fn counters_are_not_secrets() {
        assert!(is_secret_key("access_token") && is_secret_key("X-Api-Key") && is_secret_key("privateKey"));
        assert!(!is_secret_key("max_tokens") && !is_secret_key("tokens_used") && !is_secret_key("total_tokens"));
    }
}
