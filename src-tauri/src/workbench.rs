use super::*;

pub(super) struct WriteGuard<'a>(&'a App, PathBuf);
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.0.writes.lock().unwrap().remove(&self.1);
    }
}

pub(super) fn lock_write<'a>(app: &'a App, p: &Project) -> Result<WriteGuard<'a>> {
    if !app.writes.lock().unwrap().insert(p.path.clone()) {
        return Err("当前仓库已有写入操作正在处理。".into());
    }
    Ok(WriteGuard(app, p.path.clone()))
}

#[tauri::command]
pub async fn repo_commit_selected(
    app: AppHandle,
    project_id: String,
    request_id: String,
    branch: Option<String>,
    head: Option<String>,
    selected: Vec<git::Change>,
    message: String,
) -> Result<String> {
    uuid::Uuid::parse_str(&request_id).map_err(|_| "无效提交请求。")?;
    background(move || {
        let state = app.state::<App>();
        let p = state.project(&project_id)?;
        let mut expected = git::status(&p)?;
        expected.branch = branch;
        expected.head = head;
        state.commit_selected(&p, &request_id, &expected, &selected, &message)
    })
    .await
}

#[tauri::command]
pub async fn repo_commit_summary(
    app: AppHandle,
    project_id: String,
    oid: String,
) -> Result<git::Commit> {
    background(move || git::commit_summary(&app.state::<App>().project(&project_id)?, &oid)).await
}

#[tauri::command]
pub fn repo_commit_outcome(
    state: State<App>,
    project_id: String,
    request_id: String,
) -> Result<Option<String>> {
    state.project(&project_id)?;
    match state
        .journal
        .lock()
        .unwrap()
        .get(&format!("{project_id}/{request_id}"))
    {
        Some(record) if record.outcome.is_some() => Ok(record.outcome.clone()),
        Some(_) => Err("该提交仍待核对，请打开历史和暂存区确认实际结果。".into()),
        None => Err("没有可核对的提交记录。".into()),
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", untagged)]
pub(super) enum Repository {
    Registered {
        #[serde(rename = "projectId")]
        project_id: String,
    },
    Child {
        #[serde(rename = "parentProjectId")]
        parent_id: String,
        #[serde(rename = "relativePath")]
        relative_path: PathBuf,
    },
}

impl Repository {
    fn resolve(&self, state: &App) -> Result<Project> {
        match self {
            Self::Registered { project_id } => state.project(project_id),
            Self::Child {
                parent_id,
                relative_path,
            } => {
                let parent = state.project(parent_id)?;
                let path = workspace::open_child(&parent.path, relative_path)?;
                if let Some(project) = state
                    .settings
                    .lock()
                    .unwrap()
                    .projects
                    .iter()
                    .find(|p| p.path == path)
                {
                    return Ok(project.clone());
                }
                Ok(Project {
                    id: String::new(),
                    name: relative_path.to_string_lossy().into_owned(),
                    path,
                })
            }
        }
    }
}

#[tauri::command]
pub async fn project_child_summary(
    app: AppHandle,
    parent_project_id: String,
    relative_path: PathBuf,
) -> Result<git::ChildSummary> {
    background(move || {
        let state = app.state::<App>();
        let p = Repository::Child {
            parent_id: parent_project_id,
            relative_path,
        }
        .resolve(&state)?;
        let cached = state.remotes.lock().unwrap().clone();
        git::child_summary(&p, &cached)
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    repository: Repository,
    label: String,
    path: PathBuf,
    hits: Vec<git::SearchHit>,
}

#[tauri::command]
pub async fn workspace_search(
    app: AppHandle,
    repository: Repository,
    query: String,
) -> Result<SearchResult> {
    background(move || {
        let state = app.state::<App>();
        let p = repository.resolve(&state)?;
        let root = workspace::project(&p.path)?;
        if root != p.path
            || git::read_at(&p.path, &["rev-parse", "--is-inside-work-tree"])? != "true"
        {
            return Err("该项目不是可搜索的 Git 仓库。".into());
        }
        let hits = git::search_local(&p, &query)?;
        Ok(SearchResult {
            repository,
            label: p.name,
            path: p.path,
            hits,
        })
    })
    .await
}

#[tauri::command]
pub async fn repo_sync_preview(app: AppHandle, project_id: String, pushing: bool) -> Result<Value> {
    background(move || {
        let state = app.state::<App>();
        let p = state.project(&project_id)?;
        let preview = git::sync_preview(&p, pushing)?;
        let token = uuid::Uuid::new_v4().to_string();
        let response = json!({"token":token, "preview":preview});
        if pushing {
            state
                .push_previews
                .lock()
                .unwrap()
                .insert(project_id, (token, preview));
        }
        Ok(response)
    })
    .await
}

#[tauri::command]
pub async fn repo_push(app: AppHandle, project_id: String, token: String) -> Result<String> {
    background(move || {
        let state = app.state::<App>();
        let p = state.project(&project_id)?;
        let _write = lock_write(&state, &p)?;
        let preview = {
            let mut previews = state.push_previews.lock().unwrap();
            if previews
                .get(&project_id)
                .is_none_or(|(current, _)| *current != token)
            {
                return Err("推送预览已失效，请重新核对。".into());
            }
            previews.remove(&project_id).unwrap().1
        };
        git::push_preview(&p, &preview)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineSelection {
    path: String,
    group: String,
    oid: Option<String>,
    stamp: Option<String>,
    head: Option<String>,
    old_side: bool,
    number: u32,
    text: String,
}

#[tauri::command]
pub async fn repo_line_origin(
    app: AppHandle,
    project_id: String,
    selection: LineSelection,
) -> Result<Option<git::LineOrigin>> {
    background(move || {
        let p = app.state::<App>().project(&project_id)?;
        let pick = selection;
        let validate = |path: &str| {
            if path.is_empty()
                || !Path::new(path)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            {
                Err("无效文件路径。".to_owned())
            } else {
                Ok(())
            }
        };
        validate(&pick.path)?;
        let from_status = || -> Result<Option<String>> {
            let status = git::status(&p)?;
            if status.head != pick.head {
                return Err("当前提交已变化，请重新打开差异。".into());
            }
            let file = status
                .files
                .iter()
                .find(|f| {
                    f.path == pick.path
                        && f.group == pick.group
                        && Some(&f.stamp) == pick.stamp.as_ref()
                })
                .ok_or("文件版本已改变，请重新打开差异。")?;
            Ok(file.old.clone())
        };
        let old = if pick.oid.is_none() {
            from_status()?
        } else {
            None
        };
        let diff = git::diff_preview(
            &p,
            pick.oid.as_deref(),
            &pick.path,
            &pick.group,
            old.as_deref(),
        )?;
        let line = diff
            .lines
            .iter()
            .find(|line| {
                (if pick.old_side { line.old } else { line.new }) == Some(pick.number)
                    && line.text == pick.text
            })
            .ok_or("该行已改变，请重新打开差异。")?;
        let path = if pick.old_side {
            &diff.old_path
        } else {
            &diff.new_path
        };
        validate(path)?;
        if let Some(oid) = pick.oid {
            let commit = git::commit_summary(&p, &oid)?;
            let anchor = if pick.old_side {
                commit
                    .parents
                    .split_whitespace()
                    .next()
                    .ok_or("该提交没有父版本。")?
            } else {
                &oid
            };
            return git::line_origin(&p, anchor, path, pick.number).map(Some);
        }
        let answer = if !pick.old_side && line.kind == "add" {
            None
        } else if pick.group == "unstaged" {
            git::index_line_origin(&p, &diff.old_path, line.old.ok_or("缺少暂存区行号。")?)?
        } else if let Some(number) = line.old {
            pick.head
                .as_deref()
                .map(|oid| git::line_origin(&p, oid, &diff.old_path, number))
                .transpose()?
        } else {
            None
        };
        from_status()?;
        Ok(answer)
    })
    .await
}

#[tauri::command]
pub async fn snapshot_list(app: AppHandle, project_id: String) -> Result<snapshots::SnapshotList> {
    background(move || {
        let state = app.state::<App>();
        snapshots::list(&state.root, &state.project(&project_id)?)
    })
    .await
}

#[tauri::command]
pub async fn snapshot_create(
    app: AppHandle,
    project_id: String,
    name: String,
) -> Result<snapshots::Snapshot> {
    background(move || {
        let state = app.state::<App>();
        let p = state.project(&project_id)?;
        let _write = lock_write(&state, &p)?;
        snapshots::create(&state.root, &p, &name)
    })
    .await
}

#[tauri::command]
pub async fn snapshot_delete(
    app: AppHandle,
    project_id: String,
    snapshot_id: String,
) -> Result<()> {
    background(move || {
        let state = app.state::<App>();
        snapshots::delete(&state.root, &state.project(&project_id)?, &snapshot_id)
    })
    .await
}

#[tauri::command]
pub async fn snapshot_preview(
    app: AppHandle,
    project_id: String,
    snapshot_id: String,
    path: Option<String>,
) -> Result<Value> {
    background(move || {
        let state = app.state::<App>();
        let p = state.project(&project_id)?;
        if let Some(path) = path {
            return Ok(json!(snapshots::preview_file(
                &state.root,
                &p,
                &snapshot_id,
                &path
            )?));
        }
        Ok(json!(snapshots::preview(&state.root, &p, &snapshot_id)?))
    })
    .await
}

#[tauri::command]
pub async fn snapshot_restore_file(
    app: AppHandle,
    project_id: String,
    snapshot_id: String,
    path: String,
    version: String,
) -> Result<snapshots::Snapshot> {
    background(move || {
        let state = app.state::<App>();
        let p = state.project(&project_id)?;
        let _write = lock_write(&state, &p)?;
        snapshots::restore(&state.root, &p, &snapshot_id, &path, &version)
    })
    .await
}
