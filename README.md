<p align="center"><img src="brand/banner.svg" alt="envit" width="100%"></p>

# envit

Declarative repository context, skills, and subagents for agents on your
local or cloud machines. Site: https://envit.dev

`envit.json`

```json
{
  "repos": [
    "sqlite/sqlite",
    "PostHog/posthog-js",
    "getsentry/sentry-javascript",
    "earendil-works/pi",
    "pierrecomputer/pierre",
    "cloudflare/cloudflare-os"
  ],
  "skills": {
    "mattpocock/skills": ["grill-me", "grilling"],
    "emilkowalski/skills": "*",
    "dmmulroy/anti-slop": "*",
    "backnotprop/bro": "bro",
    "obra/superpowers": "*",
    "remotion-dev/skills": { "pick": "*", "modelInvocable": false },
    "cursor/plugins": { "path": "pstack", "pick": ["interrogate", "blast-radius"] }
  },
  "agents": {
    "acme/agent-library": {
      "path": "agents",
      "pick": ["reviewer"],
      "format": "envit-agent/v1",
      "targets": ["claude", "codex", "pi", "opencode"]
    }
  }
}
```

External repos that your agents can use as context, skills they load, and
subagents they can delegate to. One command materializes everything:

```
$ envit sync
fetched  sqlite  1b3f6d2a94c0  3184 files · 94.2 MB
fetched  posthog-js  9c41d2e807aa  2311 files · 18.7 MB
...
skill    grill-me  e1f0a9b62c44  (mattpocock/skills)
agent    reviewer  6a12b8c0d331  (acme/agent-library, claude)
...      reviewer  ...             (acme/agent-library, codex / pi / opencode)
✓ synced 5 repo(s) · 45 skill(s) · 4 agent target(s) · 151.4 MB added to store

$ envit sync
✓ synced 5 repo(s) · 45 skill(s) · 4 agent target(s) · store unchanged   # 9 ms
```

## Install

```sh
curl -fsSL https://envit.dev/install.sh | sh
```

Also: `brew install plannotator/tap/envit`, `cargo install envit`, `npm install -g envit`.
Windows: use WSL for now. Or build from source:

```sh
cargo build --release   # target/release/envit
```

| scope | |
|---|---|
| `envit init` | This project. Writes `envit.json`. Commit it: teammates, CI, and cloud agents reproduce the same context. |
| `envit init -g` | This machine. Writes `~/.envit/envit.json`. Skills land in `~/.agents/skills/` and `~/.claude/skills/`, for every project. |
| `envit sync` | Materializes project context and any declared global skills. |

## What agents see

Skills land in `.agents/skills/` and `.claude/skills/`, where agents
already look. Project agents land in the selected host directories:
`.claude/agents/`, `.codex/agents/`, `.pi/agents/`, and `.opencode/agents/`.
Repo source lands in `.envit/repos/<name>/`, read-only, at a pinned commit.
envit stores each repo once per machine, in
`~/.envit/store/`; every project links to that one copy, so nothing is
duplicated on your disk. Each sync generates `.envit/AGENTS.md` (with a
`CLAUDE.md` symlink) that lists every repo, its commit, and its path. If
the project root has an `AGENTS.md` or `CLAUDE.md`, `sync` appends a short
fenced note that points there.

envit is not a skill or agent registry. It manages a declared environment.
Find skills and agent definitions wherever you like, then declare what a
project keeps in `envit.json`. Manually installed skills and agents stay
where they are. envit only manages what its file declares.

## Commands

| command | |
|---|---|
| `envit sync` | resolve, fetch, check out, link, lock. Refreshes branch-tracking repos at most once per 24 h. |
| `envit sync --frozen` | lockfile only. Errors on drift. For CI and sandboxes. |
| `envit sync --offline` | no network. Links what the store has. |
| `envit add <source>` | `owner/repo`, `gitlab:…`, or any https git URL. `--ref` for branch, tag, or sha. |
| `envit update [name]` | move branch-tracking repos, skill sources, and agent sources to current remote heads. |
| `envit pin <name>` | freeze at the locked commit. `unpin` resumes tracking. |
| `envit status` | ref, commit, link health, staleness. Never touches the network. |
| `envit skills` | declared skills, both scopes. |
| `envit agents` | declared project agents, format, targets, and locked source. |
| `envit gc` | delete checkouts no project references. `--dry-run` first. |

Per-skill invocation control: `{ "name": "grill-me", "modelInvocable": false }`
in a pick list rewrites both Claude's `disable-model-invocation` and Codex's
`allow_implicit_invocation`, regardless of what the author set.

## Agent definitions

Agents are active instructions, so each manifest entry must name what to pick,
which source format to use, and which harnesses may install it. `agents` are
project-scoped; global manifests remain skills-only.

`format: "native"` passes definitions through unchanged. For example, install
the upstream HumanLayer RPI Markdown agents to the three Markdown harnesses:

```json
"agents": {
  "humanlayer/skills": {
    "path": "plugins/rpi/agents",
    "pick": "*",
    "format": "native",
    "targets": ["claude", "pi", "opencode"]
  }
}
```

Markdown files named `<name>.md` install to Claude Code, Pi, or OpenCode only
when those targets are explicitly selected. Codex uses `<name>.toml`. A
host-specific subdirectory
such as `agents/opencode/<name>.md` overrides the shared file. envit does not
infer that one host's frontmatter works on another.

`format: "envit-agent/v1"` reads Markdown files with envit's strict YAML frontmatter:

```markdown
---
name: reviewer
description: Reviews changes for correctness and safety.
adapters:
  claude:
    tools: [Read, Grep]
  codex:
    sandbox_mode: read-only
  pi:
    tools: [read, grep]
  opencode:
    permission:
      edit: deny
---
Review the change and explain concrete risks.
```

Only `name`, `description`, and `adapters` are shared fields. Unknown shared
fields are errors. Adapter fields are native to that one host and are passed
through, not translated. envit emits Markdown for Claude Code, Pi, and
OpenCode, and TOML with `developer_instructions` for Codex. OpenCode output is
always a subagent. If a target has no usable native file, sync errors instead
of silently installing nothing.

Agent sources stay at their locked commit on ordinary sync, even if the remote
branch moves. Use `envit update <source>` to advance a source deliberately; an
agent entry may set `"update": "frozen"` to opt out. Agent `update` does not
accept `auto` or `manual`, and agent entries do not support `ttl`. Installation
fails rather than following a symlinked project agent directory, so a project
entry cannot write through a link into a shared/global agent directory.

## Constraints

- No daemon. `ps` shows nothing between invocations.
- One static binary, 4 MB. Git is embedded (gitoxide): no system git, no
  libgit2, no OpenSSL.
- Plain git protocol. GitHub, GitLab, Codeberg, sourcehut, self-hosted.
  No forge APIs.
- Checkouts are immutable and content-addressed. Updates are one atomic
  symlink flip.
- `#![forbid(unsafe_code)]`. Tests run against real repositories and
  filesystems, with no mocks.

## Security

What envit contacts, what it writes, and how releases are verified:
https://envit.dev/security. Report vulnerabilities privately through
[GitHub](https://github.com/plannotator/envit/security/advisories/new).

## Agent skill

`skills/envit/` is an [Agent Skill](https://agentskills.io) that teaches an
agent how to use envit. Install it into a project or globally:

```sh
npx skills add plannotator/envit
```

## License

MIT OR Apache-2.0. The envit CLI and core library are open source. Hosted
platform services built around envit may not be.
