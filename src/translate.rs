//! 快捷键框选翻译：框住一块屏幕 → 系统文字识别（约 0.1 秒）→ Claude 翻译 → 原文/译文对照显示在原处。

use crate::claude::{extract_json, Block};
use crate::platform::{self, OcrLine};
use crate::state::app;
use crate::talk;
use crate::plog;
use serde_json::{json, Value};

/// 开始框选：sel 小窗口铺满鼠标所在的显示器，接住鼠标
pub fn begin() {
    let a = app();
    let (sx, sy) = platform::cursor_pos();
    crate::stage::ensure_on(sx, sy);
    crate::stage::highlight_clear(Some("region"));
    let g = a.stage_geom();
    let (mw, mh) = (g.w as f64 / g.scale, g.h as f64 / g.scale);
    crate::stage::place("sel", 0.0, 0.0, mw, mh, Some(true));
    a.emit_to("sel", "sel-start", json!({"mon": {"w": mw, "h": mh}}));
}

pub fn cancel() {
    crate::stage::place("sel", 0.0, 0.0, 0.0, 0.0, Some(false));
    crate::stage::highlight_clear(Some("region"));
}

fn lang_name(code: &str) -> &'static str {
    match code {
        "en" => "英文",
        "ja" => "日文",
        "ko" => "韩文",
        _ => "简体中文",
    }
}

/// 把识别出的行合并成段落
pub fn group_lines(lines: &[OcrLine]) -> Vec<(String, f32, f32, f32, f32)> {
    let mut sorted: Vec<&OcrLine> = lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    sorted.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<(String, f32, f32, f32, f32)> = Vec::new();
    for l in sorted {
        let lh = l.h.max(8.0);
        if let Some(last) = out.last_mut() {
            let (ref mut text, x, y, w, h) = *last;
            let gap = l.y - (y + h);
            let aligned = (l.x - x).abs() < lh * 2.0;
            if gap < lh * 0.9 && aligned {
                let cjk = text.chars().last().map(|c| c as u32 > 0x2e80).unwrap_or(false)
                    || l.text.chars().next().map(|c| c as u32 > 0x2e80).unwrap_or(false);
                if !cjk {
                    text.push(' ');
                }
                text.push_str(l.text.trim());
                let nx = x.min(l.x);
                let nr = (x + w).max(l.x + l.w);
                last.1 = nx;
                last.3 = nr - nx;
                last.4 = (l.y + l.h) - y;
                continue;
            }
        }
        out.push((l.text.trim().to_string(), l.x, l.y, l.w, l.h));
    }
    out
}

/// 界面框选完成后调用（坐标是舞台的 CSS 像素）
pub async fn region(x: f64, y: f64, w: f64, h: f64) {
    let a = app();
    let g = a.stage_geom();
    let (sx, sy) = g.to_screen(x, y);
    let (sw, sh) = ((w * g.scale).round() as i32, (h * g.scale).round() as i32);
    if sw < 6 || sh < 6 {
        return;
    }
    let rect = json!({"x": x, "y": y, "w": w, "h": h});
    let mon = json!({"w": g.w as f64 / g.scale, "h": g.h as f64 / g.scale});
    let Some(img) = tokio::task::spawn_blocking(move || platform::capture_screen_rect(sx, sy, sw, sh)).await.ok().flatten() else {
        a.emit_to("sel", "sel-result", json!({"rect": rect, "mon": mon.clone(), "error": "截图失败"}));
        return;
    };
    a.emit_to("sel", "sel-result", json!({"rect": rect, "mon": mon.clone(), "loading": true}));
    crate::stage::highlight(platform::RectI { x: sx, y: sy, w: sw, h: sh }, "region", 0);
    let cfg = a.cfg();
    let target = lang_name(&cfg.translate_target);
    a.s_tr.set_model(&cfg.translate_model);

    let img2 = img.clone();
    let lines = tokio::task::spawn_blocking(move || platform::ocr(img2)).await.ok();
    let paras = match &lines {
        Some(Ok(l)) => group_lines(l),
        Some(Err(e)) => {
            plog!("文字识别失败：{e}，改用截图翻译");
            vec![]
        }
        None => vec![],
    };

    let items: Vec<Value> = if !paras.is_empty() {
        let src: Vec<String> = paras.iter().map(|p| p.0.clone()).collect();
        let msg = format!("目标语言：{target}\n段落：{}", serde_json::to_string(&src).unwrap_or_default());
        match a.s_tr.ask(vec![Block::Text(msg)], |_| {}).await {
            Ok(ans) => {
                let arr = extract_json(&ans.text).and_then(|v| v.as_array().cloned()).unwrap_or_default();
                src.iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let dst = arr.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let p = &paras[i];
                        json!({"src": s, "dst": dst, "x": p.1 as f64 / g.scale, "y": p.2 as f64 / g.scale, "w": p.3 as f64 / g.scale, "h": p.4 as f64 / g.scale})
                    })
                    .collect()
            }
            Err(e) => {
                a.emit_to("sel", "sel-result", json!({"rect": rect, "mon": mon.clone(), "error": e}));
                return;
            }
        }
    } else {
        // 识别不出文字（或者系统没装识别语言包）：让 Claude 直接看图翻译
        let jpg = img.to_jpeg(1600, 85);
        let msg = format!("目标语言：{target}。按要求输出 JSON。");
        match a.s_tr.ask(vec![Block::Image(jpg), Block::Text(format!("{}\n{msg}", crate::prompts::translate_image()))], |_| {}).await {
            Ok(ans) => extract_json(&ans.text).and_then(|v| v.as_array().cloned()).unwrap_or_default(),
            Err(e) => {
                a.emit_to("sel", "sel-result", json!({"rect": rect, "mon": mon.clone(), "error": e}));
                return;
            }
        }
    };
    if items.is_empty() {
        a.emit_to("sel", "sel-result", json!({"rect": rect, "mon": mon.clone(), "error": "这块里没认出文字"}));
        return;
    }
    a.emit_to("sel", "sel-result", json!({"rect": rect, "mon": mon.clone(), "items": items}));
    if cfg.translate_speak {
        let all: Vec<String> = items.iter().filter_map(|i| i["dst"].as_str().map(|s| s.to_string())).collect();
        a.tts.stop();
        for s in all {
            a.tts.say(&s);
        }
    } else {
        talk::system_line(&format!("翻译了 {} 段文字", items.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn l(t: &str, x: f32, y: f32) -> OcrLine {
        OcrLine { text: t.into(), x, y, w: 200.0, h: 20.0 }
    }
    #[test]
    fn groups_paragraphs() {
        let g = group_lines(&[l("Hello world,", 10.0, 10.0), l("this is a test.", 10.0, 34.0), l("New para", 10.0, 90.0)]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0, "Hello world, this is a test.");
        assert_eq!(g[1].0, "New para");
    }
}
