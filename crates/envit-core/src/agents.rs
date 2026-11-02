//! Project agent installation for Claude Code, Codex, Pi, and OpenCode.
//!
//! Native definitions are passed through to explicitly selected targets.
//! Portable definitions use a small envit-owned frontmatter schema and emit
//! each target's native format; host-specific adapter fields remain opaque.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

use crate::link::{self, LinkMode};
use crate::manifest::{AgentEntry, AgentFormat, AgentPick, AgentTarget};
use crate::{Error, ident};

#[derive(Debug)]
pub struct AgentSync {
    pub name: String,
    pub source: String,
    pub commit: String,
    pub target: AgentTarget,
    pub path: PathBuf,
}

struct PortableAgent {
    path: PathBuf,
    name: String,
    description: String,
    body: String,
    adapters: BTreeMap<AgentTarget, Map<String, Value>>,
}

type NativeFiles = BTreeMap<AgentTarget, BTreeMap<String, PathBuf>>;

/// Materialize one declared agent source. `occupied` prevents two source
/// entries from silently replacing the same native agent name.
pub fn materialize_source(
    entry: &AgentEntry,
    checkout: &Path,
    commit: &str,
    project_root: &Path,
    store_root: &Path,
    link_mode: LinkMode,
    occupied: &mut BTreeSet<(AgentTarget, String)>,
) -> Result<Vec<AgentSync>, Error> {
    for target in &entry.targets {
        validate_target_dir(project_root, *target)?;
    }
    let root = source_root(checkout, entry.path.as_deref(), &entry.source)?;
    if !root.is_dir() {
        return Err(Error::AgentNotFound {
            agent: format!("path '{}'", entry.path.as_deref().unwrap_or(".")),
            from: entry.source.clone(),
            available: Vec::new(),
        });
    }

    match entry.format {
        AgentFormat::Native => materialize_native(
            entry,
            &root,
            commit,
            project_root,
            store_root,
            link_mode,
            occupied,
        ),
        AgentFormat::Portable => {
            materialize_portable(entry, &root, commit, project_root, store_root, occupied)
        }
    }
}

/// Remove only stale files envit can identify as links into its store or
/// marker-backed generated/copy-mode files. Other native agent files stay put.
pub fn prune(project_root: &Path, store_root: &Path, desired: &[AgentSync]) -> Result<(), Error> {
    let desired_paths: HashSet<&Path> = desired.iter().map(|agent| agent.path.as_path()).collect();
    for target in AgentTarget::all() {
        let dir = project_root.join(target_dir(*target));
        match validate_target_dir(project_root, *target) {
            Ok(true) => {}
            Ok(false) | Err(Error::LinkObstructed(_)) => continue,
            Err(error) => return Err(error),
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = entry.file_name().to_string_lossy().into_owned();
            if filename.ends_with(".commit") || filename.ends_with(".fresh") {
                continue;
            }
            if desired_paths.contains(path.as_path()) {
                continue;
            }
            if link::managed_file(&path, store_root) {
                std::fs::remove_file(&path)?;
                let _ = std::fs::remove_file(link::file_marker_path(&path));
            }
        }
    }
    Ok(())
}

fn materialize_native(
    entry: &AgentEntry,
    root: &Path,
    commit: &str,
    project_root: &Path,
    store_root: &Path,
    link_mode: LinkMode,
    occupied: &mut BTreeSet<(AgentTarget, String)>,
) -> Result<Vec<AgentSync>, Error> {
    let files = discover_native(root, &entry.targets, &entry.source)?;
    let available = native_names(&files);
    let names = select_names(&entry.pick, &available, &entry.source)?;
    let mut out = Vec::new();

    for name in names {
        validate_name(&name, root)?;
        let mut selected = Vec::new();
        for target in &entry.targets {
            if !occupied.insert((*target, name.clone())) {
                return Err(Error::AgentDuplicate {
                    agent: name.clone(),
                    from: entry.source.clone(),
                    target: target.as_str().to_string(),
                });
            }
            let source = files
                .get(target)
                .and_then(|by_name| by_name.get(&name))
                .ok_or_else(|| Error::AgentTargetMissing {
                    agent: name.clone(),
                    from: entry.source.clone(),
                    target: target.as_str().to_string(),
                    expected: format!("{} under {}", target_extension(*target), root.display()),
                })?;
            selected.push((*target, source.clone()));
        }
        for (target, source) in selected {
            let destination = project_root
                .join(target_dir(target))
                .join(format!("{name}.{}", target_extension(target)));
            let key = format!(
                "{}{commit}+native:{}:{}",
                ident::AGENT_FILE_MARKER_PREFIX,
                target.as_str(),
                source.display()
            );
            link::materialize_file_link(&destination, &source, link_mode, &key, store_root)?;
            out.push(AgentSync {
                name: name.clone(),
                source: entry.source.clone(),
                commit: commit.to_string(),
                target,
                path: destination,
            });
        }
    }
    Ok(out)
}

fn materialize_portable(
    entry: &AgentEntry,
    root: &Path,
    commit: &str,
    project_root: &Path,
    store_root: &Path,
    occupied: &mut BTreeSet<(AgentTarget, String)>,
) -> Result<Vec<AgentSync>, Error> {
    let agents = discover_portable(root)?;
    let available: Vec<String> = agents.keys().cloned().collect();
    let names = select_names(&entry.pick, &available, &entry.source)?;
    let mut out = Vec::new();

    for name in names {
        let agent = &agents[&name];
        for target in &entry.targets {
            if !occupied.insert((*target, name.clone())) {
                return Err(Error::AgentDuplicate {
                    agent: name,
                    from: entry.source.clone(),
                    target: target.as_str().to_string(),
                });
            }
            let content = render_portable(agent, *target)?;
            let destination = project_root
                .join(target_dir(*target))
                .join(format!("{name}.{}", target_extension(*target)));
            let key = format!(
                "{}{commit}+portable-v1:{}:{}",
                ident::AGENT_FILE_MARKER_PREFIX,
                target.as_str(),
                agent.path.display()
            );
            link::materialize_file_contents(&destination, content.as_bytes(), &key, store_root)?;
            out.push(AgentSync {
                name: name.clone(),
                source: entry.source.clone(),
                commit: commit.to_string(),
                target: *target,
                path: destination,
            });
        }
    }
    Ok(out)
}

fn source_root(checkout: &Path, relative: Option<&str>, source: &str) -> Result<PathBuf, Error> {
    let Some(relative) = relative else {
        return Ok(checkout.to_path_buf());
    };
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::ManifestShape(format!(
            "agent source path '{relative}' for {source} must stay inside the checkout"
        )));
    }
    let root = checkout.join(path);
    if root.exists() {
        let canonical_checkout = std::fs::canonicalize(checkout)?;
        let canonical_root = std::fs::canonicalize(&root)?;
        if !canonical_root.starts_with(&canonical_checkout) {
            return Err(Error::ManifestShape(format!(
                "agent source path '{relative}' for {source} resolves outside the checkout"
            )));
        }
    }
    Ok(root)
}

fn discover_portable(root: &Path) -> Result<BTreeMap<String, PortableAgent>, Error> {
    let mut entries: Vec<_> = std::fs::read_dir(root)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut found = BTreeMap::new();
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if file_type.is_symlink() {
            return Err(invalid_agent(
                &path,
                "portable agent definitions must be regular files",
            ));
        }
        if !file_type.is_file() {
            continue;
        }
        let agent = parse_portable(&path)?;
        let name = agent.name.clone();
        if found.insert(name.clone(), agent).is_some() {
            return Err(invalid_agent(
                &path,
                format!("duplicate portable agent name '{name}'"),
            ));
        }
    }
    Ok(found)
}

fn discover_native(
    root: &Path,
    selected_targets: &[AgentTarget],
    source: &str,
) -> Result<NativeFiles, Error> {
    let mut files: NativeFiles = selected_targets
        .iter()
        .copied()
        .map(|target| (target, BTreeMap::new()))
        .collect();

    let mut entries: Vec<_> = std::fs::read_dir(root)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str());
        let Some(all_targets) = native_targets_for_extension(ext) else {
            continue;
        };
        let targets: Vec<_> = all_targets
            .iter()
            .filter(|target| selected_targets.contains(target))
            .copied()
            .collect();
        if targets.is_empty() {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(invalid_agent(
                &path,
                "native agent definitions must be regular files",
            ));
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid_agent(&path, "agent filename is not valid UTF-8"))?
            .to_string();
        validate_name(&name, &path)?;
        for target in targets {
            files
                .get_mut(&target)
                .unwrap()
                .insert(name.clone(), path.clone());
        }
    }

    // A harness-specific subdirectory overrides a shared definition of the
    // same name. This keeps native frontmatter exact for each host.
    for target in selected_targets {
        let specific = root.join(target.as_str());
        let specific_meta = match std::fs::symlink_metadata(&specific) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Error::Io(error)),
        };
        if specific_meta.file_type().is_symlink() {
            return Err(invalid_agent(
                &specific,
                "target-specific agent directories cannot be symlinks",
            ));
        }
        if !specific_meta.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&specific)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some(target_extension(*target)) {
                continue;
            }
            if !entry.file_type()?.is_file() {
                return Err(invalid_agent(
                    &path,
                    "native agent definitions must be regular files",
                ));
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| invalid_agent(&path, "agent filename is not valid UTF-8"))?
                .to_string();
            validate_name(&name, &path)?;
            files.get_mut(target).unwrap().insert(name, path);
        }
    }
    if native_names(&files).is_empty() {
        return Err(Error::AgentNotFound {
            agent: "any native definition".into(),
            from: source.to_string(),
            available: Vec::new(),
        });
    }
    Ok(files)
}

fn native_targets_for_extension(ext: Option<&str>) -> Option<&'static [AgentTarget]> {
    match ext {
        Some("md") => Some(&[AgentTarget::Claude, AgentTarget::Pi, AgentTarget::OpenCode]),
        Some("toml") => Some(&[AgentTarget::Codex]),
        _ => None,
    }
}

fn native_names(files: &NativeFiles) -> Vec<String> {
    files
        .values()
        .flat_map(|by_name| by_name.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn select_names(
    pick: &AgentPick,
    available: &[String],
    source: &str,
) -> Result<Vec<String>, Error> {
    match pick {
        AgentPick::All => Ok(available.to_vec()),
        AgentPick::Named(names) => {
            for name in names {
                if !available.contains(name) {
                    return Err(Error::AgentNotFound {
                        agent: name.clone(),
                        from: source.to_string(),
                        available: available.to_vec(),
                    });
                }
            }
            Ok(names.clone())
        }
    }
}

fn parse_portable(path: &Path) -> Result<PortableAgent, Error> {
    let text = std::fs::read_to_string(path)?;
    let (frontmatter, body) = split_frontmatter(&text)
        .ok_or_else(|| invalid_agent(path, "expected YAML frontmatter between --- fences"))?;
    let value: Value = serde_yaml_ng::from_str(frontmatter)
        .map_err(|error| invalid_agent(path, format!("invalid YAML frontmatter: {error}")))?;
    let Some(object) = value.as_object() else {
        return Err(invalid_agent(path, "frontmatter must be a mapping"));
    };
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "name" | "description" | "adapters" | "tools" | "model"
        ) {
            return Err(invalid_agent(
                path,
                format!("unknown portable frontmatter field '{key}'"),
            ));
        }
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_agent(path, "frontmatter requires a string 'name'"))?
        .to_string();
    validate_name(&name, path)?;
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            invalid_agent(
                path,
                "frontmatter requires a non-empty string 'description'",
            )
        })?
        .to_string();

    // Claude-native model and tool hints are accepted when importing plugin
    // agents, but omitted from every portable rendering; each target uses its
    // configured defaults. Target-specific overrides belong under adapters.
    if let Some(model) = object.get("model")
        && model.as_str().is_none_or(|value| value.trim().is_empty())
    {
        return Err(invalid_agent(
            path,
            "source 'model' hint must be a non-empty string",
        ));
    }
    if let Some(tools) = object.get("tools") {
        let valid = tools.as_str().is_some_and(|value| !value.trim().is_empty())
            || tools.as_array().is_some_and(|values| {
                !values.is_empty()
                    && values
                        .iter()
                        .all(|value| value.as_str().is_some_and(|name| !name.trim().is_empty()))
            });
        if !valid {
            return Err(invalid_agent(
                path,
                "source 'tools' hint must be a non-empty string or list of strings",
            ));
        }
    }

    let mut adapters = BTreeMap::new();
    if let Some(value) = object.get("adapters") {
        let Some(adapters_object) = value.as_object() else {
            return Err(invalid_agent(
                path,
                "'adapters' must map targets to field mappings",
            ));
        };
        for (target_name, value) in adapters_object {
            let Some(target) = AgentTarget::parse(target_name) else {
                return Err(invalid_agent(
                    path,
                    format!("unknown adapter target '{target_name}'"),
                ));
            };
            let Some(fields) = value.as_object() else {
                return Err(invalid_agent(
                    path,
                    format!("adapter '{target_name}' must be a mapping"),
                ));
            };
            validate_adapter(path, target, fields)?;
            adapters.insert(target, fields.clone());
        }
    }

    Ok(PortableAgent {
        path: path.to_path_buf(),
        name,
        description,
        body: body.to_string(),
        adapters,
    })
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first_end = text.find('\n').unwrap_or(text.len());
    if text[..first_end].trim_end_matches('\r') != "---" {
        return None;
    }
    let body_start = if first_end < text.len() {
        first_end + 1
    } else {
        first_end
    };
    let rest = &text[body_start..];
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']).trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn validate_adapter(
    path: &Path,
    target: AgentTarget,
    fields: &Map<String, Value>,
) -> Result<(), Error> {
    for (key, value) in fields {
        if key == "name" || key == "description" {
            return Err(invalid_agent(
                path,
                format!("adapter field '{key}' is controlled by the portable definition"),
            ));
        }
        if target == AgentTarget::Codex && key == "developer_instructions" {
            return Err(invalid_agent(
                path,
                "Codex 'developer_instructions' is generated from the Markdown body",
            ));
        }
        if target == AgentTarget::OpenCode && key == "mode" && value.as_str() != Some("subagent") {
            return Err(invalid_agent(
                path,
                "OpenCode agent mode must be 'subagent'",
            ));
        }
        if contains_null(value) {
            return Err(invalid_agent(
                path,
                format!("adapter field '{key}' cannot contain null values"),
            ));
        }
    }
    Ok(())
}

fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(values) => values.iter().any(contains_null),
        Value::Object(values) => values.values().any(contains_null),
        _ => false,
    }
}

fn render_portable(agent: &PortableAgent, target: AgentTarget) -> Result<String, Error> {
    let adapter = agent.adapters.get(&target);
    if target == AgentTarget::Codex {
        let mut fields = Map::new();
        fields.insert("name".into(), Value::String(agent.name.clone()));
        fields.insert(
            "description".into(),
            Value::String(agent.description.clone()),
        );
        fields.insert(
            "developer_instructions".into(),
            Value::String(agent.body.clone()),
        );
        if let Some(adapter) = adapter {
            fields.extend(adapter.clone());
        }
        return toml::to_string(&Value::Object(fields)).map_err(|error| Error::AgentInvalid {
            path: agent.path.clone(),
            reason: format!("cannot serialize Codex TOML: {error}"),
        });
    }

    let mut fields = Map::new();
    fields.insert("name".into(), Value::String(agent.name.clone()));
    fields.insert(
        "description".into(),
        Value::String(agent.description.clone()),
    );
    if let Some(adapter) = adapter {
        fields.extend(adapter.clone());
    }
    if target == AgentTarget::OpenCode && !fields.contains_key("mode") {
        fields.insert("mode".into(), Value::String("subagent".into()));
    }
    let yaml =
        serde_yaml_ng::to_string(&Value::Object(fields)).map_err(|error| Error::AgentInvalid {
            path: agent.path.clone(),
            reason: format!("cannot serialize {} frontmatter: {error}", target.as_str()),
        })?;
    let yaml = yaml
        .strip_prefix("---\n")
        .unwrap_or(&yaml)
        .trim_end_matches(['\r', '\n']);
    Ok(format!("---\n{yaml}\n---\n{}", agent.body))
}

fn validate_name(name: &str, path: &Path) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid_agent(
            path,
            "agent name must start with an ASCII letter or digit and contain only letters, digits, '-' or '_' (maximum 64 characters)",
        ));
    }
    Ok(())
}

/// Confirm every existing component of a harness directory is a real
/// directory, so materialization and pruning cannot traverse symlinks.
/// `false` means a path component is not created yet.
fn validate_target_dir(project_root: &Path, target: AgentTarget) -> Result<bool, Error> {
    let mut path = project_root.to_path_buf();
    for component in Path::new(target_dir(target)).components() {
        let Component::Normal(name) = component else {
            continue;
        };
        path.push(name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(Error::Io(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::LinkObstructed(path));
        }
    }
    Ok(true)
}

fn target_dir(target: AgentTarget) -> &'static str {
    match target {
        AgentTarget::Claude => ident::CLAUDE_AGENTS_DIR,
        AgentTarget::Codex => ident::CODEX_AGENTS_DIR,
        AgentTarget::Pi => ident::PI_AGENTS_DIR,
        AgentTarget::OpenCode => ident::OPENCODE_AGENTS_DIR,
    }
}

fn target_extension(target: AgentTarget) -> &'static str {
    match target {
        AgentTarget::Codex => "toml",
        AgentTarget::Claude | AgentTarget::Pi | AgentTarget::OpenCode => "md",
    }
}

fn invalid_agent(path: &Path, reason: impl Into<String>) -> Error {
    Error::AgentInvalid {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}
