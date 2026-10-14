//! 不用终端也能用：
//! - `ClaudePet.exe --mcp`：给 Claude 桌面版用的 MCP 桥。桌面版按行发 JSON-RPC（stdio），
//!   这里原样转给正在运行的宠物（本地 http /mcp）；宠物没开就先把它叫起来。
//! - 把宠物注册进 Claude 桌面版的 claude_desktop_config.json（普通聊天和 Code 标签页都能用宠物工具）。
//! - 在后台安装 Claude Code、弹浏览器登录（宠物自己“思考”要用它，走的是你的 Max 额度）。

use crate::plog;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const SERVER_NAME: &str = "claude-pet";
const DEFAULT_PORT: u16 = 47321;

// ───────────────────────── MCP 桥（独立进程，不启动界面） ─────────────────────────

/// 宠物的设置文件里记的端口（桥是单独的进程，拿不到 Tauri 的路径，按固定位置读）
fn read_port() -> u16 {
    let Some(appdata) = std::env::var_os("APPDATA") else { return DEFAULT_PORT };
    let p = PathBuf::from(appdata).join("com.claudepet.app").join("config.json");
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["port"].as_u64())
        .map(|p| p as u16)
        .unwrap_or(DEFAULT_PORT)
}

/// 发一个 http 请求给本机的宠物。返回 (状态码, 正文)
fn http(port: u16, method: &str, path: &str, body: &str, timeout: Duration) -> std::io::Result<(u16, String)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(800))?;
    s.set_read_timeout(Some(timeout))?;
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let chunked = head.lines().any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    Ok((status, if chunked { dechunk(rest) } else { rest.to_string() }))
}

fn dechunk(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || after.len() < n {
            break;
        }
        out.push_str(&after[..n]);
        rest = after[n..].trim_start_matches("\r\n");
    }
    out
}

fn pet_alive(port: u16) -> bool {
    matches!(http(port, "GET", "/health", "", Duration::from_secs(2)), Ok((200, _)))
}

/// 宠物没开着就把它启动起来，最多等 12 秒
fn ensure_running(port: u16) -> bool {
    if pet_alive(port) {
        return true;
    }
    plog!("宠物没在运行，桥来启动它");
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(&exe);
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // 脱离桌面版的进程组，桌面版关掉 MCP 时宠物不跟着被关
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
            match cmd.spawn() {
                Ok(c) => plog!("已启动宠物（进程 {}，脱离了桌面版的作业）", c.id()),
                Err(e) => {
                    plog!("脱离作业启动失败（{e}），改成普通方式启动");
                    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
                    match cmd.spawn() {
                        Ok(c) => plog!("已启动宠物（进程 {}，还在桌面版的作业里，桌面版退出时可能被一起关掉）", c.id()),
                        Err(e) => plog!("!!! 启动宠物失败：{e}"),
                    }
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = cmd.spawn();
        }
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(12) {
        std::thread::sleep(Duration::from_millis(300));
        if pet_alive(port) {
            plog!("宠物 {}ms 后可以连上了", start.elapsed().as_millis());
            return true;
        }
    }
    plog!("!!! 等了 12 秒宠物还是连不上");
    false
}

/// `ClaudePet.exe --mcp [--port N]`：一直读 stdin，一行一个 JSON-RPC 消息
pub fn mcp_bridge(args: &[String]) -> ! {
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(read_port);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        let method = req["method"].as_str().unwrap_or("").to_string();
        let what = if method == "tools/call" { format!("调用 {}", req["params"]["name"].as_str().unwrap_or("?")) } else { method.clone() };
        let t0 = Instant::now();
        let send = || http(port, "POST", "/mcp", line, Duration::from_secs(300));
        let resp = match send() {
            Ok(r) => Ok(r),
            Err(_) => {
                if ensure_running(port) {
                    send()
                } else {
                    Err(std::io::Error::other("宠物没有运行"))
                }
            }
        };
        match &resp {
            Ok((code, _)) if *code == 200 || *code == 202 => {
                if method == "initialize" || method == "tools/call" {
                    plog!("桌面版 {what}：{}ms", t0.elapsed().as_millis());
                }
            }
            Ok((code, _)) => plog!("!!! 桌面版 {what}：宠物返回 {code}"),
            Err(e) => plog!("!!! 桌面版 {what}：连不上宠物（{e}）"),
        }
        let body = match resp {
            Ok((200, b)) => b.trim().to_string(),
            Ok((202, _)) => String::new(),
            Ok((code, b)) => error_for(&req, &format!("宠物返回 {code}：{}", b.trim())),
            Err(e) => error_for(&req, &format!("连不上桌面宠物（{e}）。请先打开 ClaudePet.exe。")),
        };
        if !body.is_empty() {
            let _ = writeln!(out, "{body}");
            let _ = out.flush();
        }
    }
    plog!("桌面版关掉了这个桥（输入结束）");
    std::process::exit(0)
}

/// 请求（带 id 的）出错时回一个 JSON-RPC 错误；通知（没 id）不用回
fn error_for(req: &Value, msg: &str) -> String {
    match req.get("id") {
        Some(id) if !id.is_null() => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": msg}}).to_string(),
        _ => String::new(),
    }
}

// ───────────────────────── 注册进 Claude 桌面版 ─────────────────────────

/// Claude 桌面版的设置文件。普通安装在 %APPDATA%\Claude；
/// 从微软商店装的（MSIX）会被重定向到 %LOCALAPPDATA%\Packages\Claude_xxx\LocalCache\Roaming\Claude。
fn desktop_config_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        v.push(PathBuf::from(appdata).join("Claude").join("claude_desktop_config.json"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if let Ok(rd) = std::fs::read_dir(PathBuf::from(local).join("Packages")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("Claude_") || name.contains("AnthropicPBC") {
                    v.push(e.path().join("LocalCache").join("Roaming").join("Claude").join("claude_desktop_config.json"));
                }
            }
        }
    }
    v
}

/// 装了桌面版的那几个设置文件（文件夹存在才算）
fn installed_config_files() -> Vec<PathBuf> {
    desktop_config_files().into_iter().filter(|p| p.parent().map(|d| d.is_dir()).unwrap_or(false)).collect()
}

fn our_entry(port: u16) -> Value {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    json!({"command": exe, "args": ["--mcp", "--port", port.to_string()]})
}

fn read_json(p: &PathBuf) -> Result<Value, String> {
    match std::fs::read_to_string(p) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).map_err(|e| format!("{} 格式有问题，我不敢改：{e}", p.display())),
        _ => Ok(json!({})),
    }
}

fn write_json(p: &PathBuf, v: &Value) -> Result<(), String> {
    if p.exists() {
        let bak = p.with_file_name(format!("claude_desktop_config.json.bak-claudepet-{}", chrono::Local::now().format("%Y%m%d%H%M%S")));
        std::fs::copy(p, &bak).map_err(|e| format!("备份失败：{e}"))?;
    }
    std::fs::write(p, serde_json::to_string_pretty(v).unwrap()).map_err(|e| format!("写入 {} 失败：{e}", p.display()))
}

pub fn desktop_status(port: u16) -> Value {
    let files = installed_config_files();
    let want = our_entry(port);
    let mut registered = false;
    let mut stale = false;
    for f in &files {
        if let Ok(v) = read_json(f) {
            let e = &v["mcpServers"][SERVER_NAME];
            if e.is_object() {
                registered = true;
                if *e != want {
                    stale = true;
                }
            }
        }
    }
    json!({"installed": !files.is_empty(), "registered": registered, "stale": stale, "files": files.iter().map(|f| f.to_string_lossy().to_string()).collect::<Vec<_>>()})
}

pub fn register_desktop(port: u16) -> Result<String, String> {
    let files = installed_config_files();
    if files.is_empty() {
        return Err("没找到 Claude 桌面版的设置文件夹。先装好 Claude 桌面版并打开一次，再点这里。".into());
    }
    for f in &files {
        let mut root = read_json(f)?;
        if !root.is_object() {
            return Err(format!("{} 不是对象格式，我不敢改", f.display()));
        }
        if !root["mcpServers"].is_object() {
            root["mcpServers"] = json!({});
        }
        root["mcpServers"][SERVER_NAME] = our_entry(port);
        write_json(f, &root)?;
    }
    Ok("接好了。请把 Claude 桌面版完全退出（右下角托盘图标右键 → 退出）再打开，之后在聊天里就能让 Claude 用宠物说话、看屏幕、指按钮、帮你点。".into())
}

pub fn unregister_desktop() -> Result<String, String> {
    let mut n = 0;
    for f in installed_config_files() {
        let mut root = read_json(&f)?;
        if let Some(m) = root["mcpServers"].as_object_mut() {
            if m.remove(SERVER_NAME).is_some() {
                n += 1;
                write_json(&f, &root)?;
            }
        }
    }
    Ok(if n == 0 { "本来就没接入".into() } else { "已经从 Claude 桌面版移除，重开桌面版生效。".into() })
}

/// 启动时：注册过但 exe 挪了位置（或端口改了），悄悄改成现在的路径
pub fn fix_stale_registration(port: u16) {
    let st = desktop_status(port);
    if st["registered"].as_bool() == Some(true) && st["stale"].as_bool() == Some(true) {
        match register_desktop(port) {
            Ok(_) => crate::plog!("Claude 桌面版里登记的宠物路径变了，已更新"),
            Err(e) => crate::plog!("更新桌面版登记失败：{e}"),
        }
    }
}

// ───────────────────────── 后台安装、登录 Claude Code ─────────────────────────

#[cfg(windows)]
fn hide_window(cmd: &mut tokio::process::Command) {
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
fn hide_window(_cmd: &mut tokio::process::Command) {}

fn setup_log(line: &str) {
    let line = strip_ansi(line);
    if line.trim().is_empty() {
        return;
    }
    crate::plog!("[安装/登录] {line}");
    if let Some(a) = crate::state::try_app() {
        a.emit_panel("setup-log", json!({"line": line}));
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for c2 in it.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// 把子进程的 stdout / stderr 一行行转给设置页，顺便把里面的网址挑出来
fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(r: R, urls: Option<tokio::sync::mpsc::UnboundedSender<String>>) {
    use tokio::io::AsyncBufReadExt;
    tauri::async_runtime::spawn(async move {
        let mut lines = tokio::io::BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            setup_log(&l);
            if let Some(tx) = &urls {
                if let Some(u) = find_url(&strip_ansi(&l)) {
                    let _ = tx.send(u);
                }
            }
        }
    });
}

fn find_url(s: &str) -> Option<String> {
    let i = s.find("https://")?;
    let u: String = s[i..].chars().take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'' && *c != '>').collect();
    (u.len() > 12).then_some(u)
}

/// 官方安装脚本（irm https://claude.ai/install.ps1 | iex），装到 %USERPROFILE%\.local\bin\claude.exe，不需要管理员
pub async fn install_cli() -> Result<String, String> {
    if crate::claude::find_claude().is_some() {
        return Ok("Claude Code 已经装好了。".into());
    }
    setup_log("开始安装 Claude Code（官方安装脚本，几十秒到几分钟）…");
    let mut cmd = tokio::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", "irm https://claude.ai/install.ps1 | iex"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("启动安装失败：{e}"))?;
    if let Some(o) = child.stdout.take() {
        pump(o, None);
    }
    if let Some(e) = child.stderr.take() {
        pump(e, None);
    }
    let status = tokio::time::timeout(Duration::from_secs(15 * 60), child.wait())
        .await
        .map_err(|_| "安装超过 15 分钟没结束，先停了。可以再点一次试试。".to_string())?
        .map_err(|e| e.to_string())?;
    match crate::claude::find_claude() {
        Some(_) => {
            let v = crate::claude::run_claude_cmd(&["--version"]).await.unwrap_or_default();
            Ok(format!("装好了：{v}。下一步点“登录”。"))
        }
        None => Err(format!("安装脚本结束了（{status}），但还是没找到 claude.exe。把设置页下面的安装记录截图发我。")),
    }
}

static LOGIN_STDIN: tokio::sync::Mutex<Option<tokio::process::ChildStdin>> = tokio::sync::Mutex::const_new(None);

pub async fn logged_in() -> bool {
    crate::claude::run_claude_cmd(&["auth", "status"]).await.is_ok()
}

/// `claude auth login`：会自己打开浏览器，你在网页上点授权就行。
/// 网页如果给了一串代码让你粘贴，设置页会出一个输入框，粘进去点确定。
pub async fn login_cli() -> Result<String, String> {
    let exe = crate::claude::find_claude().ok_or("还没装 Claude Code，先点“安装”。")?;
    if logged_in().await {
        return Ok("已经登录了。".into());
    }
    let mut cmd = crate::claude::command_for(&exe);
    cmd.args(["auth", "login"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::claude::clean_env(&mut cmd);
    hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("启动登录失败：{e}"))?;
    *LOGIN_STDIN.lock().await = child.stdin.take();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    if let Some(o) = child.stdout.take() {
        pump(o, Some(tx.clone()));
    }
    if let Some(e) = child.stderr.take() {
        pump(e, Some(tx));
    }
    let start = Instant::now();
    let mut sent_url = false;
    let result = loop {
        tokio::select! {
            u = rx.recv() => {
                if let (Some(u), Some(a)) = (u, crate::state::try_app()) {
                    if !sent_url {
                        sent_url = true;
                        a.emit_panel("login-url", json!({"url": u}));
                    }
                }
            }
            st = child.wait() => {
                // 登录进程自己结束了：成功就好，失败把原因交给设置页
                let ok = logged_in().await;
                break if ok { Ok("登录好了，宠物现在用你的 Max 账号思考。".to_string()) } else { Err(format!("登录没有完成（{}）。可以再点一次“登录”。", st.map(|s| s.to_string()).unwrap_or_default())) };
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                if logged_in().await {
                    break Ok("登录好了，宠物现在用你的 Max 账号思考。".to_string());
                }
                if start.elapsed() > Duration::from_secs(10 * 60) {
                    break Err("等了 10 分钟还没登录，先停了。".to_string());
                }
            }
        }
    };
    let _ = child.kill().await;
    *LOGIN_STDIN.lock().await = None;
    result
}

/// 网页给的授权码，粘到登录进程里
pub async fn login_code(code: String) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut g = LOGIN_STDIN.lock().await;
    let s = g.as_mut().ok_or("登录没在进行中，先点“登录”。")?;
    s.write_all(format!("{}\n", code.trim()).as_bytes()).await.map_err(|e| e.to_string())?;
    s.flush().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dechunks() {
        assert_eq!(dechunk("5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"), "hello world");
    }
    #[test]
    fn finds_urls() {
        assert_eq!(find_url("Browser didn't open? Use the url below: https://claude.ai/oauth/authorize?code=true&x=1").as_deref(), Some("https://claude.ai/oauth/authorize?code=true&x=1"));
        assert_eq!(strip_ansi("\u{1b}[32mok\u{1b}[0m\r"), "ok");
    }
    #[test]
    fn rpc_errors_only_for_requests() {
        assert!(error_for(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), "x").is_empty());
        assert!(error_for(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}), "x").contains("\"id\":3"));
    }
}
