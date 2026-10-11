//! The redaction rules file (`rules/redaction.rules`), compiled into the
//! binary. The file holds the lists that grow over time (secret words, URL
//! parameters, token formats, labels, content fields); the scanners in
//! `redact.rs` hold the structure (quoting, heredocs, key blocks).

use std::sync::OnceLock;

pub const SOURCE: &str = include_str!("../rules/redaction.rules");

/// One element of a pattern: a set of characters (a literal is a set of one).
#[derive(Debug, Clone, PartialEq)]
enum Elem {
    Set(Vec<(char, char)>),
    Any,
}

/// A small pattern: literals, `[sets]` with ranges, `.`, `?` (optional).
/// In labels, a space is an optional separator (" ", "_" or "-").
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    elems: Vec<(Elem, bool)>,
    ignore_case: bool,
}

fn parse_set(chars: &[char], mut i: usize) -> Result<(Vec<(char, char)>, usize), String> {
    let mut ranges = Vec::new();
    while i < chars.len() && chars[i] != ']' {
        let c = chars[i];
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&e| e != ']') {
            let end = chars[i + 2];
            if end < c {
                return Err(format!("invalid range {c}-{end}"));
            }
            ranges.push((c, end));
            i += 3;
        } else {
            ranges.push((c, c));
            i += 1;
        }
    }
    if i >= chars.len() {
        return Err("missing ]".into());
    }
    if ranges.is_empty() {
        return Err("empty []".into());
    }
    Ok((ranges, i + 1))
}

impl Pattern {
    pub fn parse(text: &str, ignore_case: bool, spaces_are_separators: bool) -> Result<Pattern, String> {
        let chars: Vec<char> = text.chars().collect();
        let mut elems: Vec<(Elem, bool)> = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let elem = match c {
                '[' => {
                    let (set, next) = parse_set(&chars, i + 1)?;
                    i = next;
                    Elem::Set(set)
                }
                '.' => {
                    i += 1;
                    Elem::Any
                }
                '?' => {
                    let last = elems.last_mut().ok_or("? with nothing before it")?;
                    if last.1 {
                        return Err("?? is not allowed".into());
                    }
                    last.1 = true;
                    i += 1;
                    continue;
                }
                ' ' if spaces_are_separators => {
                    elems.push((Elem::Set(vec![(' ', ' '), ('_', '_'), ('-', '-')]), true));
                    i += 1;
                    continue;
                }
                ']' | '(' | ')' | '|' | '*' | '+' | '\\' => return Err(format!("unsupported character {c:?}")),
                _ => {
                    i += 1;
                    Elem::Set(vec![(c, c)])
                }
            };
            elems.push((elem, false));
        }
        if elems.iter().all(|(_, optional)| *optional) {
            return Err("the pattern can match nothing".into());
        }
        Ok(Pattern { elems, ignore_case })
    }

    fn accepts(&self, elem: &Elem, c: char) -> bool {
        match elem {
            Elem::Any => true,
            Elem::Set(ranges) => {
                let hit = |x: char| ranges.iter().any(|&(a, b)| a <= x && x <= b);
                hit(c) || (self.ignore_case && (c.to_lowercase().any(hit) || c.to_uppercase().any(hit)))
            }
        }
    }

    /// Every position where a match starting at `i` can end, longest first
    /// (like a greedy regular expression trying its alternatives).
    pub fn ends(&self, s: &[char], i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        self.walk(s, 0, i, &mut out);
        out
    }

    fn walk(&self, s: &[char], k: usize, j: usize, out: &mut Vec<usize>) {
        let Some((elem, optional)) = self.elems.get(k) else {
            if !out.contains(&j) {
                out.push(j);
            }
            return;
        };
        if s.get(j).is_some_and(|&c| self.accepts(elem, c)) {
            self.walk(s, k + 1, j + 1, out);
        }
        if *optional {
            self.walk(s, k + 1, j, out);
        }
    }

    /// True when the pattern matches somewhere inside `s`.
    pub fn found_in(&self, s: &[char]) -> bool {
        (0..s.len()).any(|i| !self.ends(s, i).is_empty())
    }
}

/// `token PREFIX LENGTH CHARACTERS`
#[derive(Debug, Clone, PartialEq)]
pub struct TokenRule {
    /// Shown in the dashboard ("GitHub token"): the comment after the rule.
    pub name: String,
    pub prefix: Pattern,
    pub min: usize,
    pub max: Option<usize>,
    pub chars: Pattern,
}

/// `password-option FLAG [glued] [with=WORD] PROGRAM…`: programs where a
/// short option carries a password, either glued to it (`mysql -pSECRET`) or
/// as the next word (`docker login -p SECRET`). `with` names a word the
/// command must also contain. Elsewhere these options mean something else
/// (`mkdir -p`, `docker run -p`), so the programs are always listed.
#[derive(Debug, Clone, PartialEq)]
pub struct PasswordOption {
    pub flag: String,
    pub glued: bool,
    pub with: Option<String>,
    pub programs: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Rules {
    pub fields: Vec<Pattern>,
    pub keys: Vec<Pattern>,
    pub labels: Vec<Pattern>,
    pub url_params: Vec<String>,
    /// HTTP headers whose whole value is a credential (`Cookie: …`).
    pub headers: Vec<String>,
    pub tokens: Vec<TokenRule>,
    pub omit: Vec<String>,
    pub password_options: Vec<PasswordOption>,
}

/// Parse a rules file. Errors name the line, for contributors.
pub fn parse(source: &str) -> Result<Rules, String> {
    let mut rules = Rules::default();
    for (n, raw) in source.lines().enumerate() {
        let line = raw.split(" #").next().unwrap_or_default().split("\t#").next().unwrap_or_default().trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fail = |e: String| format!("line {}: {e}: {raw}", n + 1);
        let (kind, rest) = line.split_once(char::is_whitespace).ok_or_else(|| fail("a rule needs a value".into()))?;
        let rest = rest.trim();
        match kind {
            "field" => rules.fields.push(Pattern::parse(rest, true, false).map_err(fail)?),
            "key" => rules.keys.push(Pattern::parse(rest, true, false).map_err(fail)?),
            "label" => rules.labels.push(Pattern::parse(rest, true, true).map_err(fail)?),
            "url-param" | "omit" | "header" => {
                if rest.contains(char::is_whitespace) {
                    return Err(fail("one name per line".into()));
                }
                let list = match kind {
                    "omit" => &mut rules.omit,
                    "header" => &mut rules.headers,
                    _ => &mut rules.url_params,
                };
                let name = rest.to_lowercase();
                if list.contains(&name) {
                    return Err(fail("duplicate".into()));
                }
                list.push(name);
            }
            "token" => {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                let [prefix, length, chars] = parts[..] else {
                    return Err(fail("expected: token PREFIX LENGTH CHARACTERS".into()));
                };
                let number = |t: &str| t.parse::<usize>().map_err(|_| fail("LENGTH is a number or a range like 16-16".into()));
                let (min, max) = match length.split_once('-') {
                    Some((a, b)) => (number(a)?, Some(number(b)?)),
                    None => (number(length)?, None),
                };
                if min < 8 || max.is_some_and(|m| m < min) {
                    return Err(fail("LENGTH must be at least 8 (shorter values would mask ordinary words)".into()));
                }
                let chars = Pattern::parse(chars, false, false).map_err(fail)?;
                if chars.elems.len() != 1 || !matches!(chars.elems[0], (Elem::Set(_), false)) {
                    return Err(fail("CHARACTERS is one set, like [A-Za-z0-9]".into()));
                }
                let name = raw.split_once(" #").map(|(_, c)| c.trim().to_string()).filter(|c| !c.is_empty());
                let name = name.unwrap_or_else(|| format!("Token starting {prefix}"));
                rules.tokens.push(TokenRule { name, prefix: Pattern::parse(prefix, false, false).map_err(fail)?, min, max, chars });
            }
            "password-option" => {
                const USAGE: &str = "expected: password-option FLAG [glued] [with=WORD] PROGRAM…";
                let mut words = rest.split_whitespace();
                let flag = words.next().unwrap_or_default();
                if !flag.starts_with('-') || flag.len() < 2 || flag.chars().any(|c| c == '=' || c == '"' || c == '\'') {
                    return Err(fail(format!("FLAG is an option such as -p ({USAGE})")));
                }
                let mut option = PasswordOption { flag: flag.to_string(), glued: false, with: None, programs: Vec::new() };
                for w in words {
                    if w == "glued" && option.programs.is_empty() {
                        option.glued = true;
                    } else if let Some(word) = w.strip_prefix("with=") {
                        if word.is_empty() || !option.programs.is_empty() {
                            return Err(fail(USAGE.into()));
                        }
                        option.with = Some(word.to_string());
                    } else if w.starts_with('-') || w.contains('=') || w == "glued" {
                        return Err(fail(format!("{w:?} is not a program name ({USAGE})")));
                    } else {
                        option.programs.push(w.to_string());
                    }
                }
                if option.programs.is_empty() {
                    return Err(fail(format!("name at least one program ({USAGE})")));
                }
                let taken = |p: &String| rules.password_options.iter().any(|o| o.flag == option.flag && o.programs.contains(p));
                if let Some(p) = option.programs.iter().find(|p| taken(p)) {
                    return Err(fail(format!("duplicate: {p} {} is already listed", option.flag)));
                }
                rules.password_options.push(option);
            }
            other => return Err(fail(format!("unknown rule {other:?}"))),
        }
    }
    Ok(rules)
}

/// The rules compiled into this binary.
pub fn get() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    // The file is checked by the test suite, so this cannot fail in a release.
    RULES.get_or_init(|| parse(SOURCE).expect("rules/redaction.rules is invalid"))
}

impl TokenRule {
    /// End of a token starting at `i`, if there is one.
    pub fn end(&self, s: &[char], i: usize) -> Option<usize> {
        let class = match &self.chars.elems[0].0 {
            Elem::Set(_) => &self.chars.elems[0].0,
            Elem::Any => unreachable!(),
        };
        for j in self.prefix.ends(s, i) {
            let mut k = j;
            while k < s.len() && self.chars.accepts(class, s[k]) && self.max.is_none_or(|m| k - j < m) {
                k += 1;
            }
            if k - j >= self.min {
                return Some(k);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn the_shipped_rules_parse() {
        let r = parse(SOURCE).unwrap();
        assert!(r.fields.len() >= 9 && r.tokens.len() >= 13 && r.omit.contains(&"chars".to_string()));
        assert!(r.headers.contains(&"cookie".to_string()));
        let docker = r.password_options.iter().find(|o| o.programs.contains(&"docker".to_string())).unwrap();
        assert_eq!((docker.flag.as_str(), docker.glued, docker.with.as_deref()), ("-p", false, Some("login")));
        assert!(r.password_options.iter().any(|o| o.glued && o.programs.contains(&"mysql".to_string())));
    }

    #[test]
    fn password_options() {
        let r = parse("password-option -p glued mysql mariadb\npassword-option -p with=login docker\npassword-option -a redis-cli").unwrap();
        assert_eq!(r.password_options[0], PasswordOption { flag: "-p".into(), glued: true, with: None, programs: vec!["mysql".into(), "mariadb".into()] });
        assert_eq!(r.password_options[1].with.as_deref(), Some("login"));
        assert!(!r.password_options[2].glued);
    }

    #[test]
    fn patterns() {
        let p = Pattern::parse("mots? de passe", true, true).unwrap();
        for ok in ["mot de passe", "Mots-de-passe", "motdepasse", "MOT_DE_PASSE"] {
            assert!(!p.ends(&chars(ok), 0).is_empty(), "{ok}");
        }
        assert!(p.ends(&chars("mot passe"), 0).is_empty());
        let p = Pattern::parse("cl[ée]s? api", true, true).unwrap();
        assert_eq!(p.ends(&chars("Clé API: x"), 0), vec![7]);
        assert!(Pattern::parse("api.?key", true, false).unwrap().found_in(&chars("x-api-key")));
        let t = parse("token gh[pousr]_ 12 [A-Za-z0-9]").unwrap();
        assert_eq!(t.tokens[0].end(&chars("ghp_abcdefghijkl0 rest"), 0), Some(17));
        assert_eq!(t.tokens[0].end(&chars("ghx_abcdefghijkl0"), 0), None);
        let exact = parse("token AKIA 16-16 [A-Z0-9]").unwrap();
        assert_eq!(exact.tokens[0].end(&chars("AKIAABCDEFGHIJKLMNOPQRS"), 0), Some(20));
    }

    #[test]
    fn mistakes_are_reported_with_their_line() {
        for (source, message) in [
            ("feild token", "line 1: unknown rule"),
            ("\nlabel mot (de) passe", "line 2: unsupported character"),
            ("token abc 4 [a-z]", "at least 8"),
            ("token abc 12 abc", "one set"),
            ("url-param sig\nurl-param SIG", "duplicate"),
            ("key [abc", "missing ]"),
            ("field ?", "nothing before"),
            ("password-option p mysql", "FLAG is an option"),
            ("password-option -p glued", "at least one program"),
            ("password-option -p mysql glued", "not a program name"),
            ("password-option -p mysql with=login", "expected: password-option"),
            ("password-option -p mysql\npassword-option -p glued mysql", "line 2: duplicate"),
        ] {
            let err = parse(source).unwrap_err();
            assert!(err.contains(message), "{source:?} -> {err}");
        }
    }
}
