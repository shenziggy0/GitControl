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
fn sidebar_tags_locate_their_commit_including_tag_only_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    let project = repository(path);
    run(path, &["switch", "--detach"]);
    run(path, &["commit", "--allow-empty", "-m", "仅标签保留的提交"]);
    let target = run(path, &["rev-parse", "HEAD"]);
    run(path, &["tag", "light"]);
    run(path, &["tag", "-a", "release", "-m", "版本说明"]);
    run(path, &["tag", "-a", "nested", "release", "-m", "嵌套标签"]);
    run(path, &["tag", "file", "HEAD:first"]);
    run(path, &["switch", "main"]);

    let status = git::status(&project).unwrap();
    let tags = status.tags;
    assert_eq!(tags.len(), 4);
    for name in ["release", "nested", "light"] {
        let tag = tags.iter().find(|tag| tag.name == name).unwrap();
        assert_eq!(tag.oid.as_deref(), Some(target.as_str()), "{name}");
        assert!(!tag.unavailable, "{name}");
    }
    let file = tags.iter().find(|tag| tag.name == "file").unwrap();
    assert!(file.oid.is_none());
    assert!(!file.unavailable);
    let tips: Vec<_> = status
        .head
        .into_iter()
        .chain(tags.into_iter().filter_map(|tag| tag.oid))
        .collect();
    assert!(git::history(&project, &tips)
        .unwrap()
        .iter()
        .any(|commit| commit.sha == target));
}

#[test]
fn missing_tag_object_keeps_the_workbench_and_other_tags_available() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    let project = repository(path);
    run(path, &["tag", "healthy"]);
    run(path, &["tag", "-a", "missing", "-m", "待删除的标签对象"]);
    let oid = run(path, &["rev-parse", "refs/tags/missing"]);
    let (directory, object) = oid.split_at(2);
    fs::remove_file(path.join(".git/objects").join(directory).join(object)).unwrap();
    fs::write(path.join("first"), "修改\n").unwrap();

    let status = git::status(&project).unwrap();
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.files[0].path, "first");
    assert!(status.tags_error.is_none());
    assert_eq!(status.tags.len(), 2);
    assert_eq!(status.tags[0].name, "healthy");
    assert_eq!(status.tags[0].oid, status.head);
    assert_eq!(status.tags[1].name, "missing");
    assert!(status.tags[1].oid.is_none());
    assert!(status.tags[1].unavailable);
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

#[test]
fn rfc0006_reads_diff_origin_and_finds_unopened_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let project = repository(temp.path());
    fs::write(project.path.join("first"), "两瓶水\n").unwrap();
    run(&project.path, &["commit", "-am", "更新用水清单"]);
    let head = run(&project.path, &["rev-parse", "HEAD"]);
    let preview = git::diff_preview(&project, Some(&head), "first", "", None).unwrap();
    assert!(preview
        .lines
        .iter()
        .any(|line| line.new == Some(1) && line.text == "两瓶水"));
    assert_eq!(
        git::line_origin(&project, &head, "first", 1)
            .unwrap()
            .commit
            .sha,
        head
    );
    assert_eq!(
        git::search_local(&project, "用水")
            .unwrap()
            .iter()
            .find_map(|hit| hit.commit.as_ref())
            .unwrap()
            .sha,
        head
    );
}

#[test]
fn rfc0006_push_uses_the_previewed_target_and_rejects_a_stale_preview() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("project");
    fs::create_dir(&path).unwrap();
    let project = repository(&path);
    let remote = temp.path().join("remote.git");
    fs::create_dir(&remote).unwrap();
    run(&remote, &["init", "--bare", "-b", "main"]);
    run(
        &path,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    run(&path, &["push", "-u", "origin", "main"]);
    fs::write(path.join("first"), "下一份改动\n").unwrap();
    run(&path, &["commit", "-am", "更新第一份改动"]);
    let preview = git::sync_preview(&project, true).unwrap();
    assert_eq!(preview.commits.len(), 1);
    let head = run(&path, &["rev-parse", "HEAD"]);
    assert_eq!(git::push_preview(&project, &preview).unwrap(), head);
    assert_eq!(run(&remote, &["rev-parse", "main"]), head);
    assert!(git::push_preview(&project, &preview).is_err());
}

#[test]
fn rfc0006_snapshot_restores_only_the_selected_file_and_supports_undo() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("project");
    fs::create_dir(&path).unwrap();
    let project = repository(&path);
    let store = temp.path().join("data");
    fs::write(path.join("first"), "快照内容\n").unwrap();
    run(&path, &["add", "first"]);
    fs::write(path.join("second"), "别动\n").unwrap();
    let snapshot = super::snapshots::create(&store, &project, "改动前").unwrap();
    fs::write(path.join("first"), "之后的内容\n").unwrap();
    let preview = super::snapshots::preview(&store, &project, &snapshot.id).unwrap();
    let row = preview.iter().find(|row| row.path == "first").unwrap();
    let undo =
        super::snapshots::restore(&store, &project, &snapshot.id, "first", &row.version).unwrap();
    assert_eq!(
        fs::read_to_string(path.join("first")).unwrap(),
        "快照内容\n"
    );
    assert_eq!(fs::read_to_string(path.join("second")).unwrap(), "别动\n");
    assert_eq!(run(&path, &["show", ":first"]), "快照内容");
    let preview = super::snapshots::preview(&store, &project, &undo.id).unwrap();
    let row = preview.iter().find(|row| row.path == "first").unwrap();
    super::snapshots::restore(&store, &project, &undo.id, "first", &row.version).unwrap();
    assert_eq!(
        fs::read_to_string(path.join("first")).unwrap(),
        "之后的内容\n"
    );
}

#[test]
fn rfc0006_pushurl_is_not_rewritten_again_when_pushing() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("work");
    fs::create_dir(&path).unwrap();
    let project = repository(&path);
    let receive = temp.path().join("receive.git");
    let other = temp.path().join("other.git");
    for remote in [&receive, &other] {
        fs::create_dir(remote).unwrap();
        run(remote, &["init", "--bare", "-b", "main"]);
        run(&path, &["push", remote.to_str().unwrap(), "main"]);
    }
    let original = run(&path, &["rev-parse", "HEAD"]);
    run(&path, &["remote", "add", "origin", other.to_str().unwrap()]);
    run(&path, &["config", "branch.main.remote", "origin"]);
    run(&path, &["config", "branch.main.merge", "refs/heads/main"]);
    run(
        &path,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            receive.to_str().unwrap(),
        ],
    );
    let key = format!("url.{}.pushInsteadOf", other.display());
    run(&path, &["config", &key, receive.to_str().unwrap()]);
    fs::write(path.join("first"), "只去接收端\n").unwrap();
    run(&path, &["commit", "-am", "检查实际推送地址"]);
    let preview = git::sync_preview(&project, true).unwrap();
    let head = run(&path, &["rev-parse", "HEAD"]);
    assert_eq!(git::push_preview(&project, &preview).unwrap(), head);
    assert_eq!(run(&receive, &["rev-parse", "main"]), head);
    assert_eq!(run(&other, &["rev-parse", "main"]), original);
}

#[test]
fn rfc0006_line_origin_preserves_index_trailing_lines_and_rename_sides() {
    let temp = tempfile::tempdir().unwrap();
    let project = repository(temp.path());
    fs::write(project.path.join("first"), "开头\n\n\n").unwrap();
    run(&project.path, &["commit", "-am", "保留末尾空行"]);
    let head = run(&project.path, &["rev-parse", "HEAD"]);
    assert_eq!(
        git::index_line_origin(&project, "first", 3)
            .unwrap()
            .unwrap()
            .commit
            .sha,
        head
    );
    run(&project.path, &["mv", "first", "新名字"]);
    fs::write(project.path.join("新名字"), "另一行\n\n\n").unwrap();
    let diff = git::diff_preview(&project, None, "新名字", "unstaged", Some("first")).unwrap();
    assert_eq!(diff.old_path, "新名字");
    assert_eq!(diff.new_path, "新名字");
    assert!(diff
        .lines
        .iter()
        .any(|line| line.old == Some(3) && line.new == Some(3)));
}
