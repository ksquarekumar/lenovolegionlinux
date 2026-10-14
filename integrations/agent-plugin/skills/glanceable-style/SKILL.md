---
name: glanceable-style
description: Structure user-facing output (answers, reports, status, docs, UI and CLI text) so a person gets the point at a glance and opens more detail on demand, and an agent finds and links it with few tokens. Use for every output that a person or a tool reads; use skill ste100-style for the words.
user-invocable: false
---

# Glanceable output

1. One output serves two readers: a person who scans it in seconds, and an agent that pays for each token it reads.
2. The first line is the answer, the state or the decision, in one sentence. Never start with background or method.
3. Then give 3 to 7 key facts that the reader needs to act. Use a table to compare and a list for separate facts.
4. Then give the detail (reasons, evidence, steps) under headings that name the content.
5. Link or collapse raw material (logs, full output, long lists). Do not paste it when a reference is sufficient.
6. A reader can stop after each level and still act correctly.
7. Use the same order and names for the same kind of output, so readers learn where to look.
8. Give numbers with units and a comparison: "85 W, up from 70 W".
9. Show state in form, not only in words: a status column, a "Needs action" heading, a severity word first.
10. Write identifiers, paths, commit SHAs, errors and commands exactly as the system writes them, so a search finds
    them.
11. When a tool reads the output, give JSON with stable keys or a table with fixed columns. Do not mix prose into data.
12. Point to a file, line range or URL; do not copy its content. One idea lives in one place.
13. For state that changes, give the time or version and what changed since the last report.
14. Do not repeat the question, earlier answers or unchanged context. Remove filler ("Here is", "As requested").
15. Put the action for the reader last: one command, one question, or "nothing to do".

## Refs

- `refs/example-dual-audience.md` - read for the shape of an output that serves a person and an agent.
