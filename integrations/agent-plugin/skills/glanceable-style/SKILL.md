---
name: glanceable-style
description: Structure every user-facing output (answers, reports, status, docs, UI and CLI text) so a person gets the point at a glance and can open more detail on demand (progressive disclosure), and so agents and tools can search, link and track it with few tokens. Use with the ste100-style skill for the wording.
user-invocable: false
---

# Glanceable, progressive, agent-readable output

One output serves two readers. A person scans it in seconds and goes deeper only if needed. An agent or tool
searches it, links to parts of it, and tracks it over time, and pays for every token it reads.

## For people: answer first, then layers

1. **Level 0, the glance (first line):** the answer, the state or the decision, in one sentence. "Build passes; 2
   warnings remain." Never start with background, method or a restatement of the question.
2. **Level 1, the key facts:** 3 to 7 items the reader needs to act. Use a table to compare things and a short
   list for independent facts. Bold the decision or the one thing that needs action.
3. **Level 2, the detail:** reasons, evidence, steps. Use headings that name the content ("Why the test fails"),
   not generic ones ("Details").
4. **Level 3, raw material:** logs, full output, long lists. Link or collapse it (`<details>`, a file path, a URL).
   Do not paste it inline when a reference is enough.

Rules:
- Keep the same order and names for the same kind of output, so readers learn where to look.
- Give numbers with units and the comparison that makes them meaningful ("85 W, up from 70 W").
- Show state in form, not only in words: a status column, a "Needs action" heading, a severity word at the start.
- Put what the reader must do last and make it explicit: one command, one question, or "nothing to do".

## For agents and tools: searchable, linkable, trackable

1. **Stable identifiers:** use `file:line`, commit SHAs, issue and PR numbers, unit names, IDs and URLs exactly as the
   system writes them. These are what agents search for and link to.
2. **Exact strings:** quote error messages, commands and settings verbatim so a search finds them.
3. **Machine-readable when a tool consumes it:** emit JSON with stable keys, or a table with fixed columns. Do not
   mix prose into data.
4. **References over copies:** point to a file, line range or URL instead of repeating its content.
5. **Track changes:** for status that changes over time, give the time or version and what changed since the last
   report ("since 0bbadd5: …").

## Token budget

- Do not repeat the question, earlier answers or unchanged context.
- One idea in one place; link to it from others.
- Remove filler: "As requested", "Here is", "It is worth noting".
- A short answer is complete only when the reader can act on it. Cut words, not the information they need.

## Dual-audience pattern

When both readers matter, put the human summary first and the agent material at the end or in a referenced
artifact:

```
Build passes; 2 warnings remain.          <- glance
| Check | Result |                       <- key facts
...
Details: build.log lines 120-140          <- reference, not a paste
```

## Check before you send

- The first line answers or states, by itself.
- A reader can stop after any level and still act correctly.
- Every identifier, path and error is exact and searchable.
- Nothing is repeated; long material is linked or collapsed.
