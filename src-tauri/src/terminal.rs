use base64::{engine::general_purpose::STANDARD, Engine};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::Path,
    thread,
};
use tauri::ipc::Channel;

pub struct Terminal {
    pub id: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}
impl Terminal {
    pub fn open(path: &Path, rows: u16, cols: u16, events: Channel<Value>) -> Result<Self, String> {
        let pair = native_pty_system()
            .openpty(size(rows, cols))
            .map_err(|e| e.to_string())?;
        let mut command = CommandBuilder::new_default_prog();
        command.cwd(path);
        command.env("TERM", "xterm-256color");
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let id = uuid::Uuid::new_v4().to_string();
        let session = id.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(len) = reader.read(&mut buffer) {
                if len == 0 {
                    break;
                }
                if events.send(json!({"kind":"output","id":session,"data":STANDARD.encode(&buffer[..len])})).is_err(){break}
            }
            let exit = child.wait().ok().map(|s| s.exit_code());
            let _ = events.send(json!({"kind":"exit","id":session,"exit":exit}));
        });
        Ok(Self {
            id,
            master: pair.master,
            writer,
            killer,
        })
    }
    pub fn write(&mut self, input: &str) -> Result<(), String> {
        self.writer
            .write_all(input.as_bytes())
            .and_then(|_| self.writer.flush())
            .map_err(|e| e.to_string())
    }
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        self.master
            .resize(size(rows, cols))
            .map_err(|e| e.to_string())
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}
fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(1, 400),
        cols: cols.clamp(1, 800),
        pixel_width: 0,
        pixel_height: 0,
    }
}
