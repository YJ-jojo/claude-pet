//! 呼吸模式：后台一直盯着指定窗口，画面一变就让 Claude 讲给你听。
//!
//! 本地每 0.7 秒截一次小图比对（不花钱），画面稳定下来后读一次文字，
//! 只把“新出现 / 消失的文字”发给 Claude，Claude 觉得不值得说就回 [静默]。
//! 一发现画面变了，宠物先做个“看过去”的动作，并提前连好语音服务器。

use crate::claude::Block;
use crate::platform::{self, WinInfo};
use crate::snapshot::{self, Seen};
use crate::state::app;
use crate::talk;
use crate::plog;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static RUN_ID: AtomicU64 = AtomicU64::new(0);

struct Watch {
    hwnd: i64,
    title: String,
    thumb: Vec<u8>,
    seen: Seen,
    dirty_since: Option<Instant>,
    last_change: Instant,
    big_visual: bool,
    /// 上次用 PrintWindow 截它的时间（被挡住时才用，那个比较重，会让对方程序重画一遍）
    last_slow: Instant,
}

/// 被挡住的窗口多久才截一次（PrintWindow 会让 Chrome、Electron 这类程序整页重画）
const SLOW_CAPTURE_MS: u64 = 3000;

pub fn is_on() -> bool {
    app().breathing.load(Ordering::SeqCst)
}

pub fn set(on: bool) {
    let a = app();
    let was = a.breathing.swap(on, Ordering::SeqCst);
    a.push_flags();
    if on && !was {
        let id = RUN_ID.fetch_add(1, Ordering::SeqCst) + 1;
        tauri::async_runtime::spawn(run(id));
    } else if !on && was {
        RUN_ID.fetch_add(1, Ordering::SeqCst);
        talk::say("好，我先不看了。", "breath");
    }
}

fn alive(id: u64) -> bool {
    RUN_ID.load(Ordering::SeqCst) == id && is_on()
}

fn current_targets() -> Vec<WinInfo> {
    let a = app();
    let cfg = a.cfg();
    let ts = a.targets.lock().unwrap().clone();
    let mut out: Vec<WinInfo> = ts
        .iter()
        .filter(|t| t.enabled)
        .filter_map(|t| platform::window_info(t.hwnd))
        .filter(|w| !cfg.is_blacklisted(&w.process, &w.title))
        .collect();
    if out.is_empty() && ts.iter().all(|t| !t.enabled) {
        // 没指定窗口时就看你当前在用的那个
        if let Some(w) = a.primary_target() {
            if !cfg.is_blacklisted(&w.process, &w.title) {
                out.push(w);
            }
        }
    }
    out
}

/// 窗口的小灰度图。露在外面就直接拷屏幕（便宜）；被挡住时才让它自己画一遍（allow_slow）。
fn thumb_of(hwnd: i64, allow_slow: bool) -> Option<(Vec<u8>, bool)> {
    if let Some(img) = platform::capture_visible(hwnd) {
        return Some((img.thumb(48, 32), false));
    }
    if !allow_slow {
        return None;
    }
    platform::capture_window(hwnd).map(|(img, _, _)| (img.thumb(48, 32), true))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

async fn run(id: u64) {
    let a = app();
    let cfg = a.cfg();
    a.s_breath.set_model(&cfg.breath_model);
    a.s_breath.reset().await;

    let targets = current_targets();
    if targets.is_empty() {
        let key = a.cfg().hotkey_watch.replace("CommandOrControl", "Ctrl");
        talk::say(&format!("我还不知道看哪个窗口。右键我选“选择要看的窗口”，或者按 {key} 把当前窗口交给我。"), "breath");
        a.breathing.store(false, Ordering::SeqCst);
        a.push_flags();
        return;
    }

    // 先完整看一遍，作为开场
    let mut watches: Vec<Watch> = Vec::new();
    let mut intro = Vec::new();
    for w in &targets {
        let w2 = w.clone();
        let Some(look) = blocking(move || talk::look_at(&w2, false)).await else { continue };
        intro.push(snapshot::full(w, &look.seen, 2500));
        watches.push(Watch {
            hwnd: w.hwnd,
            title: w.title.clone(),
            thumb: blocking({
                let h = w.hwnd;
                move || thumb_of(h, true)
            })
            .await
            .flatten()
            .map(|t| t.0)
            .unwrap_or_default(),
            seen: look.seen,
            dirty_since: None,
            last_change: Instant::now(),
            big_visual: false,
            last_slow: Instant::now(),
        });
    }
    if !alive(id) {
        return;
    }
    let intro_hint = mode_hint(&cfg.breath_mode);
    let talking = Arc::new(AtomicBool::new(true));
    {
        let t = talking.clone();
        let msg = format!("开始呼吸模式（{intro_hint}）。这是你现在要盯着的窗口：\n{}\n先用一句话打个招呼，并说说你看到了什么。", Value::Array(intro));
        tauri::async_runtime::spawn(async move {
            let _ = talk::talk(&app().s_breath, vec![Block::Text(msg)], "breath").await;
            t.store(false, Ordering::SeqCst);
        });
    }

    let mut pending: Vec<Value> = Vec::new();
    let mut pending_image: Option<Vec<u8>> = None;
    let mut last_talk = Instant::now();
    let mut last_image = Instant::now() - Duration::from_secs(60);
    let mut last_target_check = Instant::now();
    let mut last_notice = Instant::now() - Duration::from_secs(60);

    while alive(id) {
        let cfg = a.cfg();
        tokio::time::sleep(Duration::from_millis(cfg.breath_interval_ms.clamp(200, 5000))).await;
        if !alive(id) {
            break;
        }

        // 观察目标有增减时同步一下
        if last_target_check.elapsed() > Duration::from_secs(2) {
            last_target_check = Instant::now();
            let now_targets = current_targets();
            watches.retain(|w| {
                let keep = now_targets.iter().any(|t| t.hwnd == w.hwnd);
                if !keep && !platform::is_window(w.hwnd) {
                    talk::say(&format!("《{}》关掉了，我不看它了。", short(&w.title)), "breath");
                }
                keep
            });
            for t in now_targets {
                if !watches.iter().any(|w| w.hwnd == t.hwnd) {
                    let t2 = t.clone();
                    if let Some(look) = blocking(move || talk::look_at(&t2, false)).await {
                        pending.push(json!({"new_window": snapshot::full(&t, &look.seen, 1500)}));
                        watches.push(Watch {
                            hwnd: t.hwnd,
                            title: t.title.clone(),
                            thumb: vec![],
                            seen: look.seen,
                            dirty_since: None,
                            last_change: Instant::now(),
                            big_visual: false,
                            last_slow: Instant::now(),
                        });
                    }
                }
            }
            if watches.is_empty() {
                talk::say("要看的窗口都关掉了，呼吸模式先停一下。", "breath");
                a.breathing.store(false, Ordering::SeqCst);
                a.push_flags();
                break;
            }
        }

        // 1. 本地比对小图，找出变化的窗口
        let mut noticed = false;
        for w in watches.iter_mut() {
            let h = w.hwnd;
            let slow_ok = w.last_slow.elapsed() > Duration::from_millis(SLOW_CAPTURE_MS);
            let Some((th, slow)) = blocking(move || thumb_of(h, slow_ok)).await.flatten() else { continue };
            if slow {
                w.last_slow = Instant::now();
            }
            let ratio = platform::thumb_changed_ratio(&w.thumb, &th, 14);
            if !w.thumb.is_empty() && ratio > 0.0 {
                if w.dirty_since.is_none() {
                    w.dirty_since = Some(Instant::now());
                    noticed = true;
                }
                w.last_change = Instant::now();
                if ratio > 0.3 {
                    w.big_visual = true;
                }
            }
            w.thumb = th;
        }

        // 发现变化：宠物先有反应（不出声），语音通道先连上，等 Claude 开口时省掉握手
        if noticed && !talking.load(Ordering::SeqCst) && !a.tts.speaking.load(Ordering::SeqCst) && last_notice.elapsed() > Duration::from_secs(3) {
            last_notice = Instant::now();
            a.emit_stage("pet-action", json!({"action": "notice"}));
            a.tts.prewarm();
        }

        // 2. 画面稳定下来（或者一直在变但等太久了）就读文字算差异
        //    一直在变的（比如正在往外冒字的回答）最多每 2 秒读一次，别让被看的程序一直被读界面
        let settle = Duration::from_millis(cfg.breath_settle_ms);
        let max_wait = (settle * 6).max(Duration::from_millis(2000));
        for w in watches.iter_mut() {
            let Some(since) = w.dirty_since else { continue };
            if w.last_change.elapsed() < settle && since.elapsed() < max_wait {
                continue;
            }
            w.dirty_since = None;
            let Some(win) = platform::window_info(w.hwnd) else { continue };
            let want_img = w.big_visual;
            let w2 = win.clone();
            let Some(look) = blocking(move || talk::look_at(&w2, want_img)).await else { continue };
            let (added, removed) = snapshot::diff(&w.seen.lines, &look.seen.lines);
            let added_chars: usize = added.iter().map(|s| s.chars().count()).sum();
            let important = snapshot::has_alert(&added);
            let quiet = cfg.breath_mode == "quiet";
            if (added_chars >= 2 || !removed.is_empty()) && (!quiet || important) {
                pending.push(snapshot::change(&win, &added, &removed, &look.seen, if cfg.breath_mode == "detailed" { 3000 } else { 1500 }));
            } else if w.big_visual && added_chars < 2 && look.seen.char_count() < 30 && !quiet {
                // 文字没变但画面大变（视频、图片、游戏）：偶尔附一张截图
                if last_image.elapsed() > Duration::from_secs(if cfg.breath_mode == "detailed" { 8 } else { 20 }) {
                    if let Some((img, _, _)) = &look.image {
                        pending_image = Some(img.to_jpeg(1024, 70));
                        pending.push(json!({"window": format!("{}（{}）", win.title, win.process), "visual_change": true}));
                        last_image = Instant::now();
                    }
                }
            }
            w.big_visual = false;
            w.seen = look.seen;
            w.title = win.title;
        }

        // 3. 有要讲的、上一段讲完了、离上次开口够久，就开讲
        let gap = Duration::from_millis(cfg.breath_min_gap_ms);
        let urgent = pending.iter().any(|p| p["dialog"].as_bool().unwrap_or(false));
        if !pending.is_empty() && !talking.load(Ordering::SeqCst) && !a.tts.speaking.load(Ordering::SeqCst) && (last_talk.elapsed() >= gap || urgent) {
            let batch = std::mem::take(&mut pending);
            let mut blocks = Vec::new();
            if let Some(img) = pending_image.take() {
                if !img.is_empty() {
                    blocks.push(Block::Image(img));
                }
            }
            let body = if batch.len() == 1 { batch[0].to_string() } else { Value::Array(batch).to_string() };
            blocks.push(Block::Text(format!("（{}）{body}", mode_hint(&cfg.breath_mode))));
            talking.store(true, Ordering::SeqCst);
            last_talk = Instant::now();
            let t = talking.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = talk::talk(&app().s_breath, blocks, "breath").await {
                    plog!("呼吸模式讲解失败：{e}");
                }
                t.store(false, Ordering::SeqCst);
            });
        }
    }
    plog!("呼吸模式结束");
}

fn mode_hint(mode: &str) -> &'static str {
    match mode {
        "quiet" => "安静模式：只在报错、警告、需要确认时开口，其它一律 [静默]",
        "detailed" => "细讲模式：可以讲得详细些",
        _ => "正常模式",
    }
}

fn short(s: &str) -> String {
    let t: String = s.chars().take(16).collect();
    if s.chars().count() > 16 {
        format!("{t}…")
    } else {
        t
    }
}
