//! 通过本机的 Claude Code（claude 命令，登录的是你的 Max 账号）来思考。
//! 每个用途开一个常驻进程（stream-json 输入输出），一问一答不用每次重启，开口更快。

use crate::plog;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

pub struct Proc {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

pub struct Session {
    pub name: String,
    model: std::sync::Mutex<String>,
    system_prompt: String,
    max_turns: u32,
    turns: AtomicU32,
    proc: Mutex<Option<Proc>>,
    /// 每次 kill() 加一，正在进行的回答看到它变了就停
    epoch: AtomicU64,
    workdir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum Block {
    Text(String),
    /// JPEG 图片
    Image(Vec<u8>),
}

pub struct Answer {
    pub text: String,
    pub interrupted: bool,
}

impl Session {
    pub fn new(name: &str, model: &str, system_prompt: String, max_turns: u32, workdir: PathBuf) -> Session {
        Session {
            name: name.into(),
            model: std::sync::Mutex::new(model.into()),
            system_prompt,
            max_turns,
            turns: AtomicU32::new(0),
            proc: Mutex::new(None),
            epoch: AtomicU64::new(0),
            workdir,
        }
    }

    pub fn set_model(&self, m: &str) {
        let mut cur = self.model.lock().unwrap();
        if *cur != m {
            *cur = m.into();
            // 下次提问时用新模型重开
            self.turns.store(u32::MAX / 2, Ordering::SeqCst);
        }
    }

    #[allow(dead_code)]
    pub fn is_busy(&self) -> bool {
        self.proc.try_lock().is_err()
    }

    /// 立刻打断当前回答（杀掉进程，下次提问自动重开）
    pub async fn kill(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        // 正在回答的那个任务持有锁，它看到 epoch 变化会自己退出并清掉进程
        if let Ok(mut g) = self.proc.try_lock() {
            if let Some(mut p) = g.take() {
                let _ = p.child.start_kill();
            }
        }
    }

    /// 重开对话（清空记忆）
    pub async fn reset(&self) {
        self.turns.store(u32::MAX / 2, Ordering::SeqCst);
    }

    /// 提前启动进程，第一次提问就不用等启动
    pub async fn warm(&self, ping: bool) {
        let mut g = self.proc.lock().await;
        if g.is_none() {
            match self.spawn().await {
                Ok(p) => *g = Some(p),
                Err(e) => {
                    plog!("[{}] 预热失败：{e}", self.name);
                    return;
                }
            }
        }
        drop(g);
        if ping {
            let _ = self.ask(vec![Block::Text("预热：只回复“好”。".into())], |_| {}).await;
        }
    }

    async fn spawn(&self) -> Result<Proc, String> {
        let model = self.model.lock().unwrap().clone();
        let exe = find_claude().ok_or("没找到 claude 命令。请先安装 Claude Code 并登录（在终端运行 claude）。")?;
        let _ = std::fs::create_dir_all(&self.workdir);
        let prompt_file = self.workdir.join(format!("prompt-{}.txt", self.name));
        std::fs::write(&prompt_file, &self.system_prompt).map_err(|e| e.to_string())?;
        let settings_file = self.workdir.join("pet-settings.json");
        let _ = std::fs::write(&settings_file, r#"{"disableAllHooks":true,"alwaysThinkingEnabled":false}"#);

        let mut args: Vec<String> = vec![
            "-p".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
            "--model".into(),
            model.clone(),
            "--system-prompt-file".into(),
            prompt_file.to_string_lossy().into(),
            "--settings".into(),
            settings_file.to_string_lossy().into(),
            "--strict-mcp-config".into(),
            "--disallowedTools".into(),
            "*".into(),
            "--no-session-persistence".into(),
            "--disable-slash-commands".into(),
        ];
        let mut cmd = command_for(&exe);
        clean_env(&mut cmd);
        cmd.args(args.drain(..))
            .current_dir(&self.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("MAX_THINKING_TOKENS", "0")
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：别弹黑框
        }
        let mut child = cmd.spawn().map_err(|e| format!("启动 claude 失败：{e}"))?;
        let stdin = child.stdin.take().ok_or("拿不到 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到 stdout")?;
        self.turns.store(0, Ordering::SeqCst);
        plog!("[{}] 启动 Claude Code（模型 {model}）", self.name);
        Ok(Proc { child, stdin, lines: BufReader::new(stdout).lines() })
    }

    /// 问一句，流式回调每段新文字，返回完整回答
    pub async fn ask(&self, content: Vec<Block>, mut on_delta: impl FnMut(&str)) -> Result<Answer, String> {
        let my_epoch = self.epoch.load(Ordering::SeqCst);
        let mut guard = self.proc.lock().await;
        if self.epoch.load(Ordering::SeqCst) != my_epoch {
            return Ok(Answer { text: String::new(), interrupted: true });
        }
        if self.turns.load(Ordering::SeqCst) >= self.max_turns {
            if let Some(mut p) = guard.take() {
                let _ = p.child.start_kill();
            }
        }
        if guard.is_none() {
            *guard = Some(self.spawn().await?);
        }
        let proc = guard.as_mut().unwrap();

        let blocks: Vec<Value> = content
            .into_iter()
            .map(|b| match b {
                Block::Text(t) => json!({"type": "text", "text": t}),
                Block::Image(bytes) => json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/jpeg", "data": base64_encode(&bytes)}
                }),
            })
            .collect();
        let line = json!({
            "type": "user",
            "message": {"role": "user", "content": blocks},
            "parent_tool_use_id": null,
            "session_id": "default"
        })
        .to_string();
        let write = async {
            proc.stdin.write_all(line.as_bytes()).await?;
            proc.stdin.write_all(b"\n").await?;
            proc.stdin.flush().await
        };
        if let Err(e) = write.await {
            *guard = None;
            return Err(format!("Claude Code 进程已退出：{e}"));
        }
        self.turns.fetch_add(1, Ordering::SeqCst);

        let mut text = String::new();
        let mut got_delta = false;
        loop {
            if self.epoch.load(Ordering::SeqCst) != my_epoch {
                if let Some(mut p) = guard.take() {
                    let _ = p.child.start_kill();
                }
                return Ok(Answer { text, interrupted: true });
            }
            let proc = guard.as_mut().unwrap();
            let next = tokio::time::timeout(Duration::from_millis(250), proc.lines.next_line()).await;
            let line = match next {
                Err(_) => continue, // 超时只是为了定期检查有没有被打断
                Ok(Ok(Some(l))) => l,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    *guard = None;
                    if text.is_empty() {
                        return Err("Claude Code 意外退出了。请确认已经在终端里运行过 claude 并登录。".into());
                    }
                    return Ok(Answer { text, interrupted: false });
                }
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            match v["type"].as_str().unwrap_or("") {
                "stream_event" => {
                    let ev = &v["event"];
                    if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                        if let Some(t) = ev["delta"]["text"].as_str() {
                            got_delta = true;
                            text.push_str(t);
                            on_delta(t);
                        }
                    }
                }
                "assistant" => {
                    // 没开流式时（或者旧版本）从完整消息里拿文字
                    if !got_delta {
                        if let Some(arr) = v["message"]["content"].as_array() {
                            for b in arr {
                                if b["type"] == "text" {
                                    if let Some(t) = b["text"].as_str() {
                                        text.push_str(t);
                                        on_delta(t);
                                    }
                                }
                            }
                        }
                    }
                }
                "result" => {
                    let is_err = v["is_error"].as_bool().unwrap_or(false);
                    if is_err {
                        let msg = v["result"].as_str().unwrap_or("未知错误").to_string();
                        plog!("[{}] Claude 返回错误：{msg}", self.name);
                        return Err(friendly_error(&msg));
                    }
                    if text.is_empty() {
                        if let Some(r) = v["result"].as_str() {
                            text = r.to_string();
                            on_delta(r);
                        }
                    }
                    return Ok(Answer { text, interrupted: false });
                }
                "system" => {
                    if v["subtype"] == "api_retry" {
                        plog!("[{}] API 重试：{}", self.name, v["error"]);
                    }
                }
                _ => {}
            }
        }
    }
}

fn friendly_error(msg: &str) -> String {
    let m = msg.to_lowercase();
    if m.contains("login") || m.contains("auth") || m.contains("api key") || m.contains("401") {
        "Claude Code 还没登录。右键我 → 设置 → 点“登录”，在弹出的网页里用你的 Max 账号授权一次。".into()
    } else if m.contains("rate") || m.contains("limit") || m.contains("quota") || m.contains("usage") {
        format!("额度暂时用完了：{msg}")
    } else {
        msg.chars().take(200).collect()
    }
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// 去掉回答外面的 ```json 包装，取出第一个 JSON 对象/数组
pub fn extract_json(s: &str) -> Option<Value> {
    let t = s.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    let start = t.find(['{', '['])?;
    let open = t.as_bytes()[start];
    let close = if open == b'{' { '}' } else { ']' };
    let end = t.rfind(close)?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&t[start..=end]).ok()
}

// ───────────────────────── 找 claude 命令 ─────────────────────────

#[derive(Debug, Clone)]
pub enum ClaudeExe {
    Exe(PathBuf),
    NodeScript { node: PathBuf, script: PathBuf },
}

static OVERRIDE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_override(path: &str) {
    *OVERRIDE.lock().unwrap() = if path.trim().is_empty() { None } else { Some(path.trim().to_string()) };
}

/// 起一个 claude 子进程（npm 装的走 node + 脚本）
pub fn command_for(exe: &ClaudeExe) -> Command {
    match exe {
        ClaudeExe::Exe(p) => Command::new(p),
        ClaudeExe::NodeScript { node, script } => {
            let mut c = Command::new(node);
            c.arg(script);
            c
        }
    }
}

/// 去掉会让 claude 改走别的计费或别的身份的环境变量：
/// 有 ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN 时 claude -p 会优先用它们（按 API 收钱），宠物要走 Max 订阅
pub fn clean_env(cmd: &mut Command) {
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN");
}

/// 这个文件是不是 Claude 桌面版（Electron 程序，也叫 claude.exe）而不是 Claude Code：
/// 桌面版的商店应用别名在 WindowsApps 里；普通安装旁边有 resources\app.asar。拿桌面版当 Claude Code 跑会把桌面版打开。
fn is_desktop_app(p: &std::path::Path) -> bool {
    let s = p.to_string_lossy().to_lowercase();
    if s.contains("\\windowsapps\\") || s.contains("/windowsapps/") || s.contains("anthropicclaude") {
        return true;
    }
    p.parent().map(|d| d.join("resources").join("app.asar").exists() || d.join("Update.exe").exists()).unwrap_or(false)
}

pub fn find_claude() -> Option<ClaudeExe> {
    if let Some(p) = OVERRIDE.lock().unwrap().clone() {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return Some(resolve_shim(pb));
        }
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let exe_names: &[&str] = if cfg!(windows) { &["claude.exe", "claude.cmd", "claude.bat"] } else { &["claude"] };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(h) = &home {
        candidates.push(h.join(".local").join("bin").join(if cfg!(windows) { "claude.exe" } else { "claude" }));
        candidates.push(h.join(".claude").join("local").join(if cfg!(windows) { "claude.exe" } else { "claude" }));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for n in exe_names {
                candidates.push(dir.join(n));
            }
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("npm").join("claude.cmd"));
    }
    candidates.into_iter().find(|p| p.is_file() && !is_desktop_app(p)).map(resolve_shim)
}

/// npm 装的是 claude.cmd 外壳，直接用 node 跑里面的脚本，免得经过 cmd 转义参数
fn resolve_shim(p: PathBuf) -> ClaudeExe {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "cmd" || ext == "bat" {
        if let Some(dir) = p.parent() {
            let script = dir.join("node_modules").join("@anthropic-ai").join("claude-code").join("cli.js");
            if script.is_file() {
                let node_local = dir.join("node.exe");
                let node = if node_local.is_file() { node_local } else { PathBuf::from("node") };
                return ClaudeExe::NodeScript { node, script };
            }
        }
    }
    ClaudeExe::Exe(p)
}

/// 跑一条 claude 子命令（比如 mcp add），返回输出
pub async fn run_claude_cmd(args: &[&str]) -> Result<String, String> {
    let exe = find_claude().ok_or("没找到 claude 命令")?;
    let mut cmd = command_for(&exe);
    clean_env(&mut cmd);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| "超时".to_string())?
        .map_err(|e| e.to_string())?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    let e = String::from_utf8_lossy(&out.stderr);
    if !e.trim().is_empty() {
        s.push('\n');
        s.push_str(&e);
    }
    if out.status.success() {
        Ok(s.trim().to_string())
    } else {
        Err(s.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_extraction() {
        assert_eq!(extract_json("```json\n{\"a\":1}\n```").unwrap()["a"], 1);
        assert_eq!(extract_json("好的：[\"x\",\"y\"]").unwrap()[1], "y");
        assert!(extract_json("没有").is_none());
    }
    #[test]
    fn skips_desktop_app() {
        assert!(is_desktop_app(std::path::Path::new("C:\\Users\\a\\AppData\\Local\\Microsoft\\WindowsApps\\claude.exe")));
        assert!(is_desktop_app(std::path::Path::new("C:\\Users\\a\\AppData\\Local\\AnthropicClaude\\claude.exe")));
        assert!(!is_desktop_app(std::path::Path::new("C:\\Users\\a\\.local\\bin\\claude.exe")));
    }
}
