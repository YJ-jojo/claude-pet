#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod breath;
mod claude;
mod commands;
mod config;
mod desktop;
mod diag;
mod update;
mod logger;
mod operate;
mod platform;
mod prompts;
mod server;
mod snapshot;
mod stage;
mod state;
mod talk;
mod translate;
mod tts;

use crate::state::{app, App, HitState, StageGeom};
use claude::Session;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

pub fn quit() {
    if let Some(a) = state::try_app() {
        let (x, y) = *a.pet_pos.lock().unwrap();
        {
            let mut c = a.cfg.write().unwrap();
            c.pet_x = Some(x);
            c.pet_y = Some(y);
        }
        a.save_cfg();
        a.tts.stop();
        a.handle.exit(0);
    } else {
        std::process::exit(0);
    }
}

pub fn register_hotkeys(handle: &tauri::AppHandle) {
    let cfg = app().cfg();
    let gs = handle.global_shortcut();
    let _ = gs.unregister_all();
    let binds: Vec<(String, &'static str)> = vec![
        (cfg.hotkey_translate.clone(), "translate"),
        (cfg.hotkey_look.clone(), "look"),
        (cfg.hotkey_breath.clone(), "breath"),
        (cfg.hotkey_watch.clone(), "watch"),
        (cfg.hotkey_stop.clone(), "shutup"),
        (cfg.hotkey_chat.clone(), "chat"),
    ];
    let mut moved: Vec<(&'static str, String)> = vec![];
    for (key, action) in binds {
        if key.trim().is_empty() {
            continue;
        }
        let bind = |k: &str| {
            gs.on_shortcut(k, move |_app, _sc, ev| {
                if ev.state == ShortcutState::Pressed {
                    commands::menu_action(action.to_string());
                }
            })
        };
        let Err(e) = bind(&key) else { continue };
        // 被别的程序占了（比如微信常用 Ctrl+Alt+W）：自动加上 Shift 再试一次，成功就记进设置
        let alt = with_shift(&key);
        match alt.as_deref().map(bind) {
            Some(Ok(())) => {
                let alt = alt.unwrap();
                plog!("快捷键 {key} 被别的程序占了（{e}），改用 {alt}");
                moved.push((action, alt));
            }
            _ => {
                plog!("快捷键 {key} 注册失败：{e}");
                talk::system_line(&format!("快捷键 {key} 被别的程序占用了，可以在设置里换一个。"));
            }
        }
    }
    if !moved.is_empty() {
        let a = app();
        {
            let mut c = a.cfg.write().unwrap();
            for (action, k) in &moved {
                match *action {
                    "translate" => c.hotkey_translate = k.clone(),
                    "look" => c.hotkey_look = k.clone(),
                    "breath" => c.hotkey_breath = k.clone(),
                    "watch" => c.hotkey_watch = k.clone(),
                    "shutup" => c.hotkey_stop = k.clone(),
                    "chat" => c.hotkey_chat = k.clone(),
                    _ => {}
                }
            }
        }
        a.save_cfg();
        let list: Vec<String> = moved.iter().map(|(_, k)| k.replace("CommandOrControl", "Ctrl")).collect();
        talk::system_line(&format!("有快捷键被别的程序占了，已自动换成 {}，设置里能看到。", list.join("、")));
    }
}

/// "CommandOrControl+Alt+W" → "CommandOrControl+Alt+Shift+W"（已经有 Shift 就不换）
fn with_shift(key: &str) -> Option<String> {
    let parts: Vec<&str> = key.split('+').map(str::trim).collect();
    if parts.len() < 2 || parts.iter().any(|p| p.eq_ignore_ascii_case("shift")) {
        return None;
    }
    let (last, mods) = parts.split_last()?;
    Some(format!("{}+Shift+{last}", mods.join("+")))
}

fn build_tray(handle: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(handle, "show", "显示 / 隐藏宠物", true, None::<&str>)?;
    let panel = MenuItem::with_id(handle, "panel", "打开对话框", true, None::<&str>)?;
    let breath = MenuItem::with_id(handle, "breath", "呼吸模式 开 / 关", true, None::<&str>)?;
    let tr = MenuItem::with_id(handle, "translate", "框选翻译", true, None::<&str>)?;
    let settings = MenuItem::with_id(handle, "settings", "设置", true, None::<&str>)?;
    let upd = MenuItem::with_id(handle, "update", format!("检查更新（现在 {}）", env!("CARGO_PKG_VERSION")), true, None::<&str>)?;
    let key = MenuItem::with_id(handle, "update-key", "生成更新钥匙", true, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(handle)?;
    let menu = Menu::with_items(handle, &[&show, &panel, &breath, &tr, &settings, &sep, &upd, &key, &sep, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Claude 宠物")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app_h, ev| match ev.id().as_ref() {
            "show" => {
                if let Some(w) = app_h.get_webview_window("stage") {
                    if w.is_visible().unwrap_or(true) {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                    }
                }
            }
            "panel" => commands::open_panel_tab("chat"),
            "settings" => commands::open_panel_tab("settings"),
            "update" => update::check_now(),
            "update-key" => update::gen_key(),
            other => commands::menu_action(other.to_string()),
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                let h = tray.app_handle();
                if let Some(w) = h.get_webview_window("stage") {
                    let _ = w.show();
                }
                commands::open_panel_tab("chat");
            }
        });
    if let Some(icon) = handle.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(handle)?;
    Ok(())
}

fn small_window(handle: &tauri::AppHandle, label: &str, page: &str) -> tauri::Result<tauri::WebviewWindow> {
    WebviewWindowBuilder::new(handle, label, WebviewUrl::App(page.into()))
        .title("Claude 宠物")
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focusable(false)
        .focused(false)
        .visible(false)
        .inner_size(200.0, 200.0)
        .build()
}

/// 宠物窗口：只比宠物大一圈，透明、置顶、不抢焦点；宠物跑的时候整个窗口跟着挪（位置由 stage.js 定）
fn create_stage(handle: &tauri::AppHandle) -> tauri::Result<()> {
    let a = app();
    let cfg = a.cfg();
    let (px, py) = match (cfg.pet_x, cfg.pet_y) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            let wa = platform::work_area_at(0, 0);
            (wa.x + wa.w - 220, wa.y + wa.h)
        }
    };
    let m = platform::monitor_at(px, py);
    let stage = small_window(handle, "stage", "stage.html")?;
    let _ = stage.set_position(tauri::PhysicalPosition::new(px - 150, py - 300));
    let scale = stage.scale_factor().unwrap_or(1.0);
    *a.stage.lock().unwrap() = StageGeom { x: m.x, y: m.y, w: m.w, h: m.h, scale };
    *a.pet_pos.lock().unwrap() = (px, py);
    #[cfg(windows)]
    {
        let _ = stage.set_ignore_cursor_events(true);
        if let Ok(h) = stage.hwnd() {
            platform::set_no_activate(h.0 as isize as i64);
        }
    }
    stage::apply_capture_flag();
    // 宠物窗口等 stage.js 摆好位置后自己显示
    Ok(())
}

/// 其余三个窗口（指示圈、框选翻译、对话框）在后台线程里建，不挡宠物和托盘、快捷键、本地服务。
/// 0.2.1 在用户电脑上 4 个窗口一个接一个建，花了 117 秒，这期间本地服务和快捷键都没起来。
fn create_other_windows(handle: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("windows".into())
        .spawn(move || {
            // 指示圈：永远穿透（Linux 上没显示的窗口不能设穿透，只在 Windows 上一开始就设）
            match small_window(&handle, "fx", "fx.html") {
                Ok(fx) => {
                    diag::mark("fx 窗口建好");
                    #[cfg(windows)]
                    {
                        let _ = fx.set_ignore_cursor_events(true);
                        if let Ok(h) = fx.hwnd() {
                            platform::set_no_activate(h.0 as isize as i64);
                        }
                    }
                    #[cfg(not(windows))]
                    let _ = fx;
                }
                Err(e) => plog!("!!! fx 窗口建不出来：{e}"),
            }
            // 框选翻译 + 翻译卡片：出现时就要接鼠标，不穿透
            match small_window(&handle, "sel", "sel.html") {
                Ok(sel) => {
                    diag::mark("sel 窗口建好");
                    #[cfg(windows)]
                    if let Ok(h) = sel.hwnd() {
                        platform::set_no_activate(h.0 as isize as i64);
                    }
                    #[cfg(not(windows))]
                    let _ = sel;
                }
                Err(e) => plog!("!!! sel 窗口建不出来：{e}"),
            }
            stage::apply_capture_flag();
            let panel = WebviewWindowBuilder::new(&handle, "panel", WebviewUrl::App("panel.html".into()))
                .title("Claude 宠物")
                .inner_size(460.0, 700.0)
                .min_inner_size(380.0, 480.0)
                .visible(false)
                .build();
            match panel {
                Ok(panel) => {
                    diag::mark("panel 窗口建好");
                    let p2 = panel.clone();
                    panel.on_window_event(move |ev| {
                        if let WindowEvent::CloseRequested { api, .. } = ev {
                            api.prevent_close();
                            let _ = p2.hide();
                        }
                    });
                    commands::panel_created();
                }
                Err(e) => plog!("!!! 对话框窗口建不出来：{e}"),
            }
            diag::startup_done();
        })
        .ok();
}

fn setup(handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    diag::mark("开始初始化");
    let cfg_dir = handle.path().app_config_dir()?;
    let data_dir = handle.path().app_data_dir()?;
    logger::init(data_dir.join("logs"));
    match tauri::webview_version() {
        Ok(v) => plog!("WebView2 版本 {v}"),
        Err(e) => plog!("!!! 读不到 WebView2 版本：{e}（系统可能没装 WebView2 运行时）"),
    }
    let cfg_path = cfg_dir.join("config.json");
    let cfg = config::Config::load(&cfg_path);
    plog!(
        "设置：形象包 {}，大小 {}，防截图 {}，鼠标附近才穿透 {}，语音 {}，呼吸模式间隔 {}ms，端口 {}（{}）",
        cfg.sprite_pack,
        cfg.pet_height,
        cfg.hide_from_capture,
        cfg.hit_relax,
        cfg.tts_engine,
        cfg.breath_interval_ms,
        cfg.port,
        cfg_path.display()
    );
    claude::set_override(&cfg.claude_path);

    let tts = tts::Tts::start(commands::new_voice(&cfg), |ev| {
        if let Some(a) = state::try_app() {
            match ev {
                tts::PlayEvent::Start(t) => a.emit_stage("speaking", json!({"text": t})),
                tts::PlayEvent::Idle => a.emit_stage("speaking", json!({"text": null})),
            }
            a.push_flags();
        }
    });
    let work = data_dir.join("claude-work");
    let name = cfg.pet_name.clone();
    let app_state = Arc::new(App {
        handle: handle.clone(),
        s_breath: Session::new("breath", &cfg.breath_model, prompts::breath(&name), 30, work.clone()),
        s_chat: Session::new("chat", &cfg.chat_model, prompts::chat(&name), 24, work.clone()),
        s_op: Session::new("operate", &cfg.operate_model, prompts::operate(&name), 8, work.clone()),
        s_tr: Session::new("translate", &cfg.translate_model, prompts::translate(), 20, work),
        cfg: RwLock::new(cfg.clone()),
        cfg_path,
        data_dir: data_dir.clone(),
        tts: tts.clone(),
        targets: Mutex::new(vec![]),
        breathing: AtomicBool::new(false),
        last_fg: Mutex::new(None),
        recent_fg: Mutex::new(Vec::new()),
        hit: Mutex::new(HitState::default()),
        stage: Mutex::new(StageGeom { x: 0, y: 0, w: 1920, h: 1080, scale: 1.0 }),
        confirms: Mutex::new(HashMap::new()),
        walks: Mutex::new(HashMap::new()),
        op_abort: AtomicBool::new(false),
        operating: AtomicBool::new(false),
        ids: AtomicU64::new(1),
        pet_pos: Mutex::new((0, 0)),
        hook_seen: Mutex::new(HashMap::new()),
        wins: Mutex::new(HashMap::new()),
        thinking: AtomicBool::new(false),
        looking: AtomicBool::new(false),
        alerting: AtomicBool::new(false),
    });
    state::set_app(app_state);
    let _ = std::fs::create_dir_all(data_dir.join("sprites"));

    commands::log_packs();
    // 托盘、快捷键、本地服务先起来：窗口建得慢时，托盘菜单和 Claude 桌面版的连接也能先用
    build_tray(handle)?;
    register_hotkeys(handle);
    server::start(cfg.port);
    diag::spawn_startup_watch();
    diag::mark("开始建宠物窗口");
    create_stage(handle)?;
    diag::mark("宠物窗口建好（还没显示，等页面加载完自己摆位置）");
    create_other_windows(handle.clone());
    diag::spawn_perf_sampler(handle.clone());
    // 10 秒后宠物还没出来：记下哪些页面没加载好
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        commands::check_started();
    });
    stage::spawn_hit_test_loop();
    stage::spawn_foreground_tracker();
    update::spawn_checker();
    update::announce_if_updated(&std::env::args().collect::<Vec<_>>());
    commands::restore_targets();

    // 预热：常用的话先合成好，Claude 进程先启动好，第一次用就快
    tts.warm(vec![
        "叮，Claude 要你确认一下".into(),
        "在等你回话".into(),
        "好，我盯着它。".into(),
        "停下了。".into(),
    ]);
    desktop::fix_stale_registration(cfg.port);
    tauri::async_runtime::spawn(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        // 没装或没登录 Claude Code：直接打开设置页，按钮点两下就好，不用开终端
        let found = claude::find_claude();
        plog!("Claude Code：{}", found.as_ref().map(|c| format!("{c:?}")).unwrap_or_else(|| "没找到".into()));
        if found.is_none() {
            talk::say("我还没有“脑子”：要装一下 Claude Code。设置页已经打开了，点“一键安装”，不用开终端。", "alert");
            commands::open_panel_tab("settings");
            return;
        }
        let logged = desktop::logged_in().await;
        plog!("Claude Code 登录状态：{}", if logged { "已登录" } else { "没登录" });
        if !logged {
            talk::say("Claude Code 装好了，但还没登录。设置页里点“登录”，在弹出的网页上用你的 Max 账号授权一下。", "alert");
            commands::open_panel_tab("settings");
            return;
        }
        let a = app();
        a.s_tr.warm(true).await;
        a.s_chat.warm(false).await;
    });
    Ok(())
}

fn main() {
    // Claude 桌面版启动的 MCP 桥：只转发消息，不开界面
    diag::start_clock();
    diag::install_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--mcp") {
        logger::init_early("桥");
        diag::log_environment("MCP 桥（Claude 桌面版启动的转发进程）");
        desktop::mcp_bridge(&args);
    }
    logger::init_early("宠物");
    diag::log_environment("界面");
    // 刚自动更新完：等旧版本退出（不然单实例检查会把新版本当成第二个实例关掉），清掉旧文件
    update::on_start(&args);
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, args, _cwd| {
            plog!("又被打开了一次（参数 {:?}）：宠物已经在跑，打开对话框", args.iter().skip(1).collect::<Vec<_>>());
            commands::open_panel_tab("chat");
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .register_uri_scheme_protocol("petres", |_ctx, req| {
            let path = req.uri().path().to_string();
            match commands::find_pack_file(&path).and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
                Some((p, bytes)) => {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    let mime = match ext.as_str() {
                        "png" => "image/png",
                        "webp" => "image/webp",
                        "gif" => "image/gif",
                        "jpg" | "jpeg" => "image/jpeg",
                        "json" => "application/json",
                        "svg" => "image/svg+xml",
                        "webm" => "video/webm",
                        "mp4" => "video/mp4",
                        _ => "application/octet-stream",
                    };
                    tauri::http::Response::builder()
                        .header("Content-Type", mime)
                        .header("Access-Control-Allow-Origin", "*")
                        .header("Cache-Control", "max-age=3600")
                        .body(bytes)
                        .unwrap()
                }
                None => tauri::http::Response::builder().status(404).body(Vec::new()).unwrap(),
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::save_config,
            commands::set_hit_rects,
            commands::set_capture_all,
            commands::win_place,
            commands::report_pet,
            commands::walk_done,
            commands::confirm_reply,
            commands::pet_clicked,
            commands::drag_end,
            commands::list_windows,
            commands::set_target,
            commands::clear_targets,
            commands::pick_window_delayed,
            commands::toggle_breath,
            commands::look,
            commands::send_chat,
            commands::stop_all,
            commands::test_voice,
            commands::system_voices,
            commands::copy_text,
            commands::claude_status,
            commands::install_hooks,
            commands::uninstall_hooks,
            commands::install_mcp,
            commands::desktop_register,
            commands::desktop_unregister,
            commands::cli_install,
            commands::cli_login,
            commands::cli_login_code,
            commands::translate_start,
            commands::translate_region,
            commands::translate_cancel,
            commands::open_panel,
            commands::open_log_dir,
            commands::open_url,
            commands::open_sprites_dir,
            commands::menu_action,
            commands::js_log,
        ])
        .setup(|app_| {
            setup(app_.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!());
    match result {
        Ok(app_) => {
            diag::mark("界面框架建好，开始运行");
            app_.run(|_h, ev| match ev {
                tauri::RunEvent::ExitRequested { api, code, .. } => {
                    // 关掉对话框不算退出；只有托盘“退出”才真的退出
                    if code.is_none() {
                        api.prevent_exit();
                    } else {
                        plog!("退出（代码 {code:?}）");
                    }
                }
                tauri::RunEvent::Exit => plog!("===== 宠物退出 ====="),
                _ => {}
            })
        }
        Err(e) => {
            logger::log(&format!("!!! 启动失败：{e}"));
            #[cfg(windows)]
            {
                let _ = std::fs::write(std::env::temp_dir().join("claude-pet-crash.txt"), format!("{e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::with_shift;

    #[test]
    fn adds_shift_to_taken_hotkeys() {
        assert_eq!(with_shift("CommandOrControl+Alt+W").as_deref(), Some("CommandOrControl+Alt+Shift+W"));
        assert_eq!(with_shift("CommandOrControl+Alt+Space").as_deref(), Some("CommandOrControl+Alt+Shift+Space"));
        assert_eq!(with_shift("Ctrl+Shift+W"), None);
        assert_eq!(with_shift("F9"), None);
    }
}
