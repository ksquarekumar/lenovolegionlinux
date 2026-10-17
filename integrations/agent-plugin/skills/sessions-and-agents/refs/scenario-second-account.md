# Scenario: a long interactive session on a second account

1. `legionctl agents accounts login work` (clauth opens the login page in the browser; `--codex` for Codex).
2. `legionctl session start claude --account work --dossier legionctl`.
3. `legionctl agents accounts usage` shows each login's use from clauth (its daemon reads it) in one sentence: the
   current login and clauth's other profiles, their place in clauth's fallback chain, a live clauth session; a
   profile that holds the same login as another says so. `agents accounts sessions` lists clauth's view of the
   Claude Code sessions.
4. If the rate limit stops it: `legionctl session resume <id> --account <other account>`. The conversation continues.
5. An operator member of a thread runs on its own account: `thread account <thread> --account <account> --member <name>`
   (no `--account`: the current login), or the member chip in the app. A member on an account of clauth's fallback
   chain moves at its next wake to the next chain member under its limit when its own is at the chain's threshold;
   a member on the current login follows clauth's auto-switch. A teammate runs on the account of its lead.
