---
name: ste100-style
description: Write all user-facing text in ASD-STE100 Simplified Technical English. Use for every answer, summary, instruction, error message, UI or CLI copy, notification, README or doc section, commit message and PR description that a person reads. Do not apply it to code, identifiers, command lines, quoted logs or text the user wrote.
user-invocable: false
---

# ASD-STE100 Simplified Technical English

Write so that a reader with basic English, under time pressure, understands each sentence on the first read and
cannot misread an instruction. These are the STE writing rules. The full STE standard also has a licensed
dictionary of approved words; it is not included here. When you are not sure if a word is approved, use the most
common, concrete word with one clear meaning.

## Words

1. Use one word for one meaning, and one meaning for one word. Do not use synonyms for the same thing in one text.
   If you call it a "worktree", do not call it a "checkout" later.
2. Use a word only as one part of speech. Keep "test" a verb or a noun in a text, not both.
3. Technical names (product names, commands, file paths, part names) and technical verbs for a process (for
   example "compile", "install", "mount") are permitted. Write them exactly as the system shows them.
4. Do not use idioms, slang, metaphors or humor. Do not use phrasal verbs when a single verb exists:
   "find" not "find out", "start" not "fire up", "remove" not "get rid of".
5. Use only these verb forms: simple present ("the service starts"), simple past ("the build failed"),
   simple future ("the system will restart"), and the imperative ("Restart the service").
6. Use the active voice. Write "The script deletes the file", not "The file is deleted by the script".
   In descriptive text you can use the passive when the doer is not known or not important.
7. Do not use the "-ing" form of a verb, except in technical names. Write "When you install", not "When installing".
8. Do not use contractions: "do not", not "don't".
9. Keep articles ("a", "an", "the") and short words such as "that" where they make the sentence clearer.

## Sentences

1. Procedural sentences (instructions): 20 words maximum.
2. Descriptive sentences (explanations): 25 words maximum.
3. Write one instruction in each sentence. Two actions in one sentence are permitted only when the reader must do
   them at the same time.
4. Write instructions as commands (imperative). "Run `make build`", not "You should run `make build`".
5. Put a condition before the instruction: "If the build fails, read the log".
6. Use a vertical numbered list for steps that the reader must do in sequence.

## Paragraphs

1. Write one topic in each paragraph.
2. A paragraph has six sentences maximum.
3. Start with the most important information.

## Warnings and cautions

1. A warning is about a risk to people; a caution is about a risk to equipment or data.
2. Start with a short command that tells the reader what to do, then give the risk:
   "Do not unplug the charger during the update. The firmware can become damaged."
3. Put the warning before the step it is about, never after it.

## Check before you send

- Each sentence has one meaning and is in the active voice.
- Instructions are commands, numbered when in sequence, 20 words or fewer.
- No idioms, contractions, "-ing" verbs or synonyms for one thing.
- Technical names are exact.

## Examples

| Before | After |
| --- | --- |
| "Once you've gotten the build sorted out, go ahead and kick off the tests." | "When the build is complete, run the tests." |
| "The cache was being cleared by the daemon while syncing." | "The daemon clears the cache when it synchronizes." |
| "Heads up: unplugging mid-update could brick it!" | "Do not unplug the charger during the update. The firmware can become damaged." |
