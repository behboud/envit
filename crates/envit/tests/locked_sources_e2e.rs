//! e2e: a missing local store must rehydrate the locked commit, not follow
//! a moving remote ref. Real projects, local Git repositories, and files.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

#[derive(Clone, Copy)]
enum SourceKind {
    Repo,
    Skill,
    Agent,
}

fn envit(dir: &Path, store: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_envit"))
        .args(args)
        .current_dir(dir)
        .env("ENVIT_HOME", store)
        .output()
        .expect("binary runs")
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn run_case(kind: SourceKind) {
    let upstream = tempfile::tempdir().unwrap();
    git(upstream.path(), &["init", "-b", "main", "."]);
    match kind {
        SourceKind::Repo => std::fs::write(upstream.path().join("lib.rs"), "v1\n").unwrap(),
        SourceKind::Skill => {
            let skill = upstream.path().join("skills/alpha");
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                "---\nname: alpha\ndescription: test\n---\nv1\n",
            )
            .unwrap();
        }
        SourceKind::Agent => {
            let agents = upstream.path().join("agents");
            std::fs::create_dir_all(&agents).unwrap();
            std::fs::write(agents.join("reviewer.md"), "native agent v1\n").unwrap();
        }
    }
    git(upstream.path(), &["add", "."]);
    git(upstream.path(), &["commit", "-m", "version one"]);
    let first = git(upstream.path(), &["rev-parse", "HEAD"]);
    let source = format!("file://{}", upstream.path().display());

    let project = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let manifest = match kind {
        SourceKind::Repo => serde_json::json!({
            "repos": [{ "source": source.clone(), "name": "fixture" }]
        }),
        SourceKind::Skill => serde_json::json!({
            "skills": { (source.clone()): "alpha" }
        }),
        SourceKind::Agent => serde_json::json!({
            "agents": {
                (source.clone()): {
                    "path": "agents",
                    "pick": ["reviewer"],
                    "format": "native",
                    "targets": ["claude"]
                }
            }
        }),
    };
    std::fs::write(
        project.path().join("envit.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    ok(&envit(project.path(), store.path(), &["sync"]));

    match kind {
        SourceKind::Repo => std::fs::write(upstream.path().join("lib.rs"), "v2\n").unwrap(),
        SourceKind::Skill => std::fs::write(
            upstream.path().join("skills/alpha/SKILL.md"),
            "---\nname: alpha\ndescription: test\n---\nv2\n",
        )
        .unwrap(),
        SourceKind::Agent => std::fs::write(
            upstream.path().join("agents/reviewer.md"),
            "native agent v2\n",
        )
        .unwrap(),
    }
    git(upstream.path(), &["add", "."]);
    git(upstream.path(), &["commit", "-m", "version two"]);

    // Keep the lock and freshness metadata, but make the entire content store
    // cold. Plain sync must fetch the locked SHA even though HEAD has moved.
    std::fs::remove_dir_all(store.path().join("store")).unwrap();
    ok(&envit(project.path(), store.path(), &["sync"]));

    let lock: Value =
        serde_json::from_slice(&std::fs::read(project.path().join("envit.lock.json")).unwrap())
            .unwrap();
    let locked_commit = match kind {
        SourceKind::Repo => &lock["repos"][0]["commit"],
        SourceKind::Skill => &lock["skillSources"][0]["commit"],
        SourceKind::Agent => &lock["agentSources"][0]["commit"],
    };
    assert_eq!(locked_commit.as_str(), Some(first.as_str()));

    let materialized = match kind {
        SourceKind::Repo => {
            std::fs::read_to_string(project.path().join(".envit/repos/fixture/lib.rs")).unwrap()
        }
        SourceKind::Skill => {
            std::fs::read_to_string(project.path().join(".agents/skills/alpha/SKILL.md")).unwrap()
        }
        SourceKind::Agent => {
            std::fs::read_to_string(project.path().join(".claude/agents/reviewer.md")).unwrap()
        }
    };
    assert!(materialized.contains("v1"), "{materialized}");
    assert!(!materialized.contains("v2"), "{materialized}");
}

#[test]
fn plain_sync_rehydrates_repo_skill_and_agent_locks_by_commit() {
    for kind in [SourceKind::Repo, SourceKind::Skill, SourceKind::Agent] {
        run_case(kind);
    }
}
