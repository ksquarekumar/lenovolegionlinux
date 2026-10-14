---
name: ste100-style
description: Write user-facing text in ASD-STE100 Simplified Technical English. Use for every answer, summary, instruction, message, UI or CLI text, doc, skill, commit message and PR description that a person reads. Do not apply it to code, identifiers, command lines, quoted logs or the user's own text.
user-invocable: false
---

# ASD-STE100 Simplified Technical English

## Skills you write (the convention, 2026-10-15)

1. A skill is an ordered list of short sentences or short paragraphs. Each item builds on the item before it.
2. One fact lives in one skill. Another skill that needs it writes one line: "See skill X."
3. Before you save a skill, read the other skills. Remove each overlap and each contradiction.
4. `SKILL.md` holds only the rules. Examples, scenarios and reference tables go into ref files.
5. Put each ref at `skills/<skill>/refs/<kind>-<slug>.md`, with `<kind>` one of `example`, `scenario`, `reference`.
6. End `SKILL.md` with a "Refs" list: one line for each ref, its path and when to read it.
7. The `description` says when to use the skill. Write all of it in STE (the rules below).

## Words

1. Use one word for one meaning. Do not use synonyms for one thing in a text.
2. Use a word as one part of speech only ("test" is a verb or a noun in a text, not both).
3. Technical names and technical verbs ("compile", "mount") are permitted. Write them exactly as the system shows
   them.
4. Do not use idioms, slang, metaphors, humor or phrasal verbs: "find", not "find out".
5. Use only the simple present, simple past, simple future and the imperative.
6. Use the active voice. In descriptions, the passive is permitted when the doer is not known or not important.
7. Do not use the "-ing" form of a verb, except in technical names.
8. Do not use contractions.
9. Keep articles and short words such as "that" when they make a sentence clearer.
10. When you are not sure that a word is approved, use the most common concrete word with one clear meaning.

## Sentences and paragraphs

1. An instruction has 20 words maximum; a description has 25 words maximum.
2. Write one instruction in each sentence, as a command: "Run `make build`."
3. Put a condition before its instruction: "If the build fails, read the log."
4. Number the steps that the reader must do in sequence.
5. A paragraph has one topic, six sentences maximum, and starts with the most important information.

## Warnings and cautions

1. A warning is about a risk to people; a caution is about a risk to equipment or data.
2. Put it before the step. Start with the command, then give the risk.

## Refs

- `refs/example-rewrites.md` - read for before-and-after rewrites.
