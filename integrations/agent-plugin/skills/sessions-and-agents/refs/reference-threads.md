# Threads

A thread is one task: `main0` owns it, and members (observers) watch it. The log, the outbox, the members and the
watch rules are in the operator's database; they survive restarts and compactions.

| Command | What it does |
| --- | --- |
| `thread start "<title>" [--dossier]` | A new thread on your dossier; you are `main0`. |
| `thread join <thread> <name> <archetype> [--member-session] [--pace]` | Add a member: `main-agent`, `thread-observer` or `ui-composer`. |
| `thread leave <thread> <name>` | The member leaves; its open blocks expire. |
| `thread rules add <thread> <member> "<command>" [--watched] [--tools] [--path] [--every] [--kind]` | When the watched member makes `--every` calls of `--tools` on `--path` with no Bash call that contains the command between them, the member sends the message with the count of misses. |
| `thread send <to> <kind> "<text>" [--thread] [--tool] [--rule]` | A message into the outbox. `--tool` with a hard-block names the tool it holds; `--rule` with a notice counts a step you did for the receiver. |
| `thread inbox`, `thread ack <id> [--reply]` | Your open messages; ack one (a reply is a note back). |
| `thread declare <word> [--note]` | `done`, `ready`, `blocked` or `tests-written`, in each of your threads. |
| `thread pace <thread> <member> <pace>` | `on-trigger` (default: wakes only when its rules find work), `on-stop`, `on-declare`. |
| `thread show <id>`, `thread list` | Members with pace and wakes, messages, rules with counts, the log. |
| `thread peers [--dossier] [--topic] [--thread]`, `thread observe <peer>` | Who shares your dossier, workspace or thread, with state, last tool and open messages; one peer's events. A subagent is a peer of its own, `<session>/<agent id>`: send to it and observe it by that id; it gets its messages at its next tool call. |
| `thread import <thread> <account>` | Copy the thread to another account profile: the log, members (without sessions), messages, rules, layout and main0's transcript. The original is read-only after it; a thread is imported once. |

| Kind | Who sends it | The receiver gets it |
| --- | --- | --- |
| `note`, `hint` | any peer | in the next prompt |
| `reminder`, `notice`, `result` | a member | in the next prompt |
| `soft-block` | a thread-observer member | at one reply end |
| `hard-block` | a thread-observer member | at each reply end and each call of its tool, until the ack |
