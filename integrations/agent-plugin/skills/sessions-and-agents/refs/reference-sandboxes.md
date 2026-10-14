# Sandboxes (`--in`)

| Where | What runs | What it needs |
| --- | --- | --- |
| `host` (default) | A systemd user scope (`legionctl-session-<id>`), then bwrap: `/` and your home read-only; writable only the workspace, the dossier, the repository's `.git`, the runtime folder and `session.writable`. | bwrap |
| `container` | `docker run` of the repository's agent image, with the workspace, `.git` and the dossier mounted at their own paths and a private home. | `legionctl workspace pack` |
| `vm` | QEMU on a throwaway layer over the repository's VM disk; the folders are shared with virtiofs. | `legionctl workspace pack --as vm` |

`workspace pack [--as container|vm] [--gpu] [--service <s>]` makes the image or the disk; it is a job.
