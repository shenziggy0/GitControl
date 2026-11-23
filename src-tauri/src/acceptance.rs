use super::{
    git,
    workspace::{self, Project},
};
use std::{fs, path::Path, process::Command};

fn run(path: &Path, argv: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(argv)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{argv:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn repository(path: &Path) -> Project {
    run(path, &["init", "-b", "main"]);
    run(path, &["config", "user.name", "GitControl"]);
    run(
        path,
        &["config", "user.email", "gitcontrol@example.invalid"],
    );
    for file in ["first", "second"] {
        fs::write(path.join(file), "原始\n").unwrap();
    }
    run(path, &["add", "."]);
    run(path, &["commit", "-m", "初始提交"]);
    Project {
        id: "real-test".into(),
        path: path.to_path_buf(),
        name: "实际仓库".into(),
    }
}

#[test]
fn discovers_worktrees_within_two_levels_and_revalidates_on_open() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().canonicalize().unwrap();
    for relative in [
        "api",
        "team",
        "empty",
        "too/deep/repo",
        "dist/repo",
        "api/nested",
    ] {
        fs::create_dir_all(parent.join(relative)).unwrap();
    }
    repository(&parent.join("api"));
    run(
        &parent.join("api"),
        &["worktree", "add", "-b", "team-api", "../team/api"],
    );
    for relative in ["empty", "too/deep/repo", "dist/repo", "api/nested"] {
        run(&parent.join(relative), &["init", "-b", "main"]);
    }
    let outside = tempfile::tempdir().unwrap();
    repository(outside.path());
    std::os::unix::fs::symlink(outside.path(), parent.join("alias")).unwrap();

    let scan = workspace::scan_children(&parent).unwrap();
    assert_eq!(
        scan.paths,
        [Path::new("api"), Path::new("empty"), Path::new("team/api")]
    );
    assert_eq!(scan.unreadable_count, 0);
    let path = workspace::open_child(&parent, Path::new("team/api")).unwrap();
    assert_eq!(run(&path, &["branch", "--show-current"]), "team-api");
    assert!(workspace::open_child(&parent, Path::new("api/nested")).is_err());
    assert!(workspace::open_child(&parent, Path::new("too/deep/repo")).is_err());
    assert!(workspace::open_child(&parent, Path::new("team/../api")).is_err());

    fs::rename(parent.join("team/api"), parent.join("team/moved")).unwrap();
    std::os::unix::fs::symlink(outside.path(), parent.join("team/api")).unwrap();
    assert!(workspace::open_child(&parent, Path::new("team/api")).is_err());
}

#[test]
fn separates_empty_and_incomplete_scans_from_parent_errors() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path();
    let empty = workspace::scan_children(parent).unwrap();
    assert!(empty.paths.is_empty());
    assert_eq!(empty.unreadable_count, 0);
    let broken = parent.join("broken");
    fs::create_dir(&broken).unwrap();
    fs::write(broken.join(".git"), "gitdir: missing\n").unwrap();
    let incomplete = workspace::scan_children(parent).unwrap();
    assert!(incomplete.paths.is_empty());
    assert_eq!(incomplete.unreadable_count, 1);
    assert!(workspace::scan_children(&parent.join("missing")).is_err());
    let repository_path = parent.join("repo");
    fs::create_dir(&repository_path).unwrap();
    repository(&repository_path);
    assert!(workspace::scan_children(&repository_path).is_err());
}

#[test]
fn reads_real_index_and_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let project = repository(temp.path());
    fs::write(temp.path().join("first"), "暂存\n").unwrap();
    run(temp.path(), &["add", "first"]);
    fs::write(temp.path().join("first"), "继续编辑\n").unwrap();
    fs::write(temp.path().join("new file"), "新增\n").unwrap();
    let status = git::status(&project).unwrap();
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.files.iter().filter(|f| f.path == "first").count(), 2);
    assert_eq!(
        status.files.iter().filter(|f| f.path == "new file").count(),
        1
    );
}

#[test]
fn claude_reference_reads_the_requested_commit_in_its_project() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    let project = repository(path);
    fs::write(path.join("first"), "旧提交的内容\n").unwrap();
    run(path, &["commit", "-am", "旧提交标题"]);
    let old = run(path, &["rev-parse", "HEAD"]);
    fs::write(path.join("second"), "当前提交的内容\n").unwrap();
    run(path, &["commit", "-am", "当前提交标题"]);
    let before = run(path, &["status", "--porcelain=v2", "--branch"]);

    let context = super::agent::commit_context(&project, &old).unwrap();
    assert_eq!(context["sha"], old);
    assert_eq!(context["subject"], "旧提交标题");
    assert_eq!(context["file_count"], 1);
    assert_eq!(context["files"][0]["path"], "first");
    assert!(context["diff_excerpts"][0]["patch"]
        .as_str()
        .unwrap()
        .contains("+旧提交的内容"));
    assert_eq!(run(path, &["status", "--porcelain=v2", "--branch"]), before);
    let blob = run(path, &["rev-parse", "HEAD:first"]);
    assert!(super::agent::commit_context(&project, &blob).is_err());
}

#[test]
fn partial_commit_preserves_index_and_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let project = repository(temp.path());
    for file in ["first", "second"] {
        fs::write(temp.path().join(file), "暂存\n").unwrap();
    }
    run(temp.path(), &["add", "."]);
    fs::write(temp.path().join("first"), "继续编辑\n").unwrap();
    let status = git::status(&project).unwrap();
    let chosen = status
        .files
        .iter()
        .filter(|f| f.path == "first" && f.group == "staged")
        .cloned()
        .collect::<Vec<_>>();
    git::commit(&project, &status, &chosen, "提交第一份改动").unwrap();
    assert_eq!(run(temp.path(), &["show", "HEAD:first"]), "暂存");
    assert_eq!(run(temp.path(), &["show", "HEAD:second"]), "原始");
    assert_eq!(
        run(temp.path(), &["diff", "--cached", "--name-only"]),
        "second"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("first")).unwrap(),
        "继续编辑\n"
    );
}

#[test]
fn remote_ref_is_verified_and_fetched_without_changing_local_refs() {
    let directory = tempfile::tempdir().unwrap();
    let clone = directory.path().join("clone");
    fs::create_dir(&clone).unwrap();
    let original = repository(&clone);
    let bare = directory.path().join("remote.git");
    run(
        directory.path(),
        &["init", "--bare", bare.to_str().unwrap()],
    );
    run(&clone, &["remote", "add", "origin", bare.to_str().unwrap()]);
    run(&clone, &["push", "-u", "origin", "main"]);
    let other = directory.path().join("other");
    run(
        directory.path(),
        &[
            "clone",
            "-b",
            "main",
            bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    run(&other, &["config", "user.name", "GitControl"]);
    run(
        &other,
        &["config", "user.email", "gitcontrol@example.invalid"],
    );
    run(&other, &["switch", "-c", "cloud-only"]);
    fs::write(other.join("new"), "云端新数据\n").unwrap();
    run(&other, &["add", "."]);
    run(&other, &["commit", "-m", "添加云端文件"]);
    run(&other, &["push", "origin", "cloud-only"]);
    let mut remote = git::verify_remote(&original, "origin").unwrap();
    assert_eq!(remote.refs.len(), 2);
    assert!(remote
        .refs
        .iter()
        .any(|r| r.name == "origin/cloud-only" && !r.available));
    git::hydrate_remote(&original, &mut remote);
    assert!(
        remote.refs.iter().all(|r| r.available),
        "{:?}",
        remote.history_error
    );
    let current = git::status(&original).unwrap();
    assert_eq!(current.branch.as_deref(), Some("main"));
    assert!(current.files.is_empty());
    assert_eq!(current.local.len(), 1);
    assert!(current
        .tracking
        .iter()
        .all(|b| b.name != "origin/cloud-only"));
    let refs = remote
        .refs
        .iter()
        .map(|r| r.oid.clone())
        .collect::<Vec<_>>();
    assert_eq!(git::history(&original, &refs).unwrap().len(), 2);
    run(&other, &["push", "origin", "--delete", "cloud-only"]);
    assert_eq!(
        git::verify_remote(&original, "origin").unwrap().refs.len(),
        1
    );
}

#[test]
fn commit_with_selected_untracked_file_preserves_other_staged_changes() {
    let temp = tempfile::tempdir().unwrap();
    let project = repository(temp.path());
    fs::write(temp.path().join("first"), "暂存\n").unwrap();
    fs::write(temp.path().join("new file"), "新增\n").unwrap();
    run(temp.path(), &["add", "first"]);
    let status = git::status(&project).unwrap();
    let chosen = status
        .files
        .iter()
        .filter(|f| f.group == "untracked")
        .cloned()
        .collect::<Vec<_>>();
    assert!(crate::agent::commit_request("这些改动可以提交吗？", &status, &chosen).is_none());
    assert!(
        crate::agent::commit_request("请提交选中的改动", &status, &chosen)
            .unwrap()
            .is_ok()
    );
    git::commit(&project, &status, &chosen, "添加新文件").unwrap();
    assert_eq!(run(temp.path(), &["show", "HEAD:new file"]), "新增");
    assert_eq!(
        run(temp.path(), &["diff", "--cached", "--name-only"]),
        "first"
    );
}
