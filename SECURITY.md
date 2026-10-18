# Security policy

Agent Work Ledger (AWL) is local-first software: there is no hosted
service to attack and no telemetry collected, so most traditional
"vulnerability disclosure" doesn't apply directly. The
threat model is:

- A user installs the `.app` and points it at the data dirs of
  every agentic CLI / IDE on their machine.
- A bug in a connector could read more than the connector's
  contract, or write back into the source dir, or skip the
  redactor and put a `sk-*` API key into the local SQLite file.
- A bug in the IPC layer could exfiltrate the redacted payload
  to a cloud LLM the user didn't opt into.
- A bug in the bundle pipeline could ship an unsigned or
  tampered binary.

The maintainer treats any of these as a security issue.

## Reporting

Please **do not** open a public GitHub issue for security
problems. Instead, use GitHub's "Report a vulnerability" interface
on the repository (`/security/advisories/new`). This sends the
report to the maintainer privately.

If GitHub is unavailable or you do not have an account, contact
the maintainer directly via the profile contact link.

Please include:

- The commit / version you reproduced against.
- The host OS + arch (we ship macOS / Apple Silicon only).
- The connector(s) and the source data dir layout involved
  (without the actual contents).
- Whether the redactor was bypassed, and if so, on which
  pattern (`sk-*` / `AKIA*` / `Bearer` / JWT / 32-char hex).

## Acknowledgements

The maintainer will reply within 72 hours, propose a fix timeline,
and credit the reporter in the relevant CHANGELOG entry unless
they ask to remain anonymous.

## Out-of-scope (please open a regular issue instead)

- "My Codex session isn't appearing" — that's a connector bug,
  not a security one, and the public tracker is the right place.
- "The Ollama integration talks to my LAN Ollama" — that's
  intended; see the privacy section of the README for how to
  opt out.
- "I think the bundle signing should use Developer ID, not
  ad-hoc" — agreed, but that's a public-roadmap item, not a
  security one for the current distribution channel.
