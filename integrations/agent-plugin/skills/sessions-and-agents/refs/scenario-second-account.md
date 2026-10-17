# Scenario: a long interactive session on a second account

1. `legionctl agents accounts add work`, then log in to the agent in that profile.
2. `legionctl session start claude --account work --dossier legionctl`.
3. `legionctl agents accounts usage` shows each login's 5h and 7d use from the provider (`--fresh` reads again): the
   current login, clauth's profiles and the legionctl profiles.
4. If the rate limit stops it: `legionctl session resume <id> --account <other profile>`. The conversation continues.
5. An operator member of a thread runs on its own account: `thread account <thread> --account <profile> --member <name>`
   (no `--account`: the current login), or the member chip in the app. With `agents.accounts.fallback.<agent>` set, a member at `switch_at` percent moves to
   the next account by itself. A teammate runs on the account of its lead.
