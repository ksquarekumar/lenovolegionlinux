# 20 — Images: build, map, cache and run distributables

Requirements (2026-10-12): legionctl reads and builds distributables (Dockerfiles, container images, VM images),
maps them to a services and resources description and a build-cache description, has a format for VMs, and can pack
all of a repository's images into a VM base image that starts them with docker compose.

Builds on 17-virtualization (providers: Docker, QEMU/libvirt) and 18-workspaces (images per workspace).

## What legionctl reads

| Source | Read from |
| --- | --- |
| Dockerfiles | the repository (any `Dockerfile`, `*.Dockerfile`, `Containerfile`), with build context and targets |
| Compose files | `compose.yaml` / `docker-compose.yml` |
| Container images | local Docker images and registries (by reference and digest) |
| VM images | qcow2 / raw disks and libvirt domains |

## Generated descriptions (per repository, in the dossier or the repo's `.legionctl/`)

`docker-compose-services.json`: one entry per service:

```json
{
  "api": {
    "build": {"context": ".", "dockerfile": "services/api/Dockerfile", "target": "runtime"},
    "image": "localhost/repo/api:2026-10-12-3f1c2a9",
    "entrypoints": {"run": ["/app/api"], "test": ["cargo", "test"], "health": ["/app/api", "--check"]},
    "ports": ["8080:8080"],
    "mounts": [{"source": "config/api", "target": "/etc/api", "read_only": true}],
    "resources": {"cpus": 4, "memory": "4g", "gpu": "none"},
    "depends_on": ["db"]
  }
}
```

`docker-compose-cache.json`: build cache per service (buildx cache source and target, base image digests, the
lockfiles that invalidate a layer), so a rebuild reuses layers and a changed lockfile rebuilds only what depends on
it.

From these two files legionctl generates the compose file it runs; the generated file is an output, not edited by
hand.

## VM format

`vm.json`: a VM built from a base image:

```json
{
  "base": "images/debian-13-docker.qcow2",
  "disk": {"overlay": "vm/repo.qcow2", "size": "60g"},
  "cpus": 8, "memory": "16g", "gpu": "none",
  "mounts": [{"source": ".", "target": "/repo", "tag": "repo", "mode": "virtiofs"}],
  "images": "docker-compose-services.json",
  "start": ["docker", "compose", "-f", "/legionctl/compose.yaml", "up", "-d"]
}
```

`legionctl images pack-vm` builds every image of the repository, saves them into the VM overlay (`docker save` into
the guest image store), adds the generated compose file, and sets the start command; the VM boots and starts all
services with docker compose, offline. The overlay is copy-on-write over the base, so several VMs share one base.

## Built (2026-10-13, I1 part 1)

`legionctl images scan [--write]` reads every `Dockerfile`, `Containerfile` and `<name>.Dockerfile` (skipping
`target/`, `node_modules/` and dot folders): a service per file (named by its folder, the repository for the root,
or `<name>`), the last stage as the target, ports from `EXPOSE`, `ENTRYPOINT` + `CMD` as the run entrypoint (the
shell form through `/bin/sh -c`), `HEALTHCHECK CMD` as health; the cache description lists the external bases (not
earlier stages, not `scratch`) and the lockfiles in each build context. `--write` puts both descriptions into the
repository's `.legionctl/`.

Built (2026-10-13, I1 part 2): `images build` builds each service with `docker buildx build --load` as
`localhost/<repo>/<service>:<short commit>` (`dev` outside git); `images run` writes `.legionctl/compose.json`
(JSON is YAML to compose: image, build, exposed ports, health check) and runs `docker compose -p <repo> up -d
--no-build`, passing compose's error on; `images status` reads the project's containers and their health from the
Engine API. Checked live (build, run, `running (healthy)`). Not yet: the buildx cache export (the docker driver
cannot export a cache), `pack-vm`.

Built (2026-10-13, I1 part 3): compose files. A `compose.yaml`, `compose.yml`, `docker-compose.yaml` or
`docker-compose.yml` at the repository root is read through `docker compose config --format json` (compose's own
interpolation, `extends` and profiles; no YAML reader in legionctl). Its services name the description: a service
that builds a scanned Dockerfile takes the compose name and keeps what the Dockerfile says unless compose says
otherwise (target, health check); a service without a build is a prebuilt `image`. Added from compose: ports
(`<published>:<port>` when published, then `expose`), mounts (bind sources relative to the repository, named volumes),
limits (`cpus`, `memory_bytes`, a GPU reservation) and `depends_on`. The environment is never read: it can hold
secrets from `.env`. `images build` builds only services with a build. `images run` passes the repository's file
first and `.legionctl/compose.json` second, as an override with only the built tags, so environment, commands,
`env_file` and volumes stay as the repository says. Tests with the real Docker: the description (and no secret in
it), and a run whose refusal comes from the repository's own service.

## Operations

`legionctl images scan | build | pack-vm | run | status`: scan writes the two JSON descriptions, build uses the cache
description, run starts with compose (containers) or libvirt (VM), status shows each service's health from its
`health` entrypoint. GPU passthrough rules from 17 apply.
