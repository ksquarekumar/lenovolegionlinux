# Scenario: a long interactive session on a second account

1. `legionctl agents accounts add work`, then log in to the agent in that profile.
2. `legionctl session start claude --account work --dossier legionctl`.
3. If the rate limit stops it: `legionctl session resume <id> --account <other profile>`. The conversation continues.
