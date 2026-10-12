# Contributing

A secret that slipped through can be reported in a pull request when it is a
format anyone could hit; if it reveals something about a real system, use
[SECURITY.md](SECURITY.md) instead.

The most useful contribution is a **redaction rule**: a secret format, a URL
parameter or a label that llca does not mask yet. Rules live in two
plain-text files; no Rust is needed.

## Add a redaction rule

1. **Find what slipped through.** Run `llca audit`. It lists values in
   your journal that look random and were kept, shortened to their first
   four characters, with the rule that would mask them:

   ```
   URL parameter "session_key" · 3 value(s), e.g. Zq81… (24 characters)
     events #88, #91, #97 · Bash
     rule: url-param session_key
   ```

   Not every finding is a secret (some are identifiers). Look at the events
   with `llca logs` and decide.

2. **Add the rule** to [`rules/redaction.rules`](rules/redaction.rules). The
   file explains each kind of rule at its top:

   | Rule | Masks | Example |
   |------|-------|---------|
   | `field PATTERN` | an argument field whose name contains PATTERN | `field password` |
   | `key PATTERN` | a value after `NAME=`, `NAME:`, `"name":` or `--name` in text | `key token` |
   | `label PATTERN` | a value after a human label, in any language | `label mots? de passe` |
   | `url-param NAME` | a URL query parameter | `url-param sig` |
   | `header NAME` | the whole value of an HTTP header | `header cookie` |
   | `token PREFIX LENGTH CHARACTERS` | a credential recognisable by its format | `token ghp_ 36 [A-Za-z0-9]` |
   | `password-option FLAG [glued] [with=WORD] PROGRAM…` | a password given as a short option, for those programs only | `password-option -a redis-cli` |
   | `omit FIELD` | a field holding content, never kept | `omit chars` |

   Patterns are deliberately small: literal characters, `[sets]` with
   ranges, `.` (any character) and `?` (optional). In a label, a space means
   "a space, `_`, `-` or nothing".

3. **Prove it** in [`rules/cases.txt`](rules/cases.txt), with a made-up
   secret, never a real one (a value that came from your journal, even an
   expired one, does not belong in a public repository):

   ```
   in:   curl 'https://api.example/v1?session_key=FAKEfake1234567890&page=2'
   out:  curl 'https://api.example/v1?session_key=[REDACTED]&page=2'
   ```

   Add a `keep:` case too if the rule could catch ordinary text by mistake,
   and a `kind:` line with the label the dashboard shows. For a `token`
   rule, that label is the comment after the rule:

   ```
   token ghp_ 36 [A-Za-z0-9]     # GitHub token
   ```

4. **Run `cargo test`.** It checks that the rules file is valid (errors give
   the line), that every case passes, and that redacting twice changes
   nothing. Then open a pull request.

After installing a build with your rule, `llca redact` applies it to
the events already in your journal.

## Name an online service

The overview's map recognises online services from
[`rules/services.rules`](rules/services.rules): a command line tool
(`cli gcloud Google Cloud`), a connected app (`mcp cloudflare Cloudflare`) or
a domain (`domain github.com GitHub`). A domain without a rule shows as a
website, and a connected app without one under its server name, so a rule
only gives the service its proper name and groups its tools together. Add a
line, run `cargo test` (it checks the file), rebuild.

To give a service its logo, add `icon SLUG NAME`, where SLUG is the logo's name
on [simpleicons.org](https://simpleicons.org) and NAME the service's name in
the other rules, then regenerate `static/brands.js`:

```sh
git clone --depth 1 https://github.com/simple-icons/simple-icons.git /tmp/simple-icons
python3 scripts/brands.py /tmp/simple-icons
```

Only CC0 logos are accepted (the script refuses the others), and `cargo test`
checks that `static/brands.js` matches the rules.

## What a rule cannot do

Rules can only add masking. There is no way to exempt a value, so a rules file
cannot weaken the protection.

Structural cases (heredoc bodies, private key blocks, `curl -u user:pass`,
`mysql -pSECRET`, `docker login -p`, credentials in URLs, JSON Web Tokens)
are handled in
`src/redact.rs`, because their shape needs code. Changes there need Rust and
a case in `rules/cases.txt` as well.
