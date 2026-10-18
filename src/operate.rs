//! 帮你操作：宠物跑到按钮旁边，伸手点它 / 帮你打字 / 指给你看。
//! 元素位置来自系统无障碍树（毫秒级），只有找不到时才让 Claude 看截图给坐标。

use crate::claude::{extract_json, Block};
use crate::platform::{self, RectI};
use crate::snapshot;
use crate::stage;
use crate::state::app;
use crate::talk;
use crate::plog;
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::oneshot;

pub fn abort() {
    let a = app();
    if a.operating.load(Ordering::SeqCst) {
        a.op_abort.store(true, Ordering::SeqCst);
    }
}

fn aborted() -> bool {
    app().op_abort.load(Ordering::SeqCst)
}

/// 指给你看时等你自己点多久（设置里可调）
pub fn point_wait() -> Duration {
    Duration::from_secs(app().cfg.read().map(|c| c.point_wait_s).unwrap_or(20).clamp(3, 600))
}

/// 问用户确认，返回是否同意（设置里的秒数内没回答算不同意）
pub async fn confirm(text: &str) -> bool {
    confirm_custom(text, ("好，点吧", "先别")).await
}

pub async fn confirm_custom(text: &str, labels: (&str, &str)) -> bool {
    let a = app();
    let id = a.next_id("c");
    let (tx, rx) = oneshot::channel();
    a.confirms.lock().unwrap().insert(id.clone(), tx);
    talk::ask_buttons(text, "op", &id, &[(labels.0, true), (labels.1, false)]);
    let wait = a.cfg.read().map(|c| c.confirm_wait_s).unwrap_or(30).clamp(5, 600);
    let ok = matches!(tokio::time::timeout(Duration::from_secs(wait), rx).await, Ok(Ok(true)));
    a.confirms.lock().unwrap().remove(&id);
    a.emit_stage("bubble-clear-buttons", serde_json::json!({}));
    ok
}

pub async fn run(text: String, point_only: bool) {
    let a = app();
    if a.operating.swap(true, Ordering::SeqCst) {
        talk::say("我手上还有活，等一下哦。", "op");
        return;
    }
    a.op_abort.store(false, Ordering::SeqCst);
    a.push_flags();
    talk::user_line(&text);
    a.tts.stop();
    let r = run_inner(&text, point_only).await;
    if let Err(e) = r {
        talk::say(&e, "error");
    }
    a.operating.store(false, Ordering::SeqCst);
    a.hit.lock().unwrap().force_through = false;
    a.push_flags();
}

async fn run_inner(text: &str, point_only: bool) -> Result<(), String> {
    let a = app();
    let cfg = a.cfg();
    let win = a.primary_target().ok_or("我不知道要在哪个窗口里操作。先点一下那个窗口，再叫我。")?;
    if cfg.is_blacklisted(&win.process, &win.title) {
        return Err("这个窗口在不碰名单里，我不动它。".into());
    }
    a.set_looking(true);
    let w2 = win.clone();
    let look = tokio::task::spawn_blocking(move || crate::talk::look_at(&w2, true)).await.map_err(|e| e.to_string())?;
    a.set_looking(false);

    let mut blocks = Vec::new();
    let mut img_map: Option<(i32, i32, f64)> = None; // 截图左上角屏幕坐标、缩放比例
    if let Some((img, ox, oy)) = &look.image {
        let jpg = img.to_jpeg(1280, 80);
        let (sw, _sh) = img.scaled_size(1280);
        if !jpg.is_empty() {
            blocks.push(Block::Image(jpg));
            img_map = Some((*ox, *oy, img.w as f64 / sw as f64));
        }
    }
    let (sw, sh) = look.image.as_ref().map(|(i, _, _)| i.scaled_size(1280)).unwrap_or((0, 0));
    let list = snapshot::element_list(&look.ui, 250);
    let hint = if point_only { "（用户只想让你指给他看，只能用 point）" } else { "" };
    blocks.push(Block::Text(format!(
        "用户说：{text}{hint}\n窗口：{}（{}）\n截图尺寸：{sw}x{sh}\n键盘焦点：{}\n可操作元素（#编号 类型「名字」@(屏幕x,y,宽x高)）：\n{}",
        win.title,
        win.process,
        look.ui.focused.clone().unwrap_or_else(|| "无".into()),
        if list.is_empty() { "（读不到元素，请看截图用坐标）".to_string() } else { list }
    )));

    a.s_op.set_model(&cfg.operate_model);
    a.set_thinking(true);
    let ans = a.s_op.ask(blocks, |_| {}).await;
    a.set_thinking(false);
    let ans = ans?;
    if ans.interrupted || aborted() {
        return Ok(());
    }
    let plan = extract_json(&ans.text).ok_or_else(|| {
        plog!("看不懂的计划：{}", ans.text);
        "我没想明白该怎么做，换个说法试试？".to_string()
    })?;
    let say = plan["say"].as_str().unwrap_or("").to_string();
    let steps = plan["steps"].as_array().cloned().unwrap_or_default();
    let needs_confirm = plan["confirm"].as_bool().unwrap_or(false) || (cfg.operate_confirm_all && !point_only);

    if steps.is_empty() {
        talk::say(if say.is_empty() { "我没找到要点的东西。" } else { &say }, "op");
        return Ok(());
    }
    if needs_confirm {
        let q = if say.is_empty() { "这一步不能撤销，要我点吗？".to_string() } else { format!("{say}，这一步不能撤销，要我点吗？") };
        if !confirm(&q).await {
            talk::say("好，那我不动了。", "op");
            return Ok(());
        }
    } else if !say.is_empty() {
        talk::say(&say, "op");
    }

    // 宠物记住出发点，做完竖个大拇指再跑回来
    stage::pet_action("op-begin", serde_json::json!({}));
    let mut result = Ok(());
    for step in steps {
        if aborted() {
            talk::say("停下了。", "op");
            break;
        }
        if let Err(e) = do_step(&win.hwnd, &look.ui, &step, img_map, &cfg).await {
            result = Err(e);
            break;
        }
    }
    let ok = result.is_ok() && !aborted();
    stage::pet_action("op-end", serde_json::json!({"ok": ok}));
    result?;
    let done = plan["done_say"].as_str().unwrap_or("");
    if ok && !done.is_empty() && !point_only {
        talk::say(done, "op");
    }
    Ok(())
}

fn rect_of(hwnd: i64, ui: &platform::UiSnapshot, step: &Value, img_map: Option<(i32, i32, f64)>) -> Option<RectI> {
    // 直接给了屏幕位置（桌面版聊天里的 pet_act 用 pet_look 那一刻的位置）
    if let Some(r) = step["rect"].as_array().filter(|r| r.len() == 4) {
        let v: Vec<i32> = r.iter().map(|x| x.as_i64().unwrap_or(0) as i32).collect();
        if v[2] > 0 && v[3] > 0 {
            return Some(RectI { x: v[0], y: v[1], w: v[2], h: v[3] });
        }
    }
    if let Some(id) = step["id"].as_u64() {
        let id = id as usize;
        if let Some(r) = platform::ui_rect(hwnd, id) {
            if r.w > 0 && r.h > 0 {
                return Some(r);
            }
        }
        if let Some(e) = ui.elements.get(id) {
            return Some(RectI { x: e.x, y: e.y, w: e.w, h: e.h });
        }
    }
    if let (Some(x), Some(y), Some((ox, oy, s))) = (step["x"].as_f64(), step["y"].as_f64(), img_map) {
        let sx = ox + (x * s) as i32;
        let sy = oy + (y * s) as i32;
        return Some(RectI { x: sx - 12, y: sy - 12, w: 24, h: 24 });
    }
    None
}

async fn do_step(hwnd: &i64, ui: &platform::UiSnapshot, step: &Value, img_map: Option<(i32, i32, f64)>, cfg: &crate::config::Config) -> Result<(), String> {
    let hwnd = *hwnd;
    let op = step["op"].as_str().unwrap_or("");
    match op {
        "wait" => {
            tokio::time::sleep(Duration::from_millis(step["ms"].as_u64().unwrap_or(400).min(5000))).await;
            return Ok(());
        }
        "key" => {
            let keys = step["keys"].as_str().unwrap_or("").to_string();
            platform::bring_to_front(hwnd);
            tokio::time::sleep(Duration::from_millis(80)).await;
            stage::pet_action("tap", serde_json::json!({}));
            if !tokio::task::spawn_blocking(move || platform::press_keys(&keys)).await.unwrap_or(false) {
                return Err(format!("这个快捷键我按不了：{}", step["keys"]));
            }
            return Ok(());
        }
        _ => {}
    }
    let r = rect_of(hwnd, ui, step, img_map).ok_or("找不到要操作的位置。")?;
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    match op {
        "point" => {
            // 跑过去指着，光圈一直亮，等你自己点（等多久看设置）
            stage::approach(r, "point").await;
            if aborted() {
                return Ok(());
            }
            stage::point_start(r);
            let s = step["say"].as_str().unwrap_or("在这里！");
            talk::say(s, "op");
            let result = wait_user_click(r, point_wait()).await;
            stage::point_end(result);
        }
        "click" | "double" | "right" => {
            let btn = match op {
                "double" => "double",
                "right" => "right",
                _ => "left",
            };
            if cfg.operate_click_first {
                // 先点后演：点击零等待，宠物随后跑过去补个按的动作
                stage::highlight(r, "press", 700);
                click(cx, cy, btn).await;
                stage::approach(r, "press").await;
                stage::press(op == "double").await;
            } else {
                stage::approach(r, "press").await;
                if aborted() {
                    return Ok(());
                }
                // 按下那一帧才真的点
                stage::press(op == "double").await;
                stage::highlight(r, "press", 700);
                click(cx, cy, btn).await;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        "type" => {
            let text = step["text"].as_str().unwrap_or("").to_string();
            let enter = step["enter"].as_bool().unwrap_or(false);
            stage::approach(r, "type").await;
            if aborted() {
                return Ok(());
            }
            stage::press(false).await;
            stage::highlight(r, "press", 700);
            click(cx, cy, "left").await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            stage::pet_action("type-start", serde_json::json!({}));
            tokio::task::spawn_blocking(move || {
                platform::type_text(&text);
                if enter {
                    platform::press_keys("enter");
                }
            })
            .await
            .ok();
            stage::pet_action("type-end", serde_json::json!({}));
        }
        "scroll" => {
            let n = step["amount"].as_i64().unwrap_or(-3) as i32;
            stage::approach(r, "press").await;
            stage::pet_action("scroll", serde_json::json!({}));
            let a = app();
            a.hit.lock().unwrap().force_through = true;
            tokio::time::sleep(Duration::from_millis(40)).await;
            tokio::task::spawn_blocking(move || platform::scroll_at(cx, cy, n)).await.ok();
            a.hit.lock().unwrap().force_through = false;
        }
        other => return Err(format!("我还不会“{other}”这个动作。")),
    }
    Ok(())
}

/// 按给好的步骤操作（Claude 桌面版聊天里的 pet_act 用）：跑过去、按、点，做完竖拇指跑回来
pub async fn run_steps(win: &platform::WinInfo, ui: &platform::UiSnapshot, steps: Vec<Value>) -> Result<(), String> {
    let a = app();
    if a.operating.swap(true, Ordering::SeqCst) {
        return Err("宠物手上还有别的活，等一下再试".into());
    }
    a.op_abort.store(false, Ordering::SeqCst);
    a.push_flags();
    let cfg = a.cfg();
    stage::pet_action("op-begin", serde_json::json!({}));
    let mut result = Ok(());
    for step in steps {
        if aborted() {
            result = Err("用户叫停了".to_string());
            break;
        }
        if let Err(e) = do_step(&win.hwnd, ui, &step, None, &cfg).await {
            result = Err(e);
            break;
        }
    }
    stage::pet_action("op-end", serde_json::json!({"ok": result.is_ok()}));
    a.operating.store(false, Ordering::SeqCst);
    a.hit.lock().unwrap().force_through = false;
    a.push_flags();
    result
}

/// 等用户自己点那个元素：点中了返回 clicked，超时 timeout，被叫停 cancel
pub async fn wait_user_click(r: RectI, limit: Duration) -> &'static str {
    let start = std::time::Instant::now();
    let pad = 8;
    let mut was_down = platform::left_button_down();
    while start.elapsed() < limit {
        if aborted() {
            return "cancel";
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        let down = platform::left_button_down();
        if down && !was_down {
            let (x, y) = platform::cursor_pos();
            if x >= r.x - pad && x <= r.x + r.w + pad && y >= r.y - pad && y <= r.y + r.h + pad {
                return "clicked";
            }
        }
        was_down = down;
    }
    "timeout"
}

/// 真正点下去：点的瞬间让舞台完全穿透，点完把鼠标放回原处
async fn click(x: i32, y: i32, btn: &'static str) {
    let a = app();
    a.hit.lock().unwrap().force_through = true;
    tokio::time::sleep(Duration::from_millis(40)).await;
    let (ox, oy) = platform::cursor_pos();
    tokio::task::spawn_blocking(move || {
        platform::click_at(x, y, btn);
        std::thread::sleep(Duration::from_millis(60));
        platform::move_cursor(ox, oy);
    })
    .await
    .ok();
    a.hit.lock().unwrap().force_through = false;
}
