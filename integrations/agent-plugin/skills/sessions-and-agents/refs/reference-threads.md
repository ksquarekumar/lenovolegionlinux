# Threads

A thread is one task: `main0` owns it, and members (observers) watch it. The log, the outbox, the members and the
watch rules are in the operator's database; they survive restarts and compactions.

| Command | What it does |
| --- | --- |
| `thread start "<title>" [--dossier] [--team <team>]` | A new thread on your dossier; you are `main0`. With `--team vanilla` its members join asleep: a Claude main agent spawns them as teammates (team mode), a Codex one as Codex subagents, any other agent gets them as operator sessions. |
| `thread join <thread> <name> <archetype> [--member-session] [--pace]` | Add a member: `main-agent`, `thread-observer` or `ui-composer`. |
| `thread leave <thread> <name>` | The member leaves; its open blocks expire. |
| `thread rules add <thread> <member> "<command>" [--watched] [--tools] [--path] [--every] [--kind]` | When the watched member makes `--every` calls of `--tools` on `--path` with no Bash call that contains the command between them, the member sends the message with the count of misses. |
| `thread send <to> <kind> "<text>" [--thread] [--tool] [--rule] [--teammate]` | A message into the outbox. `--tool` with a hard-block names the tool it holds; `--rule` with a notice counts a step you did for the receiver; `--teammate <name>` sends as that teammate of your team (team mode). |
| `thread inbox`, `thread ack <id> [--reply]` | Your open messages; ack one (a reply is a note back). |
| `thread declare <word> [--note]` | `done`, `ready`, `blocked` or `tests-written`, in each of your threads. |
| `thread pace <thread> <member> <pace>` | `on-trigger` (default: wakes only when its rules find work), `on-stop`, `on-declare`. |
| `thread show <id>`, `thread list` | Members with run, pace and wakes, messages, rules with counts, the log, the cost of each member (main0: the context its hooks got) and the effects. |
| `thread compensate <effect>` | Undo what a member did for main0: remove a backfill line, retract a message. |
| `thread branch <thread> <row>` | A new thread with the log up to that row, the same members, you as `main0`, and a branch at the last checkpoint. |
| `dossier append --member <name> <path>` | A thread-observer's backfill into main0's dossier: one line, marked `(by <name>)`, only while main0 is idle. |
| `thread peers [--dossier] [--topic] [--thread]`, `thread observe <peer>` | Who shares your dossier, workspace or thread, with state, last tool and open messages; one peer's events. A subagent is a peer of its own, `<session>/<agent id>`: send to it and observe it by that id; it gets its messages at its next tool call. |
| `thread import <thread> <account>` | Copy the thread to another clauth account: the log, members (without sessions), messages, rules and layout. The original is read-only after it; a thread is imported once. |

| Kind | Who sends it | The receiver gets it |
| --- | --- | --- |
| `note`, `hint` | any peer | in the next prompt |
| `reminder`, `notice`, `result` | a member | in the next prompt |
| `soft-block` | a thread-observer member | at one reply end |
| `hard-block` | a thread-observer member | at each reply end and each call of its tool, until the ack |

A team (`thread start --team`) has a flow: its gates hold `workspace checkpoint`, `thread declare done` and a
teammate's task until each condition passes (`thread show` names the open items); a stop condition ends the thread;
a member whose wake fails starts again within `restart.max`, then fails, and its blocks expire. An operator member
sleeps until a trigger (a missed step at a stop, a checkpoint, a test change, `declare done` or `ready`) and each
wake has a budget of tokens and seconds.
