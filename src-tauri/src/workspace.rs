use crate::git;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};
use tempfile::NamedTempFile;

#[derive(Clone, Deserialize, Serialize)]
pub struct Project {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub projects: Vec<Project>,
    pub selected: Option<String>,
    #[serde(default)]
    pub claude_path: Option<PathBuf>,
}

fn schema_version() -> u32 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            projects: Vec::new(),
            selected: None,
            claude_path: None,
        }
    }
}

impl Settings {
    pub fn register(&mut self, path: PathBuf) -> Project {
        if let Some(project) = self.projects.iter().find(|p| p.path == path) {
            return project.clone();
        }
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let project = Project {
            id: uuid::Uuid::new_v4().to_string(),
            path,
            name,
        };
        self.projects.push(project.clone());
        project
    }
}

pub fn project(path: &Path) -> Result<PathBuf, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if !path.is_dir() {
        return Err("请选择项目目录。".into());
    }
    Ok(worktree_root(&path)?.unwrap_or(path))
}

fn worktree_root(path: &Path) -> Result<Option<PathBuf>, String> {
    git::read_at(path, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|root| {
            PathBuf::from(root)
                .canonicalize()
                .map_err(|e| e.to_string())
        })
        .transpose()
}

pub struct ChildScan {
    pub paths: Vec<PathBuf>,
    pub unreadable_count: usize,
}

fn scan_root(parent: &Path) -> Result<PathBuf, String> {
    let root = parent
        .canonicalize()
        .map_err(|e| format!("无法访问父目录：{e}"))?;
    if !root.is_dir() || parent.is_symlink() {
        return Err("父目录已失效，请重新选择目录。".into());
    }
    if child_worktree(&root)? || worktree_root(&root)?.is_some() {
        return Err("此目录已属于 Git 工作树，请直接打开仓库。".into());
    }
    Ok(root)
}

fn skip_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | "node_modules" | "dist" | "target" | "build")
    )
}

fn child_worktree(path: &Path) -> Result<bool, String> {
    let marker = match fs::symlink_metadata(path.join(".git")) {
        Ok(marker) => marker,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if (marker.is_dir() || marker.is_file()) && worktree_root(path)?.as_deref() == Some(path) {
        return Ok(true);
    }
    Err("无法验证目录的 Git 工作树。".into())
}

pub fn scan_children(parent: &Path) -> Result<ChildScan, String> {
    let root = scan_root(parent)?;
    let mut scan = ChildScan {
        paths: Vec::new(),
        unreadable_count: 0,
    };
    scan_directory(&root, Path::new(""), 2, &mut scan)
        .map_err(|e| format!("无法读取父目录：{e}"))?;
    scan.paths.sort_unstable();
    Ok(scan)
}

fn scan_directory(
    root: &Path,
    relative: &Path,
    levels: u8,
    scan: &mut ChildScan,
) -> io::Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let Ok((name, kind)) = entry.and_then(|e| Ok((e.file_name(), e.file_type()?))) else {
            scan.unreadable_count += 1;
            continue;
        };
        if !kind.is_dir() || skip_directory(&name) {
            continue;
        }
        if name.to_str().is_none() {
            scan.unreadable_count += 1;
            continue;
        }
        let child = relative.join(name);
        match child_worktree(&root.join(&child)) {
            Ok(true) => scan.paths.push(child),
            Ok(false) if levels > 1 => {
                if scan_directory(root, &child, levels - 1, scan).is_err() {
                    scan.unreadable_count += 1;
                }
            }
            Ok(false) => {}
            Err(_) => scan.unreadable_count += 1,
        }
    }
    Ok(())
}

pub fn open_child(parent: &Path, relative: &Path) -> Result<PathBuf, String> {
    let stale = || "子仓库已移动或不可访问，请重新扫描。".to_string();
    let root = scan_root(parent).map_err(|_| stale())?;
    let mut candidate = root.clone();
    let mut components = relative.components().peekable();
    if !(1..=2).contains(&relative.components().count()) {
        return Err(stale());
    }
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(stale());
        };
        if skip_directory(name) {
            return Err(stale());
        }
        candidate.push(name);
        if !fs::symlink_metadata(&candidate).is_ok_and(|kind| kind.is_dir())
            || candidate.canonicalize().ok().as_deref() != Some(candidate.as_path())
        {
            return Err(stale());
        }
        let is_worktree = child_worktree(&candidate).map_err(|_| stale())?;
        if is_worktree != components.peek().is_none() {
            return Err(stale());
        }
    }
    Ok(candidate)
}

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("无法读取应用数据 {}：{e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn save<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), String> {
    let dir = path.parent().ok_or("缺少数据目录")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, value).map_err(|e| e.to_string())?;
    file.flush()
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
