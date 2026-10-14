//! 让宠物开口：把 Claude 的流式回答同时送到气泡、对话框和语音。

use crate::claude::{Block, Session};
use crate::platform::{self, Bitmap, WinInfo};
use crate::snapshot::{self, Seen};
use crate::state::app;
use crate::tts::SentenceSplitter;
use crate::plog;
use serde_json::json;

/// 宠物说一句固定的话（不经过 Claude）
pub fn say(text: &str, kind: &str) {
    let a = app();
    let id = a.next_id("b");
    a.emit_stage("bubble", json!({"id": id, "mode": "set", "text": text, "kind": kind}));
    a.emit_stage("bubble", json!({"id": id, "mode": "done"}));
    a.emit_panel("chat", json!({"id": id, "role": "pet", "mode": "new", "text": text, "kind": kind}));
    a.tts.say(text);
}

/// 带按钮的气泡（比如“确认 / 取消”）
pub fn ask_buttons(text: &str, kind: &str, confirm_id: &str, buttons: &[(&str, bool)]) {
    let a = app();
    let id = a.next_id("b");
    let btns: Vec<_> = buttons.iter().map(|(l, v)| json!({"label": l, "value": v})).collect();
    a.emit_stage("bubble", json!({"id": id, "mode": "set", "text": text, "kind": kind, "confirm": confirm_id, "buttons": btns}));
    a.emit_panel("chat", json!({"id": id, "role": "pet", "mode": "new", "text": text, "kind": kind}));
    a.tts.say(text);
}

pub fn user_line(text: &str) {
    let a = app();
    let id = a.next_id("u");
    a.emit_panel("chat", json!({"id": id, "role": "user", "mode": "new", "text": text}));
}

pub fn system_line(text: &str) {
    let a = app();
    let id = a.next_id("s");
    a.emit_panel("chat", json!({"id": id, "role": "system", "mode": "new", "text": text}));
}

/// 流式讲话。返回 Claude 的完整回答（[静默] 时返回空串）
pub async fn talk(session: &Session, blocks: Vec<Block>, kind: &str) -> Result<String, String> {
    let a = app();
    let id = a.next_id("b");
    a.set_thinking(true);
    // Claude 想的这一两秒里先把语音连接建好
    a.tts.prewarm();
    let mut split = SentenceSplitter::new();
    let mut head = String::new();
    let mut decided = false;
    let mut silent = false;
    let mut started = false;
    let mut shown = String::new();

    let feed = |t: &str, started: &mut bool, shown: &mut String, split: &mut SentenceSplitter| {
        if t.is_empty() {
            return;
        }
        if !*started {
            *started = true;
            a.set_thinking(false);
            a.emit_stage("bubble", json!({"id": id, "mode": "set", "text": "", "kind": kind}));
            a.emit_panel("chat", json!({"id": id, "role": "pet", "mode": "new", "text": "", "kind": kind}));
        }
        shown.push_str(t);
        a.emit_stage("bubble", json!({"id": id, "mode": "append", "text": t}));
        a.emit_panel("chat", json!({"id": id, "mode": "append", "text": t}));
        for s in split.push(t) {
            a.tts.say(&s);
        }
    };

    let res = session
        .ask(blocks, |delta| {
            if silent {
                return;
            }
            if !decided {
                head.push_str(delta);
                let t = head.trim_start();
                if t.is_empty() {
                    return;
                }
                if t.starts_with('[') || t.starts_with('【') {
                    if t.contains(']') || t.contains('】') || t.chars().count() > 8 {
                        decided = true;
                        silent = t.contains("静默");
                        if !silent {
                            let h = std::mem::take(&mut head);
                            feed(&h, &mut started, &mut shown, &mut split);
                        }
                    }
                    return;
                }
                decided = true;
                let h = std::mem::take(&mut head);
                feed(&h, &mut started, &mut shown, &mut split);
                return;
            }
            feed(delta, &mut started, &mut shown, &mut split);
        })
        .await;
    a.set_thinking(false);
    match res {
        Ok(ans) => {
            if !decided && !head.trim().is_empty() && !head.contains("静默") {
                let h = std::mem::take(&mut head);
                feed(&h, &mut started, &mut shown, &mut split);
            }
            if let Some(rest) = split.flush() {
                a.tts.say(&rest);
            }
            if started {
                a.emit_stage("bubble", json!({"id": id, "mode": "done"}));
                a.emit_panel("chat", json!({"id": id, "mode": "done"}));
            }
            if silent || ans.interrupted && shown.is_empty() {
                return Ok(String::new());
            }
            Ok(shown)
        }
        Err(e) => {
            plog!("[{}] 出错：{e}", session.name);
            say(&e, "error");
            Err(e)
        }
    }
}

/// 截一个窗口，读它的文字（无障碍树为主，读不到再用文字识别）
pub struct Look {
    pub win: WinInfo,
    pub image: Option<(Bitmap, i32, i32)>,
    pub ui: platform::UiSnapshot,
    pub seen: Seen,
}

pub fn look_at(win: &WinInfo, want_image: bool) -> Look {
    let ui = platform::ui_snapshot(win.hwnd, 400);
    let image = if want_image || ui.texts.len() < 3 { platform::capture_window(win.hwnd) } else { None };
    let mut seen = Seen::from_ui(&ui);
    if seen.char_count() < 20 {
        if let Some((img, _, _)) = &image {
            if let Ok(lines) = platform::ocr(img.clone()) {
                if !lines.is_empty() {
                    seen = Seen::from_ocr(&lines);
                }
            }
        }
    }
    Look { win: win.clone(), image, ui, seen }
}

/// 点眼睛：看一眼当前窗口并讲给你听
pub async fn look_once(question: Option<String>) {
    let a = app();
    let Some(win) = a.primary_target() else {
        say("我不知道该看哪个窗口，先点一下你想让我看的窗口再叫我吧。", "error");
        return;
    };
    let cfg = a.cfg();
    if cfg.is_blacklisted(&win.process, &win.title) {
        say("这个窗口在不看名单里，我就不看啦。", "talk");
        return;
    }
    a.set_looking(true);
    a.tts.stop();
    let w2 = win.clone();
    let look = tokio::task::spawn_blocking(move || look_at(&w2, true)).await.ok();
    a.set_looking(false);
    let Some(look) = look else { return };
    let mut blocks = Vec::new();
    if let Some((img, _, _)) = &look.image {
        let jpg = img.to_jpeg(1280, 80);
        if !jpg.is_empty() {
            blocks.push(Block::Image(jpg));
        }
    }
    let snap = snapshot::full(&look.win, &look.seen, 4000);
    let q = question.unwrap_or_else(|| "我点了你的眼睛：看一眼这个窗口，用两三句话告诉我现在最重要的是什么。".into());
    blocks.push(Block::Text(format!("{q}\n\n界面读到的文字：\n{snap}")));
    a.s_chat.set_model(&cfg.chat_model);
    let _ = talk(&a.s_chat, blocks, "look").await;
}

/// 和宠物聊天（附带当前观察窗口的文字，方便问“这是什么意思”）
pub async fn chat(text: String) {
    let a = app();
    user_line(&text);
    a.tts.stop();
    let cfg = a.cfg();
    let mut blocks = Vec::new();
    let wants_eye = ["看", "屏幕", "画面", "窗口", "这个", "这是", "这里", "报错"].iter().any(|k| text.contains(k));
    if wants_eye {
        if let Some(win) = a.primary_target() {
            if !cfg.is_blacklisted(&win.process, &win.title) {
                let w2 = win.clone();
                if let Ok(look) = tokio::task::spawn_blocking(move || look_at(&w2, true)).await {
                    if let Some((img, _, _)) = &look.image {
                        let jpg = img.to_jpeg(1280, 80);
                        if !jpg.is_empty() {
                            blocks.push(Block::Image(jpg));
                        }
                    }
                    blocks.push(Block::Text(format!("（当前窗口的文字：{}）", snapshot::full(&look.win, &look.seen, 3000))));
                }
            }
        }
    }
    blocks.push(Block::Text(text));
    a.s_chat.set_model(&cfg.chat_model);
    let _ = talk(&a.s_chat, blocks, "talk").await;
}
