//! 舞台：宠物所在显示器上的一套坐标（CSS 像素，左上角是显示器左上角）。
//! 0.1 版用一个铺满显示器的透明窗口画所有东西，系统每帧都要合成一整屏透明层，卡。
//! 现在拆成三个小窗口，都只在用到的地方出现：
//! - stage：宠物 + 气泡 + 右键菜单，窗口只比宠物大一圈，宠物跑的时候整个窗口跟着挪；
//! - fx：按钮上的指示圈，永远鼠标穿透；
//! - sel：框选翻译时铺满屏幕，框完缩成翻译卡片那么大。

use crate::platform::{self, RectI};
use crate::state::{app, StageGeom};
use crate::plog;
use serde_json::json;
use std::time::Duration;
use tauri::Manager;
#[cfg(not(windows))]
use tauri::{PhysicalPosition, PhysicalSize};
use tokio::sync::oneshot;

/// 把舞台铺到 (sx, sy) 所在的显示器上
pub fn ensure_on(sx: i32, sy: i32) {
    let a = app();
    let g = a.stage_geom();
    if g.contains_screen(sx, sy) {
        return;
    }
    place_on_monitor(sx, sy);
}

pub fn place_on_monitor(sx: i32, sy: i32) {
    let a = app();
    let m = platform::monitor_at(sx, sy);
    let Some(win) = a.handle.get_webview_window("stage") else { return };
    // 缩放比例按宠物窗口现在所在的显示器取；换显示器后窗口一挪过去 Windows 会自动换成那边的比例
    let scale = win.scale_factor().unwrap_or(1.0);
    let geom = StageGeom { x: m.x, y: m.y, w: m.w, h: m.h, scale };
    *a.stage.lock().unwrap() = geom;
    a.wins.lock().unwrap().clear();
    let wa = platform::work_area_at(sx, sy);
    let (wx, wy) = geom.to_css(wa.x, wa.y);
    a.emit_stage(
        "stage-geom",
        json!({"scale": scale, "work": {"x": wx, "y": wy, "w": wa.w as f64 / scale, "h": wa.h as f64 / scale}, "mon": {"w": m.w as f64 / scale, "h": m.h as f64 / scale}}),
    );
    plog!("舞台放到显示器 {:?}，缩放 {scale}", m);
}

static FIRST_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn first_shown() -> bool {
    FIRST_SHOWN.load(std::sync::atomic::Ordering::SeqCst)
}

/// 把一个小窗口摆到舞台坐标 (x, y, w, h)（CSS 像素）。show：Some(true) 顺便显示，Some(false) 藏起来。
pub fn place(label: &str, x: f64, y: f64, w: f64, h: f64, show: Option<bool>) {
    let a = app();
    let Some(win) = a.handle.get_webview_window(label) else { return };
    if show == Some(false) {
        let _ = win.hide();
        a.wins.lock().unwrap().remove(label);
        return;
    }
    let g = a.stage_geom();
    let (sx, sy) = g.to_screen(x, y);
    let r = RectI { x: sx, y: sy, w: ((w * g.scale).round() as i32).max(1), h: ((h * g.scale).round() as i32).max(1) };
    let last = a.wins.lock().unwrap().insert(label.to_string(), r);
    if label == "stage" {
        a.hit.lock().unwrap().win = r;
    }
    let same_size = matches!(last, Some(l) if l.w == r.w && l.h == r.h);
    if last.map(|l| l.x != r.x || l.y != r.y || !same_size).unwrap_or(true) {
        move_window(&win, r, same_size);
        crate::diag::WIN_MOVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if show == Some(true) && !win.is_visible().unwrap_or(false) {
        let _ = win.show();
        if label == "stage" && !FIRST_SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
            crate::diag::mark(&format!("宠物第一次显示在 ({}, {}) 大小 {}x{}", r.x, r.y, r.w, r.h));
        }
        // Linux 上窗口显示以后才能设穿透（Windows 上建窗口时已经设好）
        #[cfg(not(windows))]
        if label == "fx" {
            let _ = win.set_ignore_cursor_events(true);
        }
    }
}

#[cfg(windows)]
fn move_window(win: &tauri::WebviewWindow, r: RectI, same_size: bool) {
    if let Ok(h) = win.hwnd() {
        let (w, hh) = if same_size { (0, 0) } else { (r.w, r.h) };
        platform::set_window_rect(h.0 as isize as i64, r.x, r.y, w, hh);
    }
}

#[cfg(not(windows))]
fn move_window(win: &tauri::WebviewWindow, r: RectI, same_size: bool) {
    let _ = win.set_position(PhysicalPosition::new(r.x, r.y));
    if !same_size {
        let _ = win.set_size(PhysicalSize::new(r.w as u32, r.h as u32));
    }
}

/// 截图、录屏时要不要把宠物拍进去（指示圈和翻译框永远不拍）
pub fn apply_capture_flag() {
    #[cfg(windows)]
    {
        let a = app();
        let hide = a.cfg.read().map(|c| c.hide_from_capture).unwrap_or(true);
        for (label, on) in [("stage", hide), ("fx", true), ("sel", true)] {
            if let Some(w) = a.handle.get_webview_window(label) {
                if let Ok(h) = w.hwnd() {
                    platform::exclude_from_capture(h.0 as isize as i64, on);
                }
            }
        }
    }
}

/// 宠物大约多大（物理像素）
pub fn pet_size_phys() -> (i32, i32) {
    let a = app();
    let h = a.cfg.read().unwrap().pet_height as f64;
    let s = a.stage_geom().scale;
    ((h * 1.1 * s) as i32, (h * s) as i32)
}

/// 让宠物跑到屏幕上某点（脚底中心，物理像素），跑到了再返回
#[allow(dead_code)]
pub async fn walk_to(sx: i32, sy: i32, face: Option<&str>) {
    let a = app();
    ensure_on(sx, sy);
    let g = a.stage_geom();
    let (cx, cy) = g.to_css(sx, sy);
    let id = a.next_id("w");
    let (tx, rx) = oneshot::channel();
    a.walks.lock().unwrap().insert(id.clone(), tx);
    let speed = a.cfg.read().unwrap().run_speed / g.scale;
    a.emit_stage("walk-to", json!({"id": id, "x": cx, "y": cy, "speed": speed, "face": face}));
    let _ = tokio::time::timeout(Duration::from_millis(2500), rx).await;
    a.walks.lock().unwrap().remove(&id);
}

/// 跑到某个元素旁边准备操作，跑到了再返回。
/// purpose：press 按 / point 指 / type 打字。站哪儿由舞台根据动作素材的指尖位置算（指尖对准按钮）。
pub async fn approach(r: RectI, purpose: &str) {
    let a = app();
    ensure_on(r.x + r.w / 2, r.y + r.h / 2);
    let g = a.stage_geom();
    let (x, y) = g.to_css(r.x, r.y);
    let id = a.next_id("w");
    let (tx, rx) = oneshot::channel();
    a.walks.lock().unwrap().insert(id.clone(), tx);
    let speed = a.cfg.read().unwrap().run_speed / g.scale;
    a.emit_stage(
        "approach",
        json!({"id": id, "x": x, "y": y, "w": r.w as f64 / g.scale, "h": r.h as f64 / g.scale, "purpose": purpose, "speed": speed}),
    );
    // 转身 0.5 秒 + 跑 + 急停 0.7 秒，最长也就 2 秒多
    let _ = tokio::time::timeout(Duration::from_millis(4000), rx).await;
    a.walks.lock().unwrap().remove(&id);
}

/// 播“伸手按”的动作，在按下的那一帧返回（这时候再真的点击）
pub async fn press(double: bool) {
    let a = app();
    let id = a.next_id("p");
    let (tx, rx) = oneshot::channel();
    a.walks.lock().unwrap().insert(id.clone(), tx);
    a.emit_stage("pet-action", json!({"action": "press", "id": id, "double": double}));
    let _ = tokio::time::timeout(Duration::from_millis(1200), rx).await;
    a.walks.lock().unwrap().remove(&id);
}

/// 宠物做个动作（不等它播完）
pub fn pet_action(action: &str, extra: serde_json::Value) {
    let mut v = json!({"action": action});
    if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in e {
            m.insert(k.clone(), val.clone());
        }
    }
    app().emit_stage("pet-action", v);
}

/// 开始指着某个元素（光圈一直亮着，宠物每隔几秒回头看你）
pub fn point_start(r: RectI) {
    let a = app();
    ensure_on(r.x + r.w / 2, r.y + r.h / 2);
    let g = a.stage_geom();
    let (x, y) = g.to_css(r.x, r.y);
    pet_action("point-start", json!({"x": x, "y": y, "w": r.w as f64 / g.scale, "h": r.h as f64 / g.scale}));
    highlight_clear(Some("point"));
    highlight(r, "point", 0);
}

/// 结束指着：clicked 你点了 / timeout 等太久 / cancel 取消
pub fn point_end(result: &str) {
    highlight_clear(Some("point"));
    pet_action("point-end", json!({"result": result}));
}

/// 在屏幕上画一个提示圈（画在 fx 小窗口里）。kind：click / press / point / region；ms=0 一直留着
pub fn highlight(r: RectI, kind: &str, ms: u64) {
    let a = app();
    ensure_on(r.x + r.w / 2, r.y + r.h / 2);
    let g = a.stage_geom();
    let (x, y) = g.to_css(r.x, r.y);
    a.emit_to("fx", "fx-highlight", json!({"x": x, "y": y, "w": r.w as f64 / g.scale, "h": r.h as f64 / g.scale, "kind": kind, "ms": ms}));
}

/// 清掉提示圈（kind 为空 = 全清）
pub fn highlight_clear(kind: Option<&str>) {
    app().emit_to("fx", "fx-clear", json!({"kind": kind}));
}

/// 鼠标是否落在宠物窗口里可点的地方：每 16ms 看一次，决定宠物窗口要不要让鼠标穿透。
/// 鼠标离宠物窗口远的时候干脆不穿透：穿透要给窗口加 WS_EX_LAYERED，合成起来更费事，
/// 而鼠标不在上面时穿不穿透都一样。
pub fn spawn_hit_test_loop() {
    std::thread::Builder::new()
        .name("hit-test".into())
        .spawn(|| {
            let mut last: Option<bool> = None;
            let mut fails = 0u32;
            loop {
                std::thread::sleep(Duration::from_millis(16));
                let Some(a) = crate::state::try_app() else { continue };
                let (sx, sy) = platform::cursor_pos();
                let g = a.stage_geom();
                let (cx, cy) = g.to_css(sx, sy);
                let relax = a.cfg.read().map(|c| c.hit_relax).unwrap_or(true);
                let inside = {
                    let h = a.hit.lock().unwrap();
                    let m = (64.0 * g.scale) as i32;
                    let w = h.win;
                    let near = w.w > 0 && sx >= w.x - m && sx < w.x + w.w + m && sy >= w.y - m && sy < w.y + w.h + m;
                    if h.force_through {
                        false
                    } else if h.capture_all {
                        true
                    } else if relax && !near {
                        true
                    } else {
                        g.contains_screen(sx, sy) && h.rects.iter().any(|r| r.contains(cx, cy))
                    }
                };
                if last != Some(inside) {
                    if let Some(w) = a.handle.get_webview_window("stage").filter(|w| w.is_visible().unwrap_or(false)) {
                        let t0 = std::time::Instant::now();
                        let r = w.set_ignore_cursor_events(!inside);
                        let us = t0.elapsed().as_micros() as u64;
                        crate::diag::HIT_TOGGLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        crate::diag::HIT_TOGGLE_MAX_US.fetch_max(us, std::sync::atomic::Ordering::Relaxed);
                        match r {
                            Ok(()) => last = Some(inside),
                            Err(e) => {
                                fails += 1;
                                if fails <= 3 {
                                    plog!("设置鼠标穿透失败：{e}");
                                }
                            }
                        }
                    }
                }
            }
        })
        .ok();
}

/// 记住用户最近在用的窗口（点宠物或打开对话框后，前台窗口会变成我们自己的）
pub fn spawn_foreground_tracker() {
    std::thread::Builder::new()
        .name("fg-tracker".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_millis(400));
            let Some(a) = crate::state::try_app() else { continue };
            if let Some(w) = platform::foreground_window() {
                {
                    let mut r = a.recent_fg.lock().unwrap();
                    if r.first().map(|x| x.hwnd) != Some(w.hwnd) {
                        r.retain(|x| x.hwnd != w.hwnd);
                        r.insert(0, w.clone());
                        r.truncate(8);
                    }
                }
                *a.last_fg.lock().unwrap() = Some(w);
            }
        })
        .ok();
}
