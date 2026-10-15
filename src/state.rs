//! 全局状态

use crate::claude::Session;
use crate::config::Config;
use crate::platform::{RectI, WinInfo};
use crate::tts::Tts;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

/// 被观察的窗口
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub hwnd: i64,
    pub title: String,
    pub process: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// 舞台窗口（透明、铺满一个显示器）的位置，物理像素
#[derive(Debug, Clone, Copy)]
pub struct StageGeom {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scale: f64,
}

impl StageGeom {
    pub fn to_css(&self, sx: i32, sy: i32) -> (f64, f64) {
        ((sx - self.x) as f64 / self.scale, (sy - self.y) as f64 / self.scale)
    }
    pub fn to_screen(&self, cx: f64, cy: f64) -> (i32, i32) {
        ((cx * self.scale).round() as i32 + self.x, (cy * self.scale).round() as i32 + self.y)
    }
    pub fn contains_screen(&self, sx: i32, sy: i32) -> bool {
        sx >= self.x && sx < self.x + self.w && sy >= self.y && sy < self.y + self.h
    }
}

#[derive(Default)]
pub struct HitState {
    /// 宠物窗口现在在哪（屏幕物理像素），鼠标离它远的时候不用穿透
    pub win: RectI,
    /// 舞台上可以点的区域（宠物、气泡、菜单、翻译卡片），CSS 像素
    pub rects: Vec<RectF>,
    /// 框选翻译时整个屏幕都要接收鼠标
    pub capture_all: bool,
    /// 帮你点击的那一瞬间强制让鼠标穿透
    pub force_through: bool,
}

pub struct App {
    pub handle: AppHandle,
    pub cfg: RwLock<Config>,
    pub cfg_path: PathBuf,
    pub data_dir: PathBuf,
    pub tts: Arc<Tts>,
    pub s_breath: Session,
    pub s_chat: Session,
    pub s_op: Session,
    pub s_tr: Session,
    pub targets: Mutex<Vec<Target>>,
    pub breathing: AtomicBool,
    pub last_fg: Mutex<Option<WinInfo>>,
    /// 最近用过的几个前台窗口（新的在前）
    pub recent_fg: Mutex<Vec<WinInfo>>,
    pub hit: Mutex<HitState>,
    pub stage: Mutex<StageGeom>,
    pub confirms: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    pub walks: Mutex<HashMap<String, oneshot::Sender<()>>>,
    pub op_abort: AtomicBool,
    pub operating: AtomicBool,
    pub ids: AtomicU64,
    /// 宠物脚底中心点（屏幕物理像素）
    pub pet_pos: Mutex<(i32, i32)>,
    pub hook_seen: Mutex<HashMap<String, Instant>>,
    /// 各个窗口上次摆的位置（物理像素），没变就不挪
    pub wins: Mutex<HashMap<String, RectI>>,
    pub thinking: AtomicBool,
    pub looking: AtomicBool,
    #[allow(dead_code)]
    pub alerting: AtomicBool,
}

/// Claude 桌面版的窗口（进程名 claude.exe；Claude Code 是控制台程序，窗口属于终端，不会被认成它）
pub fn is_claude_desktop(w: &WinInfo) -> bool {
    let p = w.process.to_lowercase();
    p == "claude.exe" || p == "claude"
}

static APP: OnceLock<Arc<App>> = OnceLock::new();

pub fn set_app(a: Arc<App>) {
    let _ = APP.set(a);
}

pub fn app() -> &'static Arc<App> {
    APP.get().expect("app not initialized")
}

pub fn try_app() -> Option<&'static Arc<App>> {
    APP.get()
}

impl App {
    pub fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.ids.fetch_add(1, Ordering::SeqCst))
    }

    pub fn cfg(&self) -> Config {
        self.cfg.read().unwrap().clone()
    }

    pub fn save_cfg(&self) {
        let c = self.cfg();
        c.save(&self.cfg_path);
    }

    pub fn emit_stage(&self, event: &str, payload: Value) {
        if let Some(w) = self.handle.get_webview_window("stage") {
            let _ = w.emit(event, payload);
        }
    }

    /// 发给浏览器里的对话框（见 webui.rs）
    pub fn emit_panel(&self, event: &str, payload: Value) {
        crate::webui::push(event, &payload);
    }

    /// 宠物窗口和浏览器里的对话框都发
    pub fn emit_all(&self, event: &str, payload: Value) {
        crate::webui::push(event, &payload);
        let _ = self.handle.emit(event, payload);
    }

    /// 同步宠物的各种状态给界面（决定播放哪个动作）
    pub fn push_flags(&self) {
        let flags = json!({
            "breathing": self.breathing.load(Ordering::SeqCst),
            "thinking": self.thinking.load(Ordering::SeqCst),
            "looking": self.looking.load(Ordering::SeqCst),
            "operating": self.operating.load(Ordering::SeqCst),
            "speaking": self.tts.speaking.load(Ordering::SeqCst),
            "breathMode": self.cfg.read().unwrap().breath_mode,
            "targets": self.targets.lock().unwrap().iter().filter(|t| t.enabled).count(),
        });
        self.emit_all("flags", flags);
    }

    pub fn set_thinking(&self, v: bool) {
        self.thinking.store(v, Ordering::SeqCst);
        self.push_flags();
    }

    pub fn set_looking(&self, v: bool) {
        self.looking.store(v, Ordering::SeqCst);
        self.push_flags();
    }

    pub fn stage_geom(&self) -> StageGeom {
        *self.stage.lock().unwrap()
    }

    /// 当前要看的窗口：优先第一个开启的观察目标，否则用户最近在用的窗口
    /// Claude 桌面版里聊天时说的“我的屏幕”：勾选的观察窗口优先，否则最近用过的、不是 Claude 桌面版的那个窗口
    pub fn user_window(&self) -> Option<WinInfo> {
        let targets = self.targets.lock().unwrap().clone();
        for t in targets.iter().filter(|t| t.enabled) {
            if let Some(w) = crate::platform::window_info(t.hwnd) {
                return Some(w);
            }
        }
        let recent = self.recent_fg.lock().unwrap().clone();
        for w in recent.iter().filter(|w| !is_claude_desktop(w)) {
            if let Some(w) = crate::platform::window_info(w.hwnd) {
                return Some(w);
            }
        }
        self.primary_target()
    }

    pub fn primary_target(&self) -> Option<WinInfo> {
        let targets = self.targets.lock().unwrap().clone();
        for t in targets.iter().filter(|t| t.enabled) {
            if let Some(w) = crate::platform::window_info(t.hwnd) {
                return Some(w);
            }
        }
        crate::platform::foreground_window().or_else(|| self.last_fg.lock().unwrap().clone())
    }
}
