//! The manifest (`envit.json`): load, edit, save (PRD §9, §20 D6).
//!
//! Shapes: `repos` is an array of `"owner/repo"` strings or option objects;
//! `skills` maps a source to a skill name, a list, `"*"`, or an option
//! object. CLI edits mutate the JSON value and re-emit canonical
//! pretty-print (2-space, stable order) — with no comments in play, there
//! is nothing else to preserve (D6).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{Error, ident, source};

pub struct Manifest {
    doc: Value,
    path: PathBuf,
}

/// A declared repo, as read from the manifest.
#[derive(Debug, Clone)]
pub struct RepoEntry {
    pub name: String,
    pub source: String,
    pub git_ref: Option<String>,
    pub sparse: Vec<String>,
    pub history_full: bool,
    /// `"update": "frozen"` — never moves (PRD §12a).
    pub frozen: bool,
    /// `"update": "manual"` — only explicit `update` moves it.
    pub manual: bool,
    /// Per-repo staleness window override, e.g. `"ttl": "1h"`.
    pub ttl: Option<std::time::Duration>,
}

/// Who may invoke a skill: the model on its own, or only the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    Auto,
    Explicit,
}

fn parse_invocation(v: Option<&Value>) -> Option<Invocation> {
    match v.and_then(Value::as_bool) {
        Some(true) => Some(Invocation::Auto),
        Some(false) => Some(Invocation::Explicit),
        None => None,
    }
}

/// One picked skill: its name, plus an optional per-skill override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickItem {
    pub name: String,
    pub invocation: Option<Invocation>,
}

/// What a skills source picks from its repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    All,
    Named(Vec<PickItem>),
}

/// One `skills` entry: a source repo and which skills to take from it.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub source: String,
    pub pick: Pick,
    pub git_ref: Option<String>,
    /// Subdirectory of the source repo to discover skills in (monorepos).
    pub path: Option<String>,
    /// Source-level invocation override for every picked skill.
    pub invocation: Option<Invocation>,
    pub frozen: bool,
    pub manual: bool,
    pub ttl: Option<std::time::Duration>,
}

/// A harness that can discover an installed project agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentTarget {
    Claude,
    Codex,
    Pi,
    OpenCode,
}

impl AgentTarget {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            ident::AGENT_TARGET_CLAUDE => Some(Self::Claude),
            ident::AGENT_TARGET_CODEX => Some(Self::Codex),
            ident::AGENT_TARGET_PI => Some(Self::Pi),
            ident::AGENT_TARGET_OPENCODE => Some(Self::OpenCode),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => ident::AGENT_TARGET_CLAUDE,
            Self::Codex => ident::AGENT_TARGET_CODEX,
            Self::Pi => ident::AGENT_TARGET_PI,
            Self::OpenCode => ident::AGENT_TARGET_OPENCODE,
        }
    }

    pub const fn all() -> &'static [Self] {
        &[Self::Claude, Self::Codex, Self::Pi, Self::OpenCode]
    }
}

/// Whether agent definitions are envit's portable format or host-native files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFormat {
    Portable,
    Native,
}

impl AgentFormat {
    fn parse(name: &str) -> Option<Self> {
        match name {
            ident::PORTABLE_AGENT_FORMAT => Some(Self::Portable),
            "native" => Some(Self::Native),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portable => ident::PORTABLE_AGENT_FORMAT,
            Self::Native => "native",
        }
    }
}

/// Which agents a source entry selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPick {
    All,
    Named(Vec<String>),
}

/// One `agents` entry: source, selected definitions, source format and targets.
#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub source: String,
    pub pick: AgentPick,
    pub format: AgentFormat,
    pub targets: Vec<AgentTarget>,
    pub git_ref: Option<String>,
    /// Subdirectory of the source repo containing the definitions.
    pub path: Option<String>,
    pub frozen: bool,
}

/// Input for `add_repo`. `name` defaults to the repo name from the source.
pub struct NewRepo<'a> {
    pub source: &'a str,
    pub name: Option<&'a str>,
    pub git_ref: Option<&'a str>,
    pub sparse: &'a [String],
}

/// Parse "90s" / "30m" / "6h" / "7d" into a duration.
pub fn parse_ttl(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = num.trim().parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return None,
    };
    Some(std::time::Duration::from_secs(secs))
}

impl Manifest {
    /// Load the manifest sitting directly in `root`.
    pub fn load(root: &Path) -> Result<Self, Error> {
        let path = root.join(ident::MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::ManifestNotFound(root.to_path_buf())
            } else {
                Error::Io(e)
            }
        })?;
        let doc: Value = serde_json::from_str(&text)?;
        if !doc.is_object() {
            return Err(Error::ManifestShape(
                "top level must be a JSON object".into(),
            ));
        }
        Ok(Self { doc, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `"agentsMd": false` disables the note envit keeps in AGENTS.md / CLAUDE.md.
    pub fn agents_md_enabled(&self) -> bool {
        self.doc
            .get("agentsMd")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn repo_names(&self) -> Vec<String> {
        self.entries().into_iter().map(|e| e.name).collect()
    }

    /// All declared repo entries, in file order.
    pub fn entries(&self) -> Vec<RepoEntry> {
        let Some(arr) = self.doc.get("repos").and_then(Value::as_array) else {
            return Vec::new();
        };
        arr.iter().filter_map(entry_from_value).collect()
    }

    /// All declared skills sources (the `skills` object, keyed by source).
    /// Pick lists accept strings or `{ "name": ..., "invocation": ... }`.
    pub fn skill_entries(&self) -> Vec<SkillEntry> {
        let Some(obj) = self.doc.get("skills").and_then(Value::as_object) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (src, v) in obj {
            let source = src.clone();
            let entry = match v {
                Value::String(s) if s == "*" => SkillEntry {
                    source,
                    pick: Pick::All,
                    git_ref: None,
                    path: None,
                    invocation: None,
                    frozen: false,
                    manual: false,
                    ttl: None,
                },
                Value::String(s) => SkillEntry {
                    source,
                    pick: Pick::Named(vec![PickItem {
                        name: s.clone(),
                        invocation: None,
                    }]),
                    git_ref: None,
                    path: None,
                    invocation: None,
                    frozen: false,
                    manual: false,
                    ttl: None,
                },
                Value::Array(a) => SkillEntry {
                    source,
                    pick: Pick::Named(a.iter().filter_map(pick_item).collect()),
                    git_ref: None,
                    path: None,
                    invocation: None,
                    frozen: false,
                    manual: false,
                    ttl: None,
                },
                Value::Object(o) => {
                    let pick = match o.get("pick") {
                        Some(Value::String(s)) if s == "*" => Pick::All,
                        Some(Value::String(s)) => Pick::Named(vec![PickItem {
                            name: s.clone(),
                            invocation: None,
                        }]),
                        Some(Value::Array(a)) => {
                            Pick::Named(a.iter().filter_map(pick_item).collect())
                        }
                        _ => Pick::All,
                    };
                    let upd = o.get("update").and_then(Value::as_str);
                    SkillEntry {
                        source,
                        pick,
                        git_ref: o.get("ref").and_then(Value::as_str).map(str::to_string),
                        path: o.get("path").and_then(Value::as_str).map(str::to_string),
                        invocation: parse_invocation(o.get("modelInvocable")),
                        frozen: upd == Some("frozen"),
                        manual: upd == Some("manual"),
                        ttl: o.get("ttl").and_then(Value::as_str).and_then(parse_ttl),
                    }
                }
                _ => continue,
            };
            out.push(entry);
        }
        out
    }

    /// All declared agent sources. Agent entries require explicit format,
    /// selected names, and targets because installing them activates them.
    pub fn agent_entries(&self) -> Result<Vec<AgentEntry>, Error> {
        let Some(value) = self.doc.get("agents") else {
            return Ok(Vec::new());
        };
        let Some(obj) = value.as_object() else {
            return Err(Error::ManifestShape(
                "'agents' must be an object keyed by source".into(),
            ));
        };
        let mut out = Vec::new();
        for (src, value) in obj {
            source::expand(src)?;
            let Some(o) = value.as_object() else {
                return Err(Error::ManifestShape(format!(
                    "'agents.{src}' must be an object with pick, format, and targets"
                )));
            };
            for key in o.keys() {
                if !matches!(
                    key.as_str(),
                    "pick" | "format" | "targets" | "path" | "ref" | "update"
                ) {
                    return Err(Error::ManifestShape(format!(
                        "unknown field 'agents.{src}.{key}'"
                    )));
                }
            }
            let pick = o
                .get("pick")
                .ok_or_else(|| Error::ManifestShape(format!("'agents.{src}.pick' is required")))
                .and_then(|v| parse_agent_pick(v, src))?;
            let format = o
                .get("format")
                .and_then(Value::as_str)
                .and_then(AgentFormat::parse)
                .ok_or_else(|| {
                    Error::ManifestShape(format!(
                        "'agents.{src}.format' must be '{}' or 'native'",
                        ident::PORTABLE_AGENT_FORMAT
                    ))
                })?;
            let targets = parse_agent_targets(o.get("targets"), src)?;
            let update = match o.get("update") {
                None => None,
                Some(Value::String(update)) => Some(update.as_str()),
                Some(_) => {
                    return Err(Error::ManifestShape(format!(
                        "'agents.{src}.update' must be a string"
                    )));
                }
            };
            if update.is_some_and(|value| value != "frozen") {
                return Err(Error::ManifestShape(format!(
                    "'agents.{src}.update' only supports 'frozen'; use `envit update` to move sources"
                )));
            }
            let path = match o.get("path") {
                None => None,
                Some(Value::String(path)) => {
                    let path_components = Path::new(path);
                    if path_components.is_absolute()
                        || path_components.components().any(|component| {
                            matches!(
                                component,
                                std::path::Component::ParentDir
                                    | std::path::Component::RootDir
                                    | std::path::Component::Prefix(_)
                            )
                        })
                    {
                        return Err(Error::ManifestShape(format!(
                            "'agents.{src}.path' must stay inside the source checkout"
                        )));
                    }
                    Some(path.clone())
                }
                Some(_) => {
                    return Err(Error::ManifestShape(format!(
                        "'agents.{src}.path' must be a string"
                    )));
                }
            };
            let git_ref = match o.get("ref") {
                None => None,
                Some(Value::String(r)) => Some(r.clone()),
                Some(_) => {
                    return Err(Error::ManifestShape(format!(
                        "'agents.{src}.ref' must be a string"
                    )));
                }
            };
            out.push(AgentEntry {
                source: src.clone(),
                pick,
                format,
                targets,
                git_ref,
                path,
                frozen: update == Some("frozen"),
            });
        }
        Ok(out)
    }

    /// Append a repo entry. A bare source stays a plain string; options make
    /// it an object. Returns the name used.
    pub fn add_repo(&mut self, repo: NewRepo<'_>) -> Result<String, Error> {
        source::expand(repo.source)?; // validate; manifest keeps the shorthand
        let name = match repo.name {
            Some(n) => n.to_string(),
            None => source::default_name(repo.source)
                .ok_or_else(|| Error::NoDefaultName(repo.source.to_string()))?,
        };
        if self.repo_names().iter().any(|n| n == &name) {
            return Err(Error::DuplicateName(name));
        }

        let simple = repo.name.is_none() && repo.git_ref.is_none() && repo.sparse.is_empty();
        let value = if simple {
            Value::String(repo.source.to_string())
        } else {
            let mut o = serde_json::Map::new();
            o.insert("source".into(), json!(repo.source));
            if let Some(n) = repo.name {
                o.insert("name".into(), json!(n));
            }
            if let Some(r) = repo.git_ref {
                o.insert("ref".into(), json!(r));
            }
            if !repo.sparse.is_empty() {
                o.insert("sparse".into(), json!(repo.sparse));
            }
            Value::Object(o)
        };

        let repos = self
            .doc
            .as_object_mut()
            .expect("validated at load")
            .entry("repos")
            .or_insert_with(|| Value::Array(Vec::new()));
        let arr = repos
            .as_array_mut()
            .ok_or_else(|| Error::ManifestShape("'repos' exists but is not an array".into()))?;
        arr.push(value);
        Ok(name)
    }

    /// Remove the repo entry with this name.
    pub fn remove_repo(&mut self, name: &str) -> Result<(), Error> {
        let (arr, idx) = self.repo_index_mut(name)?;
        arr.remove(idx);
        Ok(())
    }

    /// Pin: rewrite the entry to `commit` with `"update": "frozen"`,
    /// remembering the tracked ref in `tracks` so unpin can restore it
    /// (PRD §12a). A plain string entry becomes an object.
    pub fn pin_repo(&mut self, name: &str, commit: &str) -> Result<(), Error> {
        let (arr, idx) = self.repo_index_mut(name)?;
        let mut o = match &arr[idx] {
            Value::String(s) => {
                let mut o = serde_json::Map::new();
                o.insert("source".into(), json!(s));
                o
            }
            Value::Object(o) => {
                if o.get("update").and_then(Value::as_str) == Some("frozen") {
                    return Err(Error::AlreadyPinned(name.to_string()));
                }
                o.clone()
            }
            _ => {
                return Err(Error::ManifestShape(
                    "repo entry must be a string or object".into(),
                ));
            }
        };
        let tracked = o
            .get("ref")
            .and_then(Value::as_str)
            .unwrap_or("HEAD")
            .to_string();
        o.insert("tracks".into(), json!(tracked));
        o.insert("ref".into(), json!(commit));
        o.insert("update".into(), json!("frozen"));
        arr[idx] = Value::Object(o);
        Ok(())
    }

    /// Unpin: restore the tracked ref and drop the freeze. Returns the ref
    /// now being tracked. An entry with no remaining options collapses back
    /// to a plain string.
    pub fn unpin_repo(&mut self, name: &str) -> Result<String, Error> {
        let (arr, idx) = self.repo_index_mut(name)?;
        let Value::Object(o) = &arr[idx] else {
            return Err(Error::NotPinned(name.to_string()));
        };
        if o.get("update").and_then(Value::as_str) != Some("frozen") {
            return Err(Error::NotPinned(name.to_string()));
        }
        let mut o = o.clone();
        let tracked = o
            .get("tracks")
            .and_then(Value::as_str)
            .unwrap_or("HEAD")
            .to_string();
        if tracked == "HEAD" {
            o.remove("ref");
        } else {
            o.insert("ref".into(), json!(tracked));
        }
        o.remove("tracks");
        o.remove("update");

        arr[idx] = if o.len() == 1 && o.contains_key("source") {
            Value::String(o["source"].as_str().unwrap_or_default().to_string())
        } else {
            Value::Object(o)
        };
        Ok(tracked)
    }

    fn repo_index_mut(&mut self, name: &str) -> Result<(&mut Vec<Value>, usize), Error> {
        let arr = self
            .doc
            .get_mut("repos")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| Error::NameNotFound(name.to_string()))?;
        let idx = arr
            .iter()
            .position(|v| entry_from_value(v).is_some_and(|e| e.name == name))
            .ok_or_else(|| Error::NameNotFound(name.to_string()))?;
        Ok((arr, idx))
    }

    /// Write back in canonical pretty form.
    pub fn save(&self) -> Result<(), Error> {
        let mut text = serde_json::to_string_pretty(&self.doc)?;
        text.push('\n');
        std::fs::write(&self.path, text)?;
        Ok(())
    }
}

fn parse_agent_pick(value: &Value, source: &str) -> Result<AgentPick, Error> {
    match value {
        Value::String(name) if name == "*" => Ok(AgentPick::All),
        Value::String(name) if !name.is_empty() => Ok(AgentPick::Named(vec![name.clone()])),
        Value::Array(items) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                let Some(name) = item.as_str().filter(|name| !name.is_empty()) else {
                    return Err(Error::ManifestShape(format!(
                        "'agents.{source}.pick' must contain only non-empty names"
                    )));
                };
                if names.iter().any(|existing| existing == name) {
                    return Err(Error::ManifestShape(format!(
                        "duplicate agent name '{name}' in 'agents.{source}.pick'"
                    )));
                }
                names.push(name.to_string());
            }
            if names.is_empty() {
                return Err(Error::ManifestShape(format!(
                    "'agents.{source}.pick' must select at least one agent"
                )));
            }
            Ok(AgentPick::Named(names))
        }
        _ => Err(Error::ManifestShape(format!(
            "'agents.{source}.pick' must be '*', a name, or an array of names"
        ))),
    }
}

fn parse_agent_targets(value: Option<&Value>, source: &str) -> Result<Vec<AgentTarget>, Error> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Err(Error::ManifestShape(format!(
            "'agents.{source}.targets' must be an array"
        )));
    };
    if items.is_empty() {
        return Err(Error::ManifestShape(format!(
            "'agents.{source}.targets' must select at least one harness"
        )));
    }
    let mut targets = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(Error::ManifestShape(format!(
                "'agents.{source}.targets' must contain only target names"
            )));
        };
        let Some(target) = AgentTarget::parse(name) else {
            return Err(Error::ManifestShape(format!(
                "unknown agent target '{name}' in 'agents.{source}.targets'"
            )));
        };
        if targets.contains(&target) {
            return Err(Error::ManifestShape(format!(
                "duplicate agent target '{name}' in 'agents.{source}.targets'"
            )));
        }
        targets.push(target);
    }
    Ok(targets)
}

/// Parse one item of the `repos` array: a bare source string, or an object
/// with options.
fn entry_from_value(v: &Value) -> Option<RepoEntry> {
    match v {
        Value::String(s) => Some(RepoEntry {
            name: source::default_name(s)?,
            source: s.clone(),
            git_ref: None,
            sparse: Vec::new(),
            history_full: false,
            frozen: false,
            manual: false,
            ttl: None,
        }),
        Value::Object(o) => {
            let source = o.get("source")?.as_str()?.to_string();
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| source::default_name(&source))?;
            let upd = o.get("update").and_then(Value::as_str);
            Some(RepoEntry {
                name,
                git_ref: o.get("ref").and_then(Value::as_str).map(str::to_string),
                sparse: o
                    .get("sparse")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                history_full: o.get("history").and_then(Value::as_str) == Some("full"),
                frozen: upd == Some("frozen"),
                manual: upd == Some("manual"),
                ttl: o.get("ttl").and_then(Value::as_str).and_then(parse_ttl),
                source,
            })
        }
        _ => None,
    }
}

/// One pick-list item: `"name"` or `{ "name": ..., "invocation": ... }`.
fn pick_item(v: &Value) -> Option<PickItem> {
    match v {
        Value::String(s) => Some(PickItem {
            name: s.clone(),
            invocation: None,
        }),
        Value::Object(o) => Some(PickItem {
            name: o.get("name")?.as_str()?.to_string(),
            invocation: parse_invocation(o.get("modelInvocable")),
        }),
        _ => None,
    }
}
