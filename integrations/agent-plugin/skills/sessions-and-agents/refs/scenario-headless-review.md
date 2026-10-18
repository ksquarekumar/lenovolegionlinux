# Scenario: a headless review in a container

1. The first run makes the repository's agent image (some minutes); `legionctl workspace pack` makes it ahead.
2. `legionctl session run claude "Review the last commit for bugs" --in container --dossier legionctl`.
3. Read the answer. Check `legionctl jobs show <id>` and `legionctl tools calls --session <id>`.
4. Write the session id and the findings into the dossier.
