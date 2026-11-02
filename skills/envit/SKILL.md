---
name: envit
description: How to use envit, the repo-context tool — it materializes the source code of a project's dependencies into .envit/repos/ so you can read real library source instead of guessing. Use this skill whenever you see an envit.json or .envit/ directory in a project, whenever you need to read the source of a dependency or open-source library, whenever library context seems missing or stale, or whenever the user mentions envit, repo context, or asks you to add/pin/update a context repo.
license: MIT OR Apache-2.0
---

# envit — repo context for agents

envit is a virtualenv for repo context. The project's `envit.json` declares
which git repositories an agent should be able to read; `envit sync`
materializes them, read-only and commit-pinned, under `.envit/repos/`.
The bytes live once per machine in a shared store — never edit them, and
never commit `.envit/` (it's gitignored; the manifest and `envit.lock.json` are
committed instead).

## Reading context (the common case — no commands needed)

1. Read `.envit/AGENTS.md` first: it lists every available repo, its
   commit, and its resolved path.
2. Dependency source lives at `.envit/repos/<name>/` — real, complete,
   at the exact commit the lockfile pins. Prefer reading it over guessing
   at a library's behavior from memory.
3. The entries are **symlinks**: `rg` and `fd` skip them by default.
   Use `rg --follow` / `fd -L`, or search the resolved store path from
   AGENTS.md (generated) directly.
4. Everything is **read-only by design**. If you need to modify a
   dependency, that's a fork/vendor decision for the user — say so
   instead of fighting the permissions.

If `.envit/repos/` is missing but `envit.json` exists, run `envit sync`.

## Commands

| Command | Use it to |
|---|---|
| `envit sync` | Materialize the repos, skills, and project agents the manifest declares. Fast when warm (~ms). Advances stale branch-tracking repos at most once per 24h. |
| `envit sync --frozen` | Reproduce the lockfile exactly (CI, sandboxes, teammates). Errors on manifest/lock drift instead of resolving. |
| `envit sync --offline` | Never touch the network; link only what the store already has. |
| `envit add <source> [--ref R] [--name N]` | Declare a new repo. `github:owner/repo`, other forge shorthands, or any https git URL. Omit `--ref` to track the default branch. Does not sync. |
| `envit remove <name>` | Remove a declaration. |
| `envit update [name…]` | Deliberately move branch-tracking repos, skills, and agents to current remote heads. Prints `old → new`. |
| `envit pin <name>` | Freeze a repo at its currently locked commit. |
| `envit unpin <name>` | Resume tracking its original ref. |
| `envit status` | Per repo: ref, locked commit, link health, policy/staleness. Never touches the network. |
| `envit skills` | Declared skill sources and picks, including global scope. |
| `envit agents` | Declared project agents, format, targets, and locked source. |
| `envit gc [--dry-run]` | Remove store checkouts no project references. |

Env vars: `ENVIT_HOME` (store location, default `~/.envit`),
`ENVIT_NO_REFRESH=1` (block all implicit refresh),
`ENVIT_REFRESH_TTL` (e.g. `"1h"`, global staleness window).

## Manifest format (`envit.json`)

JSON: a list of repo links, plus optional `skills` and `agents` objects. A
bare `"owner/repo"` string means GitHub at the default branch; an entry
needing options becomes an object:

```json
{
  "repos": [
    "tokio-rs/tokio",
    { "source": "Effect-TS/effect", "ref": "next" },
    { "source": "serde-rs/serde", "ref": "v1.0.200", "update": "frozen" }
  ],
  "skills": {
    "vercel-labs/agent-skills": ["web-design-guidelines"],
    "humanlayer/skills": {
      "pick": ["show-me", "rpi"],
      "includeAgents": true
    }
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

Top-level agent entries require `pick`, `format`, and `targets`. `format` is
`envit-agent/v1` or `native`. Targets must be explicitly selected from `claude`, `codex`, `pi`,
and `opencode`; global manifests do not support agents.

A project skill source may set `"includeAgents": true` alongside `pick`.
For each selected skill, envit checks a skill-local `agents/` directory and
its plugin's sibling `plugins/<plugin>/agents/` directory. Included agents
default to `envit-agent/v1` and all four targets. Portable output retains
`name`, `description`, the Markdown body, and explicit `adapters`; source
`tools` and `model` hints are accepted but omitted, so hosts use their own
defaults. This can change tool restrictions; use target adapters when those
restrictions must be preserved. Unknown other fields are errors.

Portable source files are Markdown with this strict YAML frontmatter:

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

The portable fields are `name`, `description`, and `adapters`. Source-level
`tools` and `model` hints are accepted but omitted from generated files, so each
host uses its configured defaults; this may change source tool restrictions.
Other unknown fields are errors. Adapter fields remain native to that host;
envit passes them through without translating tools, permissions, or models.
It emits Markdown for Claude Code, Pi, and OpenCode, and Codex TOML with the
Markdown body in `developer_instructions`. OpenCode agents are always emitted
as subagents.

Native definitions are passed through byte-for-byte. Markdown `<name>.md`
files can target Claude Code, Pi, or OpenCode only when the manifest names
them. Codex requires `<name>.toml`. Files in a target subdirectory, such as
`agents/opencode/reviewer.md`, override the shared file. For example,
`humanlayer/skills` can select `plugins/rpi/agents` as a native source for
Claude Code, Pi, and OpenCode. If a selected target has no definition, sync
fails instead of silently omitting it.

Repo and skill per-entry options include `source`, `name`, `ref`, `update`
(`auto`/`manual`/`frozen`), `ttl` (e.g. `"1h"`), and `history` (`"full"`).
Skill entries may also use `includeAgents: true`. Top-level agent entries accept
`path`, `ref`, and optional `update: "frozen"`; they stay at their locked commit
on ordinary sync and advance only with `envit update <source>`. `auto`, `manual`,
and `ttl` are not agent options. Agent installation fails rather than following
a symlinked destination directory, preventing project entries from writing into
shared/global agent directories.
Skills are discovered at the source root, under `skills/`, and under
`plugins/<plugin>/skills/`. Set `path` to scope discovery to another
subdirectory in a monorepo. Skill sources also take `modelInvocable`
(`true`/`false`): whether the model may invoke the skill on its own.
Overrides the author's controls for both Claude
(`disable-model-invocation`) and Codex (`allow_implicit_invocation`).
Applies per source, or per skill via
`{ "name": ..., "modelInvocable": ... }` in a pick list.
Prefer `envit add`/`remove`/`pin` over hand-editing; CLI edits re-emit
canonical pretty-printed JSON.

## Situations and the right move

**You need source the project doesn't declare yet** (debugging into a
library, checking an API): `envit add github:owner/repo && envit sync`.
Tell the user you added it — it edits their committed manifest.

**Context changed under you mid-task** (files shifted after a sync
advanced a branch): `envit pin <name>` freezes the repo at the commit
you're looking at, instantly. Note the pin in your summary so the user
knows to unpin later.

**Context looks stale** (the lockfile commit is weeks old and you need
newer code): `envit update <name>`. Never assume freshness — `envit
status` shows what's pinned and when remotes were last checked.

**Bootstrapping a fresh checkout or sandbox**: `envit sync --frozen`
gives bit-identical context to the committed lockfile. This is the only
sync mode CI and cloud environments should use.

**Merge conflict in `envit.lock.json`**: don't hand-merge — resolve the
manifest, then run `envit sync`; the lock is machine-regenerated
canonically.

## Semantics worth knowing

- The manifest is intent ("track main"); `envit.lock.json` is fact ("main was
  3f2a9c1 when materialized"). Both are committed.
- Plain `sync` does not chase branches — a locked repo stays put unless
  it is `auto` (the default) *and* its remote hasn't been checked in 24h.
  `update` is the explicit "move now."
- Checkouts are keyed by commit SHA in the store, so two projects pinned
  to different commits coexist and nothing changes behind a project's
  back. Refreshes swap the symlink atomically.
- envit runs no daemons; nothing happens between invocations.

If the `envit` binary is not on PATH, ask the user how it is installed in
this environment (in envit's own development repo it is
`target/release/envit` after `cargo build --release`).
