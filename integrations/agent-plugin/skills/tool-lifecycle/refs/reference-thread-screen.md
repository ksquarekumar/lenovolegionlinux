# The thread screen (the app)

The app (`legionctl-app [<view>]`) is the only UI: spaces and tabs (below). A
person and an agent get the same commands. Agents and scripts read the same views as text or JSON with
`legionctl views render`.

## What the chat bar sends

| Text | What runs |
| --- | --- |
| `/agent:<name> <input>` | The agent's command (ACP): the prompt `/<name> <input>`. The list comes with the agent's first run. |
| `/view <name> [name=value...]`, `/agent <name>`, `/chat` | The screen's own commands. `/chat` opens a new chat. |
| `/dossier use <name>`, `/dossier create <name> [--folder <repo>]`, `/dossier list`, `/dossier release` | The user's dossier: chats and agent runs keep their record there. A dossier made in the app is the user's at once. `/dossier add`, `write`, `append` and `edit` are an agent session's; the app says so and names these commands. |
| `/<words> [options]` | The operation of the registry with the CLI's words, as `legionctl <words> [options]`. |
| Other text in a chat tab | A message to the chat's agent: the first one starts the user's thread on the user's dossier; the answer comes into the tab as the agent writes it; the conversation is the thread's log. With no dossier the app says what a dossier is for and opens the Dossiers view. |
| Other text in a view tab | An intent (`ask`, the same in the app and the CLI): the stored view that answers it shows; else the agent (`/agent`) composes one in a headless run that goes on in the ACP session of the run before it. |

The list above the chat bar shows the commands that match the text: the agent's commands first, marked `(agent)`,
then the screen's, then the registry's with their class.

## The app

| Part | What it is |
| --- | --- |
| Rail | 56 px: the spaces Machine, Work, Agents and Activity (`Ctrl+1` to `Ctrl+4`, or `Alt`); the name is in the tooltip. A dot on Activity: an approval waits or doctor reports a failure. A click on the space that shows folds or opens the navigator. |
| Navigator | The entries of the space, 30% of the window at the first start; drag its right edge for another width (15% to 60%). Machine: the machine pages. Work: New chat and the user's chats, newest first, then the views (Dossiers among them). Agents: the agents of `session.acp` (a click puts `/agent <name>` into the chat bar) and the threads of agent sessions with their owner. Activity: approvals that wait, doctor's failures, running jobs, the events since the start. |
| Tabs | Pages, threads and views (`views place` opens the placed view as a tab; `/view <name> [name=value...]`). Close with the x or `Ctrl+W`; `Ctrl+Tab` and `Ctrl+Shift+Tab` go to the next or the one before; drag a tab onto another, or `Ctrl+Shift+Left`/`Right`, to move it. A thread tab shows its members, watch rules and timeline: the member's name only where it changes; a long row (a checkpoint's commit message) shows its first line, and More opens it. |
| Layout facts | The tabs, the active tab, the space and the navigator's width are facts of the entity `app` (`legionctl facts at --entity app`), written by the user's own changes; the next start shows them again. |
| Chat bar | Each tab has one, below its content (it never covers it). Click it to type; `Shift+Enter` adds a line. The agent chip shows the chat bar's agent; a click puts `/agent ` into the bar. |
| Intents | Text in a view tab's chat bar is a message to the agent that makes views (`legionctl ask "<intent>" --view <tab's view>` is the same): the stored view whose intent it is word for word opens (no model runs); with config `views.rlcd`, rlcd picks an earlier version of the tab's view when the text asks to go back ("go back to the table": that version shows, no composer run); else the composer makes the next version of the tab's view, or answers in words (the app shows the agent's words). The composer's run works in a scratch folder (`<workspace.root>/composer`), never in the app's folder. With no tab's view (`legionctl ask "<intent>"`): else, with config `views.rlcd`, the stored view that rlcd picks among the stored intents that share a word with it (at most 8; the state is the intent only; a failed rlcd call fails the ask and the composer does not run); else the chat bar's agent composes a view (its model from `views.composer_models`: Haiku on Claude Code, Luna on Codex; ACP `session/set_model`; an agent with no entry uses its account's default model), checked before it is saved, and the intent becomes a fact of the user. The composer is bound to the view tools (`views.list`, `check`, `parts`, `render`, `save`, `place`; no file, shell or web tools, no settings or other MCP servers; Claude Code through ACP `_meta`, Codex through its own Codex home with no default environment); its request lists the parts of the library that fit the intent, and each part that it writes in a stack goes into the library (a part the library has already becomes a reference to it). Each ask is an event `views.ask` with each step's tokens and time; the seed `composer-cost` (`views cost`) shows them per step. A composed view that fails its check, or has a view or part without a title, shows its reason; nothing is saved. A view with no `space` is listed in the space of its first source (code, plan, dossier, workspace: Work; agents, sessions, threads, ops, views, facts: Agents; else Machine); a composed view always is. An empty list or text shows `none`; a missing value is an empty cell; a table grouped by a column does not repeat it in its rows; a cut path keeps its file name (the cut is in the middle). `/agent:<name> ...` stays a command of the agent; in a view tab it starts a chat. A view that reads the code (`code.*`) of a folder in no git repository asks for one (Choose a repository; the tab keeps it as `repo=<folder>`). |
| Agent | `/agent <name>` picks the chat bar's agent (a name of config `session.acp`; the list offers each); another name is refused with the names. The pick is a `thread.agent` row of the thread's log (`thread agent <thread> <name>`), so each screen of the thread uses it. A thread with no pick uses config `app.agent` (default `claude`). Other text is a prompt for that agent. A missing adapter is installed first ("Installing the Claude adapter…"); a failed install shows its cause and Retry. |
| Place | The title bar shows the user's dossier, the tab's thread, its repository@branch and the environment (sandbox); with no dossier, "Choose a dossier". A click opens the Dossiers view (Use, New dossier, Use no dossier). A new chat shows the dossier picker when the user has none, and its environment step: "Change where the chat works" (a folder, the dossier's linked repository first, and a base ref; empty: a new empty folder) before the first message, which applies it (`thread repo <thread> <repo> [--base <ref>]`): its own worktree on `legionctl/<dossier>--thread-<id>`, or a plain folder's copy-on-write layer; a chat tab changes it with the same step. Confirms say what happens in words with the input as labelled values; the raw input is behind Details. |
| Status | The title bar says whether the operator answers and how old the values are. |
| Buttons and forms | A view's button runs its operation with the operation's own approval: a read at once; a change with a dry run shows its plan first (Apply runs it; polkit asks for a machine change); a change without one asks inline with its operation and input. A button that misses a required field, or gives no input to an operation that takes some, opens the operation's form, made from its input schema (`legionctl_api::form`): the required fields first, the others on demand, a choice as buttons, yes or no for a flag. The seed Operations (Agents space; `ops list`) lists every public operation by group: Open is its form. |
| Look | Dark or light as GNOME is set; the colours, sizes and type scale are in `crates/legionctl-app/src/theme.rs`. `scripts/app-shots.sh` takes a screenshot of each tab at 1366x768, 1920x1080 and 2560x1600 (`APP_SHOTS_MONITOR` for others). |

## Views

A view is a JSONC file (schema `docs/api/view.schema.json`) that binds one widget to a read operation. The
`ui-composer` and a person make the same views; the app draws only views. Layers, highest first: the
repository's `.legionctl/views/`, the user's `~/.config/legionctl/views/`, the seeds that legionctl ships
(`config/views/`: `machine-now`, `health`, `resources`, `bench`, `apps-gpu`). Each save is a version
(`views/.history/<name>/<n>.jsonc`; the seed is version 0); the version that shows is the fact `view:<name>`
`version` (`facts at --entity view:<name>`).

| Command | What it does |
| --- | --- |
| `views check < file.jsonc` | Check the format, a widget of the closed set, a read operation as the source, the operations of the buttons, the parameters. |
| `views save [--repo <dir>] < file.jsonc` | Check, then save as the next version into `~/.config/legionctl/views/` (or the repository's `.legionctl/views/`); that version shows. |
| `views version <name> undo\|redo\|reset\|pin\|<n>` | Show another version: reset goes to the pinned version, else the seed; pin keeps the version that shows as the one reset goes to. |
| `views list`, `views render <name> [name=value...]` | The views of the layers and the seeds (title, intent); one view with its data, as text (counts first, one line each). |
| `views place <thread> <name> [name=value...]` | Open the view as a tab of the thread's screens (a `views.place` row of its log). |
| `views parts [--intent <words>]` | The parts library: each stored view with one widget and one source, with the version that shows; with an intent, the parts that share a word with it, best first. |
| `views cost` | What each step of `ask` cost (stored, rlcd, composer): runs, answers, failures, tokens, median and longest time. |

| Field | Content |
| --- | --- |
| `name`, `title`, `intent`, `space`, `params` | a-z, 0-9 and `-`; the words; the intent it answers ("the machine now"); the navigator's space (machine, work, agents); parameters `string` or `integer`. |
| `widget` | `table`, `tree`, `gauge`, `sparkline`, `markdown`, `diff`, `list`, `stack`, `stat`, `chart`, `graph`, `form` or `timeline`. |
| `source` | `op` (a read operation), `input` (`{param}` in a string is the value), `path` (`$`, `$.a.0.b`, `$.[name=x].value`), `all` (read every page of a paged answer). |
| `columns`, `labels`, `formats` | Table fields (`a.b`; `a\|b` the first that is not null); header and button words; `ago`, `time`, `int`, `1dp`, `count`, `join`, `bytes` (1.5 KiB), `ms` (a duration: 1.2 s); another format is refused. |
| `group`, `levels`, `filter`, `last` | Rows in groups (a field, or rules `prefix`/`any` with `rest`; `labels`, `order`, `fold`); state colours of a number column (`warn`, `critical`, `when`); rows with these values; the newest N rows first. |
| `unit`, `warn`, `critical`, `note`, `missing`, `trend` | A stat's unit, limits, note (`{field}` of the answer), the cause when the value is missing, and a path to its trend. |
| `x`, `y`, `series` | A chart's fields: one line per series value (a chart never mixes kinds), x as time with `formats.x = time`. |
| `node`, `expand`, `select`, `highlight`, `edges` | A graph: its nodes' fields (`id`, `label`, `kind`, `count`, `state` with `states`, `children` in the data, `leaf`); the parameter that a click on a node sets to read its children (the same view read with it); the parameter that a click on a leaf sets (the stack's other parts read it); marks from a second read source (`source`, `fields`, `label`); edges in the answer (`path`, `from`, `to`, `kind`). Seeds: `code-graph` (the code tree with counts, what changed since a ref marked, what uses the selected symbol) and `plan-graph` (features, work items and units in their state colours), both in the Work space. |
| `form` | A form widget: the button (`label`, `op`, `input`) whose form it opens. |
| `defaults`, `missing` | A parameter's value when none is given; the text of a part while one of its parameters is empty (its source is not read). |
| `views` | The views of a stack: by name (`name`, `params`, with `{param}` of the stack; `version` pins a part: a new version of the part does not change this stack) or written in place (they get the stack's parameters that they declare). |
| `actions`, `row_actions` | Buttons: `label`, `op` (a public operation, or `{item.op}`), `input` (`{row.x}`, `{item.x}`, a whole `"{item.input}"`), `each` (one button per item of a row's array field). A button runs its operation with its own approval: a read at once, a change with its dry run first, else an inline confirm; polkit asks for the machine. |
| `refresh` | `events` (kinds after which the view reads again, `thread.*`; also after `views.save`) and `every` (seconds, for live values). A refresh reads the data, never the spec. |
