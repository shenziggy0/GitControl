use crate::workspace::Project;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs,
    io::{Read, Seek, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn tool_path() -> OsString {
    let mut dirs: Vec<_> = std::env::var_os("PATH")
        .map(|s| std::env::split_paths(&s).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend([
            home.join(".local/bin"),
            home.join("bin"),
            home.join(".cargo/bin"),
        ]);
    }
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .map(PathBuf::from),
    );
    std::env::join_paths(dirs).unwrap_or_else(|_| "/usr/bin:/bin".into())
}

pub fn command(program: impl AsRef<std::ffi::OsStr>, path: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(path)
        .env("PATH", tool_path())
        .env("LC_ALL", "en_US.UTF-8");
    for var in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_NAMESPACE",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_OBJECT_DIRECTORY",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn git_cmd(path: &Path) -> Command {
    let mut cmd = command("git", path);
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .args([
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=false",
        ]);
    cmd
}

fn output(cmd: Command, timeout: u64) -> Result<String, String> {
    run_git(cmd, timeout, Stdio::null())
}

fn output_with_input(cmd: Command, timeout: u64, input: &str) -> Result<String, String> {
    // File-backed stdin lets Git read large batches without blocking the timeout loop.
    let mut file = tempfile::tempfile().map_err(|e| e.to_string())?;
    file.write_all(input.as_bytes())
        .and_then(|_| file.rewind())
        .map_err(|e| e.to_string())?;
    run_git(cmd, timeout, Stdio::from(file))
}

fn run_git(mut cmd: Command, timeout: u64, input: Stdio) -> Result<String, String> {
    let mut child = cmd
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    fn drain(mut stream: impl Read) -> Vec<u8> {
        let mut out = Vec::new();
        let _ = stream.read_to_end(&mut out);
        out
    }
    let out = thread::spawn(move || drain(stdout));
    let err = thread::spawn(move || drain(stderr));
    let start = Instant::now();
    let status = loop {
        if let Some(code) = child.try_wait().map_err(|e| e.to_string())? {
            break code;
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            return Err("Git 命令超时。".into());
        }
        thread::sleep(Duration::from_millis(12));
    };
    let text = out.join().map_err(|_| "Git 输出读取失败")?;
    let errors = err.join().map_err(|_| "Git 错误输出读取失败")?;
    if status.success() {
        Ok(String::from_utf8_lossy(&text).into_owned())
    } else {
        let errors = String::from_utf8_lossy(&errors);
        let detail = if errors.trim().is_empty() {
            String::from_utf8_lossy(&text).trim().to_string()
        } else {
            errors.trim().to_string()
        };
        Err(if detail.is_empty() {
            format!("Git 退出状态 {status}")
        } else {
            detail
        })
    }
}

pub fn read_at(path: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = git_cmd(path);
    cmd.arg("--no-optional-locks").args(args);
    output(cmd, 15).map(|s| s.trim_end_matches(['\r', '\n']).to_owned())
}
fn read(project: &Project, args: &[&str]) -> Result<String, String> {
    read_at(&project.path, args)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub old: Option<String>,
    pub group: String,
    pub code: String,
    pub stamp: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Branch {
    pub name: String,
    pub oid: String,
    pub remote: String,
    pub upstream: String,
    pub merge: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Tag {
    pub name: String,
    // The graph uses commit IDs. Tags on trees and blobs have no graph row.
    pub oid: Option<String>,
    pub unavailable: bool,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Status {
    pub kind: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub files: Vec<Change>,
    pub local: Vec<Branch>,
    pub tracking: Vec<Branch>,
    pub tags: Vec<Tag>,
    pub tags_error: Option<String>,
    pub remotes: Vec<String>,
}

fn file_stamp(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path)
        .map(|m| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                m.ino(),
                m.mode(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime_nsec()
            )
        })
        .unwrap_or_default()
}
fn parse_branches(text: &str) -> Vec<Branch> {
    text.lines()
        .filter_map(|line| {
            let cols: Vec<_> = line.split('\0').collect();
            if cols.len() != 6 {
                return None;
            }
            let name = cols[0]
                .strip_prefix("refs/heads/")
                .or_else(|| cols[0].strip_prefix("refs/remotes/"))?;
            if !cols[5].is_empty() {
                return None;
            }
            Some(Branch {
                name: name.into(),
                oid: cols[1].into(),
                remote: cols[2].into(),
                merge: cols[3].into(),
                upstream: cols[4].into(),
            })
        })
        .collect()
}
fn read_tags(project: &Project) -> Result<Vec<Tag>, String> {
    let format = "--format=%(refname:strip=2)%00%(objectname)";
    let refs = read(project, &["for-each-ref", format, "refs/tags"])?;
    let invalid = || "无法读取 Git 标签。".to_owned();
    let mut names = Vec::new();
    let mut input = String::new();
    for line in refs.lines() {
        let (name, oid) = line.split_once('\0').ok_or_else(invalid)?;
        if !valid_oid(oid) {
            return Err(invalid());
        }
        names.push(name);
        // Peel all annotated tag layers. Git reports missing objects per input.
        input.push_str(oid);
        input.push_str("^{}\n");
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }

    let mut cmd = git_cmd(&project.path);
    cmd.args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]);
    let output = output_with_input(cmd, 15, &input)?;
    let targets = output.lines();
    if targets.clone().count() != names.len() {
        return Err(invalid());
    }
    names
        .into_iter()
        .zip(targets)
        .map(|(name, target)| {
            let (oid, kind) = target.split_once(' ').ok_or_else(invalid)?;
            let (oid, unavailable) = match kind {
                "commit" if valid_oid(oid) => (Some(oid.to_owned()), false),
                "tree" | "blob" if valid_oid(oid) => (None, false),
                "missing" => (None, true),
                _ => return Err(invalid()),
            };
            Ok(Tag {
                name: name.to_owned(),
                oid,
                unavailable,
            })
        })
        .collect()
}

pub fn status(project: &Project) -> Result<Status, String> {
    status_with_tags(project, true)
}

fn status_with_tags(project: &Project, include_tags: bool) -> Result<Status, String> {
    if !project.path.is_dir() {
        return Err("项目目录已移动或不可访问。".into());
    }
    let result = read(
        project,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ],
    );
    let mut status = Status {
        kind: "git".into(),
        branch: None,
        head: None,
        files: vec![],
        local: vec![],
        tracking: vec![],
        tags: vec![],
        tags_error: None,
        remotes: vec![],
    };
    let raw = match result {
        Ok(raw) => raw,
        Err(err) if err.contains("not a git repository") => {
            status.kind = "folder".into();
            return Ok(status);
        }
        Err(err) => return Err(err),
    };
    let mut records = raw.split('\0');
    while let Some(line) = records.next() {
        if let Some(b) = line.strip_prefix("# branch.head ") {
            if b != "(detached)" {
                status.branch = Some(b.into());
            }
            continue;
        }
        if let Some(oid) = line.strip_prefix("# branch.oid ") {
            if oid != "(initial)" {
                status.head = Some(oid.into());
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("? ") {
            status.files.push(Change {
                path: path.into(),
                old: None,
                group: "untracked".into(),
                code: "?".into(),
                stamp: file_stamp(&project.path.join(path)),
            });
            continue;
        }
        let prefix = match line.as_bytes().first() {
            Some(b'1') => 9,
            Some(b'2') => 10,
            Some(b'u') => 11,
            _ => continue,
        };
        let fields: Vec<_> = line.splitn(prefix, ' ').collect();
        if fields.len() != prefix {
            continue;
        }
        let path = fields[prefix - 1];
        let old = if prefix == 10 {
            records.next().map(str::to_owned)
        } else {
            None
        };
        let flags = fields[1].as_bytes();
        if flags.len() != 2 {
            continue;
        }
        let groups = if prefix == 11 {
            vec![("conflict", b'U')]
        } else {
            vec![("staged", flags[0]), ("unstaged", flags[1])]
        };
        for (group, code) in groups {
            if code == b'.' {
                continue;
            }
            let stamp = if group == "staged" {
                fields[..prefix - 1].join(" ")
            } else {
                format!(
                    "{} {}",
                    fields[..prefix - 1].join(" "),
                    file_stamp(&project.path.join(path))
                )
            };
            status.files.push(Change {
                path: path.into(),
                old: old.clone(),
                group: group.into(),
                code: (code as char).to_string(),
                stamp,
            });
        }
    }
    let format="--format=%(refname)%00%(objectname)%00%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream:short)%00%(symref)";
    status.local = parse_branches(&read(project, &["for-each-ref", format, "refs/heads"])?);
    status.tracking = parse_branches(&read(project, &["for-each-ref", format, "refs/remotes"])?);
    if include_tags {
        match read_tags(project) {
            Ok(tags) => status.tags = tags,
            Err(error) => status.tags_error = Some(error),
        }
    }
    status.remotes = read(project, &["remote"])?
        .lines()
        .map(str::to_owned)
        .collect();
    Ok(status)
}

#[derive(Clone, Deserialize, Serialize)]
pub struct RemoteRef {
    pub name: String,
    pub oid: String,
    pub available: bool,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Remote {
    pub name: String,
    pub timestamp: u64,
    pub refs: Vec<RemoteRef>,
    pub error: Option<String>,
    pub history_error: Option<String>,
}
pub fn remote_identity(project: &Project, name: &str) -> Result<String, String> {
    use std::hash::{Hash, Hasher};
    let url = read(project, &["remote", "get-url", "--", name])?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hash);
    Ok(format!("{:016x}", hash.finish()))
}

fn remote_refs(project: &Project, name: &str) -> Result<Vec<RemoteRef>, String> {
    let mut cmd = git_cmd(&project.path);
    cmd.args(["ls-remote", "--heads", "--", name]);
    let text = output(cmd, 15).map_err(|error| {
        let error = error.to_lowercase();
        if [
            "authentication failed",
            "permission denied",
            "could not read username",
            "terminal prompts disabled",
            "repository not found",
            "publickey",
        ]
        .iter()
        .any(|word| error.contains(word))
        {
            "云端认证失败或没有仓库访问权限。".to_owned()
        } else {
            "无法连接远端，请检查网络与 Git 设置。".to_owned()
        }
    })?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let (oid, reference) = line.split_once('\t')?;
            let branch = reference.strip_prefix("refs/heads/")?;
            if !valid_oid(oid) {
                return None;
            }
            Some(RemoteRef {
                name: format!("{name}/{branch}"),
                oid: oid.into(),
                available: has_commit(project, oid),
            })
        })
        .collect())
}
pub fn valid_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn has_commit(p: &Project, oid: &str) -> bool {
    read(p, &["cat-file", "-e", &format!("{oid}^{{commit}}")]).is_ok()
}
pub fn verify_remote(p: &Project, name: &str) -> Result<Remote, String> {
    let refs = remote_refs(p, name)?;
    Ok(Remote {
        name: name.into(),
        timestamp: now(),
        refs,
        error: None,
        history_error: None,
    })
}
pub fn hydrate_remote(p: &Project, remote: &mut Remote) {
    for attempt in 0..2 {
        let mut missing = remote.refs.iter().filter(|r| !r.available).peekable();
        if missing.peek().is_none() {
            return;
        }
        let mut cmd = git_cmd(&p.path);
        cmd.args([
            "-c",
            "fetch.writeCommitGraph=false",
            "fetch",
            "--no-write-fetch-head",
            "--no-tags",
            "--no-prune",
            "--no-recurse-submodules",
            "--no-auto-gc",
            "--refmap=",
            "--",
            &remote.name,
        ]);
        for r in missing {
            if let Some(branch) = r.name.strip_prefix(&format!("{}/", remote.name)) {
                cmd.arg(format!("refs/heads/{branch}"));
            }
        }
        let fetched = output(cmd, 45).is_ok();
        for r in &mut remote.refs {
            r.available = has_commit(p, &r.oid);
        }
        if remote.refs.iter().all(|r| r.available) {
            return;
        }
        if fetched && attempt == 0 {
            if let Ok(refs) = remote_refs(p, &remote.name) {
                remote.refs = refs;
                remote.timestamp = now();
                continue;
            }
        }
        break;
    }
    remote.history_error = Some("部分云端历史暂不可用，刷新后可重试。".into());
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Serialize)]
pub struct Commit {
    pub sha: String,
    pub date: String,
    pub subject: String,
    pub author: String,
    pub parents: String,
}
const COMMIT_FORMAT: &str = "--format=%H%x00%as%x00%s%x00%an%x00%P";

fn parse_commit(line: &str) -> Option<Commit> {
    let mut fields = line.split('\0');
    Some(Commit {
        sha: fields.next()?.into(),
        date: fields.next()?.into(),
        subject: fields.next()?.into(),
        author: fields.next()?.into(),
        parents: fields.next()?.into(),
    })
}

pub fn commit_summary(p: &Project, oid: &str) -> Result<Commit, String> {
    if !valid_oid(oid) || read(p, &["cat-file", "-t", oid])?.trim() != "commit" {
        return Err("无效提交 ID。".into());
    }
    read(p, &["show", "--no-patch", COMMIT_FORMAT, oid, "--"])?
        .lines()
        .next()
        .and_then(parse_commit)
        .ok_or_else(|| "无法读取提交摘要。".into())
}

pub fn history(p: &Project, tips: &[String]) -> Result<Vec<Commit>, String> {
    let mut seen = HashSet::new();
    let mut input = String::new();
    for tip in tips {
        if !valid_oid(tip) {
            return Err("无效提交 ID。".into());
        }
        if seen.insert(tip) {
            input.push_str(tip);
            input.push('\n');
        }
    }
    if seen.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = git_cmd(&p.path);
    cmd.args(["log", "--date-order", COMMIT_FORMAT, "--stdin", "--"]);
    Ok(output_with_input(cmd, 45, &input)?
        .lines()
        .filter_map(parse_commit)
        .collect())
}

pub fn divergence(p: &Project, local: &str, upstream: &str) -> Option<Vec<u32>> {
    if !valid_oid(local) || !valid_oid(upstream) {
        return None;
    }
    if local == upstream {
        return Some(vec![0, 0]);
    }
    if read(p, &["rev-parse", "--is-shallow-repository"])
        .ok()?
        .trim()
        == "true"
    {
        return None;
    }
    let result: Vec<u32> = read(
        p,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{local}...{upstream}"),
        ],
    )
    .ok()?
    .split_whitespace()
    .map(str::parse)
    .collect::<Result<_, _>>()
    .ok()?;
    if result.len() == 2 {
        Some(result)
    } else {
        None
    }
}

pub fn commit_files(p: &Project, oid: &str) -> Result<Vec<BTreeMap<&'static str, String>>, String> {
    if !valid_oid(oid) {
        return Err("无效提交 ID。".into());
    }
    let raw = read(
        p,
        &[
            "show",
            "--format=",
            "--first-parent",
            "--diff-merges=first-parent",
            "--no-renames",
            "--no-ext-diff",
            "--numstat",
            "-z",
            oid,
            "--",
        ],
    )?;
    Ok(raw
        .split('\0')
        .filter_map(|line| {
            let f: Vec<_> = line.splitn(3, '\t').collect();
            if f.len() != 3 {
                return None;
            }
            Some(BTreeMap::from([
                ("plus", f[0].into()),
                ("minus", f[1].into()),
                ("path", f[2].into()),
            ]))
        })
        .collect())
}
pub fn patch(p: &Project, oid: Option<&str>, path: &str, group: &str) -> Result<String, String> {
    patch_text(p, oid, path, group, None).map(|(text, _)| bounded_patch(&text))
}

fn patch_text(
    p: &Project,
    oid: Option<&str>,
    path: &str,
    group: &str,
    old: Option<&str>,
) -> Result<(String, bool), String> {
    if let Some(oid) = oid {
        if !valid_oid(oid) {
            return Err("无效提交 ID。".into());
        }
        let mut args = vec![
            "show",
            "--format=",
            "--first-parent",
            "--diff-merges=first-parent",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            oid,
            "--",
            path,
        ];
        if let Some(old) = old.filter(|old| *old != path) {
            args.push(old);
        }
        return read(p, &args).map(|text| (text, false));
    }
    if group == "untracked" {
        let path = p.path.join(path);
        if fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .is_symlink()
        {
            return Ok((
                format!(
                    "链接 → {}",
                    fs::read_link(path).map_err(|e| e.to_string())?.display()
                ),
                false,
            ));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|f| f.take(131073).read_to_end(&mut bytes))
            .map_err(|e| e.to_string())?;
        let truncated = bytes.len() > 131072;
        bytes.truncate(131072);
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) if !bytes.contains(&0) => text.to_owned(),
            Err(e) if truncated && e.error_len().is_none() => {
                String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
            }
            _ => "二进制文件".into(),
        };
        return Ok((text, truncated));
    }
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--find-renames"];
    if group == "staged" {
        args.push("--cached");
    }
    args.extend(["--", path]);
    if let Some(old) = old.filter(|old| *old != path) {
        args.push(old);
    }
    read(p, &args).map(|text| (text, false))
}

fn bounded_patch(raw: &str) -> String {
    raw.lines()
        .take(500)
        .map(|s| s.chars().take(1200).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

mod commit;
pub use commit::commit;

mod workbench;
pub use workbench::*;
