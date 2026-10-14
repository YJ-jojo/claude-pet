//! 把窗口画面变成给 Claude 读的结构化文字

use crate::platform::{OcrLine, UiSnapshot, WinInfo};
use serde_json::{json, Value};
use std::collections::HashMap;

/// 一个窗口当前“看到”的文字
#[derive(Clone, Default)]
pub struct Seen {
    pub lines: Vec<String>,
    pub focused: Option<String>,
    pub from_ocr: bool,
}

impl Seen {
    pub fn from_ui(s: &UiSnapshot) -> Seen {
        Seen { lines: s.texts.clone(), focused: s.focused.clone(), from_ocr: false }
    }
    pub fn from_ocr(lines: &[OcrLine]) -> Seen {
        Seen { lines: lines.iter().map(|l| l.text.trim().to_string()).filter(|t| !t.is_empty()).collect(), focused: None, from_ocr: true }
    }
    pub fn char_count(&self) -> usize {
        self.lines.iter().map(|l| l.chars().count()).sum()
    }
}

/// 按出现次数做差：新出现的行、消失的行（保持原顺序）
pub fn diff(prev: &[String], cur: &[String]) -> (Vec<String>, Vec<String>) {
    let mut count: HashMap<&str, i32> = HashMap::new();
    for l in prev {
        *count.entry(l.as_str()).or_default() += 1;
    }
    let mut added = Vec::new();
    for l in cur {
        let c = count.entry(l.as_str()).or_default();
        if *c > 0 {
            *c -= 1;
        } else {
            added.push(l.clone());
        }
    }
    let mut count2: HashMap<&str, i32> = HashMap::new();
    for l in cur {
        *count2.entry(l.as_str()).or_default() += 1;
    }
    let mut removed = Vec::new();
    for l in prev {
        let c = count2.entry(l.as_str()).or_default();
        if *c > 0 {
            *c -= 1;
        } else {
            removed.push(l.clone());
        }
    }
    // 流式输出时一行会不断变长：把“旧行是新行前缀”的情况合并成一条新增
    removed.retain(|r| !added.iter().any(|a| a.starts_with(r.as_str()) && a.len() > r.len()));
    (added, removed)
}

const ALERT_WORDS: &[&str] = &[
    "错误", "失败", "异常", "警告", "确认", "允许", "拒绝", "是否", "授权", "权限", "无法", "超时", "崩溃", "请输入", "需要你",
    "error", "failed", "failure", "exception", "traceback", "warning", "denied", "allow", "permission", "confirm", "are you sure",
    "do you want", "timeout", "panic", "fatal",
];

/// 文字里有没有报错 / 需要确认之类的字眼
pub fn has_alert(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        let low = l.to_lowercase();
        ALERT_WORDS.iter().any(|w| low.contains(w))
    })
}

fn clip_lines(lines: &[String], max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut total = 0;
    for l in lines {
        let n = l.chars().count();
        if total + n > max_chars {
            let left = max_chars.saturating_sub(total);
            if left > 20 {
                out.push(format!("{}…", l.chars().take(left).collect::<String>()));
            }
            out.push("…（后面还有，省略）".into());
            break;
        }
        total += n;
        out.push(l.clone());
    }
    out
}

/// 一次完整快照（开始观察时、看一眼时用）
pub fn full(win: &WinInfo, seen: &Seen, max_chars: usize) -> Value {
    json!({
        "window": format!("{}（{}）", win.title, win.process),
        "texts": clip_lines(&seen.lines, max_chars),
        "focus": seen.focused,
        "ocr": seen.from_ocr,
    })
}

/// 变化快照（呼吸模式用）
pub fn change(win: &WinInfo, added: &[String], removed: &[String], seen: &Seen, max_chars: usize) -> Value {
    let mut v = json!({
        "time": chrono::Local::now().format("%H:%M:%S").to_string(),
        "window": format!("{}（{}）", win.title, win.process),
        "added": clip_lines(added, max_chars),
    });
    if !removed.is_empty() {
        v["removed"] = json!(clip_lines(removed, max_chars / 4));
    }
    if seen.focused.is_some() {
        v["focus"] = json!(seen.focused);
    }
    if has_alert(added) {
        v["dialog"] = json!(true);
    }
    if seen.from_ocr {
        v["ocr"] = json!(true);
    }
    v
}

/// 给“帮你操作”用的元素清单
pub fn element_list(s: &UiSnapshot, max: usize) -> String {
    let mut out = String::new();
    for e in s.elements.iter().take(max) {
        let mut label = e.name.clone();
        if label.is_empty() {
            label = e.value.clone();
        }
        if label.is_empty() && e.kind != "输入框" {
            continue;
        }
        out.push_str(&format!(
            "#{} {}「{}」{}{} @({},{},{}x{})\n",
            e.id,
            e.kind,
            label.chars().take(60).collect::<String>(),
            if e.focused { " [焦点]" } else { "" },
            if !e.enabled { " [不可用]" } else { "" },
            e.x,
            e.y,
            e.w,
            e.h
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn diff_counts_and_growth() {
        let (a, r) = diff(&v(&["标题", "你好", "正在生成"]), &v(&["标题", "你好", "正在生成回答", "完成"]));
        assert_eq!(a, v(&["正在生成回答", "完成"]));
        assert!(r.is_empty());
        let (a, r) = diff(&v(&["x", "x", "y"]), &v(&["x", "y"]));
        assert!(a.is_empty());
        assert_eq!(r, v(&["x"]));
    }
    #[test]
    fn alerts() {
        assert!(has_alert(&v(&["Do you want to allow this?"])));
        assert!(has_alert(&v(&["编译失败"])));
        assert!(!has_alert(&v(&["今天天气不错"])));
    }
}
