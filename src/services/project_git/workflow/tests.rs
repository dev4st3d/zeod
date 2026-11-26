use super::*;
use std::fs;
use std::sync::atomic::AtomicU64;

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Repo {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zeod-workflow-{}-{stamp}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-b", "main"]);
        repo.configure();
        repo
    }
    fn configure(&self) {
        self.git(&["config", "user.name", "Example Developer"]);
        self.git(&["config", "user.email", "example@example.invalid"]);
        self.git(&["config", "commit.gpgsign", "false"]);
        self.git(&["config", "core.autocrlf", "false"]);
        self.git(&["config", "core.hooksPath", ".git/empty-hooks"]);
    }
    fn git(&self, values: &[&str]) -> String {
        let output = run_git_with_input(&self.0, &args(values), None, WRITE_TIMEOUT).unwrap();
        assert!(
            output.success,
            "Synthetic Git command failed: {:?}: {}",
            values,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.bytes)
            .unwrap()
            .trim_end_matches(['\r', '\n'])
            .into()
    }
    fn put(&self, name: &str, contents: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fn entry(&self, name: &str) -> GitEntry {
        read_status(&self.0)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.relative_path == Path::new(name))
            .unwrap()
    }
    fn save(&self, message: &str) -> String {
        self.git(&["add", "--all", "--", "."]);
        commit(&self.0, &repository(&self.0).unwrap(), message).unwrap()
    }
}

#[test]
fn init_turns_a_plain_folder_into_an_unborn_repository() {
    let repo = Repo::new();
    let folder = repo.0.join("plain");
    fs::create_dir(&folder).unwrap();
    // Detach the fixture's own repository so the folder reads as plain.
    fs::remove_dir_all(repo.0.join(".git")).unwrap();
    assert!(matches!(read_status(&folder), Err(GitError::NotRepository)));

    init(&folder).unwrap();

    let status = read_status(&folder).unwrap();
    assert!(status.entries.is_empty());
    let created = repository(&folder).unwrap();
    assert_eq!(created.head, None);
    assert!(created.remotes.is_empty());
}

#[test]
fn literal_staging_and_unborn_unstage_preserve_working_files_and_new_line_counts() {
    let repo = Repo::new();
    repo.put("a[1].txt", "first\nsecond\n");
    repo.put("a1.txt", "other\n");
    let entry = repo.entry("a[1].txt");
    assert_eq!(
        entry.working_stats,
        Some(GitLineStats {
            additions: 2,
            deletions: 0
        })
    );
    stage(&repo.0, &[entry], true).unwrap();
    assert_eq!(repo.git(&["diff", "--cached", "--name-only"]), "a[1].txt");
    stage(&repo.0, &[repo.entry("a[1].txt")], false).unwrap();
    assert!(repo.git(&["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(
        fs::read_to_string(repo.0.join("a[1].txt")).unwrap(),
        "first\nsecond\n"
    );
    assert_eq!(
        fs::read_to_string(repo.0.join("a1.txt")).unwrap(),
        "other\n"
    );
    repo.put("empty.txt", "");
    assert_eq!(
        repo.entry("empty.txt").working_stats,
        Some(GitLineStats::default())
    );
}

#[test]
fn discard_checks_the_preview_snapshot_and_preserves_the_staged_version() {
    let repo = Repo::new();
    repo.put("file.txt", "base\nsecond\n");
    repo.save("Initial");
    repo.put("file.txt", "staged\nsecond\n");
    stage(&repo.0, &[repo.entry("file.txt")], true).unwrap();
    repo.put("file.txt", "staged\nworking\n");
    let old = prepare_discard(&repo.0, &[repo.entry("file.txt")]).unwrap();
    repo.put("file.txt", "newer\nworking\n");
    assert!(discard(&old).is_err());
    assert_eq!(
        fs::read_to_string(repo.0.join("file.txt")).unwrap(),
        "newer\nworking\n"
    );
    let plan = prepare_discard(&repo.0, &[repo.entry("file.txt")]).unwrap();
    discard(&plan).unwrap();
    assert_eq!(
        fs::read_to_string(repo.0.join("file.txt")).unwrap(),
        "staged\nsecond\n"
    );
    let status = repo.entry("file.txt");
    assert!(status.staged());
    assert!(!status.unstaged());
    commit(&repo.0, &repository(&repo.0).unwrap(), "Only staged").unwrap();
    assert_eq!(repo.git(&["show", "HEAD:file.txt"]), "staged\nsecond");
}

#[test]
fn discarding_several_files_restores_each_and_rejects_a_stale_preview() {
    let repo = Repo::new();
    repo.put("a.txt", "a\n");
    repo.put("dir/b.txt", "b\n");
    repo.save("Initial");
    repo.put("a.txt", "a changed\n");
    fs::remove_file(repo.0.join("dir/b.txt")).unwrap();
    let entries = [repo.entry("a.txt"), repo.entry("dir/b.txt")];
    let stale = prepare_discard(&repo.0, &entries).unwrap();
    assert_eq!(stale.files.len(), 2);
    assert_eq!(stale.untracked_count(), 0);
    repo.put("dir/b.txt", "recreated\n");
    assert!(discard(&stale).is_err());
    assert_eq!(
        fs::read_to_string(repo.0.join("a.txt")).unwrap(),
        "a changed\n"
    );
    let entries = [repo.entry("a.txt"), repo.entry("dir/b.txt")];
    discard(&prepare_discard(&repo.0, &entries).unwrap()).unwrap();
    assert_eq!(fs::read_to_string(repo.0.join("a.txt")).unwrap(), "a\n");
    assert_eq!(fs::read_to_string(repo.0.join("dir/b.txt")).unwrap(), "b\n");
    assert!(read_status(&repo.0).unwrap().entries.is_empty());
}

#[test]
fn commit_all_stages_listed_changes_including_new_files() {
    let repo = Repo::new();
    repo.put("tracked.txt", "base\n");
    repo.save("Initial");
    repo.put("tracked.txt", "changed\n");
    repo.put("new.txt", "new\n");
    let entries = read_status(&repo.0).unwrap().entries;
    let expected = repository(&repo.0).unwrap();
    assert!(commit_all(&repo.0, &expected, &entries, "  ").is_err());
    assert!(repo.git(&["diff", "--cached", "--name-only"]).is_empty());
    commit_all(&repo.0, &expected, &entries, "Everything").unwrap();
    assert!(read_status(&repo.0).unwrap().entries.is_empty());
    assert_eq!(repo.git(&["show", "HEAD:new.txt"]), "new");
    assert_eq!(repo.git(&["log", "-1", "--format=%s"]), "Everything");
}

#[test]
fn committing_rejects_stale_staging_and_staged_files_outside_a_nested_project() {
    let repo = Repo::new();
    repo.put("nested/file.txt", "base\n");
    repo.put("outside.txt", "base\n");
    repo.save("Initial");
    repo.put("nested/file.txt", "change\n");
    stage(&repo.0, &[repo.entry("nested/file.txt")], true).unwrap();
    let expected = repository(&repo.0).unwrap();
    repo.put("outside.txt", "outside\n");
    stage(&repo.0, &[repo.entry("outside.txt")], true).unwrap();
    assert!(commit(&repo.0, &expected, "Stale").is_err());
    let current = repository(&repo.0).unwrap();
    assert!(commit(&repo.0.join("nested"), &current, "Outside scope").is_err());
    assert_eq!(repository(&repo.0).unwrap().head, current.head);
    assert_eq!(index_snapshot(&repo.0).unwrap(), current.index);
    assert!(stage(&repo.0.join("nested"), &[repo.entry("outside.txt")], true).is_err());
}

#[test]
fn history_covers_root_commits_multiline_messages_renames_binary_files_and_pagination() {
    let repo = Repo::new();
    repo.put("a.txt", "first\n");
    let first = repo.save("Initial\n\nA body\nwith two lines.");
    repo.git(&["mv", "a.txt", "renamed.txt"]);
    fs::write(repo.0.join("binary.dat"), [0, 1, 2, 3]).unwrap();
    let renamed = repo.save("Rename and add binary");
    let snapshot = repository(&repo.0).unwrap();
    let page = history_page(&repo.0, &snapshot, None, 0).unwrap();
    assert_eq!(page.commits[0].id, renamed);
    assert_eq!(page.commits[1].id, first);
    assert!(page.commits[1].body.contains("with two lines."));
    let root = commit_details(&repo.0, &page.commits[1], 0).unwrap();
    assert!(root.parent.is_none());
    assert_eq!(root.files.len(), 1);
    assert!(
        commit_file_diff(&repo.0, &root, 0)
            .unwrap()
            .text
            .contains("+first")
    );
    let details = commit_details(&repo.0, &page.commits[0], 0).unwrap();
    assert!(
        details
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("binary.dat") && file.stats.is_none())
    );
    assert!(
        details
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("a.txt"))
    );
    assert!(
        details
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("renamed.txt"))
    );
    for i in 0..HISTORY_PAGE_SIZE {
        repo.git(&["commit", "--allow-empty", "-m", &format!("Commit {i}")]);
    }
    let snapshot = repository(&repo.0).unwrap();
    let first_page = history_page(&repo.0, &snapshot, None, 0).unwrap();
    let second = history_page(
        &repo.0,
        &snapshot,
        first_page.anchor.as_deref(),
        first_page.commits.len(),
    )
    .unwrap();
    assert!(first_page.more);
    assert!(!second.more);
    assert!(
        first_page
            .commits
            .iter()
            .all(|a| second.commits.iter().all(|b| a.id != b.id))
    );
    assert_eq!(
        first_page.commits.len() + second.commits.len(),
        HISTORY_PAGE_SIZE + 2
    );
}

#[test]
fn merge_history_can_compare_each_parent() {
    let repo = Repo::new();
    repo.put("base.txt", "base\n");
    repo.save("Initial");
    repo.git(&["switch", "-c", "topic"]);
    repo.put("topic.txt", "topic\n");
    repo.save("Topic");
    repo.git(&["switch", "main"]);
    repo.put("main.txt", "main\n");
    repo.save("Main");
    repo.git(&["merge", "--no-ff", "topic", "-m", "Merge topic"]);
    let snapshot = repository(&repo.0).unwrap();
    let summary = history_page(&repo.0, &snapshot, None, 0)
        .unwrap()
        .commits
        .remove(0);
    assert_eq!(summary.parents.len(), 2);
    let first = commit_details(&repo.0, &summary, 0).unwrap();
    let second = commit_details(&repo.0, &summary, 1).unwrap();
    assert!(
        first
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("topic.txt"))
    );
    assert!(
        second
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("main.txt"))
    );
}

#[test]
fn publish_push_fetch_and_rejected_push_use_only_a_local_bare_remote() {
    let repo = Repo::new();
    repo.put("file.txt", "base\n");
    let initial = repo.save("Initial");
    let bare = repo.0.join("remote.git");
    repo.git(&["init", "--bare", bare.to_str().unwrap()]);
    repo.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    fs::write(repo.0.join(".gitignore"), "remote.git/\nother/\n").unwrap();
    let plan = push_plan(&repository(&repo.0).unwrap(), Some("origin")).unwrap();
    push(&plan).unwrap();
    let published = repository(&repo.0).unwrap();
    assert_eq!(published.upstream.as_ref().unwrap().label, "origin/main");
    assert_eq!(published.ahead, Some(0));
    let output = read(&bare, &args(&["rev-parse", "refs/heads/main"])).unwrap();
    assert_eq!(String::from_utf8(output).unwrap().trim(), initial);
    let page = history_page(&repo.0, &published, None, 0).unwrap();
    assert!(page.commits[0].remote_tip);
    assert_eq!(page.commits[0].local, Some(false));
    repo.put("file.txt", "local\n");
    repo.save("Local");
    let stale = push_plan(&repository(&repo.0).unwrap(), None).unwrap();
    repo.put("another.txt", "later\n");
    repo.save("Later");
    assert!(push(&stale).is_err());
    let other_path = repo.0.join("other");
    repo.git(&[
        "clone",
        "-b",
        "main",
        bare.to_str().unwrap(),
        other_path.to_str().unwrap(),
    ]);
    let other = Repo(other_path);
    other.configure();
    other.put("remote.txt", "remote\n");
    let remote_head = other.save("Remote");
    other.git(&["push", "origin", "main"]);
    fetch(&repository(&repo.0).unwrap(), "origin").unwrap();
    let diverged = repository(&repo.0).unwrap();
    assert_eq!(diverged.behind, Some(1));
    assert!(diverged.ahead.unwrap() > 0);
    assert!(push(&push_plan(&diverged, None).unwrap()).is_err());
    assert_eq!(repository(&repo.0).unwrap().head, diverged.head);
    assert_eq!(
        text(&bare, &args(&["rev-parse", "refs/heads/main"])).unwrap(),
        remote_head
    );
    // Fast-forward pull refuses diverged history without moving HEAD.
    let diverged_plan = pull_plan(&diverged).unwrap();
    let error = pull(&diverged_plan).unwrap_err();
    assert!(error.contains("diverged"), "{error}");
    assert_eq!(repository(&repo.0).unwrap().head, diverged.head);
}

#[test]
fn pull_fast_forwards_cleanly_and_detects_up_to_date_and_stale_plans() {
    let repo = Repo::new();
    repo.put("file.txt", "base\n");
    repo.save("Initial");
    let bare = repo.0.join("remote.git");
    repo.git(&["init", "--bare", bare.to_str().unwrap()]);
    repo.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    fs::write(repo.0.join(".gitignore"), "remote.git/\nother/\n").unwrap();
    push(&push_plan(&repository(&repo.0).unwrap(), Some("origin")).unwrap()).unwrap();

    let other_path = repo.0.join("other");
    repo.git(&[
        "clone",
        "-b",
        "main",
        bare.to_str().unwrap(),
        other_path.to_str().unwrap(),
    ]);
    let other = Repo(other_path);
    other.configure();
    other.put("incoming.txt", "from remote\n");
    let remote_head = other.save("Incoming");
    other.git(&["push", "origin", "main"]);

    fetch(&repository(&repo.0).unwrap(), "origin").unwrap();
    let behind = repository(&repo.0).unwrap();
    assert_eq!(behind.behind, Some(1));
    let notice = pull(&pull_plan(&behind).unwrap()).unwrap();
    assert!(notice.contains("Pulled from origin/main"), "{notice}");
    assert_eq!(
        repository(&repo.0).unwrap().head.as_deref(),
        Some(remote_head.as_str())
    );
    assert_eq!(
        fs::read_to_string(repo.0.join("incoming.txt")).unwrap(),
        "from remote\n"
    );

    // Pulling again when already aligned reports up to date.
    let synced = repository(&repo.0).unwrap();
    assert_eq!(
        pull(&pull_plan(&synced).unwrap()).unwrap(),
        "Already up to date."
    );

    // A stale pull plan is rejected after local HEAD moves.
    let stale_plan = pull_plan(&synced).unwrap();
    repo.put("local.txt", "local\n");
    repo.save("Local");
    assert!(pull(&stale_plan).is_err());
}

#[test]
fn create_branch_validates_names_and_works_on_normal_and_unborn_repositories() {
    let repo = Repo::new();
    // Unborn repository (no commits yet) accepts a valid branch creation.
    create_branch(&repo.0, "feature/start").unwrap();
    assert_eq!(read_status(&repo.0).unwrap().branch, "feature/start");

    repo.put("file.txt", "one\n");
    repo.save("Initial");
    create_branch(&repo.0, "topic").unwrap();
    assert_eq!(read_status(&repo.0).unwrap().branch, "topic");
    assert!(branches(&repo.0).unwrap().contains(&"topic".to_owned()));

    // Duplicate, empty, leading-dash, and invalid ref names are rejected.
    assert!(create_branch(&repo.0, "topic").is_err());
    assert!(create_branch(&repo.0, "   ").is_err());
    assert!(create_branch(&repo.0, "-flag").is_err());
    assert!(create_branch(&repo.0, "bad name").is_err());
    assert!(create_branch(&repo.0, "a..b").is_err());
}
