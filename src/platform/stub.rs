//! 非 Windows 系统的空实现，只为了能编译和调试界面。

use super::{Bitmap, OcrLine, ProcInfo, RectI, UiSnapshot, WinInfo};

pub fn my_pid() -> u32 {
    std::process::id()
}
pub fn list_windows() -> Vec<WinInfo> {
    vec![WinInfo { hwnd: 1, title: "示例窗口".into(), process: "demo".into(), x: 100, y: 100, w: 800, h: 600, minimized: false }]
}
pub fn window_info(hwnd: i64) -> Option<WinInfo> {
    list_windows().into_iter().find(|w| w.hwnd == hwnd)
}
pub fn is_window(hwnd: i64) -> bool {
    hwnd == 1
}
pub fn window_at_point(_x: i32, _y: i32) -> Option<WinInfo> {
    list_windows().into_iter().next()
}
pub fn foreground_window() -> Option<WinInfo> {
    list_windows().into_iter().next()
}
pub fn bring_to_front(_hwnd: i64) {}
pub fn set_no_activate(_hwnd: i64) {}
pub fn set_window_rect(_hwnd: i64, _x: i32, _y: i32, _w: i32, _h: i32) {}
pub fn exclude_from_capture(_hwnd: i64, _on: bool) {}
pub fn capture_visible(_hwnd: i64) -> Option<Bitmap> {
    None
}
pub fn cursor_pos() -> (i32, i32) {
    (0, 0)
}
pub fn left_button_down() -> bool {
    false
}
pub fn work_area_at(_x: i32, _y: i32) -> RectI {
    RectI { x: 0, y: 0, w: 1920, h: 1040 }
}
pub fn monitor_at(_x: i32, _y: i32) -> RectI {
    RectI { x: 0, y: 0, w: 1920, h: 1080 }
}
pub fn capture_window(_hwnd: i64) -> Option<(Bitmap, i32, i32)> {
    Some((Bitmap { w: 64, h: 64, bgra: vec![128; 64 * 64 * 4] }, 100, 100))
}
pub fn capture_screen_rect(_x: i32, _y: i32, w: i32, h: i32) -> Option<Bitmap> {
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Bitmap { w: w as u32, h: h as u32, bgra: vec![255; (w * h * 4) as usize] })
}
pub fn ui_snapshot(_hwnd: i64, _max: usize) -> UiSnapshot {
    UiSnapshot { texts: vec!["示例文字".into()], ..Default::default() }
}
pub fn ui_invoke(_hwnd: i64, _id: usize) -> bool {
    false
}
pub fn ui_rect(_hwnd: i64, _id: usize) -> Option<RectI> {
    None
}
pub fn ui_focus(_hwnd: i64, _id: usize) -> bool {
    false
}
pub fn ui_set_value(_hwnd: i64, _id: usize, _text: String) -> bool {
    false
}
pub fn ocr(_img: Bitmap) -> Result<Vec<OcrLine>, String> {
    Err("只有 Windows 支持文字识别".into())
}
pub fn ocr_languages() -> Vec<String> {
    vec![]
}
pub fn system_voices() -> Vec<String> {
    vec![]
}
pub fn system_tts(_text: String, _voice: String, _rate: f32) -> Result<Vec<u8>, String> {
    Err("只有 Windows 支持系统语音".into())
}
pub fn move_cursor(_x: i32, _y: i32) {}
pub fn click_at(_x: i32, _y: i32, _button: &str) {}
pub fn scroll_at(_x: i32, _y: i32, _n: i32) {}
pub fn type_text(_t: &str) {}
pub fn press_keys(_c: &str) -> bool {
    false
}

// ───────── 诊断（Linux 上从 /proc 读，方便调试日志） ─────────

pub fn process_list() -> Vec<ProcInfo> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
            // 格式：pid (名字) 状态 ppid ...
            let (Some(l), Some(r)) = (stat.find('('), stat.rfind(')')) else { continue };
            let name = stat[l + 1..r].to_string();
            let ppid = stat[r + 1..].split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
            v.push(ProcInfo { pid, ppid, name });
        }
    }
    v
}
/// (累计 CPU 时间，单位 100 纳秒；常驻内存字节)
pub fn process_usage(pid: u32) -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let r = stat.rfind(')')?;
    let f: Vec<&str> = stat[r + 1..].split_whitespace().collect();
    let ticks: u64 = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    let rss_pages: u64 = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((ticks * 100_000, rss_pages * 4096))
}
pub fn package_name() -> Option<String> {
    None
}
pub fn wait_process_exit(pid: u32, timeout: std::time::Duration) -> bool {
    let t = std::time::Instant::now();
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        if t.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    true
}
pub fn in_job() -> Option<bool> {
    None
}
pub fn os_version() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| format!("Linux {}", s.trim())).unwrap_or_else(|_| "Linux".into())
}
