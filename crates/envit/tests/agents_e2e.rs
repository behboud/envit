//! e2e: project agents install as native harness definitions. Real git repos,
//! files, links, generated Markdown frontmatter, and Codex TOML (AGENTS.md).

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn envit(dir: &Path, store: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_envit"))
        .args(args)
        .current_dir(dir)
        .env("ENVIT_HOME", store)
        .output()
        .expect("binary runs")
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn manifest(root: &Path, source: &str) {
    std::fs::write(
        root.join("envit.json"),
        format!(
            "{{\"agents\":{{\"{source}\":{{\"path\":\"agents\",\"pick\":[\"reviewer\"],\"format\":\"envit-agent/v1\",\"targets\":[\"claude\",\"codex\",\"pi\",\"opencode\"]}}}}}}\n"
        ),
    )
    .unwrap();
}

fn markdown_frontmatter(text: &str) -> (Value, &str) {
    let rest = text
        .strip_prefix("---\n")
        .expect("native Markdown frontmatter starts with ---");
    let (yaml, body) = rest
        .split_once("\n---\n")
        .expect("frontmatter closes with ---");
    (serde_yaml_ng::from_str(yaml).unwrap(), body)
}

fn agent_repo(dir: &Path) -> String {
    git(dir, &["init", "-b", "main", "."]);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "agents"]);
    git(dir, &["rev-parse", "HEAD"])
}

#[test]
fn portable_agent_renders_native_files_for_all_four_targets_and_is_locked() {
    let upstream = tempfile::tempdir().unwrap();
    let agents = upstream.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: \"Reviews changes: correctness and safety.\"\nadapters:\n  claude:\n    tools: [Read, Grep]\n  codex:\n    sandbox_mode: read-only\n  pi:\n    model: inherit\n  opencode:\n    permission:\n      edit: deny\n---\nReview the actual change and explain concrete risks.\n",
    )
    .unwrap();
    let head = agent_repo(upstream.path());
    let source = format!("file://{}", upstream.path().display());
    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    manifest(project.path(), &source);

    let out = ok(&envit(project.path(), store_dir.path(), &["sync"]));
    assert!(out.contains("agent target(s)"), "{out}");
    assert_eq!(out.matches("agent    reviewer").count(), 4, "{out}");

    let claude_text =
        std::fs::read_to_string(project.path().join(".claude/agents/reviewer.md")).unwrap();
    let (claude, claude_body) = markdown_frontmatter(&claude_text);
    assert_eq!(claude["name"], "reviewer");
    assert_eq!(
        claude["description"],
        "Reviews changes: correctness and safety."
    );
    assert_eq!(claude["tools"][0], "Read");
    assert!(claude_body.contains("explain concrete risks"));

    let codex: toml::Value = toml::from_str(
        &std::fs::read_to_string(project.path().join(".codex/agents/reviewer.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(codex["name"].as_str(), Some("reviewer"));
    assert_eq!(codex["sandbox_mode"].as_str(), Some("read-only"));
    assert!(
        codex["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("concrete risks")
    );

    let pi_text = std::fs::read_to_string(project.path().join(".pi/agents/reviewer.md")).unwrap();
    let (pi, pi_body) = markdown_frontmatter(&pi_text);
    assert_eq!(pi["model"], "inherit");
    assert!(pi_body.contains("Review the actual change"));

    let opencode_text =
        std::fs::read_to_string(project.path().join(".opencode/agents/reviewer.md")).unwrap();
    let (opencode, opencode_body) = markdown_frontmatter(&opencode_text);
    assert_eq!(opencode["mode"], "subagent");
    assert_eq!(opencode["permission"]["edit"], "deny");
    assert!(opencode_body.contains("explain concrete risks"));

    let lock: Value =
        serde_json::from_slice(&std::fs::read(project.path().join("envit.lock.json")).unwrap())
            .unwrap();
    assert_eq!(lock["agentSources"][0]["commit"], head);
    let locked_source = lock["agentSources"][0]["source"].as_str().unwrap();
    let checkout = envit_core::store::Store::at(store_dir.path())
        .checkout_dir(locked_source, &head, &[])
        .unwrap();
    ok(&envit(project.path(), store_dir.path(), &["gc"]));
    assert!(
        checkout.is_dir(),
        "gc retains checkouts referenced by agents"
    );

    let listed = ok(&envit(project.path(), store_dir.path(), &["agents"]));
    assert!(
        listed.contains("envit-agent/v1") && listed.contains("claude, codex, pi, opencode"),
        "{listed}"
    );
    assert!(listed.contains(&head[..12]), "{listed}");

    ok(&envit(
        project.path(),
        store_dir.path(),
        &["sync", "--frozen"],
    ));
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Updated review contract.\n---\nUse the revised checklist.\n",
    )
    .unwrap();
    git(upstream.path(), &["add", "."]);
    git(upstream.path(), &["commit", "-m", "update agent"]);
    let updated_head = git(upstream.path(), &["rev-parse", "HEAD"]);
    ok(&envit(
        project.path(),
        store_dir.path(),
        &["update", source.as_str()],
    ));
    let updated_lock: Value =
        serde_json::from_slice(&std::fs::read(project.path().join("envit.lock.json")).unwrap())
            .unwrap();
    assert_eq!(updated_lock["agentSources"][0]["commit"], updated_head);
    let updated_agent =
        std::fs::read_to_string(project.path().join(".claude/agents/reviewer.md")).unwrap();
    assert!(updated_agent.contains("Use the revised checklist."));
}

#[cfg(unix)]
#[test]
fn native_agent_variants_are_passed_through_and_pruning_leaves_user_files() {
    let upstream = tempfile::tempdir().unwrap();
    let agents = upstream.path().join("agents");
    for dir in ["claude", "codex", "opencode"] {
        std::fs::create_dir_all(agents.join(dir)).unwrap();
    }
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: generic\n---\nshared markdown\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("claude/reviewer.md"),
        "---\nname: claude-native\n---\nclaude bytes\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("codex/reviewer.toml"),
        "name = \"codex-native\"\ndescription = \"native\"\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("opencode/reviewer.md"),
        "---\ndescription: native\nmode: subagent\n---\nopencode bytes\n",
    )
    .unwrap();
    agent_repo(upstream.path());
    let source = format!("file://{}", upstream.path().display());

    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".claude/agents")).unwrap();
    std::fs::write(
        project.path().join(".claude/agents/manual.md"),
        "user-owned\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join(".claude/agents/manual.md.commit"),
        "unrelated tool marker\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join(".claude/agents/manual-linked.md"),
        "also user-owned\n",
    )
    .unwrap();
    let external_marker = store_dir.path().join("external-marker");
    std::fs::write(
        &external_marker,
        format!(
            "{}{}+native:claude:manual-linked",
            envit_core::ident::AGENT_FILE_MARKER_PREFIX,
            "a".repeat(40)
        ),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        &external_marker,
        project
            .path()
            .join(".claude/agents/manual-linked.md.commit"),
    )
    .unwrap();
    std::fs::write(
        project.path().join("envit.json"),
        format!(
            "{{\"agents\":{{\"{source}\":{{\"path\":\"agents\",\"pick\":[\"reviewer\"],\"format\":\"native\",\"targets\":[\"claude\",\"codex\",\"pi\",\"opencode\"]}}}}}}\n"
        ),
    )
    .unwrap();

    let expected = [
        (
            ".claude/agents/reviewer.md",
            "---\nname: claude-native\n---\nclaude bytes\n",
        ),
        (
            ".codex/agents/reviewer.toml",
            "name = \"codex-native\"\ndescription = \"native\"\n",
        ),
        (
            ".pi/agents/reviewer.md",
            "---\nname: generic\n---\nshared markdown\n",
        ),
        (
            ".opencode/agents/reviewer.md",
            "---\ndescription: native\nmode: subagent\n---\nopencode bytes\n",
        ),
    ];
    ok(&envit(project.path(), store_dir.path(), &["sync"]));
    for (path, body) in expected {
        let output = project.path().join(path);
        assert_eq!(std::fs::read_to_string(&output).unwrap(), body);
        assert!(
            std::fs::read_link(&output).is_ok(),
            "native symlink points at the source file"
        );
    }

    ok(&envit(
        project.path(),
        store_dir.path(),
        &["sync", "--link-mode", "copy"],
    ));
    for (path, body) in expected {
        let output = project.path().join(path);
        assert_eq!(std::fs::read_to_string(&output).unwrap(), body);
        assert!(
            std::fs::read_link(&output).is_err(),
            "copy mode writes a regular file"
        );
        assert!(
            output
                .with_file_name(format!(
                    "{}.commit",
                    output.file_name().unwrap().to_string_lossy()
                ))
                .is_file()
        );
    }
    assert_eq!(
        std::fs::read_to_string(project.path().join(".claude/agents/manual.md")).unwrap(),
        "user-owned\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join(".claude/agents/manual-linked.md")).unwrap(),
        "also user-owned\n"
    );

    std::fs::write(project.path().join("envit.json"), "{\"repos\":[]}\n").unwrap();
    ok(&envit(project.path(), store_dir.path(), &["sync"]));
    for (path, _) in expected {
        let output = project.path().join(path);
        assert!(
            !output.exists(),
            "stale managed file removed: {}",
            output.display()
        );
        assert!(
            !output
                .with_file_name(format!(
                    "{}.commit",
                    output.file_name().unwrap().to_string_lossy()
                ))
                .exists()
        );
    }
    assert_eq!(
        std::fs::read_to_string(project.path().join(".claude/agents/manual.md")).unwrap(),
        "user-owned\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join(".claude/agents/manual-linked.md")).unwrap(),
        "also user-owned\n"
    );
    let lock: Value =
        serde_json::from_slice(&std::fs::read(project.path().join("envit.lock.json")).unwrap())
            .unwrap();
    assert!(lock.get("agentSources").is_none());
}

#[cfg(unix)]
#[test]
fn sync_does_not_follow_symlinked_agent_destination_directories() {
    let upstream = tempfile::tempdir().unwrap();
    let agents = upstream.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("reviewer.md"), "native agent\n").unwrap();
    agent_repo(upstream.path());
    let source = format!("file://{}", upstream.path().display());

    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    std::fs::write(external.path().join("manual.md"), "user-owned\n").unwrap();
    let stale = external.path().join("stale.md");
    let stored_agent = store_dir.path().join("store/old/agent.md");
    std::fs::create_dir_all(stored_agent.parent().unwrap()).unwrap();
    std::fs::write(&stored_agent, "managed target\n").unwrap();
    std::os::unix::fs::symlink(&stored_agent, &stale).unwrap();
    std::fs::create_dir_all(project.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(external.path(), project.path().join(".claude/agents")).unwrap();
    std::fs::write(project.path().join("envit.json"), "{\"repos\":[]}\n").unwrap();

    ok(&envit(project.path(), store_dir.path(), &["sync"]));
    assert!(
        stale.exists(),
        "pruning must not traverse an external target dir"
    );
    assert_eq!(
        std::fs::read_to_string(external.path().join("manual.md")).unwrap(),
        "user-owned\n"
    );

    std::fs::write(
        project.path().join("envit.json"),
        format!(
            "{{\"agents\":{{\"{source}\":{{\"path\":\"agents\",\"pick\":[\"reviewer\"],\"format\":\"native\",\"targets\":[\"claude\"]}}}}}}\n"
        ),
    )
    .unwrap();
    let out = envit(project.path(), store_dir.path(), &["sync"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(".claude/agents"));
    assert!(!external.path().join("reviewer.md").exists());
    assert!(stale.exists());
}

#[test]
fn native_markdown_is_not_silently_converted_to_codex_toml() {
    let upstream = tempfile::tempdir().unwrap();
    let agents = upstream.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\n---\nbody\n",
    )
    .unwrap();
    agent_repo(upstream.path());
    let source = format!("file://{}", upstream.path().display());

    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("envit.json"),
        format!(
            "{{\"agents\":{{\"{source}\":{{\"path\":\"agents\",\"pick\":[\"reviewer\"],\"format\":\"native\",\"targets\":[\"claude\",\"codex\"]}}}}}}\n"
        ),
    )
    .unwrap();
    let out = envit(project.path(), store_dir.path(), &["sync"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no codex definition"));
    assert!(
        !project.path().join(".claude/agents/reviewer.md").exists(),
        "missing target validation precedes installation"
    );
}

#[test]
fn portable_frontmatter_rejects_unknown_fields() {
    let upstream = tempfile::tempdir().unwrap();
    let agents = upstream.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: test\ncolor: blue\n---\nbody\n",
    )
    .unwrap();
    agent_repo(upstream.path());
    let source = format!("file://{}", upstream.path().display());

    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    manifest(project.path(), &source);
    let out = envit(project.path(), store_dir.path(), &["sync"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown portable frontmatter field 'color'")
    );
    assert!(!project.path().join(".claude/agents/reviewer.md").exists());
}
