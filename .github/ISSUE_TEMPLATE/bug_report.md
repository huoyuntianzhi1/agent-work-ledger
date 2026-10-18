---
name: Bug report
about: Something didn't work the way the README said it would
title: "[bug] "
labels: bug
assignees: ''
---

## What I expected

What the README (or the in-app UI) said should happen.

## What happened

What actually happened, including the exact text of any error
toast, terminal line, or `tracing::error!` line in
`~/Library/Logs/com.minimaxcode.agentworkledger/*.log`.

## How to reproduce

1. (Connector X is installed at /path/...)
2. (Run `pnpm tauri dev` or the release .app)
3. (Click Y)
4. (See error Z)

## Environment

- AWL version: `defaults read /Applications/Agent\ Work\ Ledger.app/Contents/Info CFBundleShortVersionString`
- macOS version: (Settings → General → About)
- Architecture: (Apple Silicon / Intel)
- Connector(s) involved: (which `~/Library/...` data dir)
- Ollama running? (`curl http://localhost:11434/api/tags` if so)
- JWT / `sk-` key leakage observed? (should be 0; if non-zero,
  this is a security issue — see `SECURITY.md`)

## Output of the scanner (optional but very helpful)

```sh
/Applications/Agent\ Work\ Ledger.app/Contents/MacOS/agent-work-ledger --scan-once
```

paste the last 30 lines here.
