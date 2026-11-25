use crate::{git, workspace, Project, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
use tempfile::NamedTempFile;

const MAX_BYTES: u64 = 512 * 1024 * 1024;
static SNAPSHOTS: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Entry {
    kind: String,
    mode: u32,
    digest: String,
    bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub id: String,
    name: String,
    timestamp: u64,
    head: Option<String>,
    undo: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    entries: BTreeMap<String, Option<Entry>>,
}

#[derive(Serialize)]
pub struct SnapshotList {
    items: Vec<Snapshot>,
    bytes: u64,
    limit: u64,
}

#[derive(Serialize)]
pub struct PreviewRow {
    pub path: String,
    pub version: String,
    kind: &'static str,
}

#[derive(Serialize)]
pub struct FilePreview {
    before: Option<String>,
    after: Option<String>,
    version: String,
    kind: &'static str,
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn area(root: &Path, p: &Project) -> PathBuf {
    root.join("snapshots/v1")
        .join(digest(p.path.as_os_str().as_bytes()))
}

fn manifest(area: &Path, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id).map_err(|_| "无效快照 ID。")?;
    Ok(area.join("manifests").join(format!("{id}.json")))
}

fn safe_path(p: &Project, relative: &str) -> Result<PathBuf> {
    let mut parts = Path::new(relative).components().peekable();
    if relative.is_empty() || parts.peek().is_none() {
        return Err("无效文件路径。".into());
    }
    let mut path = p.path.clone();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return Err("无效文件路径。".into());
        };
        if name.as_bytes().eq_ignore_ascii_case(b".git") {
            return Err("该路径属于 Git 元数据。".into());
        }
        path.push(name);
        if parts.peek().is_some() {
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() && !path.join(".git").exists() => (),
                Ok(_) => return Err("文件的上级目录已改变或属于子模块。".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(error(e)),
            }
        }
    }
    Ok(path)
}

fn check_project(p: &Project) -> Result<()> {
    if workspace::project(&p.path)? != p.path
        || git::read_at(&p.path, &["rev-parse", "--is-inside-work-tree"])? != "true"
    {
        return Err("项目已移动或失去工作树身份。".into());
    }
    Ok(())
}

fn paths(p: &Project) -> Result<BTreeSet<String>> {
    git::snapshot_paths(p)
}

fn metadata_key(meta: &fs::Metadata) -> (u64, u64, u32, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.mode(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

fn read_entry(path: &Path) -> Result<Option<(Entry, Vec<u8>)>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(e)),
    };
    let (kind, data) = if meta.is_file() {
        if meta.len() > MAX_BYTES {
            return Err("文件超过快照容量上限。".into());
        }
        let mut data = Vec::new();
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(error)?;
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(error)?;
        if data.len() as u64 > MAX_BYTES {
            return Err("文件超过快照容量上限。".into());
        }
        ("file", data)
    } else if meta.is_symlink() {
        (
            "link",
            fs::read_link(path)
                .map_err(error)?
                .as_os_str()
                .as_bytes()
                .to_vec(),
        )
    } else {
        return Err("文件位置被目录或不支持的文件类型占用。".into());
    };
    if metadata_key(&fs::symlink_metadata(path).map_err(error)?) != metadata_key(&meta) {
        return Err("读取期间文件已改变，请重试。".into());
    }
    Ok(Some((
        Entry {
            kind: kind.into(),
            mode: meta.mode() & 0o777,
            digest: digest(&data),
            bytes: data.len() as u64,
        },
        data,
    )))
}

fn blob(area: &Path, entry: &Entry) -> Result<PathBuf> {
    if entry.digest.len() != 64 || !entry.digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("快照文件指纹无效。".into());
    }
    Ok(area.join("files").join(&entry.digest))
}

fn sum_bytes(dir: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    let rows = match fs::read_dir(dir) {
        Ok(rows) => rows,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(error(e)),
    };
    for row in rows {
        let meta = row.map_err(error)?.metadata().map_err(error)?;
        if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
        }
    }
    Ok(bytes)
}

fn put(area: &Path, entry: &Entry, data: &[u8], used: &mut u64) -> Result<()> {
    let path = blob(area, entry)?;
    if path.is_file() {
        return Ok(());
    }
    if used.saturating_add(entry.bytes) > MAX_BYTES {
        return Err("快照空间不足，请删除不再需要的快照后重试。".into());
    }
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir).map_err(error)?;
    let mut file = NamedTempFile::new_in(dir).map_err(error)?;
    file.write_all(data)
        .and_then(|_| file.as_file().sync_all())
        .map_err(error)?;
    file.persist(path).map_err(error)?;
    *used += entry.bytes;
    Ok(())
}

fn all(area: &Path) -> Result<Vec<Snapshot>> {
    let rows = match fs::read_dir(area.join("manifests")) {
        Ok(rows) => rows,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(error(e)),
    };
    let mut items = Vec::new();
    for row in rows {
        let path = row.map_err(error)?.path();
        if path.extension().and_then(|v| v.to_str()) == Some("json") {
            items.push(load(&path)?);
        }
    }
    items.sort_by(|a, b| (b.timestamp, &b.id).cmp(&(a.timestamp, &a.id)));
    Ok(items)
}

fn load(path: &Path) -> Result<Snapshot> {
    let file = File::open(path).map_err(error)?;
    serde_json::from_reader(file).map_err(error)
}

fn cleanup(area: &Path) -> Result<()> {
    let keep: BTreeSet<_> = all(area)?
        .into_iter()
        .flat_map(|item| {
            item.entries
                .into_values()
                .flatten()
                .map(|entry| entry.digest)
        })
        .collect();
    if let Ok(rows) = fs::read_dir(area.join("files")) {
        for row in rows {
            let row = row.map_err(error)?;
            if !keep.contains(&row.file_name().to_string_lossy().into_owned()) {
                fs::remove_file(row.path()).map_err(error)?;
            }
        }
    }
    Ok(())
}

pub fn list(root: &Path, p: &Project) -> Result<SnapshotList> {
    let _guard = SNAPSHOTS.lock().unwrap();
    let area = area(root, p);
    let bytes = sum_bytes(&area.join("files"))?;
    let mut items = all(&area)?;
    for item in &mut items {
        item.entries.clear();
    }
    Ok(SnapshotList {
        items,
        bytes,
        limit: MAX_BYTES,
    })
}

pub fn create(root: &Path, p: &Project, name: &str) -> Result<Snapshot> {
    let _guard = SNAPSHOTS.lock().unwrap();
    check_project(p)?;
    let area = area(root, p);
    cleanup(&area)?;
    let result = (|| {
        let mut used = sum_bytes(&area.join("files"))?;
        let head = git::snapshot_head(p)?;
        let initial = paths(p)?;
        let mut entries = BTreeMap::new();
        for relative in &initial {
            let path = safe_path(p, relative)?;
            if path.is_dir() && !fs::symlink_metadata(&path).map_err(error)?.is_symlink() {
                if path.join(".git").exists() {
                    continue;
                }
                return Err("跟踪文件位置被目录占用，请处理后重试。".into());
            }
            let record = read_entry(&path)?;
            if let Some((entry, data)) = &record {
                put(&area, entry, data, &mut used)?;
            }
            entries.insert(relative.clone(), record.map(|(entry, _)| entry));
        }
        if paths(p)? != initial || git::snapshot_head(p)? != head {
            return Err("快照期间 Git 或文件列表发生变化，请重试。".into());
        }
        for (relative, entry) in &entries {
            if &read_entry(&safe_path(p, relative)?)?.map(|(entry, _)| entry) != entry {
                return Err(format!("{relative} 在创建快照期间发生变化，请重试。"));
            }
        }
        publish(&area, name, head, false, entries)
    })();
    if result.is_err() {
        let _ = cleanup(&area);
    }
    result
}

fn publish(
    area: &Path,
    name: &str,
    head: Option<String>,
    undo: bool,
    entries: BTreeMap<String, Option<Entry>>,
) -> Result<Snapshot> {
    let name: String = name.trim().chars().take(80).collect();
    let mut item = Snapshot {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        timestamp: git::now(),
        head,
        undo,
        entries,
    };
    workspace::save(&manifest(area, &item.id)?, &item)?;
    item.entries.clear();
    Ok(item)
}

pub fn delete(root: &Path, p: &Project, id: &str) -> Result<()> {
    let _guard = SNAPSHOTS.lock().unwrap();
    let area = area(root, p);
    fs::remove_file(manifest(&area, id)?).map_err(error)?;
    cleanup(&area)
}

fn version(entry: Option<&Entry>) -> String {
    entry
        .map(|e| format!("{}:{}:{}", e.kind, e.mode, e.digest))
        .unwrap_or_else(|| "missing".into())
}

fn change(before: Option<&Entry>, after: Option<&Entry>) -> &'static str {
    match (before, after) {
        (None, Some(_)) => "add",
        (Some(_), None) => "remove",
        _ => "modify",
    }
}

pub fn preview(root: &Path, p: &Project, id: &str) -> Result<Vec<PreviewRow>> {
    let _guard = SNAPSHOTS.lock().unwrap();
    check_project(p)?;
    let area = area(root, p);
    let item = load(&manifest(&area, id)?)?;
    let mut candidates: BTreeSet<_> = item.entries.keys().cloned().collect();
    if !item.undo {
        candidates.extend(paths(p)?);
    }
    let mut rows = Vec::new();
    for relative in candidates {
        let path = safe_path(p, &relative)?;
        if path.is_dir() && !item.entries.contains_key(&relative) {
            continue;
        }
        let before = read_entry(&path)?.map(|(entry, _)| entry);
        let after = item.entries.get(&relative).and_then(Option::as_ref);
        if before.as_ref() != after {
            rows.push(PreviewRow {
                path: relative,
                version: version(before.as_ref()),
                kind: change(before.as_ref(), after),
            });
        }
    }
    Ok(rows)
}

fn show(entry: &Entry, bytes: &[u8]) -> String {
    let text = &bytes[..bytes.len().min(65536)];
    if entry.kind == "link" {
        return format!("符号链接 → {}", String::from_utf8_lossy(text));
    }
    if text.contains(&0) || std::str::from_utf8(bytes).is_err() {
        return format!("二进制文件 · {} 字节 · {}", entry.bytes, entry.digest);
    }
    let mut rendered = String::from_utf8_lossy(text).into_owned();
    if text.len() < bytes.len() {
        rendered.push_str("\n仅显示部分内容");
    }
    rendered
}

fn intended(p: &Project, item: &Snapshot, relative: &str) -> Result<Option<Entry>> {
    if let Some(entry) = item.entries.get(relative) {
        return Ok(entry.clone());
    }
    if !item.undo && paths(p)?.contains(relative) {
        return Ok(None);
    }
    Err("该文件不在快照比较范围内。".into())
}

fn contents(area: &Path, entry: &Entry) -> Result<Vec<u8>> {
    let data = fs::read(blob(area, entry)?).map_err(error)?;
    if data.len() as u64 != entry.bytes || digest(&data) != entry.digest {
        return Err("快照文件完整性校验失败。".into());
    }
    Ok(data)
}

pub fn preview_file(root: &Path, p: &Project, id: &str, relative: &str) -> Result<FilePreview> {
    let _guard = SNAPSHOTS.lock().unwrap();
    check_project(p)?;
    let area = area(root, p);
    let item = load(&manifest(&area, id)?)?;
    let current = read_entry(&safe_path(p, relative)?)?;
    let before = current.as_ref().map(|(entry, _)| entry);
    let after = intended(p, &item, relative)?;
    let text = after
        .as_ref()
        .map(|entry| contents(&area, entry).map(|bytes| show(entry, &bytes)))
        .transpose()?;
    Ok(FilePreview {
        before: current.as_ref().map(|(entry, data)| show(entry, data)),
        after: text,
        version: version(before),
        kind: change(before, after.as_ref()),
    })
}

pub fn restore(
    root: &Path,
    p: &Project,
    id: &str,
    relative: &str,
    expected: &str,
) -> Result<Snapshot> {
    let _guard = SNAPSHOTS.lock().unwrap();
    check_project(p)?;
    let area = area(root, p);
    let item = load(&manifest(&area, id)?)?;
    let path = safe_path(p, relative)?;
    let current = read_entry(&path)?;
    let before = current.as_ref().map(|(entry, _)| entry);
    if version(before) != expected {
        return Err("文件已改变，请重新预览。".into());
    }
    let after = intended(p, &item, relative)?;
    if before == after.as_ref() {
        return Err("文件已与快照一致。".into());
    }
    let data = after
        .as_ref()
        .map(|entry| contents(&area, entry))
        .transpose()?;
    let mut used = sum_bytes(&area.join("files"))?;
    if let Some((entry, bytes)) = &current {
        put(&area, entry, bytes, &mut used)?;
    }
    let undo = publish(
        &area,
        &format!("恢复前的 {relative}"),
        git::snapshot_head(p)?,
        true,
        BTreeMap::from([(relative.to_owned(), before.cloned())]),
    )?;
    if version(
        read_entry(&safe_path(p, relative)?)?
            .as_ref()
            .map(|(entry, _)| entry),
    ) != expected
    {
        return Err("恢复前文件又发生变化，请重新预览。".into());
    }
    match (after, data) {
        (Some(entry), Some(bytes)) => {
            let parent = path.parent().ok_or("文件缺少上级目录。")?;
            fs::create_dir_all(parent).map_err(error)?;
            if entry.kind == "link" {
                use std::os::unix::{ffi::OsStringExt, fs::symlink};
                let temp = tempfile::tempdir_in(parent).map_err(error)?;
                let link = temp.path().join("link");
                symlink(std::ffi::OsString::from_vec(bytes), &link).map_err(error)?;
                fs::rename(link, path).map_err(error)?;
            } else {
                let mut file = NamedTempFile::new_in(parent).map_err(error)?;
                file.write_all(&bytes).map_err(error)?;
                file.as_file()
                    .set_permissions(fs::Permissions::from_mode(entry.mode))
                    .map_err(error)?;
                file.as_file().sync_all().map_err(error)?;
                file.persist(path).map_err(error)?;
            }
        }
        _ => fs::remove_file(path).map_err(error)?,
    }
    Ok(undo)
}
