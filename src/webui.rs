//! 对话框（聊天、观察窗口、设置）用浏览器打开：http://127.0.0.1:端口/ui/panel.html?t=口令
//!
//! 以前对话框是程序里的第二个网页窗口。0.3.0 在用户电脑上，每建一个 WebView2 窗口要 8–20 秒，
//! 有时一直建不出来，建的时候程序主线程整个卡住（宠物点不动、拖不动、托盘也没反应）。
//! 现在程序只建宠物一个窗口，对话框交给浏览器：浏览器慢也卡不到宠物。
//!
//! 页面还是原来的 panel.html / panel.js，只在最前面加一段 bridge.js，
//! 把 window.__TAURI__ 的 invoke / listen 换成发到本地服务：
//! - POST /ui/invoke {t, cmd, args} → 调同名命令
//! - POST /ui/poll   {t, since}     → 等新事件（最多 25 秒），返回 since 之后的事件
//!
//! 安全：本地服务只听 127.0.0.1，并检查 Host（防 DNS 重绑定）；/ui 的 POST 还要
//! Origin 是本机这个端口、带对每次启动随机生成的口令。别的网页拿不到口令，也就调不了命令。

use crate::commands;
use crate::plog;
use crate::state::app;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 浏览器标签页的标题：认出它是我们自己的页面，看屏幕、选窗口时跳过它
pub const PAGE_TITLE: &str = "Claude 宠物 · 控制台";

/// 页面能拿的文件
const FILES: &[&str] = &["panel.html", "panel.css", "panel.js", "diag.js"];

const BRIDGE_JS: &str = r#"
// 浏览器里的对话框：把 Tauri 的 invoke / listen 换成发到宠物的本地服务
(function () {
  'use strict';
  const q = new URLSearchParams(location.search);
  const token = q.get('t') || '';
  const firstTab = q.get('tab') || '';
  const handlers = {};
  let since = 0, dead = false;
  async function post(path, body) {
    const r = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(Object.assign({ t: token }, body)) });
    if (r.status === 403) { dead = true; throw new Error('expired'); }
    return r.json();
  }
  async function invoke(cmd, args) {
    const v = await post('/ui/invoke', { cmd, args: args || {} });
    if (v && v.error !== undefined) throw v.error;
    return v ? v.value : null;
  }
  function expired() {
    if (document.getElementById('pet-expired')) return;
    const d = document.createElement('div');
    d.id = 'pet-expired';
    d.textContent = '宠物重启过或已退出，这个页面失效了。请从宠物的托盘菜单重新打开。';
    d.style.cssText = 'position:fixed;left:0;right:0;top:0;z-index:9999;padding:10px 14px;background:#e5484d;color:#fff;font:14px sans-serif;text-align:center';
    document.body.appendChild(d);
  }
  async function loop() {
    let fails = 0;
    while (!dead) {
      try {
        const v = await post('/ui/poll', { since });
        fails = 0;
        since = v.seq;
        for (const e of v.events) for (const fn of handlers[e.event] || []) { try { fn({ event: e.event, payload: e.payload }); } catch (err) { console.error(err); } }
      } catch (err) {
        if (dead) break;
        // 连不上：宠物没在跑。多试几次再说失效
        if (++fails >= 4) { dead = true; break; }
        await new Promise((r) => setTimeout(r, 1500));
      }
    }
    if (document.body) expired(); else window.addEventListener('DOMContentLoaded', expired);
  }
  window.__TAURI__ = {
    core: { invoke },
    event: {
      listen: (ev, fn) => {
        (handlers[ev] = handlers[ev] || []).push(fn);
        if (ev === 'open-tab' && firstTab) setTimeout(() => fn({ event: ev, payload: { tab: firstTab } }), 0);
        return Promise.resolve(() => { handlers[ev] = (handlers[ev] || []).filter((f) => f !== fn); });
      },
    },
  };
  // 先拿到当前的事件序号，之后只收新的
  post('/ui/poll', { since: -1 }).then((v) => { since = v.seq; loop(); }).catch(() => { dead = true; expired(); });
})();
"#;

fn token() -> &'static str {
    static T: OnceLock<String> = OnceLock::new();
    T.get_or_init(|| {
        let mut b = [0u8; 16];
        let _ = getrandom::fill(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect()
    })
}

struct Events {
    seq: u64,
    list: VecDeque<(u64, String, Value)>,
}

static EVENTS: Mutex<Events> = Mutex::new(Events { seq: 0, list: VecDeque::new() });
static NEW_EVENT: Condvar = Condvar::new();

/// 对话框页面关心的事件
const PAGE_EVENTS: &[&str] = &["chat", "config", "flags", "login-url", "open-tab", "setup-log", "targets"];

/// 发给浏览器里的对话框（没人开着也没关系，只留最近 300 条）
pub fn push(event: &str, payload: &Value) {
    if !PAGE_EVENTS.contains(&event) {
        return;
    }
    let mut e = EVENTS.lock().unwrap();
    e.seq += 1;
    let s = e.seq;
    e.list.push_back((s, event.to_string(), payload.clone()));
    while e.list.len() > 300 {
        e.list.pop_front();
    }
    NEW_EVENT.notify_all();
}

pub fn url(tab: &str) -> String {
    format!("http://127.0.0.1:{}/ui/panel.html?t={}&tab={}", app().cfg().port, token(), tab)
}

/// 用默认浏览器打开对话框的某个标签页
pub fn open(tab: &str) {
    let u = url(tab);
    #[cfg(windows)]
    {
        if let Err(e) = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &u]).spawn() {
            plog!("打开浏览器失败：{e}");
            crate::talk::say("浏览器打不开，没法显示设置页。", "error");
            return;
        }
    }
    plog!("在浏览器里打开对话框（{tab}）");
    debug_print(&u);
}

/// 测试用：设了 CLAUDEPET_PRINT_UI_URL 就把网址（含口令）打到标准错误，不进日志
pub fn debug_print(u: &str) {
    if std::env::var_os("CLAUDEPET_PRINT_UI_URL").is_some() {
        eprintln!("UI_URL {u}");
    }
}

/// GET /ui/文件
pub fn file(name: &str) -> Option<(Vec<u8>, String)> {
    if name == "bridge.js" {
        return Some((BRIDGE_JS.as_bytes().to_vec(), "text/javascript; charset=utf-8".into()));
    }
    if !FILES.contains(&name) {
        return None;
    }
    let asset = app().handle.asset_resolver().get(name.to_string())?;
    let mut bytes = asset.bytes;
    if name == "panel.html" {
        // bridge.js 要在所有脚本前面；标题改成认得出的
        let html = String::from_utf8_lossy(&bytes)
            .replacen("<head>", "<head>\n  <script src=\"bridge.js\"></script>\n  <link rel=\"icon\" href=\"data:,\" />", 1)
            // 页面是按 460 宽的小窗口排的：浏览器里居中，别铺满整个屏幕
            .replacen("</head>", "  <style>html { background: #e9e9ee; } body { max-width: 680px; margin: 0 auto; box-shadow: 0 0 0 1px rgba(0,0,0,.06), 0 8px 30px rgba(0,0,0,.08); }</style>\n</head>", 1)
            .replacen("<title>Claude 宠物</title>", &format!("<title>{PAGE_TITLE}</title>"), 1);
        bytes = html.into_bytes();
    }
    let mime = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        _ => "text/javascript; charset=utf-8",
    };
    Some((bytes, mime.into()))
}

/// 口令对不对
pub fn authorized(v: &Value) -> bool {
    v["t"].as_str().map(|t| !t.is_empty() && t == token()).unwrap_or(false)
}

/// POST /ui/poll
pub fn poll(v: &Value) -> Value {
    let since = v["since"].as_i64().unwrap_or(-1);
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut e = EVENTS.lock().unwrap();
    if since < 0 {
        return json!({"seq": e.seq, "events": []});
    }
    let since = since as u64;
    loop {
        if e.seq > since {
            let events: Vec<Value> = e.list.iter().filter(|(s, _, _)| *s > since).map(|(_, ev, p)| json!({"event": ev, "payload": p})).collect();
            return json!({"seq": e.seq, "events": events});
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return json!({"seq": e.seq, "events": []});
        }
        e = NEW_EVENT.wait_timeout(e, left).unwrap().0;
    }
}

fn arg<T: serde::de::DeserializeOwned>(args: &Value, k: &str) -> Result<T, String> {
    serde_json::from_value(args.get(k).cloned().unwrap_or(Value::Null)).map_err(|e| format!("参数 {k} 不对：{e}"))
}

fn ok<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tauri::async_runtime::block_on(f)
}

/// POST /ui/invoke：和窗口里的 invoke 一样调命令。只放行对话框用得到的命令
pub fn invoke(v: &Value) -> Value {
    let cmd = v["cmd"].as_str().unwrap_or("");
    let a = &v["args"];
    let r: Result<Value, String> = (|| match cmd {
        "get_state" => ok(commands::get_state()),
        "save_config" => ok(commands::save_config(arg(a, "config")?)?),
        "menu_action" => ok(commands::menu_action(arg(a, "action")?)),
        "toggle_breath" => ok(commands::toggle_breath(arg(a, "on")?)),
        "test_voice" => ok(commands::test_voice(arg(a, "text")?)),
        "system_voices" => ok(commands::system_voices()),
        "stop_all" => ok(commands::stop_all()),
        "set_target" => ok(commands::set_target(arg(a, "hwnd")?, arg(a, "on")?)),
        "clear_targets" => ok(commands::clear_targets()),
        "list_windows" => ok(commands::list_windows()),
        "pick_window_delayed" => ok(block(commands::pick_window_delayed(arg(a, "ms")?))),
        "send_chat" => ok(commands::send_chat(arg(a, "text")?, arg(a, "mode")?)),
        "look" => ok(commands::look(arg(a, "question")?)),
        "open_sprites_dir" => ok(commands::open_sprites_dir()),
        "open_log_dir" => ok(commands::open_log_dir()),
        "open_url" => ok(commands::open_url(arg(a, "url")?)?),
        "claude_status" => ok(block(commands::claude_status())),
        "cli_install" => ok(block(commands::cli_install())?),
        "cli_login" => ok(block(commands::cli_login())?),
        "cli_login_code" => ok(block(commands::cli_login_code(arg(a, "code")?))?),
        "install_hooks" => ok(commands::install_hooks()?),
        "uninstall_hooks" => ok(commands::uninstall_hooks()?),
        "install_mcp" => ok(block(commands::install_mcp())?),
        "desktop_register" => ok(commands::desktop_register()?),
        "desktop_unregister" => ok(commands::desktop_unregister()?),
        "copy_text" => ok(commands::copy_text(arg(a, "text")?)?),
        "js_log" => ok(commands::js_log(arg(a, "win")?, arg(a, "level")?, arg(a, "msg")?)),
        _ => Err(format!("没有这个命令：{cmd}")),
    })();
    match r {
        Ok(value) => json!({"value": value}),
        Err(e) => json!({"error": e}),
    }
}
