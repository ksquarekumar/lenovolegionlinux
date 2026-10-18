#!/usr/bin/env python3
"""A stand-in `codex` for the setup tests, with the behaviour of codex 0.163 seen on 2026-10-18 (EVIDENCE/validation/
64-codex-parity): `plugin marketplace add <folder>` reads the folder's Claude Code marketplace
(.claude-plugin/marketplace.json) and records it in ~/.codex/config.toml ([marketplaces.<name>], source = the folder);
`plugin marketplace remove <name>` removes that table; `plugin add <plugin>@<market>` copies the plugin's folder to
~/.codex/plugins/cache/<market>/<plugin>/<version of .codex-plugin/plugin.json> (one version) and records
[plugins."<plugin>@<market>"] enabled = true; `app-server` answers initialize and hooks/list (each hook of the
installed plugin's hooks file for an event Codex has, keyed as Codex keys it, `trusted` when the key is a line of
~/codex-trusted). Each call is logged to ~/codex.log.
"""

import json
import os
import shutil
import sys

HOME = os.environ["HOME"]
CONFIG = os.path.join(HOME, ".codex/config.toml")
EVENTS = {"PreToolUse": "pre_tool_use", "PermissionRequest": "permission_request", "PostToolUse": "post_tool_use",
          "PreCompact": "pre_compact", "PostCompact": "post_compact", "SessionStart": "session_start",
          "SessionEnd": "session_end", "UserPromptSubmit": "user_prompt_submit", "SubagentStart": "subagent_start",
          "SubagentStop": "subagent_stop", "Stop": "stop", "Interrupt": "interrupt"}

with open(os.path.join(HOME, "codex.log"), "a") as log:
    log.write(" ".join(sys.argv[1:]) + "\n")


def config():
    try:
        return open(CONFIG).read()
    except OSError:
        return ""


def without(text, header):
    out, skip = [], False
    for line in text.splitlines(keepends=True):
        if line.startswith("["):
            skip = line.strip() == header
        if not skip:
            out.append(line)
    return "".join(out)


def save(text):
    os.makedirs(os.path.dirname(CONFIG), exist_ok=True)
    open(CONFIG, "w").write(text)


def market_source(name):
    lines = config().splitlines()
    at = lines.index(f"[marketplaces.{name}]")
    return json.loads(next(l for l in lines[at:] if l.startswith("source = ")).split(" = ", 1)[1])


def installed():
    root = os.path.join(HOME, ".codex/plugins/cache/lenovolegionlinux/legionctl")
    versions = os.listdir(root) if os.path.isdir(root) else []
    return os.path.join(root, versions[0]) if versions else None


args = sys.argv[1:]
if args[:3] == ["plugin", "marketplace", "add"]:
    folder = args[3]
    name = json.load(open(os.path.join(folder, ".claude-plugin/marketplace.json")))["name"]
    save(config() + f'\n[marketplaces.{name}]\nsource_type = "local"\nsource = {json.dumps(folder)}\n')
elif args[:3] == ["plugin", "marketplace", "remove"]:
    save(without(config(), f"[marketplaces.{args[3]}]"))
elif args[:2] == ["plugin", "add"]:
    plugin, market = args[2].split("@")
    folder = market_source(market)
    manifest = json.load(open(os.path.join(folder, ".claude-plugin/marketplace.json")))
    source = next(p["source"] for p in manifest["plugins"] if p["name"] == plugin)
    source = os.path.join(folder, source)
    version = json.load(open(os.path.join(source, ".codex-plugin/plugin.json")))["version"]
    root = os.path.join(HOME, ".codex/plugins/cache", market, plugin)
    shutil.rmtree(root, ignore_errors=True)
    shutil.copytree(source, os.path.join(root, version))
    header = f'[plugins."{plugin}@{market}"]'
    save(without(config(), header) + f"\n{header}\nenabled = true\n")
elif args[:1] == ["app-server"]:
    for line in sys.stdin:
        message = json.loads(line)
        if message.get("method") == "initialize":
            print(json.dumps({"id": message["id"], "result": {}}), flush=True)
        elif message.get("method") == "hooks/list":
            hooks, plugin = [], installed()
            try:
                trusted = open(os.path.join(HOME, "codex-trusted")).read().split()
            except OSError:
                trusted = []
            if plugin:
                manifest = json.load(open(os.path.join(plugin, ".codex-plugin/plugin.json")))
                path = manifest["hooks"].removeprefix("./")
                for event, groups in json.load(open(os.path.join(plugin, path)))["hooks"].items():
                    if event not in EVENTS:
                        continue
                    for g, group in enumerate(groups):
                        for h, _ in enumerate(group["hooks"]):
                            key = f"legionctl@lenovolegionlinux:{path}:{EVENTS[event]}:{g}:{h}"
                            hooks.append({"key": key, "pluginId": "legionctl@lenovolegionlinux",
                                          "trustStatus": "trusted" if key in trusted else "untrusted"})
            print(json.dumps({"id": message["id"], "result": {"data": [{"cwd": HOME, "hooks": hooks}]}}), flush=True)
else:
    sys.exit(64)
