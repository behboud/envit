//! e2e: the minimum-age gate. A skill or agent source adopts a new commit
//! from a moving ref only when its committer date is old enough; a fresh
//! tip falls back to the newest old-enough first-parent ancestor.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

const OLD: &str = "2020-01-01T00:00:00Z";

fn envit(dir: &Path, store: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_envit"));
    c.args(args)
        .current_dir(dir)
        .env("ENVIT_HOME", store)
        .env_remove("ENVIT_MIN_AGE");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("binary runs")
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run git; `date` backdates author and committer, `None` means now.
fn git(dir: &Path, args: &[&str], date: Option<&str>) -> String {
    let mut c = Command::new("git");
    c.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false");
    if let Some(d) = date {
        c.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
    }
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Write `files` (path, content), commit them at `date`, return the sha.
fn commit(dir: &Path, files: &[(&str, &str)], date: Option<&str>) -> String {
    if !dir.join(".git").exists() {
        git(dir, &["init", "-b", "main", "."], None);
    }
    for (path, content) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    git(dir, &["add", "."], None);
    git(dir, &["commit", "-m", "change"], date);
    git(dir, &["rev-parse", "HEAD"], None)
}

fn skill_md(body: &str) -> String {
    format!("---\nname: x\ndescription: test skill\n---\n{body}\n")
}

fn agent_md(body: &str) -> String {
    format!("---\nname: reviewer\ndescription: Reviews changes.\n---\n{body}\n")
}

fn write_manifest(project: &Path, manifest: Value) {
    std::fs::write(
        project.join("envit.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn skill_manifest(project: &Path, source: &str, git_ref: Option<&str>) {
    let mut entry = serde_json::json!({ "pick": ["x"] });
    if let Some(r) = git_ref {
        entry["ref"] = r.into();
    }
    write_manifest(project, serde_json::json!({ "skills": { (source): entry } }));
}

fn lock(project: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(project.join("envit.lock.json")).unwrap()).unwrap()
}

fn skill_body(project: &Path) -> String {
    std::fs::read_to_string(project.join(".agents/skills/x/SKILL.md")).unwrap()
}

fn url(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

#[test]
fn first_sync_skips_a_fresh_tip() {
    let upstream = tempfile::tempdir().unwrap();
    let a = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], a);
    assert!(skill_body(project.path()).contains("A"), "{out}");
    assert!(
        out.contains(&format!("held     skills:{}", url(upstream.path())))
            && out.contains(&format!("tip {}", &b[..8]))
            && out.contains(&format!("→ {}", &a[..8])),
        "{out}"
    );
}

#[test]
fn update_holds_back_to_the_newest_old_commit_and_says_so() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);
    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    assert!(!out.contains("held"), "an old tip is adopted silently: {out}");

    let a2 = commit(
        upstream.path(),
        &[("skills/x/SKILL.md", &skill_md("A2"))],
        Some("2020-01-02T00:00:00Z"),
    );
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("C"))], None);

    let out = ok(&envit(project.path(), store.path(), &["update"], &[]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], a2);
    assert!(skill_body(project.path()).contains("A2"));
    assert!(out.contains("held     skills:") && out.contains("< minAge 24h"), "{out}");
}

#[test]
fn env_bypass_adopts_a_fresh_tip() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[("ENVIT_MIN_AGE", "0s")]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], b);
    assert!(skill_body(project.path()).contains("B"));
    assert!(!out.contains("held"), "{out}");
}

#[test]
fn sha_ref_skips_the_gate() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), Some(&b));

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], b);
    assert!(!out.contains("held"), "{out}");
}

#[test]
fn agent_sources_are_gated_too() {
    let upstream = tempfile::tempdir().unwrap();
    let a = commit(upstream.path(), &[("agents/reviewer.md", &agent_md("A"))], Some(OLD));
    commit(upstream.path(), &[("agents/reviewer.md", &agent_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write_manifest(
        project.path(),
        serde_json::json!({ "agents": { (url(upstream.path())): {
            "path": "agents", "pick": ["reviewer"],
            "format": "envit-agent/v1", "targets": ["claude"]
        } } }),
    );

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    assert_eq!(lock(project.path())["agentSources"][0]["commit"], a);
    let claude = std::fs::read_to_string(project.path().join(".claude/agents/reviewer.md")).unwrap();
    assert!(claude.contains("A") && !claude.contains("B"), "{claude}");
    assert!(out.contains("held     agents:"), "{out}");
}

#[test]
fn include_agents_follow_the_gated_skill_commit() {
    let upstream = tempfile::tempdir().unwrap();
    let a = commit(
        upstream.path(),
        &[
            ("plugins/p/skills/x/SKILL.md", &skill_md("A")),
            ("plugins/p/agents/reviewer.md", &agent_md("A")),
        ],
        Some(OLD),
    );
    commit(
        upstream.path(),
        &[
            ("plugins/p/skills/x/SKILL.md", &skill_md("B")),
            ("plugins/p/agents/reviewer.md", &agent_md("B")),
        ],
        None,
    );
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write_manifest(
        project.path(),
        serde_json::json!({ "skills": { (url(upstream.path())): {
            "pick": ["x"], "includeAgents": true
        } } }),
    );

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    let lock = lock(project.path());
    assert_eq!(lock["skillSources"][0]["commit"], a);
    assert_eq!(lock["agentSources"][0]["commit"], a);
    assert_eq!(out.matches("held").count(), 1, "the skill decides once: {out}");
}

#[test]
fn plain_sync_never_regates_a_locked_commit() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);
    ok(&envit(project.path(), store.path(), &["sync"], &[("ENVIT_MIN_AGE", "0s")]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], b);

    // The remote disappears: a plain sync must not need it.
    let gone = upstream.path().with_extension("gone");
    std::fs::rename(upstream.path(), &gone).unwrap();
    let out = envit(project.path(), store.path(), &["sync"], &[]);
    std::fs::rename(&gone, upstream.path()).unwrap();
    let out = ok(&out);
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], b);
    assert!(skill_body(project.path()).contains("B"));
    assert!(!out.contains("held"), "{out}");
}

fn fails(out: &Output) -> String {
    assert!(
        !out.status.success(),
        "expected failure, stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn first_add_with_only_fresh_commits_fails_naming_the_bypass() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);

    let err = fails(&envit(project.path(), store.path(), &["sync"], &[]));
    assert!(err.contains("ENVIT_MIN_AGE=0s") && err.contains("older than 24h"), "{err}");
    assert!(!project.path().join("envit.lock.json").exists());
}

#[test]
fn update_with_nothing_old_enough_keeps_the_lock() {
    let upstream = tempfile::tempdir().unwrap();
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);
    ok(&envit(project.path(), store.path(), &["sync"], &[("ENVIT_MIN_AGE", "0s")]));
    let c = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("C"))], None);

    let out = ok(&envit(project.path(), store.path(), &["update"], &[]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], b);
    assert!(skill_body(project.path()).contains("B"));
    assert!(
        out.contains(&format!("tip {}", &c[..8])) && out.contains(&format!("→ kept {} (lock)", &b[..8])),
        "{out}"
    );
}

#[test]
fn update_keeps_the_lock_when_upstream_history_was_rewritten() {
    let upstream = tempfile::tempdir().unwrap();
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);
    ok(&envit(project.path(), store.path(), &["sync"], &[("ENVIT_MIN_AGE", "0s")]));
    // Force-push shape: the new tip no longer descends from the lock.
    std::fs::write(upstream.path().join("skills/x/SKILL.md"), skill_md("C")).unwrap();
    git(upstream.path(), &["commit", "-am", "rewrite", "--amend"], None);

    let out = ok(&envit(project.path(), store.path(), &["update"], &[]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], b);
    assert!(out.contains(&format!("→ kept {} (lock)", &b[..8])), "{out}");
}

#[test]
fn update_never_moves_backward_past_the_lock() {
    let upstream = tempfile::tempdir().unwrap();
    let a = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), None);
    ok(&envit(project.path(), store.path(), &["sync"], &[("ENVIT_MIN_AGE", "0s")]));
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("C"))], None);

    let out = ok(&envit(project.path(), store.path(), &["update"], &[]));
    let locked = lock(project.path())["skillSources"][0]["commit"].clone();
    assert_eq!(locked, b);
    assert_ne!(locked, a);
    assert!(skill_body(project.path()).contains("B"));
    assert!(out.contains(&format!("→ kept {} (lock)", &b[..8])), "{out}");
}

#[test]
fn too_new_tag_fails_without_falling_back() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    git(upstream.path(), &["tag", "v2"], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), Some("v2"));

    let err = fails(&envit(project.path(), store.path(), &["sync"], &[]));
    assert!(err.contains("ENVIT_MIN_AGE=0s") && err.contains("'v2'"), "{err}");
    assert!(!project.path().join("envit.lock.json").exists());
}

#[test]
fn old_enough_tag_is_adopted() {
    let upstream = tempfile::tempdir().unwrap();
    let a = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    git(upstream.path(), &["tag", "v1"], None);
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill_manifest(project.path(), &url(upstream.path()), Some("v1"));

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], a);
    assert!(skill_body(project.path()).contains("A"));
    assert!(!out.contains("held"), "{out}");
}

/// A git date `hours` before now.
fn hours_ago(hours: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("@{} +0000", now - hours * 3600)
}

#[test]
fn per_source_min_age_zero_adopts_a_fresh_tip() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write_manifest(
        project.path(),
        serde_json::json!({ "minAge": "7d", "skills": { (url(upstream.path())): {
            "pick": ["x"], "minAge": "0s"
        } } }),
    );

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], b);
    assert!(skill_body(project.path()).contains("B"));
    assert!(!out.contains("held") && !out.contains("note"), "{out}");
}

#[test]
fn top_level_min_age_sets_the_default_for_skills_and_agents() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(
        upstream.path(),
        &[
            ("skills/x/SKILL.md", &skill_md("B")),
            ("agents/reviewer.md", &agent_md("B")),
        ],
        Some(&hours_ago(2)),
    );
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let source = url(upstream.path());
    write_manifest(
        project.path(),
        serde_json::json!({
            "minAge": "1h",
            "skills": { (source.clone()): { "pick": ["x"] } },
            "agents": { (source): {
                "path": "agents", "pick": ["reviewer"],
                "format": "envit-agent/v1", "targets": ["claude"]
            } }
        }),
    );

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[]));
    let lock = lock(project.path());
    assert_eq!(lock["skillSources"][0]["commit"], b, "2h old passes 1h: {out}");
    assert_eq!(lock["agentSources"][0]["commit"], b);
    assert!(!out.contains("held"), "{out}");
}

#[test]
fn env_beats_per_source_min_age_and_says_so() {
    let upstream = tempfile::tempdir().unwrap();
    let a = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write_manifest(
        project.path(),
        serde_json::json!({ "skills": { (url(upstream.path())): {
            "pick": ["x"], "minAge": "0s"
        } } }),
    );

    let out = ok(&envit(project.path(), store.path(), &["sync"], &[("ENVIT_MIN_AGE", "12h")]));
    assert_eq!(lock(project.path())["skillSources"][0]["commit"], a);
    assert!(out.contains("< minAge 12h"), "{out}");
    assert!(out.contains("note     ENVIT_MIN_AGE=12h overrides minAge"), "{out}");
}

#[test]
fn status_shows_the_env_override() {
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write_manifest(project.path(), serde_json::json!({ "repos": [] }));

    let out = ok(&envit(project.path(), store.path(), &["status"], &[("ENVIT_MIN_AGE", "0s")]));
    assert!(out.contains("note     ENVIT_MIN_AGE=0s overrides minAge"), "{out}");
    let out = ok(&envit(project.path(), store.path(), &["status"], &[]));
    assert!(!out.contains("ENVIT_MIN_AGE"), "{out}");
}

#[test]
fn bad_manifest_min_age_fails_instead_of_weakening_the_gate() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let (project, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write_manifest(
        project.path(),
        serde_json::json!({ "skills": { (url(upstream.path())): {
            "pick": ["x"], "minAge": "1 day"
        } } }),
    );

    let err = fails(&envit(project.path(), store.path(), &["sync"], &[]));
    assert!(err.contains("minAge") && err.contains("\"1 day\""), "{err}");
    assert!(!project.path().join("envit.lock.json").exists());
}

#[test]
fn global_manifest_min_age_applies_to_global_skills() {
    let upstream = tempfile::tempdir().unwrap();
    commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("A"))], Some(OLD));
    let b = commit(upstream.path(), &[("skills/x/SKILL.md", &skill_md("B"))], None);
    let (home, nowhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let store = home.path().join(".envit");
    std::fs::create_dir_all(&store).unwrap();
    write_manifest(
        &store,
        serde_json::json!({ "minAge": "0s", "skills": { (url(upstream.path())): ["x"] } }),
    );
    let home_env = [("HOME", home.path().to_str().unwrap())];

    let out = ok(&envit(nowhere.path(), &store, &["sync", "-g"], &home_env));
    assert_eq!(lock(&store)["skillSources"][0]["commit"], b, "{out}");
    let body = std::fs::read_to_string(home.path().join(".agents/skills/x/SKILL.md")).unwrap();
    assert!(body.contains("B"), "{body}");
}
