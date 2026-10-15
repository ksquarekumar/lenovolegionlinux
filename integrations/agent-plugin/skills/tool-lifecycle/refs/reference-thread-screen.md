# The thread screen (`legionctl tui` and the app)

`legionctl tui [--thread <id>] [--agent <name>] [--dossier <name>]` shows three panes and a chat bar. The app
(`legionctl-app [<page>] [--agent <name> [--dossier <name>]]`) shows the same screen. A person and an agent get the
same screen and the same commands.

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
| `/fold 1`, `/fold 2`, `/view <name>` | The screen's own commands. |
| `/<words> [options]` | The operation of the registry with the CLI's words, as `legionctl <words> [options]`. |
| Other text | A prompt for `--agent`: a headless run (`session run`) that goes on in the ACP session of the run before it. |

The list above the chat bar shows the commands that match the text: the agent's commands first, marked `(agent)`,
then the screen's, then the registry's with their class.

## The app

| Part | What is different from the TUI |
| --- | --- |
| Pane 1 | The pages (Now, Apps & GPU, Health, Bench, Resources) as icons, and the thread's view when it has one. Click an icon. |
| Pane 2 | The newest open thread, with its account. It reads the thread again at each event of the event stream, not on a timer. |
| Pane 3 | The page, or the view (`views place`, `/view <name> [name=value...]`); the output of a command shows below it. |
| Fold | `Ctrl+1`, `Ctrl+2` (or `Alt`), the `«` button of a pane, `/fold 1`, `/fold 2`. A folded pane is a 24 px rail; click it to open it. |
| Chat bar | Click it to type. `Shift+Enter` adds a line. |

## Views

A view is a JSONC file (schema `docs/api/view.schema.json`) that binds one widget to a read operation. The
`ui-composer` and a person make the same views.

| Command | What it does |
| --- | --- |
| `views check < file.jsonc` | Check the format, a widget of the closed set, a read operation as the source, the parameters. |
| `views save [--repo <dir>] < file.jsonc` | Check, then save into `~/.config/legionctl/views/` (or the repository's `.legionctl/views/`). The screens that show the view read it again. |
| `views list`, `views render <name> [name=value...]` | The view files of both layers; one view with its data, as text. |
| `views place <thread> <name> [name=value...]` | Show the view in pane 3 of the thread's screens (a `views.place` row of its log). |

| Field | Content |
| --- | --- |
| `name`, `params` | a-z, 0-9 and `-`; parameters with the type `string` or `integer`. |
| `widget` | `table`, `tree`, `gauge`, `sparkline`, `markdown`, `diff`, `list` or `stack`. |
| `source` | `op` (a read operation), `input` (`{param}` in a string is the value), `path` (`$` or `$.a.0.b`). |
| `columns`, `max`, `views` | Table and list fields; the full gauge; the views of a stack (`name`, `params`). |
| `refresh.events` | Event kinds after which the view reads again (`thread.*`); also after each `views.save`. |
