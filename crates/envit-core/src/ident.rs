//! The ONLY place name-derived strings may live (see AGENTS.md).
//! When the product name lands, this file changes and nothing else does
//! (besides crate/binary renames in Cargo.toml).

/// CLI binary / brand name.
pub const TOOL_NAME: &str = "envit";
/// Manifest filename at the project root. Committed.
pub const MANIFEST_FILE: &str = "envit.json";
/// Lockfile filename at the project root. Committed.
pub const LOCK_FILE: &str = "envit.lock.json";
/// Context directory inside the project. Gitignored.
pub const CONTEXT_DIR: &str = ".envit";
/// The versioned portable agent-definition format.
pub const PORTABLE_AGENT_FORMAT: &str = "envit-agent/v1";
/// Target name used in the manifest for Claude Code agents.
pub const AGENT_TARGET_CLAUDE: &str = "claude";
/// Target name used in the manifest for Codex agents.
pub const AGENT_TARGET_CODEX: &str = "codex";
/// Target name used in the manifest for Pi agents.
pub const AGENT_TARGET_PI: &str = "pi";
/// Target name used in the manifest for OpenCode agents.
pub const AGENT_TARGET_OPENCODE: &str = "opencode";
/// Project directory where Claude Code discovers project agents.
pub const CLAUDE_AGENTS_DIR: &str = ".claude/agents";
/// Project directory where Codex discovers project agents.
pub const CODEX_AGENTS_DIR: &str = ".codex/agents";
/// Project directory where pi-subagents discovers project agents.
pub const PI_AGENTS_DIR: &str = ".pi/agents";
/// Project directory where OpenCode discovers project agents.
pub const OPENCODE_AGENTS_DIR: &str = ".opencode/agents";
/// Env var overriding the machine-wide store root (default `~/.envit`).
pub const HOME_ENV: &str = "ENVIT_HOME";
/// Env var that blocks all implicit refresh (PRD §12a kill switches).
pub const NO_REFRESH_ENV: &str = "ENVIT_NO_REFRESH";
/// Env var overriding the global refresh TTL (e.g. "1h", "30m", "7d").
pub const REFRESH_TTL_ENV: &str = "ENVIT_REFRESH_TTL";
