//! e2e: opt-in companion agents are discovered from selected skills and
//! rendered portably for all harnesses. Uses real repositories and files.

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
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
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

fn skill(path: &Path, name: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill\n---\nSkill body.\n"),
    )
    .unwrap();
}

fn markdown_frontmatter(text: &str) -> (Value, &str) {
    let rest = text.strip_prefix("---\n").unwrap();
    let (yaml, body) = rest.split_once("\n---\n").unwrap();
    (serde_yaml_ng::from_str(yaml).unwrap(), body)
}

#[test]
fn selected_plugin_skills_include_companion_agents_as_portable_all_targets() {
    let upstream = tempfile::tempdir().unwrap();
    git(upstream.path(), &["init", "-b", "main", "."]);
    skill(
        &upstream.path().join("plugins/show-me/skills/show-me"),
        "show-me",
    );
    skill(&upstream.path().join("plugins/rpi/skills/rpi"), "rpi");
    let agents = upstream.path().join("plugins/rpi/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("codebase-analyzer.md"),
        "---\nname: codebase-analyzer\ndescription: Explains the codebase.\ntools: Read, Grep, Glob, Bash, Skill\nmodel: sonnet\n---\nDocument the code as it exists.\n",
    )
    .unwrap();
    git(upstream.path(), &["add", "."]);
    git(
        upstream.path(),
        &["commit", "-m", "plugin skills and agents"],
    );
    let commit = git(upstream.path(), &["rev-parse", "HEAD"]);
    let source = format!("file://{}", upstream.path().display());

    let project = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let manifest = serde_json::json!({
        "skills": {
            (source.clone()): {
                "pick": ["show-me", "rpi"],
                "includeAgents": true
            }
        }
    });
    std::fs::write(
        project.path().join("envit.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let output = ok(&envit(project.path(), store.path(), &["sync"]));
    assert!(output.contains("skill    show-me"), "{output}");
    assert!(output.contains("skill    rpi"), "{output}");
    assert_eq!(
        output.matches("agent    codebase-analyzer").count(),
        4,
        "{output}"
    );

    let claude_text =
        std::fs::read_to_string(project.path().join(".claude/agents/codebase-analyzer.md"))
            .unwrap();
    let (claude, body) = markdown_frontmatter(&claude_text);
    assert_eq!(claude["name"], "codebase-analyzer");
    assert_eq!(claude["description"], "Explains the codebase.");
    assert!(claude.get("tools").is_none());
    assert!(claude.get("model").is_none());
    assert!(body.contains("Document the code as it exists."));

    let codex: toml::Value = toml::from_str(
        &std::fs::read_to_string(project.path().join(".codex/agents/codebase-analyzer.toml"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(codex["name"].as_str(), Some("codebase-analyzer"));
    assert!(codex.get("tools").is_none());
    assert!(codex.get("model").is_none());
    assert!(
        codex["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("Document the code as it exists.")
    );

    let pi_text =
        std::fs::read_to_string(project.path().join(".pi/agents/codebase-analyzer.md")).unwrap();
    let (pi, _) = markdown_frontmatter(&pi_text);
    assert!(pi.get("tools").is_none());
    assert!(pi.get("model").is_none());

    let opencode_text =
        std::fs::read_to_string(project.path().join(".opencode/agents/codebase-analyzer.md"))
            .unwrap();
    let (opencode, _) = markdown_frontmatter(&opencode_text);
    assert_eq!(opencode["mode"], "subagent");
    assert!(opencode.get("tools").is_none());
    assert!(opencode.get("model").is_none());

    let lock: Value =
        serde_json::from_slice(&std::fs::read(project.path().join("envit.lock.json")).unwrap())
            .unwrap();
    assert_eq!(lock["skillSources"][0]["commit"], commit);
    assert_eq!(lock["agentSources"][0]["commit"], commit);

    let listed = ok(&envit(project.path(), store.path(), &["agents"]));
    assert!(listed.contains("from skills"), "{listed}");
    assert!(listed.contains("envit-agent/v1"), "{listed}");
}
