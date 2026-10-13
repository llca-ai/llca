# Security policy

llca sees what AI agents run on your machine, so a flaw in it can expose
secrets. Please report security issues privately, not in a public issue.

## What to report privately

- **A secret that is stored in clear.** Redaction is best-effort, but a common
  credential format or pattern that slips through is a bug worth fixing fast.
- **A way to read the journal or the dashboard** from another user account,
  another machine or a web page.
- **Anything that makes the hooks block, change or slow down an agent**, or
  run something other than llca.

## How to report

Use GitHub's private vulnerability reporting: the **Security** tab of this
repository, then **Report a vulnerability**. Only the maintainers see it.

**Never include a real secret.** Describe its format with a made-up value of
the same shape (`ghp_` followed by 36 random letters and digits, for example),
and the command or tool call it appeared in, also made up.

You can expect a first answer within a week. A redaction fix is released as
a new version with a rule in `rules/redaction.rules` and a case in
`rules/cases.txt`; you will be credited unless you prefer not to be.

## After a redaction fix

Update llca, then:

```sh
llca init --global   # the hooks use a copy of the binary: refresh it
llca redact                      # apply the new rules to events already recorded
```

Until the copy is refreshed, `llca doctor` and the dashboard warn that
the hooks run an older version.

Remember that a secret masked by llca was still seen in clear by the
agent and by the service that runs its model. Rotate credentials that do not
expire on their own.

## Supported versions

Only the latest release receives fixes.
