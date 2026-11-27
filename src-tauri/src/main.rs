#[cfg(test)]
mod acceptance;
mod agent;
mod git;
mod snapshots;
mod terminal;
mod workbench;
mod workspace;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{ipc::Channel, AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use workspace::{Project, Settings};

type Result<T> = std::result::Result<T, String>;
const AGENT_LOGIN_TERMINAL: &str = "antigravity-login";

#[derive(Clone)]
struct Request {
    pid: agent::Running,
    canceled: Arc<AtomicBool>,
    writes: Arc<AtomicBool>,
    id: String,
}
#[derive(Clone, Deserialize, Serialize)]
struct Journal {
    head: Option<String>,
    outcome: Option<String>,
    running: bool,
}

#[derive(Serialize)]
struct AgentChoices {
    options: Vec<agent::Choice>,
    selected: Option<agent::Kind>,
}

#[derive(Clone)]
enum AgentSession {
    Native(String),
    LastExchange(String),
}

fn active_agent(
    preference: Option<agent::Kind>,
    installed: &[agent::Installed],
) -> Option<&agent::Installed> {
    installed
        .iter()
        .find(|cli| Some(cli.kind) == preference)
        .or_else(|| installed.iter().find(|cli| cli.questions))
        .or_else(|| installed.first())
}

struct App {
    root: PathBuf,
    settings: Mutex<Settings>,
    remotes: Mutex<HashMap<String, git::Remote>>,
    checking: Mutex<HashSet<String>>,
    watcher: Mutex<Option<RecommendedWatcher>>,
    terminal: Mutex<HashMap<String, terminal::Terminal>>,
    agents: Mutex<HashMap<String, Request>>,
    sessions: Mutex<HashMap<String, AgentSession>>,
    installed_agents: Mutex<Vec<agent::Installed>>,
    agent_generation: AtomicU64,
    journal: Mutex<HashMap<String, Journal>>,
    writes: Mutex<HashSet<PathBuf>>,
    push_previews: Mutex<HashMap<String, (String, git::SyncPreview)>>,
}
impl App {
    fn project(&self, id: &str) -> Result<Project> {
        self.settings
            .lock()
            .unwrap()
            .projects
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or("项目已移除。".into())
    }
    fn update<T>(&self, edit: impl FnOnce(&mut Settings) -> Result<T>) -> Result<T> {
        let mut slot = self.settings.lock().unwrap();
        let mut settings = slot.clone();
        let result = edit(&mut settings)?;
        workspace::save(&self.root.join("settings.json"), &settings)?;
        if slot.agent_kind != settings.agent_kind {
            self.reset_agent_sessions();
        }
        *slot = settings;
        Ok(result)
    }
    fn reset_agent_sessions(&self) {
        self.agent_generation.fetch_add(1, Ordering::SeqCst);
        self.sessions.lock().unwrap().clear();
    }

    fn save_journal(&self, key: String, record: Journal) -> Result<()> {
        let mut entries = self.journal.lock().unwrap();
        let previous = entries.insert(key.clone(), record);
        if let Err(error) = workspace::save(&self.root.join("commits.json"), &*entries) {
            if let Some(v) = previous {
                entries.insert(key, v);
            } else {
                entries.remove(&key);
            }
            return Err(error);
        }
        Ok(())
    }
    fn clear_journal(&self, key: &str) -> Result<()> {
        let mut entries = self.journal.lock().unwrap();
        if let Some(old) = entries.remove(key) {
            if let Err(e) = workspace::save(&self.root.join("commits.json"), &*entries) {
                entries.insert(key.into(), old);
                return Err(e);
            }
        }
        Ok(())
    }
    fn commit_selected(
        &self,
        project: &Project,
        request_id: &str,
        expected: &git::Status,
        changes: &[git::Change],
        message: &str,
    ) -> Result<String> {
        let _write = workbench::lock_write(self, project)?;
        let key = format!("{}/{}", project.id, request_id);
        if let Some(record) = self.journal.lock().unwrap().get(&key) {
            return record
                .outcome
                .clone()
                .ok_or_else(|| "上次提交结果待核查，请检查当前历史与暂存区。".into());
        }
        let current = git::status(project)?;
        if current.branch != expected.branch
            || current.head != expected.head
            || !changes.iter().all(|change| current.files.contains(change))
        {
            return Err("分支或选中的改动已改变，请重新核对。".into());
        }
        self.save_journal(
            key.clone(),
            Journal {
                head: expected.head.clone(),
                outcome: None,
                running: true,
            },
        )?;
        match git::commit(project, expected, changes, message) {
            Ok(oid) => {
                self.save_journal(
                    key,
                    Journal {
                        head: expected.head.clone(),
                        outcome: Some(oid.clone()),
                        running: false,
                    },
                )
                .map_err(|e| format!("提交 {oid} 已生成，但提交记录保存失败：{e}"))?;
                Ok(oid)
            }
            Err(error) => {
                if git::status(project).is_ok_and(|s| s.head == expected.head) {
                    self.clear_journal(&key)?;
                }
                Err(error)
            }
        }
    }

    fn stop_project(&self, id: &str) {
        self.terminal.lock().unwrap().remove(id);
        let requests = self.agents.lock().unwrap();
        if let Some(request) = requests.get(id) {
            request.canceled.store(true, Ordering::SeqCst);
            if !request.writes.load(Ordering::SeqCst) {
                agent::stop(&request.pid);
            }
        }
        self.sessions.lock().unwrap().remove(id);
        self.push_previews.lock().unwrap().remove(id);
    }
}

async fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn project_list(state: State<App>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
async fn project_open(app: AppHandle) -> Result<Option<Project>> {
    background(move || {
        let Some(folder) = app.dialog().file().blocking_pick_folder() else {
            return Ok(None);
        };
        let path = workspace::project(&folder.into_path().map_err(|e| e.to_string())?)?;
        let state = app.state::<App>();
        state.update(|settings| {
            let project = settings.register(path);
            settings.selected = Some(project.id.clone());
            Ok(Some(project))
        })
    })
    .await
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChildRepository {
    relative_path: PathBuf,
    project_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChildRepositories {
    repositories: Vec<ChildRepository>,
    unreadable_count: usize,
}

#[tauri::command]
async fn project_scan_children(
    app: AppHandle,
    parent_project_id: String,
) -> Result<ChildRepositories> {
    background(move || {
        let state = app.state::<App>();
        let parent = state.project(&parent_project_id)?;
        let scan = workspace::scan_children(&parent.path)?;
        let settings = state.settings.lock().unwrap();
        let project_ids: HashMap<_, _> =
            settings.projects.iter().map(|p| (&p.path, &p.id)).collect();
        let repositories = scan
            .paths
            .into_iter()
            .map(|relative_path| {
                let project_id = project_ids
                    .get(&parent.path.join(&relative_path))
                    .map(|id| id.to_string());
                ChildRepository {
                    relative_path,
                    project_id,
                }
            })
            .collect();
        Ok(ChildRepositories {
            repositories,
            unreadable_count: scan.unreadable_count,
        })
    })
    .await
}

#[tauri::command]
async fn project_open_child(
    app: AppHandle,
    parent_project_id: String,
    relative_path: PathBuf,
) -> Result<Project> {
    background(move || {
        let state = app.state::<App>();
        let parent = state.project(&parent_project_id)?;
        let path = workspace::open_child(&parent.path, &relative_path)?;
        state.update(|settings| {
            if !settings.projects.iter().any(|p| p.id == parent.id) {
                return Err("父项目已移除。".into());
            }
            Ok(settings.register(path))
        })
    })
    .await
}

#[tauri::command]
async fn project_select(app: AppHandle, project_id: String) -> Result<()> {
    background(move || {
        let state = app.state::<App>();
        let p = state.project(&project_id)?;
        state.update(|s| {
            if !s.projects.iter().any(|item| item.id == project_id) {
                return Err("项目已移除。".into());
            }
            s.selected = Some(project_id);
            Ok(())
        })?;
        let mut slot = state.watcher.lock().unwrap();
        if state.settings.lock().unwrap().selected.as_deref() != Some(&p.id) {
            return Ok(());
        }
        let event_id = p.id.clone();
        let emitter = app.clone();
        let (changed, changes) = std::sync::mpsc::sync_channel(1);
        let mut watcher = notify::recommended_watcher(move |e: notify::Result<notify::Event>| {
            if e.is_ok_and(|event| !event.kind.is_access()) {
                let _ = changed.try_send(());
            }
        })
        .map_err(|e| e.to_string())?;
        // Read after the batch, including Git's final lockfile rename.
        std::thread::spawn(move || {
            while changes.recv().is_ok() {
                std::thread::sleep(Duration::from_millis(130));
                let _ = changes.try_recv();
                let _ = emitter.emit("workspace-changed", &event_id);
            }
        });
        let mut paths = HashSet::from([p.path.clone()]);
        let mut mode = RecursiveMode::NonRecursive;
        for option in ["--absolute-git-dir", "--git-common-dir"] {
            if let Ok(path) = git::read_at(&p.path, &["rev-parse", option]) {
                mode = RecursiveMode::Recursive;
                if let Ok(path) = p.path.join(path).canonicalize() {
                    paths.insert(path);
                }
            }
        }
        for path in paths {
            let _ = watcher.watch(&path, mode);
        }
        *slot = Some(watcher);
        Ok(())
    })
    .await
}

#[tauri::command]
fn project_remove(state: State<App>, project_id: String) -> Result<Settings> {
    let next = state.update(|s| {
        s.projects.retain(|p| p.id != project_id);
        if s.selected.as_deref() == Some(&project_id) {
            s.selected = s.projects.first().map(|p| p.id.clone());
        }
        Ok(s.clone())
    })?;
    state.stop_project(&project_id);
    Ok(next)
}
#[tauri::command]
async fn repo_status(state: State<'_, App>, project_id: String) -> Result<git::Status> {
    let p = state.project(&project_id)?;
    background(move || git::status(&p)).await
}

#[tauri::command]
async fn repo_compare(
    state: State<'_, App>,
    project_id: String,
    pairs: Vec<(String, String)>,
) -> Result<Vec<Option<Vec<u32>>>> {
    let p = state.project(&project_id)?;
    background(move || {
        Ok(pairs
            .iter()
            .map(|(a, b)| git::divergence(&p, a, b))
            .collect())
    })
    .await
}

#[tauri::command]
async fn remote_refresh(
    app: AppHandle,
    project_id: String,
    name: String,
    events: Channel<git::Remote>,
) -> Result<git::Remote> {
    background(move || {
        let state = app.state::<App>();
        let project = state.project(&project_id)?;
        let identity = git::remote_identity(&project, &name)?;
        let key = format!("{project_id}/{name}/{identity}");
        if !state.checking.lock().unwrap().insert(key.clone()) {
            return Err("该远端正在刷新。".into());
        }
        let result = (|| {
            let cache_path = state.root.join("remotes.json");
            let known = state.remotes.lock().unwrap().get(&key).cloned();
            match git::verify_remote(&project, &name) {
                Ok(mut remote) => {
                    let _ = events.send(remote.clone());
                    git::hydrate_remote(&project, &mut remote);
                    if git::remote_identity(&project, &name)? != identity {
                        return Err("远端地址已改变，请刷新。".into());
                    }
                    let mut cache = state.remotes.lock().unwrap();
                    cache.insert(key.clone(), remote.clone());
                    if let Err(e) = workspace::save(&cache_path, &*cache) {
                        eprintln!("{e}");
                    }
                    Ok(remote)
                }
                Err(error) => {
                    let mut cached = known.unwrap_or(git::Remote {
                        name,
                        timestamp: 0,
                        refs: Vec::new(),
                        error: None,
                        history_error: None,
                    });
                    cached.error = Some(error);
                    Ok(cached)
                }
            }
        })();
        state.checking.lock().unwrap().remove(&key);
        result
    })
    .await
}

#[tauri::command]
async fn repo_history(
    state: State<'_, App>,
    project_id: String,
    tips: Vec<String>,
) -> Result<Vec<git::Commit>> {
    let p = state.project(&project_id)?;
    background(move || git::history(&p, &tips)).await
}
#[tauri::command]
async fn repo_detail(
    state: State<'_, App>,
    project_id: String,
    oid: String,
) -> Result<Vec<std::collections::BTreeMap<&'static str, String>>> {
    let p = state.project(&project_id)?;
    background(move || git::commit_files(&p, &oid)).await
}
#[tauri::command]
async fn repo_patch(
    state: State<'_, App>,
    project_id: String,
    oid: Option<String>,
    path: String,
    group: String,
    structured: Option<bool>,
    stamp: Option<String>,
) -> Result<Value> {
    let p = state.project(&project_id)?;
    if path.is_empty()
        || Path::new(&path).components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err("无效文件路径。".into());
    }
    background(move || {
        if structured.unwrap_or(false) {
            let old = if oid.is_none() {
                let status = git::status(&p)?;
                let file = status
                    .files
                    .iter()
                    .find(|c| {
                        c.path == path
                            && c.group == group
                            && stamp.as_ref().is_none_or(|stamp| stamp == &c.stamp)
                    })
                    .ok_or("文件状态已变化，请刷新。")?;
                file.old.clone()
            } else {
                None
            };
            return serde_json::to_value(git::diff_preview(
                &p,
                oid.as_deref(),
                &path,
                &group,
                old.as_deref(),
            )?)
            .map_err(|e| e.to_string());
        }
        if oid.is_none()
            && !git::status(&p)?
                .files
                .iter()
                .any(|c| c.path == path && c.group == group)
        {
            return Err("文件状态已变化，请刷新。".into());
        }
        git::patch(&p, oid.as_deref(), &path, &group).map(Value::String)
    })
    .await
}

#[tauri::command]
fn terminal_open(
    state: State<App>,
    project_id: String,
    rows: u16,
    cols: u16,
    events: Channel<Value>,
) -> Result<String> {
    let project = state.project(&project_id)?;
    let mut sessions = state.terminal.lock().unwrap();
    let terminal = terminal::Terminal::open(&project.path, rows, cols, events)?;
    let id = terminal.id.clone();
    sessions.insert(project_id, terminal);
    Ok(id)
}
#[tauri::command]
fn terminal_write(state: State<App>, project_id: String, id: String, data: String) -> Result<()> {
    let mut sessions = state.terminal.lock().unwrap();
    sessions
        .get_mut(&project_id)
        .filter(|t| t.id == id)
        .ok_or("终端已关闭。")?
        .write(&data)
}
#[tauri::command]
fn terminal_resize(
    state: State<App>,
    project_id: String,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<()> {
    let sessions = state.terminal.lock().unwrap();
    sessions
        .get(&project_id)
        .filter(|t| t.id == id)
        .ok_or("终端已关闭。")?
        .resize(rows, cols)
}
#[tauri::command]
fn terminal_close(state: State<App>, project_id: String, id: String) {
    let mut sessions = state.terminal.lock().unwrap();
    if sessions.get(&project_id).is_some_and(|t| t.id == id) {
        sessions.remove(&project_id);
    }
}

#[tauri::command]
fn agent_login_open(
    state: State<App>,
    rows: u16,
    cols: u16,
    events: Channel<Value>,
) -> Result<String> {
    let path = state
        .installed_agents
        .lock()
        .unwrap()
        .iter()
        .find(|cli| cli.kind == agent::Kind::Antigravity)
        .map(|cli| cli.path.clone())
        .ok_or("找不到 Antigravity CLI。")?;
    let mut command = portable_pty::CommandBuilder::new(path.as_os_str());
    command.args(["acp", "--login"]);
    command.cwd(std::env::temp_dir());
    command.env("PATH", agent::cli_path(&path));
    open_agent_terminal(state, command, rows, cols, events)
}

#[tauri::command]
fn agent_terminal_open(
    state: State<App>,
    project_id: String,
    rows: u16,
    cols: u16,
    events: Channel<Value>,
) -> Result<String> {
    let project = state.project(&project_id)?;
    let settings = state.settings.lock().unwrap();
    let installed = state.installed_agents.lock().unwrap();
    let cli = active_agent(settings.agent_kind, &installed)
        .filter(|cli| !cli.questions)
        .cloned()
        .ok_or("当前 Agent 可以直接在问答中使用。")?;
    drop(installed);
    drop(settings);
    let mut command = portable_pty::CommandBuilder::new(cli.path.as_os_str());
    if cli.kind == agent::Kind::DeepSeek {
        command.args(["--profile", "tui"]);
    }
    command.cwd(&project.path);
    command.env("PATH", agent::cli_path(&cli.path));
    open_agent_terminal(state, command, rows, cols, events)
}

fn open_agent_terminal(
    state: State<App>,
    command: portable_pty::CommandBuilder,
    rows: u16,
    cols: u16,
    events: Channel<Value>,
) -> Result<String> {
    let terminal = terminal::Terminal::open_command(command, rows, cols, events)?;
    let id = terminal.id.clone();
    state
        .terminal
        .lock()
        .unwrap()
        .insert(AGENT_LOGIN_TERMINAL.into(), terminal);
    Ok(id)
}

#[tauri::command]
async fn agent_list(app: AppHandle) -> Result<AgentChoices> {
    background(move || {
        let installed = agent::discover();
        let state = app.state::<App>();
        let settings = state.settings.lock().unwrap();
        let mut cached = state.installed_agents.lock().unwrap();
        let selected = active_agent(settings.agent_kind, &installed);
        if active_agent(settings.agent_kind, &cached) != selected {
            state.reset_agent_sessions();
        }
        let choices = AgentChoices {
            options: installed
                .iter()
                .map(|cli| agent::Choice {
                    kind: cli.kind,
                    label: cli.kind.label(),
                    questions: cli.questions,
                })
                .collect(),
            selected: selected.map(|cli| cli.kind),
        };
        *cached = installed;
        Ok(choices)
    })
    .await
}

#[tauri::command]
fn agent_select(state: State<App>, kind: agent::Kind) -> Result<()> {
    state.update(|settings| {
        if !state
            .installed_agents
            .lock()
            .unwrap()
            .iter()
            .any(|cli| cli.kind == kind)
        {
            return Err("此 Agent 已不可用，请重新打开问答。".into());
        }
        settings.agent_kind = Some(kind);
        Ok(())
    })
}

#[tauri::command]
async fn agent_send(
    app: AppHandle,
    project_id: String,
    request_id: String,
    question: String,
    selected: Vec<git::Change>,
    commit_oid: Option<String>,
    purpose: Option<String>,
    events: Channel<Value>,
) -> Result<()> {
    let project = app.state::<App>().project(&project_id)?;
    let request = Request {
        pid: Arc::new(Mutex::new(None)),
        canceled: Arc::new(AtomicBool::new(false)),
        writes: Arc::new(AtomicBool::new(false)),
        id: request_id.clone(),
    };
    let (cli, generation, session) = {
        let state = app.state::<App>();
        let mut agents = state.agents.lock().unwrap();
        if agents.contains_key(&project_id) {
            return Err("当前项目已有请求正在处理。".into());
        }
        agents.insert(project_id.clone(), request.clone());
        let settings = state.settings.lock().unwrap();
        let installed = state.installed_agents.lock().unwrap();
        let session = if purpose.as_deref() == Some("commit_message") {
            None
        } else {
            state.sessions.lock().unwrap().get(&project_id).cloned()
        };
        (
            active_agent(settings.agent_kind, &installed).cloned(),
            state.agent_generation.load(Ordering::SeqCst),
            session,
        )
    };
    background(move||{
        let state=app.state::<App>();
        let result=(||{
            let key=format!("{project_id}/{request_id}");
                let prior=state.journal.lock().unwrap().get(&key).cloned();
                if let Some(record)=prior {
                    if let Some(id)=record.outcome {
                        let _=events.send(json!({"kind":"commit","oid":id,"message":"此请求已提交。"}));
                        return Ok(())
                    }
                    return Err("上次提交结果待核查，请检查当前历史与暂存区。".into())
                }
            let snapshot=git::status(&project)?;
            let commit = if purpose.is_none() { agent::commit_request(&question,&snapshot,&selected) } else { None };
            if let Some(commit)=commit {
                let (changes,message)=commit?;
                request.writes.store(true,Ordering::SeqCst);
                if request.canceled.load(Ordering::SeqCst){return Err("已取消本次请求。".into())}
                let oid = state.commit_selected(&project, &request_id, &snapshot, &changes, &message)?;
                let _ = events.send(json!({"kind":"commit","oid":oid,"message":message}));
                Ok(())
            }else{
                if request.canceled.load(Ordering::SeqCst){return Err("已停止回答。".into())}
                let referenced=commit_oid.as_deref().filter(|oid|!oid.is_empty())
                    .map(|oid|agent::commit_context(&project,oid))
                    .transpose()
                    .map_err(|_|"无法读取引用的提交，请重新选择或移除引用。".to_string())?;
                if request.canceled.load(Ordering::SeqCst){return Err("已停止回答。".into())}
                let refs=state.remotes.lock().unwrap().iter().filter(|(k,_)|k.starts_with(&format!("{project_id}/")))
                    .map(|(_,r)|json!({"remote":r.name,"timestamp":r.timestamp,"refs":r.refs})).collect::<Vec<_>>();
                let status=json!({"branch":snapshot.branch,"head":snapshot.head,"files":snapshot.files.iter().take(100).map(|f|json!({"path":f.path,"group":f.group})).collect::<Vec<_>>(),"total_rows":snapshot.files.len(),"remote_last_verified":refs});
                let mut context=format!("用户问题：{question}\n\n本机 Git 读出的资料：{status}\n用户选中的文件：{}",serde_json::to_string(&selected).unwrap_or_default());
                if let Some(commit)=referenced {
                    context.push_str(&format!("\n用户引用的历史提交资料：{commit}\n问题中的「这次提交」指该历史提交。"));
                }
                if purpose.as_deref() == Some("commit_message") {
                    if selected.is_empty() || !selected.iter().all(|change| snapshot.files.contains(change)) {
                        return Err("选中的改动已变化，请重新选择。".into());
                    }
                    for change in selected.iter().take(12) {
                        let patch = git::patch(&project, None, &change.path, &change.group)?;
                        context.push_str(&format!("\n{}（{}）\n{}", change.path, change.group, patch.chars().take(2200).collect::<String>()));
                    }
                }
                let cli = cli.ok_or("没有找到可用的 Agent CLI。")?;
                if !cli.questions {
                    return Err(format!("{} 已安装，可以在终端中使用。", cli.kind.label()));
                }
                let session_id = match session {
                    Some(AgentSession::Native(ref id)) => Some(id.as_str()),
                    Some(AgentSession::LastExchange(ref exchange)) => {
                        context = format!("上一轮问答（供参考）：\n{exchange}\n\n{context}");
                        None
                    }
                    None => None,
                };
                let _ = events.send(json!({"kind":"started","agent":cli.kind.label()}));
                let reply = agent::answer(
                    &project, &cli, session_id, &context,
                    |text| { let _ = events.send(json!({"kind":"delta","text":text})); },
                    &request.pid, &request.canceled,
                ).map_err(|error| {
                    if cli.kind.needs_login(&error) {
                        let _ = events.send(json!({"kind":"auth_required"}));
                        "请先在 Agent 问答中登录 Antigravity，再重新发送。".into()
                    } else if cli.kind == agent::Kind::Antigravity && error.to_ascii_lowercase().contains("authentication failed or timed out") {
                        "Antigravity 暂时无法确认本机登录状态。请稍后重新发送。".into()
                    } else {
                        error
                    }
                })?;
                if purpose.as_deref() != Some("commit_message") {
                    let settings = state.settings.lock().unwrap();
                    if state.agent_generation.load(Ordering::SeqCst) == generation
                        && settings.projects.iter().any(|p| p.id == project_id)
                        && !request.canceled.load(Ordering::SeqCst)
                    {
                        let session = reply.session_id.map(AgentSession::Native).unwrap_or_else(|| {
                            let question = question.chars().take(1500).collect::<String>();
                            let text = reply.text.chars().take(2500).collect::<String>();
                            AgentSession::LastExchange(format!("用户：{question}\nAgent：{text}"))
                        });
                        state.sessions.lock().unwrap().insert(project_id.clone(), session);
                    }
                }
                Ok(())
            }
        })();
        state.agents.lock().unwrap().remove(&project_id);
        result
    }).await
}
#[tauri::command]
fn agent_cancel(state: State<App>, project_id: String, request_id: String) -> Result<()> {
    if let Some(r) = state
        .agents
        .lock()
        .unwrap()
        .get(&project_id)
        .filter(|r| r.id == request_id)
    {
        if r.writes.load(Ordering::SeqCst) {
            return Err("提交正在完成，请等待结果。".into());
        }
        r.canceled.store(true, Ordering::SeqCst);
        agent::stop(&r.pid);
    }
    Ok(())
}

// WebKit versions shipped with supported macOS releases may lack the transparency media query.
#[cfg(target_os = "macos")]
#[tauri::command]
fn prefers_solid_surfaces() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

fn main() {
    let default_root = std::env::var_os("GITCONTROL_DATA_DIR").map(PathBuf::from);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let root = default_root.clone().unwrap_or(app.path().app_data_dir()?);
            let settings =
                workspace::load(&root.join("settings.json")).map_err(std::io::Error::other)?;
            let remotes = workspace::load(&root.join("remotes.json")).unwrap_or_default();
            let journal =
                workspace::load(&root.join("commits.json")).map_err(std::io::Error::other)?;
            app.manage(App {
                root,
                settings: Mutex::new(settings),
                remotes: Mutex::new(remotes),
                checking: Mutex::default(),
                watcher: Mutex::default(),
                terminal: Mutex::default(),
                agents: Mutex::default(),
                sessions: Mutex::default(),
                installed_agents: Mutex::default(),
                agent_generation: AtomicU64::default(),
                journal: Mutex::new(journal),
                writes: Mutex::default(),
                push_previews: Mutex::default(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            #[cfg(target_os = "macos")]
            prefers_solid_surfaces,
            project_list,
            project_open,
            project_scan_children,
            project_open_child,
            project_select,
            project_remove,
            repo_status,
            repo_compare,
            remote_refresh,
            repo_history,
            repo_detail,
            repo_patch,
            terminal_open,
            terminal_write,
            terminal_resize,
            terminal_close,
            agent_list,
            agent_select,
            agent_login_open,
            agent_terminal_open,
            agent_send,
            agent_cancel,
            workbench::repo_commit_selected,
            workbench::repo_commit_summary,
            workbench::repo_commit_outcome,
            workbench::project_child_summary,
            workbench::workspace_search,
            workbench::repo_sync_preview,
            workbench::repo_push,
            workbench::repo_line_origin,
            workbench::snapshot_list,
            workbench::snapshot_create,
            workbench::snapshot_delete,
            workbench::snapshot_preview,
            workbench::snapshot_restore_file
        ])
        .build(tauri::generate_context!())
        .expect("无法启动 GitControl")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                let state = app.state::<App>();
                state.terminal.lock().unwrap().clear();
                for request in state.agents.lock().unwrap().values() {
                    agent::stop(&request.pid);
                }
            }
        });
}
