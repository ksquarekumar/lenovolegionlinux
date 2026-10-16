# The thread screen (`legionctl tui` and the app)

`legionctl tui [--thread <id>] [--agent <name>] [--dossier <name>]` shows three panes and a chat bar. The app
(`legionctl-app [<page or view>] [--dossier <name>]`) shows spaces and tabs (DESIGN-09; below). A person and an
agent get the same commands.

| Part | Size | What it shows |
| --- | --- | --- |
| Pane 1 | 7% of the width | The views as icons with their keys. |
| Pane 2 | 30% | The log of the thread (`--thread`, else the newest open thread) and the commands of this screen. |
| Pane 3 | The rest | The view: dashboard `d`, resources `r`, sensors `s`, output of the last command `o`. |
| Chat bar | 60% of pane 3, at its bottom centre | The text to send. It grows taller with the text, to 40% of the height. |

## Keys

| Key | On the view | In the chat bar |
| --- | --- | --- |
| `d`, `r`, `s`, `o` | Show the view; the key of the view on screen shows the dashboard again. | Text. |
| `/` | Open the chat bar with `/`. | Text. |
| `Tab` | Open the chat bar. | Complete the first command in the list. |
| `Enter` | Resources: apply the action whose dry run is shown. | Send. |
| `Alt+Enter`, `Ctrl+J` | - | Add a line. |
| `Ctrl+Left`, `Ctrl+Right` | - | Make the chat bar narrower or wider by 10%. |
| `Alt+1`, `Alt+2` (`Ctrl+1`, `Ctrl+2`) | Fold or open pane 1 or 2. | The same. |
| `Esc` | Resources: discard the dry run. | Go back to the view. |
| `q` | Quit. | Text. |
| Up, Down, `1`-`9` | Resources: select a row; show the dry run of an action. | - |

## What the chat bar sends

| Text | What runs |
| --- | --- |
| `/agent:<name> <input>` | The agent's command (ACP): the prompt `/<name> <input>`. The list comes with the agent's first run. |
| `/fold 1`, `/fold 2` (TUI), `/view <name>`, `/agent <name>` (app) | The screen's own commands. |
| `/<words> [options]` | The operation of the registry with the CLI's words, as `legionctl <words> [options]`. |
| Other text | A prompt for the agent (`--agent` in the TUI, `/agent` in the app): a headless run (`session run`) that goes on in the ACP session of the run before it. |

The list above the chat bar shows the commands that match the text: the agent's commands first, marked `(agent)`,
then the screen's, then the registry's with their class.

## The app

| Part | What it is |
| --- | --- |
| Rail | 56 px: the spaces Machine, Work, Agents and Activity (`Ctrl+1` to `Ctrl+4`, or `Alt`); the name is in the tooltip. A dot on Activity: an approval waits or doctor reports a failure. A click on the space that shows folds or opens the navigator. |
| Navigator | The entries of the space, 30% of the window at the first start; drag its right edge for another width (15% to 60%). Machine: the machine pages. Work: the threads. Agents: the agents of `session.acp` (a click puts `/agent <name>` into the chat bar). Activity: approvals that wait, doctor's failures, running jobs, the events since the start. |
| Tabs | Pages, threads and views (`views place` opens the placed view as a tab; `/view <name> [name=value...]`). Close with the x or `Ctrl+W`; `Ctrl+Tab` and `Ctrl+Shift+Tab` go to the next or the one before; drag a tab onto another, or `Ctrl+Shift+Left`/`Right`, to move it. A thread tab shows its members, watch rules and timeline: the member's name only where it changes; a long row (a checkpoint's commit message) shows its first line, and More opens it. |
| Layout facts | The tabs, the active tab, the space and the navigator's width are facts of the entity `app` (`legionctl facts at --entity app`), written by the user's own changes; the next start shows them again. |
| Chat bar | Each tab has one, below its content (it never covers it). Click it to type; `Shift+Enter` adds a line. The agent chip shows the chat bar's agent; a click puts `/agent ` into the bar. |
| Agent | `/agent <name>` picks the chat bar's agent (a name of config `session.acp`; the list offers each); another name is refused with the names. The pick is a `thread.agent` row of the thread's log (`thread agent <thread> <name>`), so each screen of the thread uses it. A thread with no pick uses config `app.agent` (default `claude`). Other text is a prompt for that agent. |
| Status | The title bar says whether the operator answers and how old the values are. |
| Look | Dark or light as GNOME is set; the colours, sizes and type scale are in `crates/legionctl-app/src/theme.rs`. `scripts/app-shots.sh` takes a screenshot of each tab at 1366x768, 1920x1080 and 2560x1600 (`APP_SHOTS_MONITOR` for others). |

## Views

A view is a JSONC file (schema `docs/api/view.schema.json`) that binds one widget to a read operation. The
`ui-composer` and a person make the same views; the app draws only views (DESIGN-09). Layers, highest first: the
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

| Field | Content |
| --- | --- |
| `name`, `title`, `intent`, `space`, `params` | a-z, 0-9 and `-`; the words; the intent it answers ("the machine now"); the navigator's space (machine, work, agents); parameters `string` or `integer`. |
| `widget` | `table`, `tree`, `gauge`, `sparkline`, `markdown`, `diff`, `list`, `stack`, `stat` or `chart`. |
| `source` | `op` (a read operation), `input` (`{param}` in a string is the value), `path` (`$`, `$.a.0.b`, `$.[name=x].value`), `all` (read every page of a paged answer). |
| `columns`, `labels`, `formats` | Table fields (`a.b`; `a\|b` the first that is not null); header and button words; `ago`, `time`, `int`, `1dp`, `count`, `join`. |
| `group`, `levels`, `filter`, `last` | Rows in groups (a field, or rules `prefix`/`any` with `rest`; `labels`, `order`, `fold`); state colours of a number column (`warn`, `critical`, `when`); rows with these values; the newest N rows first. |
| `unit`, `warn`, `critical`, `note`, `missing`, `trend` | A stat's unit, limits, note (`{field}` of the answer), the cause when the value is missing, and a path to its trend. |
| `x`, `y`, `series` | A chart's fields: one line per series value (a chart never mixes kinds), x as time with `formats.x = time`. |
| `views` | The views of a stack: by name (`name`, `params`) or written in place. |
| `actions`, `row_actions` | Buttons: `label`, `op` (a public operation, or `{item.op}`), `input` (`{row.x}`, `{item.x}`, a whole `"{item.input}"`), `each` (one button per item of a row's array field). A button runs its operation with its own approval: a read at once, a change with its dry run first, else an inline confirm; polkit asks for the machine. |
| `refresh` | `events` (kinds after which the view reads again, `thread.*`; also after `views.save`) and `every` (seconds, for live values). A refresh reads the data, never the spec. |
