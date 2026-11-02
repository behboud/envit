//! `envit sync` orchestration (PRD §8, §11):
//! resolve → fetch → checkout → link → lock → CONTEXT.md.
//!
//! Repos are processed in parallel with plain scoped threads (bounded; no
//! async runtime — we orchestrate a handful of blocking operations, not
//! thousands of sockets). Within each repo, gix parallelizes pack decoding
//! and checkout across cores on its own.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::link::LinkMode;
use crate::lockfile::{LockedRepo, Lockfile};
use crate::manifest::{AgentEntry, AgentFormat, AgentPick, AgentTarget, Manifest, RepoEntry};
use crate::store::Store;
use crate::{Error, git, ident, link, source};

/// Max repos in flight at once. Enough to hide network latency, few enough
/// to be polite to forges and the disk.
const MAX_PARALLEL: usize = 6;

/// Staleness window for `auto` repos (PRD §12a). A remote checked within
/// this window is not re-contacted by plain `sync`.
const REFRESH_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Fetched from the network and materialized.
    Fetched,
    /// Checkout already in the store; only linked.
    LinkedExisting,
    /// Link already correct; nothing done.
    UpToDate,
}

#[derive(Debug)]
pub struct RepoSync {
    pub name: String,
    pub commit: String,
    /// What the lock said before this run, if anything.
    pub prev_commit: Option<String>,
    pub action: Action,
    pub path: PathBuf,
    /// Present when this sync materialized a new checkout.
    pub stats: Option<crate::git::CheckoutStats>,
}

pub struct SyncReport {
    pub repos: Vec<RepoSync>,
    pub skills: Vec<crate::skills::SkillSync>,
    pub agents: Vec<crate::agents::AgentSync>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SyncOptions {
    /// Use the lockfile exactly; never resolve refs; error on drift.
    pub frozen: bool,
    /// Never touch the network; error if anything is missing locally.
    pub offline: bool,
    pub link_mode: LinkMode,
}

/// Synchronize one project against the store with default options.
pub fn sync(project_root: &Path, store: &Store) -> Result<SyncReport, Error> {
    sync_with(project_root, store, SyncOptions::default())
}

/// Synchronize one project against the store.
pub fn sync_with(
    project_root: &Path,
    store: &Store,
    opts: SyncOptions,
) -> Result<SyncReport, Error> {
    run(project_root, store, opts, None)
}

/// Re-resolve moving refs to their current remote heads, rewrite the lock,
/// re-link. `names` empty = all repos. SHA-pinned entries are naturally
/// unaffected (they resolve to themselves).
pub fn update(project_root: &Path, store: &Store, names: &[String]) -> Result<SyncReport, Error> {
    let m = Manifest::load(project_root)?;
    let repo_names = m.repo_names();
    let skill_sources: Vec<String> = m.skill_entries().iter().map(|e| e.source.clone()).collect();
    let agent_sources: Vec<String> = m
        .agent_entries()?
        .iter()
        .map(|e| e.source.clone())
        .collect();
    for n in names {
        if !repo_names.contains(n) && !skill_sources.contains(n) && !agent_sources.contains(n) {
            return Err(Error::NameNotFound(n.clone()));
        }
    }
    run(project_root, store, SyncOptions::default(), Some(names))
}

fn run(
    project_root: &Path,
    store: &Store,
    opts: SyncOptions,
    update_names: Option<&[String]>,
) -> Result<SyncReport, Error> {
    let manifest = Manifest::load(project_root)?;
    let mut agent_entries = manifest.agent_entries()?;
    let entries = manifest.entries();
    let mut lock = Lockfile::load(project_root)?.unwrap_or_default();

    // Job per repo; results collected under a mutex (tiny critical section).
    let results: Mutex<Vec<Result<(RepoSync, LockedRepo), Error>>> =
        Mutex::new(Vec::with_capacity(entries.len()));

    for chunk in entries.chunks(MAX_PARALLEL) {
        std::thread::scope(|scope| {
            for entry in chunk {
                let locked = lock.get(&entry.name).cloned();
                let force_update = !entry.frozen
                    && match update_names {
                        None => false,
                        Some([]) => true,
                        Some(names) => names.contains(&entry.name),
                    };
                let results = &results;
                scope.spawn(move || {
                    let outcome = sync_one(entry, locked, store, project_root, opts, force_update);
                    results.lock().unwrap().push(outcome);
                });
            }
        });
    }

    let mut repos = Vec::new();
    for r in results.into_inner().unwrap() {
        let (repo_sync, locked) = r?;
        lock.set(locked);
        repos.push(repo_sync);
    }
    repos.sort_by(|a, b| a.name.cmp(&b.name));

    lock.retain_named(&entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>());

    let (skills, included_agents) = sync_skills_section(
        &manifest,
        &mut lock,
        store,
        opts,
        project_root,
        update_names,
    )?;
    agent_entries.extend(included_agents);
    let agents = sync_agents_section(
        &agent_entries,
        &mut lock,
        store,
        opts,
        project_root,
        update_names,
    )?;

    // Prune only entries envit can identify as its own.
    prune_undeclared(project_root, store, &entries, &skills, Some(&agents))?;

    lock.save(project_root)?;
    write_agents_md(project_root, &entries, &repos)?;
    if manifest.agents_md_enabled() {
        crate::project::inject_agent_notes(project_root)?;
    }
    store.register_project(project_root)?;

    Ok(SyncReport {
        repos,
        skills,
        agents,
    })
}

/// Resolve + materialize every `skills` source; update the lock's skills
/// section. `link_base` is where .agents/.claude live: the project root, or
/// the user's home for global scope.
fn sync_skills_section(
    manifest: &Manifest,
    lock: &mut Lockfile,
    store: &Store,
    opts: SyncOptions,
    link_base: &Path,
    update_names: Option<&[String]>,
) -> Result<(Vec<crate::skills::SkillSync>, Vec<AgentEntry>), Error> {
    let mut skills = Vec::new();
    let mut included_agents = Vec::new();
    let mut included_roots = std::collections::BTreeSet::new();
    let skill_entries = manifest.skill_entries();
    for se in &skill_entries {
        let force = !se.frozen
            && match update_names {
                None => false,
                Some([]) => true,
                Some(names) => names.contains(&se.source),
            };
        let locked = lock.get_skill_source(&se.source).cloned();
        let (url, commit, checkout, tag) = resolve_source(
            &se.source,
            se.git_ref.as_deref(),
            locked.as_ref().filter(|_| !force),
            store,
            opts,
            "skills",
        )?;
        let materialized =
            crate::skills::materialize_source(se, &checkout, &commit, link_base, opts)?;
        let agent_dirs = if se.include_agents {
            crate::skills::companion_agent_dirs(&checkout, &materialized)
        } else {
            Vec::new()
        };
        skills.extend(materialized);
        let git_ref = se.git_ref.clone().unwrap_or_else(|| "HEAD".to_string());
        lock.set_skill_source(LockedRepo {
            name: se.source.clone(),
            source: url.clone(),
            git_ref: git_ref.clone(),
            commit: commit.clone(),
            tag,
        });
        if se.include_agents {
            for path in agent_dirs {
                let Some(path_text) = path.to_str() else {
                    return Err(Error::ManifestShape(format!(
                        "agent directory in skill source '{}' is not valid UTF-8",
                        se.source
                    )));
                };
                if !included_roots.insert((se.source.clone(), path_text.to_string())) {
                    continue;
                }
                lock.set_agent_source(LockedRepo {
                    name: se.source.clone(),
                    source: url.clone(),
                    git_ref: git_ref.clone(),
                    commit: commit.clone(),
                    tag,
                });
                included_agents.push(AgentEntry {
                    source: se.source.clone(),
                    pick: AgentPick::All,
                    format: AgentFormat::Portable,
                    targets: AgentTarget::all().to_vec(),
                    git_ref: se.git_ref.clone(),
                    path: Some(path_text.to_string()),
                    // The skill resolver has already applied this update and
                    // seeded the matching agent lock for the same checkout.
                    frozen: true,
                });
            }
        }
    }
    lock.retain_skill_sources(
        &skill_entries
            .iter()
            .map(|e| e.source.clone())
            .collect::<Vec<_>>(),
    );
    Ok((skills, included_agents))
}

fn sync_agents_section(
    entries: &[AgentEntry],
    lock: &mut Lockfile,
    store: &Store,
    opts: SyncOptions,
    project_root: &Path,
    update_names: Option<&[String]>,
) -> Result<Vec<crate::agents::AgentSync>, Error> {
    let mut agents = Vec::new();
    let mut occupied = std::collections::BTreeSet::new();
    for entry in entries {
        let force = !entry.frozen
            && match update_names {
                None => false,
                Some([]) => true,
                Some(names) => names.contains(&entry.source),
            };
        let locked = lock.get_agent_source(&entry.source).cloned();
        let (url, commit, checkout, tag) = resolve_source(
            &entry.source,
            entry.git_ref.as_deref(),
            locked.as_ref().filter(|_| !force),
            store,
            opts,
            "agents",
        )?;
        agents.extend(crate::agents::materialize_source(
            entry,
            &checkout,
            &commit,
            project_root,
            store.root(),
            opts.link_mode,
            &mut occupied,
        )?);
        lock.set_agent_source(LockedRepo {
            name: entry.source.clone(),
            source: url,
            git_ref: entry.git_ref.clone().unwrap_or_else(|| "HEAD".to_string()),
            commit,
            tag,
        });
    }
    lock.retain_agent_sources(
        &entries
            .iter()
            .map(|entry| entry.source.clone())
            .collect::<Vec<_>>(),
    );
    Ok(agents)
}

/// Resolve a source repo shared by skills and agents through the same locked
/// checkout path and single-flight fetch.
fn resolve_source(
    source_name: &str,
    git_ref: Option<&str>,
    locked: Option<&LockedRepo>,
    store: &Store,
    opts: SyncOptions,
    section: &str,
) -> Result<(String, String, PathBuf, bool), Error> {
    let url = source::expand(source_name)?;
    let wanted = git_ref.unwrap_or("HEAD");
    let reusable = locked
        .filter(|entry| entry.source == url && entry.git_ref == wanted)
        .map(|entry| (entry.commit.clone(), entry.tag));

    if opts.frozen && reusable.is_none() {
        return Err(Error::FrozenDrift(format!("{section}:{source_name}")));
    }
    if let Some((commit, tag)) = &reusable {
        let checkout = store.checkout_dir(&url, commit, &[])?;
        if checkout.is_dir() {
            return Ok((url, commit.clone(), checkout, *tag));
        }
    }

    let _lock = store.lock_remote(&url)?;
    if let Some((commit, tag)) = &reusable {
        let checkout = store.checkout_dir(&url, commit, &[])?;
        if checkout.is_dir() {
            return Ok((url, commit.clone(), checkout, *tag));
        }
    }
    if opts.offline {
        return Err(Error::OfflineMiss(format!("{section}:{source_name}")));
    }
    let bare = git::ensure_bare(&store.bare_dir(&url)?)?;
    let (commit, tag) = match &reusable {
        Some((commit, tag)) => {
            let id = gix::ObjectId::from_hex(commit.as_bytes())
                .map_err(|error| Error::Git(error.to_string()))?;
            if bare.find_object(id).is_err() {
                // A retained lock is immutable even when the checkout and
                // bare objects are missing. Only an explicit update drops
                // `reusable` and resolves the moving ref below.
                let fetched = git::fetch_ref(&bare, &url, commit, false)?;
                store.mark_remote_checked(&url)?;
                (fetched.commit, fetched.tag || *tag)
            } else {
                (commit.clone(), *tag)
            }
        }
        None => {
            let fetched = git::fetch_ref(&bare, &url, wanted, false)?;
            store.mark_remote_checked(&url)?;
            (fetched.commit, fetched.tag)
        }
    };
    let checkout = store.checkout_dir(&url, &commit, &[])?;
    if !checkout.is_dir() {
        materialize_checkout(&bare, &commit, &checkout)?;
    }
    Ok((url, commit, checkout, tag))
}

/// Sync the global manifest (`~/.envit/envit.json`): skills only, linked
/// under the user's home (`~/.agents/skills` + `~/.claude/skills`).
pub fn sync_global(store: &Store, opts: SyncOptions) -> Result<SyncReport, Error> {
    sync_global_with(store, opts, None)
}

/// Global update: re-resolve global skill sources (empty names = all).
pub fn update_global(store: &Store, names: &[String]) -> Result<SyncReport, Error> {
    sync_global_with(store, SyncOptions::default(), Some(names))
}

fn sync_global_with(
    store: &Store,
    opts: SyncOptions,
    update_names: Option<&[String]>,
) -> Result<SyncReport, Error> {
    let root = store.root().to_path_buf();
    if !root.join(ident::MANIFEST_FILE).is_file() {
        return Err(Error::NoGlobalManifest(root.join(ident::MANIFEST_FILE)));
    }
    let manifest = Manifest::load(&root)?;
    if !manifest.entries().is_empty() {
        return Err(Error::GlobalRepos(root.join(ident::MANIFEST_FILE)));
    }
    if !manifest.agent_entries()?.is_empty()
        || manifest
            .skill_entries()
            .iter()
            .any(|entry| entry.include_agents)
    {
        return Err(Error::GlobalAgents(root.join(ident::MANIFEST_FILE)));
    }
    let home = crate::store::home_dir()
        .ok_or_else(|| Error::Io(std::io::Error::other("cannot determine home directory")))?;
    let mut lock = Lockfile::load(&root)?.unwrap_or_default();
    let (skills, included_agents) =
        sync_skills_section(&manifest, &mut lock, store, opts, &home, update_names)?;
    if !included_agents.is_empty() {
        return Err(Error::GlobalAgents(root.join(ident::MANIFEST_FILE)));
    }
    prune_undeclared(&home, store, &[], &skills, None)?;
    lock.save(&root)?;
    Ok(SyncReport {
        repos: Vec::new(),
        skills,
        agents: Vec::new(),
    })
}

/// Does a global manifest exist? (Used by the CLI to decide whether plain
/// `sync` also materializes the global scope.)
pub fn global_manifest_exists(store: &Store) -> bool {
    store.root().join(ident::MANIFEST_FILE).is_file()
}

fn prune_undeclared(
    project_root: &Path,
    store: &Store,
    entries: &[RepoEntry],
    skills: &[crate::skills::SkillSync],
    agents: Option<&[crate::agents::AgentSync]>,
) -> Result<(), Error> {
    // Repos: .envit/repos is wholly envit-managed.
    let repo_dir = project_root.join(ident::CONTEXT_DIR).join("repos");
    let declared: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    prune_dir(&repo_dir, |name, _target| !declared.contains(&name))?;

    // Skills: ours when a symlink points into the store, or when a marker
    // file records a managed (patched or copy-mode) directory.
    let managed: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    let store_root = store.root().to_path_buf();
    let canonical = project_root.join(crate::skills::CANONICAL_DIR);
    prune_dir(&canonical, |name, target| {
        target.is_none_or(|t| t.starts_with(&store_root)) && !managed.contains(&name)
    })?;
    // Claude fan-out: ours only if it points at our canonical dir.
    let claude = project_root.join(crate::skills::CLAUDE_DIR);
    prune_dir(&claude, |name, target| {
        target.is_none_or(|t| t.starts_with(&canonical)) && !managed.contains(&name)
    })?;
    if let Some(agents) = agents {
        crate::agents::prune(project_root, &store_root, agents)?;
    }
    Ok(())
}

/// Remove managed entries in `dir` for which `should_remove` holds.
/// A symlink passes its target (`Some`); a directory with a sibling
/// `<name>.commit` marker is a managed copy and passes `None`. Anything
/// else is the user's and is never touched.
fn prune_dir(dir: &Path, should_remove: impl Fn(&str, Option<&Path>) -> bool) -> Result<(), Error> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".commit") || name.ends_with(".fresh") {
            continue;
        }
        if let Ok(target) = std::fs::read_link(&path) {
            if should_remove(&name, Some(&target)) {
                std::fs::remove_file(&path)?;
            }
        } else if path.is_dir() {
            let marker = dir.join(format!("{name}.commit"));
            if marker.exists() && should_remove(&name, None) {
                std::fs::remove_dir_all(&path)?;
                let _ = std::fs::remove_file(&marker);
                let _ = std::fs::remove_file(dir.join(format!("{name}.fresh")));
            }
        }
    }
    // Leave empty dirs in place for repos (.envit is ours); tidy the
    // shared skills dirs so we don't leave husks in .agents/.claude.
    if dir.ends_with("skills") {
        let _ = std::fs::remove_dir(dir); // fails harmlessly if non-empty
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(())
}

fn sync_one(
    entry: &RepoEntry,
    locked: Option<LockedRepo>,
    store: &Store,
    project_root: &Path,
    opts: SyncOptions,
    force_update: bool,
) -> Result<(RepoSync, LockedRepo), Error> {
    if !entry.sparse.is_empty() {
        return Err(Error::Git(format!(
            "repo '{}': sparse checkouts are not supported yet (planned for M3)",
            entry.name
        )));
    }
    let url = source::expand(&entry.source)?;
    let wanted = entry.git_ref.as_deref().unwrap_or("HEAD");
    let prev_commit = locked.as_ref().map(|l| l.commit.clone());

    // TTL freshness (PRD §12a): an `auto` repo (branch-tracking default)
    // whose remote hasn't been checked within the TTL re-resolves on plain
    // sync. Killed by: pin/frozen, manual, --frozen, --offline, sha refs,
    // and ENVIT_NO_REFRESH=1.
    let is_sha_ref = wanted.len() == 40 && wanted.bytes().all(|b| b.is_ascii_hexdigit());
    let no_refresh_env = std::env::var_os(ident::NO_REFRESH_ENV).is_some_and(|v| !v.is_empty());
    let ttl = entry
        .ttl
        .or_else(|| {
            std::env::var(ident::REFRESH_TTL_ENV)
                .ok()
                .and_then(|v| crate::manifest::parse_ttl(&v))
        })
        .unwrap_or(REFRESH_TTL);
    // A ref that resolved via refs/tags/* never goes stale — tags don't
    // move (PRD §12a: tag/sha refs default frozen).
    let locked_tag = locked.as_ref().is_some_and(|l| l.tag);
    let auto_stale = !entry.frozen
        && !entry.manual
        && !is_sha_ref
        && !locked_tag
        && !opts.frozen
        && !opts.offline
        && !no_refresh_env
        && store.remote_check_age(&url).is_none_or(|age| age > ttl);

    // Branches do not move on plain sync (PRD §10) unless stale-`auto`:
    // reuse the locked commit when the manifest entry is unchanged.
    // `update` forces re-resolution.
    let reusable = if force_update || auto_stale {
        None
    } else {
        locked
            .as_ref()
            .filter(|l| l.source == url && l.git_ref == wanted)
            .map(|l| l.commit.clone())
    };

    // --frozen: the lock is the only source of truth. No entry or a
    // changed manifest entry = drift = hard error (PRD §8).
    if opts.frozen && reusable.is_none() {
        return Err(Error::FrozenDrift(entry.name.clone()));
    }

    let known_commit = reusable.or(is_sha_ref.then(|| wanted.to_string()));

    // Fast path: checkout already materialized in the store.
    if let Some(commit) = &known_commit {
        let checkout = store.checkout_dir(&url, commit, &[])?;
        if checkout.is_dir() {
            let link_path = project_root
                .join(ident::CONTEXT_DIR)
                .join("repos")
                .join(&entry.name);
            let outcome = link::materialize_link(&link_path, &checkout, opts.link_mode, commit)?;
            let action = if outcome == link::LinkOutcome::Unchanged {
                Action::UpToDate
            } else {
                Action::LinkedExisting
            };
            return Ok((
                RepoSync {
                    name: entry.name.clone(),
                    commit: commit.clone(),
                    prev_commit,
                    action,
                    path: checkout,
                    stats: None,
                },
                LockedRepo {
                    name: entry.name.clone(),
                    source: url,
                    git_ref: wanted.to_string(),
                    commit: commit.clone(),
                    tag: locked_tag,
                },
            ));
        }
    }

    // Slow path: single-flight per remote (PRD §12a) — concurrent syncs
    // needing this remote block here; the winner fetches once.
    let _remote_lock = store.lock_remote(&url)?;

    // Re-check after acquiring: the lock holder before us may have
    // materialized exactly what we need.
    if let Some(commit) = &known_commit {
        let checkout = store.checkout_dir(&url, commit, &[])?;
        if checkout.is_dir() {
            let link_path = project_root
                .join(ident::CONTEXT_DIR)
                .join("repos")
                .join(&entry.name);
            link::materialize_link(&link_path, &checkout, opts.link_mode, commit)?;
            return Ok((
                RepoSync {
                    name: entry.name.clone(),
                    commit: commit.clone(),
                    prev_commit,
                    action: Action::LinkedExisting,
                    path: checkout,
                    stats: None,
                },
                LockedRepo {
                    name: entry.name.clone(),
                    source: url,
                    git_ref: wanted.to_string(),
                    commit: commit.clone(),
                    tag: locked_tag,
                },
            ));
        }
    }

    let bare_path = store.bare_dir(&url)?;
    let bare = git::ensure_bare(&bare_path)?;
    let mut resolved_tag = locked_tag;
    let commit = match &known_commit {
        Some(c) => {
            // Commit known but checkout missing; objects may be missing too.
            let id =
                gix::ObjectId::from_hex(c.as_bytes()).map_err(|e| Error::Git(e.to_string()))?;
            if bare.find_object(id).is_err() {
                if opts.offline {
                    return Err(Error::OfflineMiss(entry.name.clone()));
                }
                // A retained lock fetches by exact SHA. Only a fresh ref
                // resolution below follows the moving manifest ref.
                let r = git::fetch_ref(&bare, &url, c, entry.history_full)?;
                store.mark_remote_checked(&url)?;
                r.commit
            } else {
                c.clone()
            }
        }
        None => {
            if opts.offline {
                return Err(Error::OfflineMiss(entry.name.clone()));
            }
            let r = git::fetch_ref(&bare, &url, wanted, entry.history_full)?;
            store.mark_remote_checked(&url)?;
            resolved_tag = r.tag;
            r.commit
        }
    };

    let checkout = store.checkout_dir(&url, &commit, &[])?;
    let stats = if !checkout.is_dir() {
        materialize(&bare, &commit, &checkout)?
    } else {
        None
    };

    let link_path = project_root
        .join(ident::CONTEXT_DIR)
        .join("repos")
        .join(&entry.name);
    link::materialize_link(&link_path, &checkout, opts.link_mode, &commit)?;

    Ok((
        RepoSync {
            name: entry.name.clone(),
            commit: commit.clone(),
            prev_commit,
            action: Action::Fetched,
            path: checkout,
            stats,
        },
        LockedRepo {
            name: entry.name.clone(),
            source: url,
            git_ref: wanted.to_string(),
            commit,
            tag: resolved_tag,
        },
    ))
}

/// Extract to a temp sibling, strip write bits, then atomically rename into
/// place — a half-built checkout is never observable (PRD §11).
pub(crate) fn materialize_checkout(
    bare: &gix::Repository,
    commit: &str,
    checkout: &Path,
) -> Result<(), Error> {
    materialize(bare, commit, checkout).map(|_| ())
}

fn materialize(
    bare: &gix::Repository,
    commit: &str,
    checkout: &Path,
) -> Result<Option<crate::git::CheckoutStats>, Error> {
    let parent = checkout.parent().expect("checkout dirs have parents");
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".tmp-{}-{}",
        commit,
        std::process::id() // unique enough per concurrent process
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    let stats = git::extract_checkout(bare, commit, &tmp)?;
    make_files_readonly(&tmp)?;
    match std::fs::rename(&tmp, checkout) {
        Ok(()) => Ok(Some(stats)),
        Err(_) if checkout.is_dir() => {
            // Another process won the race; ours is redundant.
            let _ = std::fs::remove_dir_all(&tmp);
            Ok(None)
        }
        Err(e) => Err(Error::Io(e)),
    }
}

/// Strip write bits from files (dirs stay writable so `gc` can remove
/// trees without a chmod pass). Agents editing context fail loudly.
fn make_files_readonly(dir: &Path) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            make_files_readonly(&path)?;
        } else if ft.is_file() {
            let mut perms = entry.metadata()?.permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&path, perms)?;
        }
    }
    Ok(())
}

/// Write the generated context inventory as `.envit/AGENTS.md`, with a
/// `CLAUDE.md` symlink beside it. Agents read these filenames natively
/// when they enter the directory, so the inventory is discovered without
/// configuration.
fn write_agents_md(
    project_root: &Path,
    entries: &[RepoEntry],
    repos: &[RepoSync],
) -> Result<(), Error> {
    let dir = project_root.join(ident::CONTEXT_DIR);
    std::fs::create_dir_all(&dir)?;
    let mut md = String::from(
        "# Repo context\n\n\
         Generated by `envit sync`. Do not edit. Each entry below is the full\n\
         source of a dependency, read-only, at an exact commit.\n\n\
         > Search note: these are symlinks. `rg`/`fd` need `--follow`/`-L`,\n\
         > or search the resolved path directly.\n\n",
    );
    let _ = entries;
    for r in repos {
        md.push_str(&format!(
            "## {}\n- commit: `{}`\n- path: `{}`\n\n",
            r.name,
            r.commit,
            r.path.display()
        ));
    }
    std::fs::write(dir.join("AGENTS.md"), md)?;

    let claude = dir.join("CLAUDE.md");
    #[cfg(unix)]
    {
        if std::fs::symlink_metadata(&claude).is_ok() {
            std::fs::remove_file(&claude)?;
        }
        std::os::unix::fs::symlink("AGENTS.md", &claude)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&claude, "See AGENTS.md in this directory.\n")?;
    }
    Ok(())
}
