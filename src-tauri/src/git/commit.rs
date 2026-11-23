use super::*;
use std::{fs::OpenOptions, io::ErrorKind};

struct IndexLock(PathBuf, bool);
impl Drop for IndexLock {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_file(&self.0);
        }
    }
}

fn index_git(project: &Project, index: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = git_cmd(&project.path);
    cmd.env("GIT_INDEX_FILE", index).args(args);
    output(cmd, 90)
}

fn copy_entry(project: &Project, source: &Path, target: &Path, path: &str) -> Result<(), String> {
    let out = index_git(project, source, &["ls-files", "--stage", "-z", "--", path])?;
    let mut entries = out
        .split('\0')
        .filter_map(|entry| entry.split_once('\t'))
        .filter(|(_, candidate)| *candidate == path);
    if let Some((fields, _)) = entries.next() {
        let fields: Vec<_> = fields.split_whitespace().collect();
        if fields.len() != 3 || fields[2] != "0" || entries.next().is_some() {
            return Err("索引包含未解决的冲突。".into());
        }
        index_git(
            project,
            target,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                fields[0],
                fields[1],
                path,
            ],
        )?;
    } else {
        index_git(
            project,
            target,
            &["update-index", "--force-remove", "--", path],
        )?;
    }
    Ok(())
}

pub fn commit(
    project: &Project,
    expected: &Status,
    chosen: &[Change],
    message: &str,
) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty()
        || !message
            .chars()
            .any(|c| matches!(c, '\u{3400}'..='\u{9fff}'))
    {
        return Err("提交说明需要包含中文。".into());
    }
    if chosen.is_empty() {
        return Err("请明确要提交的改动。".into());
    }
    let git_dir = PathBuf::from(read(project, &["rev-parse", "--absolute-git-dir"])?);
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        if git_dir.join(marker).exists() {
            return Err("当前有进行中的 Git 操作，请在终端处理后再提交。".into());
        }
    }
    let index = git_dir.join("index");
    let mut lock = IndexLock(git_dir.join("index.lock"), false);
    let mut lock_file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock.0)
    {
        Ok(file) => {
            lock.1 = true;
            file
        }
        Err(err) => {
            // A lock created by another Git process is never ours to remove.
            return Err(format!("无法锁定工作树索引：{err}"));
        }
    };
    let current = status(project)?;
    let valid = |next: &Status| {
        next.head == expected.head
            && next.branch == expected.branch
            && chosen.iter().all(|c| next.files.contains(c))
            && !next.files.iter().any(|c| c.group == "conflict")
    };
    if !valid(&current) || expected.branch.is_none() {
        return Err("分支、索引或选中的内容已改变，或者存在冲突。请刷新后重试。".into());
    }
    let temp = tempfile::tempdir_in(&git_dir).map_err(|e| e.to_string())?;
    let snapshot = temp.path().join("original-index");
    match fs::copy(&index, &snapshot) {
        Ok(_) => (),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            index_git(project, &snapshot, &["read-tree", "--empty"])?;
        }
        Err(e) => return Err(e.to_string()),
    }
    let target = temp.path().join("commit-index");
    if let Some(head) = &expected.head {
        index_git(project, &target, &["read-tree", head])?;
    } else {
        index_git(project, &target, &["read-tree", "--empty"])?;
    }
    let mut staged = HashSet::new();
    let mut working = HashSet::new();
    for change in chosen {
        if change.group == "staged" {
            staged.insert(change.path.as_str());
            if let Some(old) = change.old.as_deref() {
                staged.insert(old);
            }
        } else {
            if current
                .files
                .iter()
                .any(|c| c.group == "staged" && c.path == change.path)
                && !chosen
                    .iter()
                    .any(|c| c.group == "staged" && c.path == change.path)
            {
                return Err(format!(
                    "{} 同时有暂存和未暂存内容，请明确提交哪一组。",
                    change.path
                ));
            }
            working.insert(change.path.as_str());
            if let Some(old) = change.old.as_deref() {
                working.insert(old);
            }
        }
    }
    for path in staged {
        copy_entry(project, &snapshot, &target, path)?;
    }
    for path in &working {
        index_git(project, &target, &["add", "-A", "--", path])?;
    }
    if !valid(&status(project)?) {
        return Err("准备提交期间内容发生变化，请刷新并重新选择。".into());
    }
    let tree = index_git(project, &target, &["write-tree"])?
        .trim()
        .to_string();
    let mut cmd = git_cmd(&project.path);
    cmd.env("GIT_INDEX_FILE", &target)
        .env("GIT_EDITOR", "true")
        .args(["commit", "-m", message]);
    let result = output(cmd, 90);
    let head = read(project, &["rev-parse", "HEAD"]).ok();
    let Some(head) = head.filter(|h| Some(h) != expected.head.as_ref()) else {
        return Err(result.err().unwrap_or_else(|| "无法核对提交结果。".into()));
    };
    let committed_tree = read(project, &["show", "-s", "--format=%T", &head])?;
    let parent = read(project, &["show", "-s", "--format=%P", &head])?;
    if committed_tree != tree || parent != expected.head.as_deref().unwrap_or("") {
        return Err(format!(
            "HEAD 现在是 {head}，提交结果与预期范围不同，请核对历史与暂存区。"
        ));
    }
    if !working.is_empty() {
        let reconcile = (|| -> Result<(), String> {
            for path in working {
                copy_entry(project, &target, &snapshot, path)?;
            }
            let bytes = fs::read(&snapshot).map_err(|e| e.to_string())?;
            lock_file
                .write_all(&bytes)
                .and_then(|_| lock_file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&lock.0, &index).map_err(|e| e.to_string())?;
            lock.1 = false;
            Ok(())
        })();
        if let Err(error) = reconcile {
            return Err(format!("提交 {head} 已生成，暂存区同步失败：{error}"));
        }
    }
    Ok(head)
}
