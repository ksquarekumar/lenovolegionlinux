# The thread screen (`legionctl tui`)

`legionctl tui [--thread <id>] [--agent <name>] [--dossier <name>]` shows three panes and a chat bar. A person and
an agent get the same screen and the same commands.

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
