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
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;

/// 正在指路、等你自己点时，气泡上“你帮我点”按钮对应的确认编号（聊天里说“你帮我点”也走它）
static POINT_TAKE: Mutex<Option<String>> = Mutex::new(None);

/// 指路时你说“你帮我点”：让宠物自己点下去。没在等你点就返回 false
pub fn take_over() -> bool {
    let id = POINT_TAKE.lock().unwrap().clone();
    match id.and_then(|id| app().confirms.lock().unwrap().remove(&id)) {
        Some(tx) => tx.send(true).is_ok(),
        None => false,
    }
}

/// 聊天里这句话是不是叫宠物自己动手点（指路等你点的时候用）
pub fn is_take_over(text: &str) -> bool {
    let t: String = text.chars().filter(|c| !c.is_whitespace() && !"，。！？,.!?~～".contains(*c)).collect();
    t.chars().count() <= 12 && ["你帮我点", "你来点", "帮我点", "你点", "你来", "你帮我弄", "帮我弄", "你帮我", "你去点", "你操作"].iter().any(|k| t.contains(k))
}

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
    talk::user_line(&text);
    run_quiet(text, point_only).await;
}

/// 和 run 一样，只是不再把你的话写进对话框（聊天里已经写过了）
pub async fn run_quiet(text: String, point_only: bool) {
    let a = app();
    if a.operating.swap(true, Ordering::SeqCst) {
        if !take_over() {
            talk::say("我手上还有活，等一下哦。", "op");
        }
        return;
    }
    a.op_abort.store(false, Ordering::SeqCst);
    a.push_flags();
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
    let rounds = cfg.operate_rounds.clamp(1, 30);
    let mut began = false;
    let mut result = Ok(());
    // 一轮：看一眼画面 → 让 Claude 规划能确定的几步 → 做完；没做完就看新画面接着来
    for round in 0..rounds {
        if aborted() {
            break;
        }
        let ask = if round == 0 {
            let hint = if point_only { "（用户只想让你指给他看，只能用 point）" } else { "" };
            format!("用户说：{text}{hint}")
        } else {
            format!("（继续）上一轮的步骤做完了{}。这是现在的画面，接着完成用户的要求：{text}。已经完成了就 steps 留空、done 填 true，say 说一句结果。", if point_only { "，用户已经自己点了" } else { "" })
        };
        let plan = match plan_round(&win, &ask, &cfg).await? {
            Some(p) => p,
            None => break,
        };
        let say = plan.plan["say"].as_str().unwrap_or("").to_string();
        let steps = plan.plan["steps"].as_array().cloned().unwrap_or_default();
        let done = plan.plan["done"].as_bool().unwrap_or(true);
        if steps.is_empty() {
            let fallback = if round == 0 { "我没找到要点的东西。" } else { "好了。" };
            talk::say(if say.is_empty() { fallback } else { &say }, "op");
            break;
        }
        let needs_confirm = plan.plan["confirm"].as_bool().unwrap_or(false) || (cfg.operate_confirm_all && !point_only);
        if needs_confirm {
            let q = if say.is_empty() { "这一步不能撤销，要我点吗？".to_string() } else { format!("{say}，这一步不能撤销，要我点吗？") };
            if !confirm(&q).await {
                talk::say("好，那我不动了。", "op");
                break;
            }
        } else if !say.is_empty() {
            talk::say(&say, "op");
        }
        // 宠物记住出发点，做完竖个大拇指再跑回来
        if !began {
            began = true;
            stage::pet_action("op-begin", serde_json::json!({}));
        }
        let mut user_skipped = false;
        for step in steps {
            if aborted() {
                talk::say("停下了。", "op");
                break;
            }
            match do_step(&win.hwnd, &plan.ui, &step, plan.img_map, &cfg).await {
                Ok(StepEnd::Timeout) => user_skipped = true,
                Ok(_) => {}
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
            if user_skipped {
                break;
            }
        }
        if result.is_err() || aborted() {
            break;
        }
        if user_skipped {
            talk::say("你没点，那我先不往下走了。要我自己点，就说“你帮我点”。", "op");
            break;
        }
        if done {
            let d = plan.plan["done_say"].as_str().unwrap_or("");
            if !d.is_empty() && !point_only {
                talk::say(d, "op");
            }
            break;
        }
        if round + 1 == rounds {
            talk::say("步骤有点多，我先做到这儿。还要继续就再叫我。", "op");
            break;
        }
        // 等界面换到下一屏再看
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    if began {
        stage::pet_action("op-end", serde_json::json!({"ok": result.is_ok() && !aborted()}));
    }
    result
}

struct Plan {
    plan: Value,
    ui: platform::UiSnapshot,
    img_map: Option<(i32, i32, f64)>,
}

/// 看一眼窗口，把画面和可操作元素交给 Claude，拿回这一轮的步骤。被叫停时返回 None
async fn plan_round(win: &platform::WinInfo, ask: &str, cfg: &crate::config::Config) -> Result<Option<Plan>, String> {
    let a = app();
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
    blocks.push(Block::Text(format!(
        "{ask}\n窗口：{}（{}）\n截图尺寸：{sw}x{sh}\n键盘焦点：{}\n可操作元素（#编号 类型「名字」@(屏幕x,y,宽x高)）：\n{}",
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
        return Ok(None);
    }
    let plan = extract_json(&ans.text).ok_or_else(|| {
        plog!("看不懂的计划：{}", ans.text);
        "我没想明白该怎么做，换个说法试试？".to_string()
    })?;
    plog!("操作计划：{}", plan);
    Ok(Some(Plan { plan, ui: look.ui, img_map }))
}

/// 一步做完的结果：指路时你没点（等超时）要单独处理
enum StepEnd {
    Done,
    Timeout,
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

async fn do_step(hwnd: &i64, ui: &platform::UiSnapshot, step: &Value, img_map: Option<(i32, i32, f64)>, cfg: &crate::config::Config) -> Result<StepEnd, String> {
    let hwnd = *hwnd;
    let op = step["op"].as_str().unwrap_or("");
    match op {
        "wait" => {
            tokio::time::sleep(Duration::from_millis(step["ms"].as_u64().unwrap_or(400).min(5000))).await;
            return Ok(StepEnd::Done);
        }
        "key" => {
            let keys = step["keys"].as_str().unwrap_or("").to_string();
            platform::bring_to_front(hwnd);
            tokio::time::sleep(Duration::from_millis(80)).await;
            stage::pet_action("tap", serde_json::json!({}));
            if !tokio::task::spawn_blocking(move || platform::press_keys(&keys)).await.unwrap_or(false) {
                return Err(format!("这个快捷键我按不了：{}", step["keys"]));
            }
            return Ok(StepEnd::Done);
        }
        _ => {}
    }
    let r = rect_of(hwnd, ui, step, img_map).ok_or("找不到要操作的位置。")?;
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    match op {
        "point" => {
            // 跑过去指着，光圈一直亮，等你自己点（等多久看设置）；气泡上有“你帮我点”，点了宠物就自己点
            stage::approach(r, "point").await;
            if aborted() {
                return Ok(StepEnd::Done);
            }
            stage::point_start(r);
            let s = step["say"].as_str().unwrap_or("在这里！");
            let a = app();
            let id = a.next_id("c");
            let (tx, rx) = oneshot::channel();
            a.confirms.lock().unwrap().insert(id.clone(), tx);
            *POINT_TAKE.lock().unwrap() = Some(id.clone());
            talk::ask_buttons(s, "op", &id, &[("你帮我点", true)]);
            let result = tokio::select! {
                r = wait_user_click(r, point_wait()) => r,
                Ok(true) = rx => "take",
            };
            *POINT_TAKE.lock().unwrap() = None;
            a.confirms.lock().unwrap().remove(&id);
            a.emit_stage("bubble-clear-buttons", serde_json::json!({}));
            if result == "take" {
                stage::point_end("cancel");
                stage::approach(r, "press").await;
                if aborted() {
                    return Ok(StepEnd::Done);
                }
                stage::press(false).await;
                stage::highlight(r, "press", 700);
                click(cx, cy, "left").await;
                tokio::time::sleep(Duration::from_millis(150)).await;
                return Ok(StepEnd::Done);
            }
            stage::point_end(result);
            if result == "timeout" {
                return Ok(StepEnd::Timeout);
            }
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
                    return Ok(StepEnd::Done);
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
                return Ok(StepEnd::Done);
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
    Ok(StepEnd::Done)
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

#[cfg(test)]
mod tests {
    use super::is_take_over;

    #[test]
    fn take_over_phrases() {
        for t in ["你帮我点", "你帮我点吧！", "你来点", "帮我点一下", "你来", "你帮我弄"] {
            assert!(is_take_over(t), "{t}");
        }
        for t in ["这个按钮在哪", "帮我点开设置里的蓝牙然后把它关掉再打开一下", "你好"] {
            assert!(!is_take_over(t), "{t}");
        }
    }
}
