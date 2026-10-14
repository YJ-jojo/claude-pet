//! 极简日志：写到 %APPDATA%\com.claudepet.app\logs\pet.log，超过 2MB 自动轮换成 pet.old.log。
//! 每行带上进程身份：[宠物 pid] 是界面本体，[桥 pid] 是 Claude 桌面版启动的 MCP 转发进程，
//! 两种进程写同一个文件，出问题时能看出是谁、按什么顺序发生的。

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static ROLE: OnceLock<String> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// 程序一启动就调用（在 Tauri 起来之前），这样连“窗口建不出来”这种失败也能记下来
pub fn init_early(role: &str) {
    let _ = ROLE.set(format!("[{role} {}]", std::process::id()));
    if let Some(dir) = default_dir() {
        init(dir);
    }
}

/// 和 Tauri 的 app_data_dir 一致：Windows 上是 %APPDATA%\com.claudepet.app
fn default_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
    }?;
    Some(base.join("com.claudepet.app").join("logs"))
}

pub fn init(dir: PathBuf) {
    if LOG_PATH.get().is_some() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("pet.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 2 * 1024 * 1024 {
            let _ = std::fs::rename(&path, dir.join("pet.old.log"));
        }
    }
    let _ = LOG_PATH.set(path);
}

pub fn log_path() -> Option<PathBuf> {
    LOG_PATH.get().cloned()
}

pub fn log(msg: &str) {
    let role = ROLE.get().map(String::as_str).unwrap_or("[宠物]");
    let line = format!("{} {role} {}\n", chrono::Local::now().format("%m-%d %H:%M:%S%.3f"), msg);
    #[cfg(debug_assertions)]
    eprint!("{line}");
    if let Some(p) = LOG_PATH.get() {
        let _g = LOCK.lock();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[macro_export]
macro_rules! plog {
    ($($arg:tt)*) => { $crate::logger::log(&format!($($arg)*)) };
}
