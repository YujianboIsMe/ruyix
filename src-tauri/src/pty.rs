use portable_pty::{CommandBuilder, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// 单个 PTY 会话
struct PtySession {
    reader: Box<dyn Read + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    #[allow(dead_code)]
    master: Box<dyn portable_pty::MasterPty + Send>,
    #[allow(dead_code)]
    child: Box<dyn portable_pty::Child + Send + 'static>,
}

/// PTY 管理器
pub struct PtyManager {
    sessions: HashMap<String, Arc<Mutex<PtySession>>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// 启动 PTY 并绑定到 tab_id
    pub fn spawn(
        &mut self,
        window: tauri::Window,
        tab_id: String,
        cmd: &str,
    ) -> Result<(), String> {
        let parts = crate::split_cmd(cmd);
        if parts.is_empty() {
            return Err("空命令".to_string());
        }

        let pty_system = portable_pty::native_pty_system();
        let pty_pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("创建 PTY 失败: {}", e))?;

        let reader = pty_pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone reader 失败: {}", e))?;
        let writer = Arc::new(Mutex::new(
            pty_pair
                .master
                .take_writer()
                .map_err(|e| format!("take writer 失败: {}", e))?,
        ));

        let mut cmd_builder = CommandBuilder::new(&parts[0]);
        cmd_builder.args(&parts[1..]);
        cmd_builder.cwd(std::env::current_dir().unwrap_or_default());
        cmd_builder.env("PYTHONIOENCODING", "utf-8");
        cmd_builder.env("PYTHONUTF8", "1");
        cmd_builder.env("TERM", "xterm-256color");

        let child = pty_pair
            .slave
            .spawn_command(cmd_builder)
            .map_err(|e| format!("启动进程失败: {}", e))?;

        let session = Arc::new(Mutex::new(PtySession {
            reader,
            writer,
            master: pty_pair.master,
            child,
        }));

        // 读取线程：PTY → 前端
        let session_clone = session.clone();
        let tid = tab_id.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = {
                    let mut s = session_clone.lock().unwrap();
                    match s.reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    }
                };
                let data = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = window.emit(&format!("pty-out-{}", tid), data);
            }
            let _ = window.emit(&format!("pty-exit-{}", tid), ());
        });

        self.sessions.insert(tab_id, session);
        Ok(())
    }

    /// 向 PTY 写入数据
    pub fn write(&self, tab_id: &str, data: &str) -> Result<(), String> {
        if let Some(session) = self.sessions.get(tab_id) {
            let mut s = session.lock().map_err(|e| e.to_string())?;
            s.writer
                .lock()
                .map_err(|e| e.to_string())?
                .write_all(data.as_bytes())
                .map_err(|e| format!("写入 PTY 失败: {}", e))?;
        }
        Ok(())
    }

    /// 调整 PTY 大小
    pub fn resize(&self, tab_id: &str, rows: u16, cols: u16) -> Result<(), String> {
        if let Some(session) = self.sessions.get(tab_id) {
            let s = session.lock().map_err(|e| e.to_string())?;
            s.master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| format!("调整终端大小失败: {}", e))?;
        }
        Ok(())
    }

    /// 关闭 PTY 会话
    pub fn close(&mut self, tab_id: &str) {
        self.sessions.remove(tab_id);
    }
}
