//! Git engine integration tests: real git fixture repos, real fetches over
//! the file transport, real checkouts. No mocks (AGENTS.md).
//! System `git` is used ONLY to author fixtures — the code under test is
//! pure gix.

use std::path::Path;
use std::process::Command;

use envit_core::{git, Error};

/// Create a real repo with a commit history, a branch, and a tag.
/// Returns (repo_dir, head_sha, tagged_sha).
fn fixture(dir: &Path) -> (String, String) {
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    run(&["init", "-b", "main", "."]);
    std::fs::write(dir.join("README.md"), "v1 readme\n").unwrap();
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() {}\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "first"]);
    run(&["tag", "v1.0.0"]);
    let tagged = run(&["rev-parse", "HEAD"]);
    std::fs::write(dir.join("src/lib.rs"), "pub fn two() {}\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "second"]);
    let head = run(&["rev-parse", "HEAD"]);
    (head, tagged)
}

#[test]
fn fetch_branch_tag_and_extract_checkout() {
    let upstream = tempfile::tempdir().unwrap();
    let (head, tagged) = fixture(upstream.path());
    let url = format!("file://{}", upstream.path().display());

    let store = tempfile::tempdir().unwrap();
    let bare_path = store.path().join("repo.git");

    // Branch fetch resolves to the branch head.
    let bare = git::ensure_bare(&bare_path).unwrap();
    let r = git::fetch_ref(&bare, &url, "main", false).unwrap();
    assert_eq!(r.commit, head);
    assert!(!r.tag, "branch resolution is not a tag");

    // Tag fetch resolves to the tagged commit (peeled), sharing the bare repo.
    let r_tag = git::fetch_ref(&bare, &url, "v1.0.0", false).unwrap();
    assert_eq!(r_tag.commit, tagged);
    assert!(r_tag.tag, "tag resolution is flagged");

    // Extract the tag's tree and verify actual file content (v1, not v2).
    let co = store.path().join("checkout-v1");
    git::extract_checkout(&bare, &tagged, &co).unwrap();
    assert_eq!(
        std::fs::read_to_string(co.join("src/lib.rs")).unwrap(),
        "pub fn one() {}\n"
    );
    assert_eq!(std::fs::read_to_string(co.join("README.md")).unwrap(), "v1 readme\n");

    // Extract head too — different content from the same bare repo.
    let co2 = store.path().join("checkout-head");
    git::extract_checkout(&bare, &head, &co2).unwrap();
    assert_eq!(
        std::fs::read_to_string(co2.join("src/lib.rs")).unwrap(),
        "pub fn two() {}\n"
    );

    // A ref that doesn't exist fails loudly with the right error.
    match git::fetch_ref(&bare, &url, "does-not-exist", false) {
        Err(Error::RefNotFound { rref, .. }) => assert_eq!(rref, "does-not-exist"),
        other => panic!("expected RefNotFound, got {other:?}"),
    }

    // Reopening an existing bare repo works (second sync path).
    let again = git::ensure_bare(&bare_path).unwrap();
    let r2 = git::fetch_ref(&again, &url, "main", false).unwrap();
    assert_eq!(r2.commit, head);
}

/// A depth-1 fetch sees only the tip; re-fetching the same bare repo at a
/// larger depth deepens it, and the first-parent walk reads committer times.
#[test]
fn deepen_shallow_fetch_and_read_commit_times() {
    let upstream = tempfile::tempdir().unwrap();
    let dir = upstream.path();
    let run = |args: &[&str], date: Option<&str>| {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t");
        if let Some(d) = date {
            cmd.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    run(&["init", "-b", "main", "."], None);
    let mut shas = Vec::new();
    for (i, date) in [Some("2020-01-01T00:00:00Z"), Some("2020-01-02T00:00:00Z"), None]
        .into_iter()
        .enumerate()
    {
        std::fs::write(dir.join("f"), format!("{i}\n")).unwrap();
        run(&["add", "."], None);
        run(&["commit", "-m", &format!("c{i}")], date);
        shas.push(run(&["rev-parse", "HEAD"], None));
    }
    let url = format!("file://{}", dir.display());

    let store = tempfile::tempdir().unwrap();
    let bare = git::ensure_bare(&store.path().join("repo.git")).unwrap();

    let tip = git::fetch_ref(&bare, &url, "main", false).unwrap();
    assert_eq!(tip.commit, shas[2]);
    let shallow = git::first_parent_times(&bare, &tip.commit, 20).unwrap();
    assert_eq!(shallow.len(), 1, "depth 1 holds only the tip: {shallow:?}");

    // Deepening moves the shallow boundary; the walk stops there.
    git::fetch_ref_at_depth(&bare, &url, "main", Some(2)).unwrap();
    assert_eq!(git::first_parent_times(&bare, &tip.commit, 20).unwrap().len(), 2);

    let deep = git::fetch_ref_at_depth(&bare, &url, "main", Some(20)).unwrap();
    assert_eq!(deep.commit, tip.commit);
    let times = git::first_parent_times(&bare, &tip.commit, 20).unwrap();
    let got: Vec<&str> = times.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(got, [shas[2].as_str(), shas[1].as_str(), shas[0].as_str()]);
    assert_eq!(times[1].1, 1_577_923_200, "2020-01-02T00:00:00Z");
    assert_eq!(times[2].1, 1_577_836_800, "2020-01-01T00:00:00Z");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!((now - times[0].1).abs() < 300, "tip is fresh: {}", times[0].1);

    // `limit` caps the walk.
    assert_eq!(git::first_parent_times(&bare, &tip.commit, 2).unwrap().len(), 2);
}
