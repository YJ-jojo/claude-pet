//! 本地服务（只监听 127.0.0.1）：
//! - /hook  接收 Claude Code 钩子事件（需要你确认、空闲等你、回答完成）→ 宠物提醒
//! - /mcp   MCP 工具，让 Claude Code 能叫宠物说话、看屏幕、指东西
//! - /say   让宠物说一句话
//! - /ui/   浏览器里的对话框（见 webui.rs）

use crate::platform;
use crate::snapshot;
use crate::state::app;
use crate::talk;
use crate::plog;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Response, Server};

pub fn start(port: u16) {
    let server = match Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(e) => {
            plog!("本地端口 {port} 打开失败：{e}");
            talk::system_line(&format!("本地端口 {port} 被占用，Claude Code 提醒暂时收不到。可以在设置里换个端口。"));
            return;
        }
    };
    plog!("本地服务已启动：http://127.0.0.1:{port}");
    std::thread::Builder::new()
        .name("server".into())
        .spawn(move || {
            for req in server.incoming_requests() {
                std::thread::spawn(move || handle(req, port));
            }
        })
        .ok();
}

fn json_resp(v: &Value, code: u16) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(v.to_string())
        .with_status_code(code)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn handle(mut req: tiny_http::Request, port: u16) {
    // 防止网页通过 DNS 重绑定来调本地服务
    let host_ok = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| {
            let v = h.value.as_str();
            v == format!("127.0.0.1:{port}") || v == format!("localhost:{port}")
        })
        .unwrap_or(false);
    if !host_ok {
        let _ = req.respond(Response::from_string("forbidden").with_status_code(403));
        return;
    }
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = req.method().clone();
    if method == Method::Get && path == "/health" {
        let _ = req.respond(json_resp(&json!({"ok": true, "app": "claude-pet"}), 200));
        return;
    }
    if method == Method::Get && path.starts_with("/ui/") {
        match crate::webui::file(&path[4..]) {
            Some((bytes, mime)) => {
                let _ = req.respond(
                    Response::from_data(bytes)
                        .with_header(Header::from_bytes(&b"Content-Type"[..], mime.as_bytes()).unwrap())
                        .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap()),
                );
            }
            None => {
                let _ = req.respond(Response::from_string("not found").with_status_code(404));
            }
        }
        return;
    }
    if method != Method::Post {
        let _ = req.respond(Response::from_string("method not allowed").with_status_code(405));
        return;
    }
    // 只接受 JSON，浏览器网页没法不经预检就发这种请求
    let is_json = req
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Type") && h.value.as_str().to_lowercase().starts_with("application/json"));
    if !is_json {
        let _ = req.respond(Response::from_string("json only").with_status_code(415));
        return;
    }
    let mut body = String::new();
    let _ = req.as_reader().take(4 * 1024 * 1024).read_to_string(&mut body);
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if path.starts_with("/ui/") {
        // 浏览器里的对话框：只认本机这个端口的页面，还要带口令
        let origin_ok = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Origin"))
            .map(|h| {
                let o = h.value.as_str();
                o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
            })
            .unwrap_or(false);
        if !origin_ok || !crate::webui::authorized(&v) {
            let _ = req.respond(json_resp(&json!({"error": "forbidden"}), 403));
            return;
        }
        let r = match path.as_str() {
            "/ui/invoke" => crate::webui::invoke(&v),
            "/ui/poll" => crate::webui::poll(&v),
            _ => json!({"error": "not found"}),
        };
        let _ = req.respond(json_resp(&r, 200));
        return;
    }
    match path.as_str() {
        "/hook" => {
            on_hook(&v);
            let _ = req.respond(json_resp(&json!({}), 200));
        }
        "/say" => {
            if let Some(t) = v["text"].as_str() {
                talk::say(t, "talk");
            }
            let _ = req.respond(json_resp(&json!({"ok": true}), 200));
        }
        "/mcp" => match mcp(&v) {
            Some(r) => {
                let _ = req.respond(json_resp(&r, 200));
            }
            None => {
                let _ = req.respond(Response::from_string("").with_status_code(202));
            }
        },
        _ => {
            let _ = req.respond(Response::from_string("not found").with_status_code(404));
        }
    }
}

// ───────────────────────── Claude Code 钩子 ─────────────────────────

fn project_of(v: &Value) -> String {
    let cwd = v["cwd"].as_str().unwrap_or("");
    cwd.rsplit(['\\', '/']).find(|s| !s.is_empty()).unwrap_or("").to_string()
}

/// 同一件事短时间内只提醒一次
fn first_time(key: String, within: Duration) -> bool {
    let a = app();
    let mut seen = a.hook_seen.lock().unwrap();
    let now = Instant::now();
    seen.retain(|_, t| now.duration_since(*t) < Duration::from_secs(600));
    match seen.get(&key) {
        Some(t) if now.duration_since(*t) < within => false,
        _ => {
            seen.insert(key, now);
            true
        }
    }
}

fn tool_cn(tool: &str) -> String {
    match tool {
        "Bash" | "PowerShell" => "运行命令".into(),
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "改文件".into(),
        "Read" => "读文件".into(),
        "WebFetch" => "打开网页".into(),
        "WebSearch" => "上网搜索".into(),
        t if t.starts_with("mcp__") => format!("用工具 {}", t.rsplit("__").next().unwrap_or(t)),
        t => format!("用 {t}"),
    }
}

/// 把 Claude Code 的英文提示翻成口语
fn notification_cn(msg: &str) -> String {
    let m = msg.trim();
    if let Some(rest) = m.strip_prefix("Claude needs your permission to use ") {
        return format!("要你批准它{}", tool_cn(rest.trim().trim_end_matches('.')));
    }
    if m.starts_with("Claude is waiting for your input") {
        return "在等你回话".into();
    }
    if m.to_lowercase().contains("permission") {
        return "要你确认一下".into();
    }
    m.to_string()
}

fn alert(text: &str, detail: Option<&str>) {
    let a = app();
    a.tts.chime();
    let id = a.next_id("b");
    let shown = match detail {
        Some(d) if !d.is_empty() => format!("{text}\n{d}"),
        _ => text.to_string(),
    };
    a.emit_stage("bubble", json!({"id": id, "mode": "set", "text": shown, "kind": "alert"}));
    a.emit_stage("bubble", json!({"id": id, "mode": "done"}));
    a.emit_stage("pet-action", json!({"action": "alert"}));
    a.emit_panel("chat", json!({"id": id, "role": "pet", "mode": "new", "text": shown, "kind": "alert"}));
    a.tts.say(text);
}

fn on_hook(v: &Value) {
    let a = app();
    let cfg = a.cfg();
    let event = v["hook_event_name"].as_str().unwrap_or("");
    let sid = v["session_id"].as_str().unwrap_or("").to_string();
    let proj = project_of(v);
    let who = if proj.is_empty() { "Claude".to_string() } else { format!("{proj} 那边的 Claude") };
    plog!("钩子：{event} {}", v["notification_type"].as_str().unwrap_or(""));
    match event {
        "PermissionRequest" => {
            if !cfg.alert_permission_immediately {
                return;
            }
            let tool = v["tool_name"].as_str().unwrap_or("");
            let detail = v["tool_input"]["command"]
                .as_str()
                .or_else(|| v["tool_input"]["file_path"].as_str())
                .or_else(|| v["tool_input"]["url"].as_str())
                .map(|s| s.chars().take(120).collect::<String>());
            if first_time(format!("perm:{sid}"), Duration::from_secs(4)) {
                alert(&format!("{who}要你批准它{}", tool_cn(tool)), detail.as_deref());
            }
        }
        "Notification" => {
            let kind = v["notification_type"].as_str().unwrap_or("");
            let msg = v["message"].as_str().unwrap_or("");
            match kind {
                "permission_prompt" => {
                    // 立即提醒已经说过的，这里就不重复了
                    if !cfg.alert_permission_immediately || first_time(format!("perm:{sid}"), Duration::from_secs(4)) {
                        alert(&format!("{who}{}", notification_cn(msg)), None);
                    } else if first_time(format!("perm-again:{sid}"), Duration::from_secs(60)) {
                        // 过了一会儿你还没处理，再轻轻提醒一次
                        alert(&format!("{who}还在等你确认哦"), None);
                    }
                }
                "idle_prompt" => {
                    if cfg.alert_idle && first_time(format!("idle:{sid}"), Duration::from_secs(120)) {
                        alert(&format!("{who}做完了，在等你回话"), None);
                    }
                }
                "elicitation_dialog" | "agent_needs_input" => {
                    if first_time(format!("ask:{sid}"), Duration::from_secs(10)) {
                        alert(&format!("{who}有问题要问你"), Some(msg));
                    }
                }
                "agent_completed" => {
                    if first_time(format!("done:{sid}:{msg}"), Duration::from_secs(10)) {
                        alert(&format!("{who}的后台任务做完了"), Some(msg));
                    }
                }
                "" => {
                    if !msg.is_empty() && first_time(format!("n:{sid}:{msg}"), Duration::from_secs(10)) {
                        alert(&format!("{who}{}", notification_cn(msg)), None);
                    }
                }
                _ => {}
            }
        }
        "Stop" => {
            // 你那边的 Claude 答完一轮：宠物开心一下（不出声）
            a.emit_stage("pet-action", json!({"action": "happy"}));
            if cfg.announce_stop && first_time(format!("stop:{sid}"), Duration::from_secs(5)) {
                let last = v["last_assistant_message"].as_str().unwrap_or("");
                let first: String = last.split(['。', '\n', '.']).next().unwrap_or("").chars().take(40).collect();
                alert(&format!("{who}回答完了"), if first.is_empty() { None } else { Some(&first) });
            }
        }
        _ => {}
    }
}

// ───────────────────────── MCP（给 Claude Code 用的工具） ─────────────────────────

fn tools() -> Value {
    json!([
        {
            "name": "pet_say",
            "description": "让桌面宠物把一句话说出来（语音 + 气泡），用来提醒用户。",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string", "description": "要说的话，口语、简短"}}, "required": ["text"]}
        },
        {
            "name": "pet_windows",
            "description": "列出用户电脑上打开的窗口：[窗口编号] 标题（程序名）。窗口编号可以填进其它工具的 window。",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "pet_look",
            "description": "通过桌面宠物的“眼睛”看用户电脑上的一个窗口，返回窗口里的文字和可操作元素（#编号 类型「名字」@(屏幕坐标)）。不填 window 时看用户最近在用的窗口（会跳过 Claude 桌面版自己）。要点击、输入之前先用它看一眼，拿到元素编号。",
            "inputSchema": {"type": "object", "properties": {
                "window": {"type": ["string", "integer"], "description": "窗口编号（来自 pet_windows），或标题/程序名里的一段文字；不填 = 用户最近在用的窗口"},
                "include_image": {"type": "boolean", "description": "是否附上截图，默认 false（文字读不到时会自动截图识别）"}
            }}
        },
        {
            "name": "pet_point",
            "description": "让桌面宠物跑到一个按钮/元素旁边指给用户看（不点击，等用户自己点，最多 20 秒）。",
            "inputSchema": {"type": "object", "properties": {
                "element": {"type": "integer", "description": "pet_look 返回的元素编号"},
                "name": {"type": "string", "description": "没有编号时：元素名字里包含的文字，比如“设置”"},
                "window": {"type": ["string", "integer"], "description": "同 pet_look；不填 = 上次 pet_look 看的窗口"},
                "say": {"type": "string", "description": "指的时候说的话"}
            }}
        },
        {
            "name": "pet_act",
            "description": "让桌面宠物跑过去替用户操作：点击、双击、右键、在输入框里打字、滚动、按快捷键。目标用 name（按钮/输入框上的字，最快，不用先 pet_look）或 element（最近一次 pet_look 的编号）。名字找不到或有好几个时会返回候选编号。删除、发送、付款这类不能撤销的操作，先在聊天里问过用户再做。",
            "inputSchema": {"type": "object", "properties": {
                "window": {"type": ["string", "integer"], "description": "同 pet_look；不填 = 3 分钟内 pet_look 看过的窗口，否则用户最近在用的窗口"},
                "steps": {"type": "array", "description": "按顺序做的步骤", "items": {"type": "object", "properties": {
                    "op": {"type": "string", "enum": ["click", "double", "right", "type", "scroll", "key", "wait"]},
                    "name": {"type": "string", "description": "目标元素上的字，比如“保存”“搜索”（和 element 二选一）"},
                    "element": {"type": "integer", "description": "pet_look 返回的元素编号（和 name 二选一）"},
                    "text": {"type": "string", "description": "type：要输入的文字"},
                    "enter": {"type": "boolean", "description": "type：输完按回车"},
                    "keys": {"type": "string", "description": "key：快捷键，比如 ctrl+s"},
                    "amount": {"type": "integer", "description": "scroll：负数往下，正数往上，默认 -3"},
                    "ms": {"type": "integer", "description": "wait：等多少毫秒"}
                }, "required": ["op"]}},
                "say": {"type": "string", "description": "开始前宠物说的话（可选）"}
            }, "required": ["steps"]}
        }
    ])
}

fn mcp(req: &Value) -> Option<Value> {
    if let Some(arr) = req.as_array() {
        let out: Vec<Value> = arr.iter().filter_map(mcp_one).collect();
        return if out.is_empty() { None } else { Some(Value::Array(out)) };
    }
    mcp_one(req)
}

fn mcp_one(req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req["method"].as_str().unwrap_or("");
    let id = id?; // 没有 id 的是通知，不用回
    let result = match method {
        "initialize" => {
            let ver = req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18");
            json!({
                "protocolVersion": ver,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "claude-pet", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "这是用户的桌面宠物，在用户的 Windows 电脑上。需要提醒用户时用 pet_say；想知道用户屏幕上是什么时用 pet_look；要指给用户看用 pet_point；要替用户点击、输入用 pet_act。知道按钮上的字就直接用 name，一次调用就够，不用先 pet_look；找不到或有歧义时工具会返回候选编号。"
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call_tool(&req["params"]["name"], &req["params"]["arguments"]),
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn text_result(s: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": s}], "isError": is_error})
}

/// 每个窗口最近一次 pet_look 看到的元素（pet_point / pet_act 的编号指的就是它）
static LAST_UI: Mutex<Option<(i64, platform::UiSnapshot)>> = Mutex::new(None);
/// 上次 pet_look 的时间：太久以前看的窗口，不填 window 时就不再默认用它
static LAST_LOOK_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const LAST_LOOK_FRESH: Duration = Duration::from_secs(180);

fn remember_ui(hwnd: i64, ui: platform::UiSnapshot) {
    *LAST_UI.lock().unwrap() = Some((hwnd, ui));
    *LAST_LOOK_AT.lock().unwrap() = Some(std::time::Instant::now());
}

fn last_look_fresh() -> bool {
    LAST_LOOK_AT.lock().unwrap().map(|t| t.elapsed() < LAST_LOOK_FRESH).unwrap_or(false)
}

/// 按参数找窗口：编号 / 标题或程序名里的字 / 不填 = 用户最近在用的（跳过 Claude 桌面版）
fn pick_window(arg: &Value, prefer_last_look: bool) -> Result<platform::WinInfo, String> {
    let a = app();
    let w = if let Some(n) = arg.as_i64().or_else(|| arg.as_str().and_then(|s| s.trim().trim_matches(['[', ']']).parse().ok())) {
        platform::window_info(n).ok_or("没有这个窗口编号了，先用 pet_windows 看一下")?
    } else if let Some(q) = arg.as_str().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) {
        platform::list_windows()
            .into_iter()
            .find(|w| w.title.to_lowercase().contains(&q) || w.process.to_lowercase().contains(&q))
            .ok_or_else(|| format!("没找到标题或程序名里有“{q}”的窗口"))?
    } else if let Some(w) = (prefer_last_look && last_look_fresh()).then(|| LAST_UI.lock().unwrap().as_ref().map(|(h, _)| *h)).flatten().and_then(platform::window_info) {
        w
    } else {
        a.user_window().ok_or("不知道用户在看哪个窗口。让用户先点一下那个窗口，或者用 pet_windows 选一个")?
    };
    if a.cfg().is_blacklisted(&w.process, &w.title) {
        return Err("这个窗口在用户的不看名单里，宠物不看也不碰它".into());
    }
    Ok(w)
}

fn window_line(w: &platform::WinInfo) -> String {
    format!("[{}] {}（{}）", w.hwnd, w.title, w.process)
}

fn call_tool(name: &Value, args: &Value) -> Value {
    let a = app();
    match name.as_str().unwrap_or("") {
        "pet_say" => {
            let t = args["text"].as_str().unwrap_or("");
            if t.is_empty() {
                return text_result("text 不能为空", true);
            }
            talk::say(t, "talk");
            text_result("宠物已经说了", false)
        }
        "pet_windows" => {
            let cfg = a.cfg();
            let list: Vec<String> = platform::list_windows().iter().filter(|w| !cfg.is_blacklisted(&w.process, &w.title)).map(window_line).collect();
            text_result(&list.join("\n"), false)
        }
        "pet_look" => {
            let win = match pick_window(&args["window"], false) {
                Ok(w) => w,
                Err(e) => return text_result(&e, true),
            };
            let want = args["include_image"].as_bool().unwrap_or(false);
            a.set_looking(true);
            let look = talk::look_at(&win, want);
            a.set_looking(false);
            let elements = snapshot::element_list(&look.ui, 200);
            remember_ui(win.hwnd, look.ui.clone());
            let text = format!(
                "窗口 {}\n画面文字：{}\n可操作元素（#编号 类型「名字」@(屏幕x,y,宽x高)）：\n{}",
                window_line(&look.win),
                snapshot::full(&look.win, &look.seen, 8000)["texts"],
                if elements.is_empty() { "（读不到元素）".into() } else { elements }
            );
            let mut content = vec![json!({"type": "text", "text": text})];
            if want {
                if let Some((img, _, _)) = &look.image {
                    use base64::Engine as _;
                    let jpg = img.to_jpeg(1280, 80);
                    content.push(json!({"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&jpg), "mimeType": "image/jpeg"}));
                }
            }
            json!({"content": content})
        }
        "pet_point" => {
            let win = match pick_window(&args["window"], true) {
                Ok(w) => w,
                Err(e) => return text_result(&e, true),
            };
            let found = match element_rect(&win, args) {
                Ok(f) => f,
                Err(e) => return text_result(&e, true),
            };
            let (r, label) = found;
            let say = args["say"].as_str().unwrap_or("在这里！").to_string();
            tauri::async_runtime::spawn(async move {
                crate::stage::approach(r, "point").await;
                crate::stage::point_start(r);
                talk::say(&say, "op");
                let result = crate::operate::wait_user_click(r, Duration::from_secs(20)).await;
                crate::stage::point_end(result);
            });
            text_result(&format!("宠物跑过去指着了：{label}。它会等用户自己点（最多 20 秒）。"), false)
        }
        "pet_act" => {
            let win = match pick_window(&args["window"], true) {
                Ok(w) => w,
                Err(e) => return text_result(&e, true),
            };
            let last = LAST_UI.lock().unwrap().clone().filter(|(h, _)| *h == win.hwnd).map(|(_, ui)| ui);
            // 按名字点的：现读一次这个窗口的界面（比让 Claude 再来回一次 pet_look 快得多）
            let mut fresh: Option<platform::UiSnapshot> = None;
            let mut steps = Vec::new();
            for st in args["steps"].as_array().cloned().unwrap_or_default() {
                let mut st = st;
                if let Some(id) = st["element"].as_u64() {
                    // 用 pet_look 那一刻的位置（之后别的功能再读这个窗口，编号会变）
                    let Some(ui) = &last else {
                        return text_result("元素编号要先 pet_look 才有；也可以不用编号，直接用 name 写按钮上的字", true);
                    };
                    match ui.elements.get(id as usize) {
                        Some(e) => st["rect"] = json!([e.x, e.y, e.w, e.h]),
                        None => return text_result(&format!("没有 #{id} 这个元素，重新 pet_look 一下"), true),
                    }
                } else if let Some(q) = st["name"].as_str().map(str::trim).filter(|q| !q.is_empty()) {
                    let ui = fresh.get_or_insert_with(|| platform::ui_snapshot(win.hwnd, 400));
                    match find_named(&ui.elements, q) {
                        Found::One(e) => st["rect"] = json!([e.x, e.y, e.w, e.h]),
                        other => {
                            // 把这次读到的界面记下来，Claude 可以直接用下面列出的编号，不用再 pet_look
                            let msg = not_found_msg(&ui.elements, q, other);
                            remember_ui(win.hwnd, ui.clone());
                            return text_result(&msg, true);
                        }
                    }
                }
                steps.push(st);
            }
            if steps.is_empty() {
                return text_result("steps 是空的", true);
            }
            let ui = fresh.or(last).unwrap_or_default();
            if let Some(s) = args["say"].as_str().filter(|s| !s.is_empty()) {
                talk::say(s, "op");
            }
            let r = tauri::async_runtime::block_on(crate::operate::run_steps(&win, &ui, steps));
            match r {
                Ok(()) => text_result("做完了。要确认结果的话再 pet_look 看一眼。", false),
                Err(e) => text_result(&e, true),
            }
        }
        _ => text_result("没有这个工具", true),
    }
}

/// pet_point 要指的元素：编号优先，其次按名字在窗口里找
fn element_rect(win: &platform::WinInfo, args: &Value) -> Result<(platform::RectI, String), String> {
    if let Some(id) = args["element"].as_u64() {
        let last = LAST_UI.lock().unwrap().clone();
        let Some((h, ui)) = last.filter(|(h, _)| *h == win.hwnd) else {
            return Err("先用 pet_look 看一下这个窗口，拿到元素编号".into());
        };
        let _ = h;
        let e = ui.elements.get(id as usize).ok_or(format!("没有 #{id} 这个元素，重新 pet_look 一下"))?;
        return Ok((platform::RectI { x: e.x, y: e.y, w: e.w, h: e.h }, format!("{}「{}」", e.kind, e.name)));
    }
    let q = args["name"].as_str().unwrap_or("").trim();
    if q.is_empty() {
        return Err("要给 element（编号）或 name（名字）".into());
    }
    let ui = platform::ui_snapshot(win.hwnd, 400);
    match find_named(&ui.elements, q) {
        Found::One(e) => Ok((platform::RectI { x: e.x, y: e.y, w: e.w, h: e.h }, format!("{}「{}」", e.kind, e.name))),
        other => {
            let msg = not_found_msg(&ui.elements, q, other);
            remember_ui(win.hwnd, ui);
            Err(msg)
        }
    }
}

enum Found<'a> {
    One(&'a platform::Element),
    /// 好几个一样像的，不敢乱点
    Many(Vec<&'a platform::Element>),
    None,
}

/// 能点的类型：同名时优先它们
fn clickable(kind: &str) -> bool {
    matches!(kind, "按钮" | "链接" | "菜单项" | "标签页" | "勾选框" | "单选项" | "输入框" | "下拉框")
}

/// 按名字找元素：完全一样 > 开头一样 > 包含（不分大小写、忽略空格）。
/// 同一档里只有一个能点的就选它；还有好几个就报歧义，让 Claude 用编号选。
fn find_named<'a>(elems: &'a [platform::Element], q: &str) -> Found<'a> {
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let q = norm(q);
    if q.is_empty() {
        return Found::None;
    }
    let score = |e: &platform::Element| {
        let n = norm(&e.name);
        if n.is_empty() {
            0
        } else if n == q {
            3
        } else if n.starts_with(&q) {
            2
        } else if n.contains(&q) {
            1
        } else {
            0
        }
    };
    let best = elems.iter().map(score).max().unwrap_or(0);
    if best == 0 {
        return Found::None;
    }
    let mut top: Vec<&platform::Element> = elems.iter().filter(|e| score(e) == best && e.w > 0 && e.h > 0).collect();
    if top.iter().any(|e| e.enabled) {
        top.retain(|e| e.enabled);
    }
    if top.len() > 1 && top.iter().any(|e| clickable(&e.kind)) {
        top.retain(|e| clickable(&e.kind));
    }
    // 位置一样的（同一个按钮被读到两次）算一个
    top.dedup_by(|a, b| a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h);
    match top.len() {
        0 => Found::None,
        1 => Found::One(top[0]),
        _ => Found::Many(top),
    }
}

fn not_found_msg(elems: &[platform::Element], q: &str, found: Found) -> String {
    let line = |e: &platform::Element| format!("#{} {}「{}」@({},{})", e.id, e.kind, e.name, e.x, e.y);
    match found {
        Found::Many(v) => format!(
            "有 {} 个都像「{q}」，没敢乱点。用 element 编号选一个：\n{}",
            v.len(),
            v.iter().take(10).map(|e| line(e)).collect::<Vec<_>>().join("\n")
        ),
        _ => {
            let names: Vec<String> = elems.iter().filter(|e| clickable(&e.kind) && !e.name.trim().is_empty()).take(30).map(line).collect();
            if names.is_empty() {
                format!("这个窗口里没找到「{q}」，也读不到可点的元素。试试 pet_look 带上 include_image 看截图")
            } else {
                format!("这个窗口里没找到「{q}」。能点的有（可以直接用 element 编号）：\n{}", names.join("\n"))
            }
        }
    }
}

// ───────────────────────── 安装 / 卸载钩子 ─────────────────────────

fn settings_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(std::path::PathBuf::from(home).join(".claude").join("settings.json"))
}

const HOOK_EVENTS: &[&str] = &["Notification", "PermissionRequest", "Stop"];

fn is_ours(h: &Value, port: u16) -> bool {
    h["type"] == "http" && h["url"].as_str().map(|u| u.contains(&format!("127.0.0.1:{port}/hook"))).unwrap_or(false)
}

/// 往 ~/.claude/settings.json 里加钩子（会先备份原文件）
pub fn install_hooks(port: u16) -> Result<String, String> {
    let path = settings_path().ok_or("找不到用户目录")?;
    let mut root: Value = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).map_err(|e| format!("settings.json 格式有问题，我不敢改：{e}"))?,
        _ => json!({}),
    };
    if path.exists() {
        let bak = path.with_file_name(format!("settings.json.bak-claudepet-{}", chrono::Local::now().format("%Y%m%d%H%M%S")));
        std::fs::copy(&path, &bak).map_err(|e| format!("备份失败：{e}"))?;
    }
    if !root.is_object() {
        return Err("settings.json 不是对象格式".into());
    }
    if !root["hooks"].is_object() {
        root["hooks"] = json!({});
    }
    let url = format!("http://127.0.0.1:{port}/hook");
    let mut added = 0;
    for ev in HOOK_EVENTS {
        let list = &mut root["hooks"][*ev];
        if !list.is_array() {
            *list = json!([]);
        }
        let already = list.as_array().unwrap().iter().any(|g| g["hooks"].as_array().map(|hs| hs.iter().any(|h| is_ours(h, port))).unwrap_or(false));
        if !already {
            list.as_array_mut().unwrap().push(json!({"hooks": [{"type": "http", "url": url, "timeout": 5}]}));
            added += 1;
        }
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).map_err(|e| format!("写入失败：{e}"))?;
    Ok(if added == 0 { "钩子本来就装好了".into() } else { format!("装好了 {added} 个钩子。已经开着的 Claude Code 窗口要重开一下才生效。") })
}

pub fn uninstall_hooks(port: u16) -> Result<String, String> {
    let path = settings_path().ok_or("找不到用户目录")?;
    let s = std::fs::read_to_string(&path).map_err(|_| "没有找到 settings.json".to_string())?;
    let mut root: Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    let mut removed = 0;
    if let Some(hooks) = root["hooks"].as_object_mut() {
        for (_ev, list) in hooks.iter_mut() {
            if let Some(groups) = list.as_array_mut() {
                for g in groups.iter_mut() {
                    if let Some(hs) = g["hooks"].as_array_mut() {
                        let before = hs.len();
                        hs.retain(|h| !is_ours(h, port));
                        removed += before - hs.len();
                    }
                }
                groups.retain(|g| g["hooks"].as_array().map(|h| !h.is_empty()).unwrap_or(true));
            }
        }
        hooks.retain(|_, l| l.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).map_err(|e| e.to_string())?;
    Ok(format!("移除了 {removed} 个宠物钩子"))
}

pub fn hooks_installed(port: u16) -> bool {
    let Some(path) = settings_path() else { return false };
    let Ok(s) = std::fs::read_to_string(&path) else { return false };
    let Ok(root) = serde_json::from_str::<Value>(&s) else { return false };
    root["hooks"]["Notification"]
        .as_array()
        .map(|gs| gs.iter().any(|g| g["hooks"].as_array().map(|hs| hs.iter().any(|h| is_ours(h, port))).unwrap_or(false)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translates_notifications() {
        assert_eq!(notification_cn("Claude needs your permission to use Bash"), "要你批准它运行命令");
        assert_eq!(notification_cn("Claude is waiting for your input"), "在等你回话");
    }

    fn el(id: usize, kind: &str, name: &str, x: i32) -> platform::Element {
        platform::Element { id, kind: kind.into(), name: name.into(), x, y: 10, w: 40, h: 20, enabled: true, ..Default::default() }
    }

    #[test]
    fn finds_elements_by_name() {
        let v = vec![el(0, "文本", "保存设置后重启", 0), el(1, "按钮", "保存", 100), el(2, "按钮", "另存为", 200), el(3, "菜单项", "Save As", 300)];
        // 完全一样的优先
        assert!(matches!(find_named(&v, "保存"), Found::One(e) if e.id == 1));
        // 不分大小写、忽略空格
        assert!(matches!(find_named(&v, "saveas"), Found::One(e) if e.id == 3));
        assert!(matches!(find_named(&v, "删除"), Found::None));
        // 两个一样像的能点元素：不乱点
        let two = vec![el(0, "按钮", "发送", 0), el(1, "按钮", "发送", 500)];
        assert!(matches!(find_named(&two, "发送"), Found::Many(m) if m.len() == 2));
        // 同名但一个是文字一个是按钮：选按钮
        let mixed = vec![el(0, "文本", "发送", 0), el(1, "按钮", "发送", 500)];
        assert!(matches!(find_named(&mixed, "发送"), Found::One(e) if e.id == 1));
    }
}
