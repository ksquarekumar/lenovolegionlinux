# Sandboxes (`--in`)

| Where | What runs | What it needs |
| --- | --- | --- |
| `host` (default) | A systemd user scope (`legionctl-session-<id>`), then bwrap in its own PID, IPC and network namespaces: `/` and your home read-only, keys and tokens hidden; writable only the workspace, the dossier and what a commit writes in `.git`; the agent folders and caches are layers of the session. Network: only the model gateway. | bwrap, `gateway.port` for a model |
| `container` | `docker run --network none` of the repository's agent image, with the workspace, `.git` and the dossier mounted at their own paths, a private home and copies of the agent folders. Network: only the model gateway. | `legionctl workspace pack` |
| `vm` | QEMU on a throwaway layer over the repository's VM disk; the folders are shared with virtiofs. | `legionctl workspace pack --as vm` |

`workspace pack [--as container|vm] [--gpu] [--service <s>]` makes the image or the disk; it is a job.
The image and the disk get git, Node.js, the agents, their ACP adapters and bun (config `session.container.npm`).
bun runs the ACP adapter in each sandbox; on the host it finds it in bun's global folder ~/.bun/bin, where `setup
apply` installs it.
