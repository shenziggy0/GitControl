use super::*;

#[derive(Serialize)]
pub struct DiffLine {
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
    pub kind: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPreview {
    pub raw: String,
    pub lines: Vec<DiffLine>,
    pub kind: &'static str,
    pub truncated: bool,
    pub old_path: String,
    pub new_path: String,
}

fn hunk_start(range: &str) -> Option<u32> {
    range.get(1..)?.split(',').next()?.parse().ok()
}

pub fn diff_preview(
    project: &Project,
    oid: Option<&str>,
    path: &str,
    group: &str,
    old: Option<&str>,
) -> Result<DiffPreview, String> {
    let rename = oid
        .map(|oid| historical_rename(project, oid, path))
        .transpose()?
        .flatten();
    let (before, after) = rename
        .as_ref()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .unwrap_or((
            if group == "staged" {
                old.unwrap_or(path)
            } else {
                path
            },
            path,
        ));
    let (text, byte_limit) = patch_text(project, oid, after, group, Some(before))?;
    let kind = if text.starts_with("链接 → ") {
        "link"
    } else if text == "二进制文件" || text.lines().any(|line| line.starts_with("Binary files "))
    {
        "binary"
    } else if oid.is_none() && group == "untracked" {
        "new"
    } else {
        "patch"
    };
    let truncated = byte_limit
        || text.lines().count() > 500
        || text
            .lines()
            .take(500)
            .any(|line| line.chars().count() > 1200);
    let raw = bounded_patch(&text);
    let old_path = before.to_owned();
    let mut lines = Vec::new();
    let (mut old_line, mut new_line) = (0, 0);
    let mut in_hunk = false;
    for (index, line) in raw.lines().enumerate() {
        if kind == "new" {
            lines.push(DiffLine {
                old: None,
                new: Some(index as u32 + 1),
                text: line.into(),
                kind: "add",
            });
            continue;
        }
        if line.starts_with("diff --git ") {
            in_hunk = false;
        }
        if line.starts_with("@@ ") {
            let mut parts = line.split_whitespace().skip(1);
            if let (Some(a), Some(b)) = (
                parts.next().and_then(hunk_start),
                parts.next().and_then(hunk_start),
            ) {
                (old_line, new_line) = (a, b);
                in_hunk = true;
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (before, after, label) = match line.as_bytes().first() {
            Some(b'-') => (true, false, "remove"),
            Some(b'+') => (false, true, "add"),
            Some(b' ') => (true, true, "context"),
            _ => continue,
        };
        lines.push(DiffLine {
            old: before.then_some(old_line),
            new: after.then_some(new_line),
            text: line[1..].into(),
            kind: label,
        });
        old_line += u32::from(before);
        new_line += u32::from(after);
    }
    Ok(DiffPreview {
        raw,
        lines,
        kind,
        truncated,
        old_path,
        new_path: after.to_owned(),
    })
}

fn historical_rename(
    p: &Project,
    oid: &str,
    path: &str,
) -> Result<Option<(String, String)>, String> {
    if !valid_oid(oid) {
        return Err("无效提交 ID。".into());
    }
    let text = read(
        p,
        &[
            "show",
            "--format=",
            "--diff-merges=first-parent",
            "--name-status",
            "-z",
            "-M",
            oid,
            "--",
        ],
    )?;
    let mut parts = text.trim_start_matches('\n').split('\0');
    while let Some(kind) = parts.next().filter(|kind| !kind.is_empty()) {
        let first = parts.next().ok_or("无法读取重命名路径。")?;
        if kind.starts_with('R') {
            let second = parts.next().ok_or("无法读取重命名路径。")?;
            if first == path || second == path {
                return Ok(Some((first.into(), second.into())));
            }
        }
    }
    Ok(None)
}

#[derive(Serialize)]
pub struct LineOrigin {
    pub commit: Commit,
    pub path: String,
    pub line: u32,
}

fn blame(
    p: &Project,
    anchor: Option<&str>,
    path: &str,
    line: u32,
    contents: Option<&Path>,
) -> Result<Option<LineOrigin>, String> {
    if line == 0 || anchor.is_some_and(|oid| !valid_oid(oid)) {
        return Err("无效的版本或行号。".into());
    }
    let range = format!("{line},{line}");
    let mut cmd = git_cmd(&p.path);
    cmd.args(["blame", "--line-porcelain", "-L", &range]);
    if let Some(contents) = contents {
        cmd.arg("--contents").arg(contents);
    }
    if let Some(anchor) = anchor {
        cmd.arg(anchor);
    }
    cmd.args(["--", path]);
    let text = output(cmd, 20)?;
    let mut header = text.lines().next().unwrap_or("").split_whitespace();
    let oid = header.next().ok_or("未能读取行来源。")?;
    if oid.bytes().all(|b| b == b'0') {
        return Ok(None);
    }
    let original = header
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or("未能读取原始行号。")?;
    let name = text
        .lines()
        .find_map(|row| row.strip_prefix("filename "))
        .unwrap_or(path);
    Ok(Some(LineOrigin {
        commit: commit_summary(p, oid)?,
        path: name.into(),
        line: original,
    }))
}

pub fn line_origin(p: &Project, oid: &str, path: &str, line: u32) -> Result<LineOrigin, String> {
    blame(p, Some(oid), path, line, None)?.ok_or("该行尚未提交。".into())
}

pub fn index_line_origin(p: &Project, path: &str, line: u32) -> Result<Option<LineOrigin>, String> {
    if status(p)?.head.is_none() {
        return Ok(None);
    }
    let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let mut cmd = git_cmd(&p.path);
    cmd.args(["show", &format!(":{path}")]);
    let blob = output(cmd, 15)?;
    file.write_all(blob.as_bytes()).map_err(|e| e.to_string())?;
    blame(p, None, path, line, Some(file.path()))
}

#[derive(Serialize)]
pub struct SearchHit {
    pub branch: Option<Branch>,
    pub cloud: bool,
    pub commit: Option<Commit>,
}

pub fn search_local(p: &Project, query: &str) -> Result<Vec<SearchHit>, String> {
    let query = query.trim();
    if query.is_empty() || query.len() > 240 {
        return Ok(Vec::new());
    }
    let q = query.to_lowercase();
    let format = "--format=%(refname)%00%(objectname)%00%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream:short)%00%(symref)";
    let mut hits = Vec::new();
    for (namespace, cloud) in [("refs/heads", false), ("refs/remotes", true)] {
        for branch in parse_branches(&read(p, &["for-each-ref", format, namespace])?) {
            if branch.name.to_lowercase().contains(&q) && hits.len() < 12 {
                hits.push(SearchHit {
                    branch: Some(branch),
                    cloud,
                    commit: None,
                });
            }
        }
    }
    let mut seen = HashSet::new();
    if q.len() >= 4 && q.bytes().all(|b| b.is_ascii_hexdigit()) {
        let candidates = read(p, &["rev-parse", &format!("--disambiguate={q}")])?;
        for oid in candidates.lines().take(24) {
            if let Ok(commit) = commit_summary(p, oid) {
                if seen.insert(commit.sha.clone()) && hits.len() < 12 {
                    hits.push(SearchHit {
                        branch: None,
                        cloud: false,
                        commit: Some(commit),
                    });
                }
            }
        }
    }
    if hits.len() < 12 {
        let text = read(
            p,
            &[
                "log",
                "--all",
                "-i",
                "-F",
                &format!("--grep={query}"),
                "-n",
                "48",
                COMMIT_FORMAT,
                "--",
            ],
        )?;
        for commit in text.lines().filter_map(parse_commit) {
            if commit.subject.to_lowercase().contains(&q) && seen.insert(commit.sha.clone()) {
                hits.push(SearchHit {
                    branch: None,
                    cloud: false,
                    commit: Some(commit),
                });
            }
            if hits.len() == 12 {
                break;
            }
        }
    }
    Ok(hits)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildSummary {
    pub branch: Option<String>,
    pub files: usize,
    pub ahead: Option<u32>,
    pub timestamp: Option<u64>,
    pub source: &'static str,
}

pub fn child_summary(
    p: &Project,
    cached: &std::collections::HashMap<String, Remote>,
) -> Result<ChildSummary, String> {
    let state = status_with_tags(p, false)?;
    let branch = state
        .local
        .iter()
        .find(|b| Some(&b.name) == state.branch.as_ref());
    let files = state
        .files
        .iter()
        .map(|c| &c.path)
        .collect::<HashSet<_>>()
        .len();
    let mut summary = ChildSummary {
        branch: state.branch,
        files,
        ahead: None,
        timestamp: None,
        source: "unknown",
    };
    if let Some(branch) = branch.filter(|b| !b.upstream.is_empty()) {
        let cached = remote_identity(p, &branch.remote)
            .ok()
            .and_then(|id| cached.get(&format!("{}/{}/{id}", p.id, branch.remote)));
        let verified = cached
            .filter(|r| r.timestamp > 0 && r.error.is_none() && r.name == branch.remote)
            .and_then(|remote| {
                remote
                    .refs
                    .iter()
                    .find(|r| r.name == branch.upstream)
                    .map(|r| (remote.timestamp, r))
            });
        if let Some((timestamp, reference)) = verified {
            summary.timestamp = Some(timestamp);
            summary.ahead = divergence(p, &branch.oid, &reference.oid).map(|d| d[0]);
            summary.source = if summary.ahead.is_some() {
                "verified"
            } else {
                "unknown"
            };
        } else if let Some(reference) = state.tracking.iter().find(|r| r.name == branch.upstream) {
            summary.ahead = divergence(p, &branch.oid, &reference.oid).map(|d| d[0]);
            if summary.ahead.is_some() {
                summary.source = "tracking";
            }
        }
    } else if state.remotes.is_empty() {
        summary.source = "local";
    } else {
        summary.source = "no_upstream";
    }
    Ok(summary)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPreview {
    pub branch: String,
    pub local_oid: String,
    pub remote: String,
    pub remote_ref: String,
    pub remote_oid: String,
    pub target: String,
    pub commits: Vec<Commit>,
    pub can_push: bool,
    pub total: u32,
    pub timestamp: u64,
    #[serde(skip)]
    url: String,
}

fn target_oid(p: &Project, url: &str, reference: &str) -> Result<String, String> {
    let raw = read(
        p,
        &["ls-remote", "--exit-code", "--refs", "--", url, reference],
    )
    .map_err(|_| "无法核对目标分支，请检查远端分支、网络与 Git 认证。")?;
    raw.lines()
        .filter_map(|row| row.split_once('\t'))
        .find(|(oid, name)| *name == reference && valid_oid(oid))
        .map(|(oid, _)| oid.to_owned())
        .ok_or("目标分支不存在。".into())
}

fn safe_target(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or("");
    if let Some((scheme, rest)) = without_query.split_once("://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit('@').next().unwrap_or("");
        return if path.is_empty() {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}/{path}")
        };
    }
    without_query.rsplit('@').next().unwrap_or("").to_owned()
}

pub fn sync_preview(p: &Project, pushing: bool) -> Result<SyncPreview, String> {
    let current = status_with_tags(p, false)?;
    if current.remotes.is_empty() {
        return Err("尚未配置远端。".into());
    }
    let branch = current
        .local
        .iter()
        .find(|b| Some(&b.name) == current.branch.as_ref())
        .filter(|b| current.remotes.contains(&b.remote) && b.merge.starts_with("refs/heads/"))
        .ok_or("当前分支尚未关联远端上游。")?;
    let mode = if pushing {
        &["remote", "get-url", "--push", "--all", "--", &branch.remote][..]
    } else {
        &["remote", "get-url", "--all", "--", &branch.remote][..]
    };
    let url = read(p, mode)?;
    if url.is_empty() || url.contains('\n') {
        return Err("远端配置了多个地址，请在项目终端操作。".into());
    }
    if pushing
        && read(
            p,
            &[
                "config",
                "--bool",
                "--get",
                &format!("remote.{}.mirror", branch.remote),
            ],
        )
        .is_ok_and(|value| value == "true")
    {
        return Err("远端使用镜像推送，请在项目终端操作。".into());
    }
    if read(p, &["ls-remote", "--get-url", "--", &url])? != url {
        return Err("该远端地址会被再次重写，请在项目终端操作。".into());
    }
    let remote_oid = target_oid(p, &url, &branch.merge)?;
    let counts =
        divergence(p, &branch.oid, &remote_oid).ok_or("当前缺少远端历史，请刷新后再核对。")?;
    let (from, to, total) = if pushing {
        (&remote_oid, &branch.oid, counts[0])
    } else {
        (&branch.oid, &remote_oid, counts[1])
    };
    let range = format!("{from}..{to}");
    let commits = read(p, &["log", COMMIT_FORMAT, &range, "--"])?
        .lines()
        .filter_map(parse_commit)
        .collect();
    Ok(SyncPreview {
        branch: branch.name.clone(),
        local_oid: branch.oid.clone(),
        remote: branch.remote.clone(),
        remote_ref: branch.merge.clone(),
        remote_oid,
        target: safe_target(&url),
        commits,
        can_push: pushing && counts[0] > 0 && counts[1] == 0,
        total,
        timestamp: now(),
        url,
    })
}

pub fn push_preview(p: &Project, preview: &SyncPreview) -> Result<String, String> {
    if !preview.can_push {
        return Err("当前预览无法推送，请核对上下游状态。".into());
    }
    let current = sync_preview(p, true).map_err(|_| "无法确认预览目标或版本，请重新预览。")?;
    if !current.can_push
        || current.branch != preview.branch
        || current.local_oid != preview.local_oid
        || current.remote_oid != preview.remote_oid
        || current.remote_ref != preview.remote_ref
        || current.remote != preview.remote
        || current.url != preview.url
    {
        return Err("目标地址、分支或提交已改变，请重新预览。".into());
    }
    // The exact lease guards the interval after preflight. The ancestry check above permits fast-forward updates only.
    let lease = format!(
        "--force-with-lease={}:{}",
        preview.remote_ref, preview.remote_oid
    );
    let refs = format!("{}:{}", preview.local_oid, preview.remote_ref);
    let mut cmd = git_cmd(&p.path);
    cmd.args([
        "-c",
        "push.followTags=false",
        "push",
        "--porcelain",
        "--no-follow-tags",
        "--recurse-submodules=no",
        &lease,
        "--",
        &preview.remote,
        &refs,
    ]);
    let result = output(cmd, 75);
    match target_oid(p, &preview.url, &preview.remote_ref) {
        Ok(oid)
            if oid == preview.local_oid
                || divergence(p, &preview.local_oid, &oid).is_some_and(|counts| counts[0] == 0) =>
        {
            Ok(preview.local_oid.clone())
        }
        Ok(_) if result.is_err() => {
            Err("推送未到达目标。请重新预览，并检查远端更新及 Git 认证。".into())
        }
        _ => Err("推送结果待核对，请刷新实际目标后再决定是否重试。".into()),
    }
}

pub fn snapshot_paths(p: &Project) -> Result<std::collections::BTreeSet<String>, String> {
    let mut paths = std::collections::BTreeSet::new();
    let mut gitlinks = std::collections::BTreeSet::new();
    let mut classify = |raw: &str| {
        for (metadata, path) in raw.split('\0').filter_map(|row| row.split_once('\t')) {
            if metadata.starts_with("160000 ") {
                gitlinks.insert(path.to_owned());
            } else {
                paths.insert(path.to_owned());
            }
        }
    };
    classify(&read(p, &["ls-files", "--stage", "-z"])?);
    if let Some(head) = snapshot_head(p)? {
        classify(&read(p, &["ls-tree", "-r", "-z", &head])?);
    }
    let others = read(p, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    paths.extend(
        others
            .split('\0')
            .filter(|path| !path.is_empty() && !path.ends_with('/'))
            .map(str::to_owned),
    );
    paths.retain(|path| {
        !gitlinks
            .iter()
            .any(|link| Path::new(path).starts_with(link))
    });
    Ok(paths)
}

pub fn snapshot_head(p: &Project) -> Result<Option<String>, String> {
    let head = read(p, &["rev-parse", "--verify", "--quiet", "HEAD"]).ok();
    Ok(head.filter(|head| valid_oid(head)))
}
