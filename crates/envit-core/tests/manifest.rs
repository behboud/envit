//! Integration tests: real files in real temp dirs. No mocks (AGENTS.md).
//! Manifest format (D6): JSON — `repos` array of strings/objects, `skills`
//! object keyed by source.

use envit_core::manifest::{AgentFormat, AgentPick, AgentTarget, Manifest, NewRepo, Pick};
use envit_core::{Error, ident, project, source};

fn add(m: &mut Manifest, src: &str, git_ref: Option<&str>) -> Result<String, Error> {
    m.add_repo(NewRepo {
        source: src,
        name: None,
        git_ref,
        sparse: &[],
    })
}

#[test]
fn json_format_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project::init(root).unwrap();
    let path = root.join(ident::MANIFEST_FILE);

    // Bare add → plain string entry.
    let mut m = Manifest::load(root).unwrap();
    add(&mut m, "tokio-rs/tokio", None).unwrap();
    m.save().unwrap();
    let out = std::fs::read_to_string(&path).unwrap();
    assert!(out.contains("\"tokio-rs/tokio\""), "{out}");

    // Add with options → object entry.
    let mut m = Manifest::load(root).unwrap();
    add(&mut m, "Effect-TS/effect", Some("next")).unwrap();
    m.save().unwrap();
    let out = std::fs::read_to_string(&path).unwrap();
    assert!(out.contains("\"source\": \"Effect-TS/effect\""), "{out}");
    assert!(out.contains("\"ref\": \"next\""), "{out}");

    // Both entries parse with the right defaults.
    let m = Manifest::load(root).unwrap();
    let entries = m.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "tokio");
    assert_eq!(
        entries[0].git_ref, None,
        "string entry tracks the default branch"
    );
    assert_eq!(entries[1].name, "effect");
    assert_eq!(entries[1].git_ref.as_deref(), Some("next"));

    // Duplicates rejected across both entry shapes.
    let mut m = Manifest::load(root).unwrap();
    assert!(matches!(
        add(&mut m, "other/tokio", None),
        Err(Error::DuplicateName(n)) if n == "tokio"
    ));

    // Remove a string entry; the other survives.
    let mut m = Manifest::load(root).unwrap();
    m.remove_repo("tokio").unwrap();
    m.save().unwrap();
    let out = std::fs::read_to_string(&path).unwrap();
    assert!(!out.contains("tokio"), "{out}");
    assert!(out.contains("effect"), "{out}");
    let mut m = Manifest::load(root).unwrap();
    assert!(matches!(
        m.remove_repo("tokio"),
        Err(Error::NameNotFound(_))
    ));
}

#[test]
fn hand_written_minimal_manifest_just_works() {
    // The whole point: a human (or platform) writes plain JSON.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(ident::MANIFEST_FILE),
        r#"{
  "repos": [
    "tokio-rs/tokio",
    "https://git.corp.example/team/api.git"
  ],
  "skills": {
    "vercel-labs/agent-skills": {
      "pick": ["web-design-guidelines", "react-best-practices"],
      "includeAgents": true
    },
    "acme/team-skills": "*"
  },
  "agents": {
    "humanlayer/skills": {
      "path": "plugins/rpi/agents",
      "pick": ["planner"],
      "format": "envit-agent/v1",
      "targets": ["claude", "codex", "pi", "opencode"],
      "update": "frozen"
    }
  }
}"#,
    )
    .unwrap();
    let m = Manifest::load(dir.path()).unwrap();
    let e = m.entries();
    assert_eq!(e.len(), 2);
    assert_eq!(e[0].name, "tokio");
    assert_eq!(e[1].name, "api");

    let s = m.skill_entries().unwrap();
    assert_eq!(s.len(), 2);
    let names: Vec<&str> = match &s[0].pick {
        Pick::Named(items) => items.iter().map(|i| i.name.as_str()).collect(),
        Pick::All => vec![],
    };
    assert_eq!(names, ["web-design-guidelines", "react-best-practices"]);
    assert_eq!(s[1].pick, Pick::All);
    assert!(s[0].include_agents);
    assert!(!s[1].include_agents);

    let agents = m.agent_entries().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].source, "humanlayer/skills");
    assert_eq!(agents[0].path.as_deref(), Some("plugins/rpi/agents"));
    assert_eq!(agents[0].pick, AgentPick::Named(vec!["planner".into()]));
    assert_eq!(agents[0].format, AgentFormat::Portable);
    assert!(agents[0].frozen);
    assert_eq!(
        agents[0].targets,
        [
            AgentTarget::Claude,
            AgentTarget::Codex,
            AgentTarget::Pi,
            AgentTarget::OpenCode
        ]
    );

    // Malformed JSON errors loudly (trailing comma).
    std::fs::write(
        dir.path().join(ident::MANIFEST_FILE),
        "{\"repos\": [\"a/b\",]}",
    )
    .unwrap();
    assert!(Manifest::load(dir.path()).is_err());
}

#[test]
fn agent_manifest_requires_explicit_format_and_valid_targets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(ident::MANIFEST_FILE);
    for doc in [
        r#"{"agents":{"owner/repo":{"pick":"*","targets":["claude"]}}}"#,
        r#"{"agents":{"owner/repo":{"pick":"*","format":"envit-agent/v1"}}}"#,
        r#"{"agents":{"owner/repo":{"pick":"*","format":"native","targets":["cursor"]}}}"#,
        r#"{"agents":{"owner/repo":{"pick":[],"format":"native","targets":["claude"]}}}"#,
        r#"{"agents":{"owner/repo":{"pick":["reviewer","reviewer"],"format":"native","targets":["claude"]}}}"#,
        r#"{"agents":{"owner/repo":{"pick":"*","format":"native","targets":["claude"],"taget":"codex"}}}"#,
        r#"{"agents":{"owner/repo":{"path":"../agents","pick":"*","format":"native","targets":["claude"]}}}"#,
        r#"{"agents":{"owner/repo":{"pick":"*","format":"native","targets":["claude"],"update":true}}}"#,
        r#"{"agents":{"owner/repo":{"pick":"*","format":"native","targets":["claude"],"update":"auto"}}}"#,
        r#"{"agents":{"owner/repo":{"pick":"*","format":"native","targets":["claude"],"update":"manual"}}}"#,
        r#"{"agents":{"owner/repo":{"pick":"*","format":"native","targets":["claude"],"ttl":"1h"}}}"#,
    ] {
        std::fs::write(&path, doc).unwrap();
        let manifest = Manifest::load(dir.path()).unwrap();
        assert!(manifest.agent_entries().is_err(), "{doc}");
    }
}

#[test]
fn min_age_parses_at_every_layer_and_rejects_bad_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(ident::MANIFEST_FILE);
    std::fs::write(
        &path,
        r#"{
  "minAge": "48h",
  "skills": {
    "owner/skills": { "pick": "*", "minAge": "0s" },
    "owner/other": "*"
  },
  "agents": {
    "owner/agents": {
      "pick": "*", "format": "native", "targets": ["claude"], "minAge": "7d"
    }
  }
}"#,
    )
    .unwrap();
    let m = Manifest::load(dir.path()).unwrap();
    let hours = |h: u64| Some(std::time::Duration::from_secs(h * 3600));
    assert_eq!(m.min_age().unwrap(), hours(48));
    let skills = m.skill_entries().unwrap();
    assert_eq!(skills[0].min_age, hours(0));
    assert_eq!(skills[1].min_age, None);
    assert_eq!(m.agent_entries().unwrap()[0].min_age, hours(7 * 24));

    // A typo must not silently weaken the gate.
    for (doc, at) in [
        (r#"{"minAge":"1 day"}"#, "'minAge'"),
        (r#"{"minAge":24}"#, "'minAge'"),
        (r#"{"skills":{"owner/skills":{"pick":"*","minAge":"24hrs"}}}"#, "'skills.owner/skills.minAge'"),
        (
            r#"{"agents":{"owner/agents":{"pick":"*","format":"native","targets":["claude"],"minAge":"soon"}}}"#,
            "'agents.owner/agents.minAge'",
        ),
    ] {
        std::fs::write(&path, doc).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        let err = m
            .min_age()
            .and_then(|_| m.skill_entries())
            .and_then(|_| m.agent_entries())
            .err()
            .unwrap_or_else(|| panic!("{doc} should be rejected"));
        assert!(
            matches!(&err, Error::ManifestShape(msg) if msg.contains(at)),
            "{doc}: {err}"
        );
    }
}

#[test]
fn pin_unpin_shape_transitions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project::init(root).unwrap();
    let mut m = Manifest::load(root).unwrap();
    add(&mut m, "tokio-rs/tokio", None).unwrap();
    m.save().unwrap();

    // Pin a plain string entry → object with sha/frozen/tracks.
    let mut m = Manifest::load(root).unwrap();
    m.pin_repo("tokio", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    m.save().unwrap();
    let out = std::fs::read_to_string(root.join(ident::MANIFEST_FILE)).unwrap();
    assert!(out.contains("\"update\": \"frozen\""), "{out}");
    assert!(out.contains("\"tracks\": \"HEAD\""), "{out}");
    let m2 = Manifest::load(root).unwrap();
    assert!(m2.entries()[0].frozen);

    // Unpin → collapses back to the plain string.
    let mut m = Manifest::load(root).unwrap();
    let tracked = m.unpin_repo("tokio").unwrap();
    assert_eq!(tracked, "HEAD");
    m.save().unwrap();
    let out = std::fs::read_to_string(root.join(ident::MANIFEST_FILE)).unwrap();
    assert!(out.contains("\"tokio-rs/tokio\""), "{out}");
    assert!(!out.contains("frozen") && !out.contains("tracks"), "{out}");
}

#[test]
fn init_is_not_repeatable_and_updates_existing_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    project::init(root).unwrap();
    assert!(!root.join(".gitignore").exists());
    assert!(matches!(
        project::init(root),
        Err(Error::AlreadyInitialized(_))
    ));

    let dir2 = tempfile::tempdir().unwrap();
    let root2 = dir2.path();
    std::fs::write(root2.join(".gitignore"), "target/\n").unwrap();
    let report = project::init(root2).unwrap();
    assert!(report.gitignore_updated);
    let gi = std::fs::read_to_string(root2.join(".gitignore")).unwrap();
    assert!(gi.contains(&format!("{}/", ident::CONTEXT_DIR)));
    assert!(gi.contains("target/"));
}

#[test]
fn find_root_walks_up_from_nested_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project::init(root).unwrap();
    let nested = root.join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(project::find_root(&nested).unwrap(), root);

    let orphan = tempfile::tempdir().unwrap();
    assert!(matches!(
        project::find_root(orphan.path()),
        Err(Error::ManifestNotFound(_))
    ));
}

#[test]
fn source_expansion_and_default_names() {
    let cases = [
        (
            "tokio-rs/tokio",
            "https://github.com/tokio-rs/tokio.git",
            "tokio",
        ),
        (
            "github:tokio-rs/tokio",
            "https://github.com/tokio-rs/tokio.git",
            "tokio",
        ),
        (
            "gitlab:group/sub/repo",
            "https://gitlab.com/group/sub/repo.git",
            "repo",
        ),
        (
            "bitbucket:owner/repo",
            "https://bitbucket.org/owner/repo.git",
            "repo",
        ),
        (
            "codeberg:owner/repo",
            "https://codeberg.org/owner/repo.git",
            "repo",
        ),
        (
            "sourcehut:~user/repo",
            "https://git.sr.ht/~user/repo",
            "repo",
        ),
        (
            "https://git.corp.example/team/api.git",
            "https://git.corp.example/team/api.git",
            "api",
        ),
    ];
    for (src, url, name) in cases {
        assert_eq!(source::expand(src).unwrap(), url, "expanding {src}");
        assert_eq!(source::default_name(src).unwrap(), name, "naming {src}");
    }

    for bad in [
        "github:justowner",
        "github:a/b/c",
        "a/b/c",
        "sourcehut:user/repo",
        "ssh://git@host/x",
        "tokio",
        "https://",
    ] {
        assert!(source::expand(bad).is_err(), "{bad} should be rejected");
    }
}

#[test]
fn init_notes_envit_in_existing_agent_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("AGENTS.md"), "# My project\n\nDo things.\n").unwrap();
    std::fs::write(root.join("CLAUDE.md"), "Keep it simple.").unwrap(); // no trailing newline

    let written = project::inject_agent_notes(root).unwrap();
    assert_eq!(written, ["AGENTS.md", "CLAUDE.md"]);
    let a = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(a.starts_with("# My project"), "user content stays first");
    assert!(a.contains("<!-- envit:begin -->") && a.contains("## envit: repo context"));
    assert!(a.contains(".envit/repos/<name>/"));
    let c = std::fs::read_to_string(root.join("CLAUDE.md")).unwrap();
    assert!(
        c.starts_with("Keep it simple.\n\n<!-- envit:begin -->"),
        "{c}"
    );

    // Idempotent: a second pass writes nothing and duplicates nothing.
    let written = project::inject_agent_notes(root).unwrap();
    assert!(written.is_empty());
    let a2 = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert_eq!(a2.matches("envit:begin").count(), 1);

    // A stale block is replaced in place, user text after it preserved.
    let stale = a2.replace("## envit: repo context", "## old note") + "\nTrailing user note.\n";
    std::fs::write(root.join("AGENTS.md"), stale).unwrap();
    project::inject_agent_notes(root).unwrap();
    let a3 = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(a3.contains("## envit: repo context") && !a3.contains("## old note"));
    assert!(a3.ends_with("Trailing user note.\n"), "{a3}");

    // Absent files are never created.
    let empty = tempfile::tempdir().unwrap();
    assert!(
        project::inject_agent_notes(empty.path())
            .unwrap()
            .is_empty()
    );
    assert!(!empty.path().join("AGENTS.md").exists());
}
