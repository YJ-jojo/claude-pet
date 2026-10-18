//! 给网页界面调用的命令

use crate::breath;
use crate::config::{Config, SavedTarget};
use crate::platform::{self, WinInfo};
use crate::server;
use crate::state::{app, RectF, Target};
use crate::talk;
use crate::tts::VoiceSettings;
use crate::plog;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use tauri::Manager;

fn voice_from(cfg: &Config) -> VoiceSettings {
    VoiceSettings {
        engine: cfg.tts_engine.clone(),
        edge_voice: cfg.edge_voice.clone(),
        system_voice: cfg.system_voice.clone(),
        rate: cfg.tts_rate,
        enabled: cfg.tts_enabled,
    }
}

pub fn apply_config(cfg: &Config) {
    let a = app();
    a.tts.set_voice(voice_from(cfg));
    crate::claude::set_override(&cfg.claude_path);
    crate::stage::apply_capture_flag();
    a.emit_all("config", serde_json::to_value(cfg).unwrap_or_default());
    a.push_flags();
}

pub fn new_voice(cfg: &Config) -> VoiceSettings {
    voice_from(cfg)
}

#[tauri::command]
pub fn get_state() -> Value {
    let a = app();
    let cfg = a.cfg();
    let g = a.stage_geom();
    let wa = platform::work_area_at(g.x + g.w / 2, g.y + g.h / 2);
    let (wx, wy) = g.to_css(wa.x, wa.y);
    let pet = *a.pet_pos.lock().unwrap();
    let (px, py) = g.to_css(pet.0, pet.1);
    json!({
        "config": cfg,
        "version": env!("CARGO_PKG_VERSION"),
        "edgeVoices": crate::tts::edge_voices().iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "targets": *a.targets.lock().unwrap(),
        "breathing": a.breathing.load(Ordering::SeqCst),
        "stage": {"scale": g.scale, "work": {"x": wx, "y": wy, "w": wa.w as f64 / g.scale, "h": wa.h as f64 / g.scale}, "mon": {"w": g.w as f64 / g.scale, "h": g.h as f64 / g.scale}},
        "pet": {"x": px, "y": py},
        "packs": list_packs(),
        "packBase": pack_base(),
    })
}

fn pack_dirs() -> Vec<std::path::PathBuf> {
    let a = app();
    let mut v = vec![a.data_dir.join("sprites")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("sprites"));
        }
    }
    v
}

pub fn find_pack_file(rel: &str) -> Option<std::path::PathBuf> {
    let rel = rel.trim_start_matches('/');
    if rel.contains("..") {
        return None;
    }
    let decoded = percent_decode(rel);
    // 一个形象包的文件只从同一个文件夹拿，不在两处之间东拿一个西拿一个
    if let Some((name, rest)) = decoded.split_once('/') {
        return pack_root(name).map(|d| d.join(rest)).filter(|p| p.is_file());
    }
    pack_dirs().into_iter().map(|d| d.join(&decoded)).find(|p| p.is_file())
}

/// 形象包所在的文件夹。%APPDATA% 和程序旁边都有同名形象包时，用 pack.json 较新的那份
/// （安装包里带的新版形象包不会被以前手动放进 %APPDATA% 的旧版挡住）
pub fn pack_root(name: &str) -> Option<std::path::PathBuf> {
    pack_dirs()
        .into_iter()
        .map(|d| d.join(name))
        .filter_map(|d| {
            let t = std::fs::metadata(d.join("pack.json")).and_then(|m| m.modified()).ok()?;
            Some((t, d))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, d)| d)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn list_packs() -> Vec<String> {
    let mut v = vec!["placeholder".to_string()];
    for d in pack_dirs() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.path().join("pack.json").is_file() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if !v.contains(&n) {
                        v.push(n);
                    }
                }
            }
        }
    }
    v
}

/// 启动时记下找到了哪些形象包、选中的那个里面有什么文件
pub fn log_packs() {
    let a = app();
    let want = a.cfg().sprite_pack;
    let dirs: Vec<String> = pack_dirs().iter().map(|d| d.display().to_string()).collect();
    plog!("形象包目录 {:?}，找到 {:?}", dirs, list_packs());
    if want == "placeholder" {
        return;
    }
    match pack_root(&want) {
        Some(d) => {
            let files: Vec<String> = std::fs::read_dir(&d)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| format!("{} {}KB", e.file_name().to_string_lossy(), e.metadata().map(|m| m.len() / 1024).unwrap_or(0)))
                        .collect()
                })
                .unwrap_or_default();
            plog!("选中的形象包 {want} 在 {}：{}", d.display(), files.join("，"));
        }
        None => plog!("!!! 选中的形象包 {want} 找不到，会改用占位形象"),
    }
}

/// 各页面报“加载好了”的时间（启动后毫秒）
static PAGES_READY: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn pages_ready() -> Vec<String> {
    PAGES_READY.lock().unwrap().clone()
}

/// 页面里的报错、警告、性能统计写进日志
#[tauri::command]
pub fn js_log(win: String, level: String, msg: String) {
    let msg: String = msg.chars().take(600).collect();
    if level == "ready" {
        PAGES_READY.lock().unwrap().push(win.clone());
        crate::diag::mark(&format!("{win} 页面加载好了 {msg}"));
        if win == "stage" {
            crate::diag::startup_done();
        }
        // 对话框页面后加载好：要打开的标签页再发一次（页面里的监听可能比 init 脚本晚）
        if win == "panel" {
            if let Some(tab) = PANEL_TAB.lock().unwrap().clone() {
                app().emit_panel("open-tab", json!({"tab": tab}));
            }
        }
        return;
    }
    let tag = match level.as_str() {
        "error" => "!!! 页面出错",
        "warn" => "页面警告",
        "perf" => "画面",
        _ => "页面",
    };
    plog!("{tag}［{win}］{msg}");
}

/// 启动 10 秒后检查宠物出来没有
pub fn check_started() {
    let a = app();
    let shown = a.handle.get_webview_window("stage").map(|w| w.is_visible().unwrap_or(false)).unwrap_or(false);
    let ready = PAGES_READY.lock().unwrap().clone();
    if shown {
        plog!("启动检查：宠物已显示，加载好的页面 {:?}", ready);
    } else {
        plog!("!!! 启动 10 秒了宠物还没显示，强制显示。加载好的页面 {:?}", ready);
        if let Some(w) = a.handle.get_webview_window("stage") {
            let _ = w.show();
        }
    }
}

fn pack_base() -> &'static str {
    if cfg!(windows) {
        "http://petres.localhost/"
    } else {
        "petres://localhost/"
    }
}

#[tauri::command]
pub fn save_config(config: Value) -> Result<(), String> {
    let a = app();
    let mut merged = serde_json::to_value(a.cfg()).map_err(|e| e.to_string())?;
    if let (Some(m), Some(c)) = (merged.as_object_mut(), config.as_object()) {
        for (k, v) in c {
            m.insert(k.clone(), v.clone());
        }
    }
    let cfg: Config = serde_json::from_value(merged).map_err(|e| e.to_string())?;
    *a.cfg.write().unwrap() = cfg.clone();
    a.save_cfg();
    apply_config(&cfg);
    crate::register_hotkeys(&a.handle);
    Ok(())
}

#[tauri::command]
pub fn set_hit_rects(rects: Vec<RectF>) {
    app().hit.lock().unwrap().rects = rects;
}

/// 宠物窗口摆到舞台坐标上；show=false 藏起来
#[tauri::command]
pub fn win_place(label: String, x: f64, y: f64, w: f64, h: f64, show: Option<bool>) {
    if label == "stage" {
        crate::stage::place(&label, x, y, w, h, show);
    }
}

#[tauri::command]
pub fn set_capture_all(on: bool) {
    app().hit.lock().unwrap().capture_all = on;
}

#[tauri::command]
pub fn report_pet(x: f64, y: f64) {
    let a = app();
    let g = a.stage_geom();
    *a.pet_pos.lock().unwrap() = g.to_screen(x, y);
}

#[tauri::command]
pub fn walk_done(id: String) {
    if let Some(tx) = app().walks.lock().unwrap().remove(&id) {
        let _ = tx.send(());
    }
}

#[tauri::command]
pub fn confirm_reply(id: String, ok: bool) {
    if let Some(tx) = app().confirms.lock().unwrap().remove(&id) {
        let _ = tx.send(ok);
    }
}

#[tauri::command]
pub fn pet_clicked(part: String) {
    match part.as_str() {
        "eye" => {
            tauri::async_runtime::spawn(talk::look_once(None));
        }
        "double" => open_panel_tab("chat"),
        _ => {}
    }
}

/// 拖完宠物：只是挪位置，不再问要不要盯着那个窗口（想盯哪个在右键菜单里点）
#[tauri::command]
pub fn drag_end() {
    app().hit.lock().unwrap().capture_all = false;
}

pub fn add_target(win: &WinInfo) {
    let a = app();
    {
        let mut ts = a.targets.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.hwnd == win.hwnd) {
            t.enabled = true;
        } else {
            ts.push(Target { hwnd: win.hwnd, title: win.title.clone(), process: win.process.clone(), enabled: true });
        }
    }
    persist_targets();
    a.push_flags();
    a.emit_all("targets", json!(*a.targets.lock().unwrap()));
}

fn persist_targets() {
    let a = app();
    let saved: Vec<SavedTarget> = a
        .targets
        .lock()
        .unwrap()
        .iter()
        .filter(|t| t.enabled)
        .map(|t| SavedTarget { process: t.process.clone(), title: t.title.clone() })
        .collect();
    a.cfg.write().unwrap().targets = saved;
    a.save_cfg();
}

/// 启动时按进程名 + 标题找回上次观察的窗口
pub fn restore_targets() {
    let a = app();
    let saved = a.cfg().targets;
    if saved.is_empty() {
        return;
    }
    let wins = platform::list_windows();
    let mut ts = Vec::new();
    for s in saved {
        let hit = wins
            .iter()
            .find(|w| w.process == s.process && w.title == s.title)
            .or_else(|| wins.iter().find(|w| w.process == s.process && !s.process.is_empty()));
        if let Some(w) = hit {
            if !ts.iter().any(|t: &Target| t.hwnd == w.hwnd) {
                ts.push(Target { hwnd: w.hwnd, title: w.title.clone(), process: w.process.clone(), enabled: true });
            }
        }
    }
    *a.targets.lock().unwrap() = ts;
}

#[tauri::command]
pub fn list_windows() -> Value {
    let a = app();
    let ts = a.targets.lock().unwrap().clone();
    let cfg = a.cfg();
    let wins: Vec<Value> = platform::list_windows()
        .into_iter()
        .map(|w| {
            let on = ts.iter().any(|t| t.hwnd == w.hwnd && t.enabled);
            let blocked = cfg.is_blacklisted(&w.process, &w.title);
            json!({"hwnd": w.hwnd, "title": w.title, "process": w.process, "watching": on, "blocked": blocked, "minimized": w.minimized})
        })
        .collect();
    json!(wins)
}

#[tauri::command]
pub fn set_target(hwnd: i64, on: bool) {
    let a = app();
    if on {
        if let Some(w) = platform::window_info(hwnd) {
            add_target(&w);
        }
    } else {
        a.targets.lock().unwrap().retain(|t| t.hwnd != hwnd);
        persist_targets();
        a.push_flags();
        a.emit_all("targets", json!(*a.targets.lock().unwrap()));
    }
}

#[tauri::command]
pub fn clear_targets() {
    let a = app();
    a.targets.lock().unwrap().clear();
    persist_targets();
    a.push_flags();
    a.emit_all("targets", json!([]));
}

/// 几秒后把鼠标下面的窗口加为观察目标
#[tauri::command]
pub async fn pick_window_delayed(ms: u64) -> Option<WinInfo> {
    tokio::time::sleep(std::time::Duration::from_millis(ms.min(10_000))).await;
    let (x, y) = platform::cursor_pos();
    let w = platform::window_at_point(x, y)?;
    add_target(&w);
    talk::say(&format!("好，我盯着《{}》。", w.title.chars().take(20).collect::<String>()), "talk");
    Some(w)
}

/// 把你最近在用的窗口交给宠物
pub fn watch_foreground() {
    let a = app();
    let w = platform::foreground_window().or_else(|| a.last_fg.lock().unwrap().clone());
    match w {
        Some(w) => {
            if a.cfg().is_blacklisted(&w.process, &w.title) {
                talk::say("这个窗口在不看名单里。", "talk");
                return;
            }
            add_target(&w);
            talk::say(&format!("好，我盯着《{}》。", w.title.chars().take(20).collect::<String>()), "talk");
        }
        None => talk::say("没找到当前窗口。", "error"),
    }
}

#[tauri::command]
pub fn toggle_breath(on: Option<bool>) {
    let next = on.unwrap_or(!breath::is_on());
    breath::set(next);
}

#[tauri::command]
pub fn look(question: Option<String>) {
    tauri::async_runtime::spawn(talk::look_once(question));
}

#[tauri::command]
pub fn send_chat(text: String, mode: String) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    match mode.as_str() {
        "operate" => {
            tauri::async_runtime::spawn(crate::operate::run(text, false));
        }
        "point" => {
            tauri::async_runtime::spawn(crate::operate::run(text, true));
        }
        _ => {
            tauri::async_runtime::spawn(talk::chat(text));
        }
    }
}

/// 闭嘴 + 停手
#[tauri::command]
pub fn stop_all() {
    let a = app();
    a.tts.stop();
    crate::operate::abort();
    tauri::async_runtime::spawn(async {
        let a = app();
        a.s_chat.kill().await;
        a.s_op.kill().await;
    });
    a.emit_stage("bubble", json!({"mode": "hide"}));
    a.set_thinking(false);
}

#[tauri::command]
pub fn test_voice(text: Option<String>) {
    let a = app();
    a.tts.stop();
    let t = text.unwrap_or_else(|| format!("你好呀，我是{}，有事叫我。", a.cfg().pet_name));
    talk::say(&t, "talk");
}

#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn system_voices() -> Vec<String> {
    platform::system_voices()
}

#[tauri::command]
pub async fn claude_status() -> Value {
    let a = app();
    let port = a.cfg().port;
    let found = crate::claude::find_claude();
    let path = match &found {
        Some(crate::claude::ClaudeExe::Exe(p)) => p.to_string_lossy().to_string(),
        Some(crate::claude::ClaudeExe::NodeScript { script, .. }) => script.to_string_lossy().to_string(),
        None => String::new(),
    };
    let version = if found.is_some() { crate::claude::run_claude_cmd(&["--version"]).await.unwrap_or_default() } else { String::new() };
    let auth = if found.is_some() { crate::claude::run_claude_cmd(&["auth", "status", "--text"]).await.unwrap_or_else(|e| e) } else { String::new() };
    json!({
        "found": found.is_some(),
        "path": path,
        "version": version,
        "auth": auth,
        "hooks": server::hooks_installed(port),
        "loggedIn": found.is_some() && crate::desktop::logged_in().await,
        "desktop": crate::desktop::desktop_status(port),
        "ocrLangs": platform::ocr_languages(),
        "log": crate::logger::log_path().map(|p| p.to_string_lossy().to_string()),
    })
}

#[tauri::command]
pub fn install_hooks() -> Result<String, String> {
    server::install_hooks(app().cfg().port)
}

#[tauri::command]
pub fn uninstall_hooks() -> Result<String, String> {
    server::uninstall_hooks(app().cfg().port)
}

#[tauri::command]
pub async fn install_mcp() -> Result<String, String> {
    let port = app().cfg().port;
    let url = format!("http://127.0.0.1:{port}/mcp");
    let _ = crate::claude::run_claude_cmd(&["mcp", "remove", "--scope", "user", "claude-pet"]).await;
    crate::claude::run_claude_cmd(&["mcp", "add", "--transport", "http", "--scope", "user", "claude-pet", &url])
        .await
        .map(|s| format!("已注册。Claude Code 里现在可以用 pet_say / pet_look / pet_point 了。\n{s}"))
}

/// 把宠物登记进 Claude 桌面版（普通聊天、Code 标签页都能用宠物工具）
#[tauri::command]
pub fn desktop_register() -> Result<String, String> {
    crate::desktop::register_desktop(app().cfg().port)
}

#[tauri::command]
pub fn desktop_unregister() -> Result<String, String> {
    crate::desktop::unregister_desktop()
}

/// 后台安装 Claude Code（不弹终端）
#[tauri::command]
pub async fn cli_install() -> Result<String, String> {
    crate::desktop::install_cli().await
}

/// 登录 Claude Code（会打开浏览器）
#[tauri::command]
pub async fn cli_login() -> Result<String, String> {
    let r = crate::desktop::login_cli().await;
    if r.is_ok() {
        let a = app();
        a.s_chat.warm(false).await;
    }
    r
}

#[tauri::command]
pub async fn cli_login_code(code: String) -> Result<(), String> {
    crate::desktop::login_code(code).await
}

#[tauri::command]
pub fn translate_start() {
    crate::translate::begin();
}

#[tauri::command]
pub fn translate_region(x: f64, y: f64, w: f64, h: f64) {
    tauri::async_runtime::spawn(crate::translate::region(x, y, w, h));
}

#[tauri::command]
pub fn translate_cancel() {
    crate::translate::cancel();
}

#[tauri::command]
pub fn open_panel(tab: Option<String>) {
    open_panel_tab(tab.as_deref().unwrap_or("chat"));
}

/// 最近一次要打开的标签页
static PANEL_TAB: Mutex<Option<String>> = Mutex::new(None);
/// 对话框窗口正在建（别建两个）
static PANEL_CREATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 打开对话框窗口的某个标签页。窗口用到时才建，不拖慢宠物启动；关掉只是藏起来，再开是瞬间的。
/// 建窗口放在单独的线程里发起：托盘菜单、快捷键这些回调本身在主线程上，在那里直接建会卡住（Tauri 在 Windows 上的限制）。
pub fn open_panel_tab(tab: &str) {
    let a = app();
    *PANEL_TAB.lock().unwrap() = Some(tab.to_string());
    if let Some(w) = a.handle.get_webview_window("panel") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        a.emit_panel("open-tab", json!({"tab": tab}));
        return;
    }
    if PANEL_CREATING.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = a.handle.clone();
    let tab = tab.to_string();
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let built = tauri::WebviewWindowBuilder::new(&handle, "panel", tauri::WebviewUrl::App("panel.html".into()))
            .title("Claude 宠物")
            .inner_size(640.0, 700.0)
            .min_inner_size(380.0, 480.0)
            .center()
            .initialization_script(&format!("window.__PET_TAB__ = {};", serde_json::to_string(&tab).unwrap_or_default()))
            .build();
        match built {
            Ok(w) => {
                plog!("对话框窗口建好，用了 {} 毫秒", t0.elapsed().as_millis());
                let w2 = w.clone();
                w.on_window_event(move |ev| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                        api.prevent_close();
                        let _ = w2.hide();
                    }
                });
                let _ = w.set_focus();
            }
            Err(e) => {
                plog!("!!! 对话框窗口建不出来：{e}");
                talk::say("设置窗口打不开，请把日志发给开发者。", "error");
            }
        }
        PANEL_CREATING.store(false, Ordering::SeqCst);
    });
}

/// 用默认浏览器打开登录页（只放行 claude.ai / anthropic.com 的网址）
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    let ok = ["https://claude.ai/", "https://claude.com/", "https://console.anthropic.com/", "https://platform.claude.com/"].iter().any(|p| url.starts_with(p));
    if !ok {
        return Err("只能打开 Claude 的登录页".into());
    }
    #[cfg(windows)]
    {
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &url]).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        plog!("打开网址：{url}");
    }
    Ok(())
}

#[tauri::command]
pub fn open_log_dir() {
    if let Some(p) = crate::logger::log_path() {
        if let Some(d) = p.parent() {
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("explorer").arg(d).spawn();
            }
            #[cfg(not(windows))]
            {
                plog!("日志目录：{}", d.display());
            }
        }
    }
}

#[tauri::command]
pub fn open_sprites_dir() {
    let d = app().data_dir.join("sprites");
    let _ = std::fs::create_dir_all(&d);
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(&d).spawn();
    }
}

#[tauri::command]
pub fn menu_action(action: String) {
    let a = app();
    plog!("菜单：{action}");
    match action.as_str() {
        "look" => {
            tauri::async_runtime::spawn(talk::look_once(None));
        }
        "breath" => toggle_breath(None),
        "mode-quiet" | "mode-normal" | "mode-detailed" => {
            let m = action.trim_start_matches("mode-").to_string();
            a.cfg.write().unwrap().breath_mode = m.clone();
            a.save_cfg();
            a.push_flags();
            let cn = match m.as_str() {
                "quiet" => "安静模式：只说要紧的",
                "detailed" => "细讲模式：我多讲点",
                _ => "正常模式",
            };
            talk::say(cn, "talk");
        }
        "watch" => watch_foreground(),
        "windows" => open_panel_tab("watch"),
        "operate" => open_panel_tab("operate"),
        "chat" => open_panel_tab("chat"),
        "settings" => open_panel_tab("settings"),
        "translate" => crate::translate::begin(),
        "update" => crate::update::check_now(),
        "shutup" => stop_all(),
        "voice" => {
            let on = !a.cfg().tts_enabled;
            a.cfg.write().unwrap().tts_enabled = on;
            a.save_cfg();
            apply_config(&a.cfg());
            if !on {
                a.tts.stop();
            }
        }
        "wander" => {
            let on = !a.cfg().wander;
            a.cfg.write().unwrap().wander = on;
            a.save_cfg();
            apply_config(&a.cfg());
        }
        "hide" => {
            if let Some(w) = a.handle.get_webview_window("stage") {
                let _ = w.hide();
            }
        }
        "quit" => crate::quit(),
        _ => {}
    }
}
