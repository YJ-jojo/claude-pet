//! Windows 实现。所有 COM / WinRT 调用都放在专用工作线程里执行，避免跨线程使用 COM 对象。

use super::{Bitmap, Element, OcrLine, ProcInfo, RectI, UiSnapshot, WinInfo};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{BOOL, BSTR, HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, GetMonitorInfoW,
    MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// ───────────────────────── 工作线程 ─────────────────────────

struct Ctx {
    uia: Option<IUIAutomation>,
    /// 每个窗口最近一次快照里的元素，按 id 取
    elems: HashMap<i64, Vec<IUIAutomationElement>>,
    ocr: Option<windows::Media::Ocr::OcrEngine>,
}

type Job = Box<dyn FnOnce(&mut Ctx) + Send>;

struct Worker {
    tx: mpsc::Sender<Job>,
}

impl Worker {
    fn new(name: &str) -> Worker {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                let mut ctx = Ctx { uia: None, elems: HashMap::new(), ocr: None };
                while let Ok(job) = rx.recv() {
                    job(&mut ctx);
                }
            })
            .expect("spawn worker");
        Worker { tx }
    }

    fn run<R: Send + 'static>(&self, timeout: Duration, f: impl FnOnce(&mut Ctx) -> R + Send + 'static) -> Option<R> {
        let (rtx, rrx) = mpsc::channel();
        let job: Job = Box::new(move |ctx| {
            let _ = rtx.send(f(ctx));
        });
        self.tx.send(job).ok()?;
        rrx.recv_timeout(timeout).ok()
    }
}

fn uia_worker() -> &'static Worker {
    static W: OnceLock<Worker> = OnceLock::new();
    W.get_or_init(|| Worker::new("uia-worker"))
}

fn media_worker() -> &'static Worker {
    static W: OnceLock<Worker> = OnceLock::new();
    W.get_or_init(|| Worker::new("media-worker"))
}

fn hwnd_of(h: i64) -> HWND {
    HWND(h as isize as *mut core::ffi::c_void)
}

// ───────────────────────── 窗口 ─────────────────────────

fn window_title(h: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

fn window_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
    }
    pid
}

fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string()
    }
}

fn is_cloaked(h: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok() && cloaked != 0
    }
}

/// 窗口在屏幕上的可见范围（去掉 Win10/11 看不见的阴影边框）
fn visible_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).is_err() {
            let _ = GetWindowRect(h, &mut r);
        }
    }
    r
}

fn info_of(h: HWND) -> WinInfo {
    let r = visible_rect(h);
    WinInfo {
        hwnd: h.0 as isize as i64,
        title: window_title(h),
        process: process_name(window_pid(h)),
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
        minimized: unsafe { IsIconic(h).as_bool() },
    }
}

/// 是不是用户能看到的普通应用窗口
fn is_app_window(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() || is_cloaked(h) {
            return false;
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        if let Ok(owner) = GetWindow(h, GW_OWNER) {
            if !owner.is_invalid() && ex & WS_EX_APPWINDOW.0 == 0 {
                return false;
            }
        }
        GetWindowTextLengthW(h) > 0
    }
}

fn enum_top_windows() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let v = &mut *(lp.0 as *mut Vec<HWND>);
        v.push(h);
        BOOL(1)
    }
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut Vec<HWND> as isize));
    }
    v
}

pub fn my_pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

/// 列出可以观察的窗口（按 Z 序，最上面的在前），不含本程序自己的窗口
pub fn list_windows() -> Vec<WinInfo> {
    let me = my_pid();
    enum_top_windows()
        .into_iter()
        .filter(|h| is_app_window(*h) && window_pid(*h) != me)
        .map(info_of)
        .filter(|w| w.w > 40 && w.h > 40)
        .collect()
}

pub fn window_info(hwnd: i64) -> Option<WinInfo> {
    let h = hwnd_of(hwnd);
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return None;
        }
    }
    Some(info_of(h))
}

pub fn is_window(hwnd: i64) -> bool {
    unsafe { IsWindow(Some(hwnd_of(hwnd))).as_bool() }
}

/// 屏幕某点下面最上层的应用窗口（跳过本程序的窗口）
pub fn window_at_point(x: i32, y: i32) -> Option<WinInfo> {
    let me = my_pid();
    for h in enum_top_windows() {
        if !is_app_window(h) || window_pid(h) == me {
            continue;
        }
        let r = visible_rect(h);
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return Some(info_of(h));
        }
    }
    None
}

/// 当前前台窗口（如果是本程序自己的窗口则返回 None）
pub fn foreground_window() -> Option<WinInfo> {
    unsafe {
        let h = GetForegroundWindow();
        if h.is_invalid() {
            return None;
        }
        let root = GetAncestor(h, GA_ROOT);
        let h = if root.is_invalid() { h } else { root };
        if window_pid(h) == my_pid() {
            return None;
        }
        Some(info_of(h))
    }
}

pub fn bring_to_front(hwnd: i64) {
    let h = hwnd_of(hwnd);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

/// 让窗口点了也不抢焦点（宠物窗口用）
pub fn set_no_activate(hwnd: i64) {
    let h = hwnd_of(hwnd);
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
    }
}

/// 挪窗口（物理像素）。w、h 给 0 表示只挪位置不改大小。
pub fn set_window_rect(hwnd: i64, x: i32, y: i32, w: i32, h: i32) {
    let mut flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
    if w <= 0 || h <= 0 {
        flags |= SWP_NOSIZE;
    }
    unsafe {
        let _ = SetWindowPos(hwnd_of(hwnd), None, x, y, w.max(0), h.max(0), flags);
    }
}

/// 截图、录屏、共享屏幕时不拍进这个窗口（Windows 10 2004 及以上）
pub fn exclude_from_capture(hwnd: i64, on: bool) {
    unsafe {
        let _ = SetWindowDisplayAffinity(hwnd_of(hwnd), if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE });
    }
}

/// 窗口整个露在外面、没被别的窗口挡住时，直接从屏幕上拷这块（便宜，不打扰那个程序）；
/// 被挡住、最小化、在别的虚拟桌面上时返回 None。
pub fn capture_visible(hwnd: i64) -> Option<Bitmap> {
    let h = hwnd_of(hwnd);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() || !IsWindowVisible(h).as_bool() || is_cloaked(h) {
            return None;
        }
    }
    let r = visible_rect(h);
    let (w, hh) = (r.right - r.left, r.bottom - r.top);
    if w <= 8 || hh <= 8 {
        return None;
    }
    let me = my_pid();
    // 宠物没开防截图时会被拍进去：压在这个窗口上就不直接拷屏幕，让它自己画（不然宠物一动就当成画面变了）
    if own_window_in_capture(r) {
        return None;
    }
    // 抽 9 个点：每个点最上面的窗口都得是它自己（或者是本程序的窗口）
    for fy in [0.12f32, 0.5, 0.88] {
        for fx in [0.12f32, 0.5, 0.88] {
            let pt = POINT { x: r.left + (w as f32 * fx) as i32, y: r.top + (hh as f32 * fy) as i32 };
            unsafe {
                let top = WindowFromPoint(pt);
                if top.is_invalid() {
                    return None;
                }
                let root = GetAncestor(top, GA_ROOT);
                let root = if root.is_invalid() { top } else { root };
                if root != h && window_pid(root) != me {
                    return None;
                }
            }
        }
    }
    capture_screen_rect_plain(r.left, r.top, w, hh)
}

/// 本程序有没有会被截图拍进去的窗口压在 r 上（开着防截图的窗口不算）
fn own_window_in_capture(r: RECT) -> bool {
    let me = my_pid();
    enum_top_windows().into_iter().any(|h| unsafe {
        if window_pid(h) != me || !IsWindowVisible(h).as_bool() {
            return false;
        }
        let mut aff = 0u32;
        if GetWindowDisplayAffinity(h, &mut aff).is_ok() && aff == WDA_EXCLUDEFROMCAPTURE.0 {
            return false;
        }
        let mut wr = RECT::default();
        if GetWindowRect(h, &mut wr).is_err() {
            return false;
        }
        wr.left < r.right && wr.right > r.left && wr.top < r.bottom && wr.bottom > r.top
    })
}

/// 和 capture_screen_rect 一样，但不带 CAPTUREBLT（带它会让鼠标指针闪）
fn capture_screen_rect_plain(x: i32, y: i32, w: i32, h: i32) -> Option<Bitmap> {
    unsafe {
        let screen = GetDC(None);
        let memdc = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(memdc, bmp.into());
        let ok = BitBlt(memdc, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok();
        let img = if ok { read_dc_bitmap(memdc, bmp, w, h) } else { None };
        SelectObject(memdc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(memdc);
        ReleaseDC(None, screen);
        img
    }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

pub fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

/// 点所在显示器的工作区（不含任务栏）
pub fn work_area_at(x: i32, y: i32) -> RectI {
    unsafe {
        let m = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(m, &mut mi).as_bool() {
            let r = mi.rcWork;
            return RectI { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top };
        }
        RectI { x: 0, y: 0, w: GetSystemMetrics(SM_CXSCREEN), h: GetSystemMetrics(SM_CYSCREEN) }
    }
}

/// 点所在显示器的完整范围
pub fn monitor_at(x: i32, y: i32) -> RectI {
    unsafe {
        let m = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(m, &mut mi).as_bool() {
            let r = mi.rcMonitor;
            return RectI { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top };
        }
        RectI { x: 0, y: 0, w: GetSystemMetrics(SM_CXSCREEN), h: GetSystemMetrics(SM_CYSCREEN) }
    }
}

// ───────────────────────── 截图 ─────────────────────────

unsafe fn read_dc_bitmap(memdc: windows::Win32::Graphics::Gdi::HDC, bmp: windows::Win32::Graphics::Gdi::HBITMAP, w: i32, h: i32) -> Option<Bitmap> {
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // 负数表示从上到下
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let lines = GetDIBits(memdc, bmp, 0, h as u32, Some(buf.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
    if lines == 0 {
        return None;
    }
    for px in buf.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Some(Bitmap { w: w as u32, h: h as u32, bgra: buf })
}

/// 截一个窗口（被遮挡也能截，最小化的不行）。返回图片和图片左上角对应的屏幕坐标。
pub fn capture_window(hwnd: i64) -> Option<(Bitmap, i32, i32)> {
    let h = hwnd_of(hwnd);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
        let mut wr = RECT::default();
        GetWindowRect(h, &mut wr).ok()?;
        let (w, hh) = (wr.right - wr.left, wr.bottom - wr.top);
        if w <= 0 || hh <= 0 || w > 16384 || hh > 16384 {
            return None;
        }
        let screen = GetDC(None);
        let memdc = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, hh);
        let old = SelectObject(memdc, bmp.into());
        // 2 = PW_RENDERFULLCONTENT，能截到 Chrome / Electron / UWP 这类硬件加速的窗口
        let ok = PrintWindow(h, memdc, PRINT_WINDOW_FLAGS(2)).as_bool();
        let img = if ok { read_dc_bitmap(memdc, bmp, w, hh) } else { None };
        SelectObject(memdc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(memdc);
        ReleaseDC(None, screen);

        let img = match img {
            Some(i) if !is_blank(&i) => Some(i),
            _ => capture_screen_rect(wr.left, wr.top, w, hh),
        }?;
        // 裁掉看不见的阴影边框，让图片和窗口可见范围对齐
        let vr = visible_rect(h);
        let (cx, cy) = ((vr.left - wr.left).max(0) as u32, (vr.top - wr.top).max(0) as u32);
        let (cw, ch) = ((vr.right - vr.left).max(1) as u32, (vr.bottom - vr.top).max(1) as u32);
        if cx + cw <= img.w && cy + ch <= img.h && (cx > 0 || cy > 0 || cw < img.w || ch < img.h) {
            return Some((img.crop(cx, cy, cw, ch), vr.left, vr.top));
        }
        Some((img, wr.left, wr.top))
    }
}

fn is_blank(img: &Bitmap) -> bool {
    // 抽样看是不是全黑
    let step = ((img.bgra.len() / 4) / 997).max(1) * 4;
    !img.bgra.iter().step_by(step).any(|v| *v > 8)
}

/// 截屏幕上的一块区域（物理像素坐标）
pub fn capture_screen_rect(x: i32, y: i32, w: i32, h: i32) -> Option<Bitmap> {
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let memdc = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(memdc, bmp.into());
        let ok = BitBlt(memdc, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let img = if ok { read_dc_bitmap(memdc, bmp, w, h) } else { None };
        SelectObject(memdc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(memdc);
        ReleaseDC(None, screen);
        img
    }
}

// ───────────────────────── 无障碍树 ─────────────────────────

fn kind_name(ct: i32) -> Option<&'static str> {
    Some(match ct {
        50000 => "按钮",
        50002 => "勾选框",
        50003 => "下拉框",
        50004 => "输入框",
        50005 => "链接",
        50007 => "列表项",
        50011 => "菜单项",
        50013 => "单选项",
        50015 => "滑块",
        50019 => "标签页",
        50024 => "树节点",
        50029 => "数据项",
        50031 => "按钮",
        _ => return None,
    })
}

fn ensure_uia(ctx: &mut Ctx) -> Option<IUIAutomation> {
    if ctx.uia.is_none() {
        ctx.uia = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok() };
    }
    ctx.uia.clone()
}

fn bstr_prop(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> String {
    unsafe {
        el.GetCachedPropertyValue(id)
            .ok()
            .and_then(|v| BSTR::try_from(&v).ok())
            .map(|b| b.to_string())
            .unwrap_or_default()
    }
}

fn bool_prop(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> bool {
    unsafe { el.GetCachedPropertyValue(id).ok().and_then(|v| bool::try_from(&v).ok()).unwrap_or(false) }
}

fn clean_text(s: &str, max: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    } else {
        s
    }
}

fn snapshot_inner(ctx: &mut Ctx, hwnd: i64, max_elems: usize) -> UiSnapshot {
    let t0 = Instant::now();
    let mut snap = UiSnapshot::default();
    let Some(uia) = ensure_uia(ctx) else { return snap };
    unsafe {
        let Ok(root) = uia.ElementFromHandle(hwnd_of(hwnd)) else { return snap };
        let Ok(cache) = uia.CreateCacheRequest() else { return snap };
        for p in [
            UIA_NamePropertyId,
            UIA_ControlTypePropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsEnabledPropertyId,
            UIA_HasKeyboardFocusPropertyId,
            UIA_ValueValuePropertyId,
            UIA_IsPasswordPropertyId,
        ] {
            let _ = cache.AddProperty(p);
        }
        let cond = match uia.CreatePropertyCondition(UIA_IsOffscreenPropertyId, &windows::Win32::System::Variant::VARIANT::from(false)) {
            Ok(c) => c,
            Err(_) => match uia.CreateTrueCondition() {
                Ok(c) => c,
                Err(_) => return snap,
            },
        };
        let Ok(arr) = root.FindAllBuildCache(TreeScope_Descendants, &cond, &cache) else { return snap };
        let n = arr.Length().unwrap_or(0).min(4000);

        struct Item {
            y: i32,
            x: i32,
            text: String,
        }
        let mut texts: Vec<Item> = Vec::new();
        let mut kept: Vec<IUIAutomationElement> = Vec::new();
        for i in 0..n {
            let Ok(el) = arr.GetElement(i) else { continue };
            let ct = el.CachedControlType().map(|c| c.0).unwrap_or(0);
            let Ok(r) = el.CachedBoundingRectangle() else { continue };
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if w <= 1 || h <= 1 {
                continue;
            }
            let name = clean_text(&bstr_prop(&el, UIA_NamePropertyId), 300);
            let password = bool_prop(&el, UIA_IsPasswordPropertyId);
            let value = if password { String::new() } else { clean_text(&bstr_prop(&el, UIA_ValueValuePropertyId), 300) };
            let focused = bool_prop(&el, UIA_HasKeyboardFocusPropertyId);

            match ct {
                // 文字、标题、文档、编辑框内容
                50020 | 50034 | 50035 | 50037 => {
                    if !name.is_empty() {
                        texts.push(Item { y: r.top, x: r.left, text: name.clone() });
                    }
                }
                _ => {}
            }
            if focused {
                let label = if !name.is_empty() { name.clone() } else { value.clone() };
                snap.focused = Some(format!("{}{}", kind_name(ct).unwrap_or("元素"), if label.is_empty() { String::new() } else { format!("「{label}」") }));
            }
            if let Some(kind) = kind_name(ct) {
                if kept.len() < max_elems {
                    if ct == 50004 && !value.is_empty() {
                        texts.push(Item { y: r.top, x: r.left, text: format!("[输入框] {value}") });
                    }
                    if ct == 50005 || ct == 50007 || ct == 50024 || ct == 50029 {
                        if !name.is_empty() {
                            texts.push(Item { y: r.top, x: r.left, text: name.clone() });
                        }
                    }
                    let enabled = bool_prop(&el, UIA_IsEnabledPropertyId);
                    snap.elements.push(Element {
                        id: kept.len(),
                        kind: kind.into(),
                        name,
                        value,
                        x: r.left,
                        y: r.top,
                        w,
                        h,
                        enabled,
                        focused,
                    });
                    kept.push(el);
                }
            }
        }
        // 从上到下、从左到右；去掉完全重复的相邻文字
        texts.sort_by_key(|t| (t.y / 6, t.x));
        let mut last = String::new();
        for t in texts {
            if t.text != last {
                last = t.text.clone();
                snap.texts.push(t.text);
            }
            if snap.texts.len() >= 800 {
                break;
            }
        }
        ctx.elems.insert(hwnd, kept);
    }
    snap.took_ms = t0.elapsed().as_millis() as u64;
    snap
}

/// 读窗口的无障碍树：可见文字 + 可操作元素
pub fn ui_snapshot(hwnd: i64, max_elems: usize) -> UiSnapshot {
    uia_worker()
        .run(Duration::from_secs(6), move |ctx| snapshot_inner(ctx, hwnd, max_elems))
        .unwrap_or_default()
}

/// 用无障碍接口直接“按”一个元素（快照里的 id）。成功返回 true。
#[allow(dead_code)]
pub fn ui_invoke(hwnd: i64, id: usize) -> bool {
    uia_worker()
        .run(Duration::from_secs(3), move |ctx| unsafe {
            let Some(el) = ctx.elems.get(&hwnd).and_then(|v| v.get(id)).cloned() else { return false };
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
                if p.Invoke().is_ok() {
                    return true;
                }
            }
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
                if p.Toggle().is_ok() {
                    return true;
                }
            }
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) {
                if p.Select().is_ok() {
                    return true;
                }
            }
            false
        })
        .unwrap_or(false)
}

/// 元素现在的位置（滚动后会变）
pub fn ui_rect(hwnd: i64, id: usize) -> Option<RectI> {
    uia_worker()
        .run(Duration::from_secs(2), move |ctx| unsafe {
            let el = ctx.elems.get(&hwnd)?.get(id)?.clone();
            let r = el.CurrentBoundingRectangle().ok()?;
            Some(RectI { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
        })
        .flatten()
}

#[allow(dead_code)]
pub fn ui_focus(hwnd: i64, id: usize) -> bool {
    uia_worker()
        .run(Duration::from_secs(2), move |ctx| unsafe {
            let Some(el) = ctx.elems.get(&hwnd).and_then(|v| v.get(id)).cloned() else { return false };
            el.SetFocus().is_ok()
        })
        .unwrap_or(false)
}

#[allow(dead_code)]
pub fn ui_set_value(hwnd: i64, id: usize, text: String) -> bool {
    uia_worker()
        .run(Duration::from_secs(3), move |ctx| unsafe {
            let Some(el) = ctx.elems.get(&hwnd).and_then(|v| v.get(id)).cloned() else { return false };
            match el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
                Ok(p) => p.SetValue(&BSTR::from(text.as_str())).is_ok(),
                Err(_) => false,
            }
        })
        .unwrap_or(false)
}

// ───────────────────────── 文字识别 ─────────────────────────

fn ocr_inner(ctx: &mut Ctx, img: Bitmap) -> Result<Vec<OcrLine>, String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    if ctx.ocr.is_none() {
        ctx.ocr = Some(OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| format!("系统没有可用的文字识别语言包：{e}"))?);
    }
    let engine = ctx.ocr.clone().unwrap();
    // 太大的图先缩小
    let max = OcrEngine::MaxImageDimension().unwrap_or(4096).max(1024);
    let img = if img.w > max || img.h > max {
        let scale = max as f32 / img.w.max(img.h) as f32;
        resize_bitmap(&img, ((img.w as f32 * scale) as u32).max(1), ((img.h as f32 * scale) as u32).max(1))
    } else if img.h < 40 || img.w < 40 {
        // 太小的图放大两倍，识别更准
        resize_bitmap(&img, img.w * 2, img.h * 2)
    } else {
        img
    };
    let scale_back = 1.0f32;
    let writer = DataWriter::new().map_err(|e| e.to_string())?;
    writer.WriteBytes(&img.bgra).map_err(|e| e.to_string())?;
    let buffer = writer.DetachBuffer().map_err(|e| e.to_string())?;
    let sb = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, img.w as i32, img.h as i32)
        .map_err(|e| e.to_string())?;
    let result = engine.RecognizeAsync(&sb).map_err(|e| e.to_string())?.join().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let lines = result.Lines().map_err(|e| e.to_string())?;
    for i in 0..lines.Size().unwrap_or(0) {
        let Ok(line) = lines.GetAt(i) else { continue };
        let text = line.Text().map(|t| t.to_string()).unwrap_or_default();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, 0f32, 0f32);
        if let Ok(words) = line.Words() {
            for j in 0..words.Size().unwrap_or(0) {
                if let Ok(r) = words.GetAt(j).and_then(|w| w.BoundingRect()) {
                    x0 = x0.min(r.X);
                    y0 = y0.min(r.Y);
                    x1 = x1.max(r.X + r.Width);
                    y1 = y1.max(r.Y + r.Height);
                }
            }
        }
        if x0 == f32::MAX {
            x0 = 0.0;
            y0 = 0.0;
        }
        out.push(OcrLine { text: join_cjk(&text), x: x0 * scale_back, y: y0 * scale_back, w: (x1 - x0).max(0.0), h: (y1 - y0).max(0.0) });
    }
    Ok(out)
}

/// Windows OCR 识别中文时会在每个字之间加空格，这里把汉字之间的空格去掉
fn join_cjk(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let is_cjk = |c: char| ('\u{3000}'..='\u{9fff}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c);
    let mut out = String::with_capacity(s.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == ' ' && i > 0 && i + 1 < chars.len() && (is_cjk(chars[i - 1]) || is_cjk(chars[i + 1])) {
            continue;
        }
        out.push(*c);
    }
    out
}

fn resize_bitmap(img: &Bitmap, w: u32, h: u32) -> Bitmap {
    use image::{imageops::FilterType, RgbaImage};
    let Some(src) = RgbaImage::from_raw(img.w, img.h, img.bgra.clone()) else { return img.clone() };
    let dst = image::imageops::resize(&src, w, h, FilterType::Triangle);
    Bitmap { w, h, bgra: dst.into_raw() }
}

/// 识别图片里的文字（坐标相对图片）
pub fn ocr(img: Bitmap) -> Result<Vec<OcrLine>, String> {
    media_worker()
        .run(Duration::from_secs(10), move |ctx| ocr_inner(ctx, img))
        .unwrap_or_else(|| Err("文字识别超时".into()))
}

pub fn ocr_languages() -> Vec<String> {
    media_worker()
        .run(Duration::from_secs(5), |_ctx| {
            let mut v = Vec::new();
            if let Ok(list) = windows::Media::Ocr::OcrEngine::AvailableRecognizerLanguages() {
                for i in 0..list.Size().unwrap_or(0) {
                    if let Ok(l) = list.GetAt(i) {
                        v.push(l.LanguageTag().map(|t| t.to_string()).unwrap_or_default());
                    }
                }
            }
            v
        })
        .unwrap_or_default()
}

// ───────────────────────── 系统语音 ─────────────────────────

pub fn system_voices() -> Vec<String> {
    media_worker()
        .run(Duration::from_secs(5), |_ctx| {
            let mut v = Vec::new();
            if let Ok(list) = windows::Media::SpeechSynthesis::SpeechSynthesizer::AllVoices() {
                for i in 0..list.Size().unwrap_or(0) {
                    if let Ok(voice) = list.GetAt(i) {
                        let name = voice.DisplayName().map(|s| s.to_string()).unwrap_or_default();
                        let lang = voice.Language().map(|s| s.to_string()).unwrap_or_default();
                        v.push(format!("{name} ({lang})"));
                    }
                }
            }
            v
        })
        .unwrap_or_default()
}

/// 用 Windows 自带语音合成，返回 WAV 字节
pub fn system_tts(text: String, voice: String, rate: f32) -> Result<Vec<u8>, String> {
    media_worker()
        .run(Duration::from_secs(20), move |_ctx| -> Result<Vec<u8>, String> {
            use windows::Media::SpeechSynthesis::SpeechSynthesizer;
            use windows::Storage::Streams::DataReader;
            let synth = SpeechSynthesizer::new().map_err(|e| e.to_string())?;
            if let Ok(list) = SpeechSynthesizer::AllVoices() {
                let want = voice.split(" (").next().unwrap_or("").trim().to_string();
                let mut chosen = None;
                for i in 0..list.Size().unwrap_or(0) {
                    if let Ok(v) = list.GetAt(i) {
                        let name = v.DisplayName().map(|s| s.to_string()).unwrap_or_default();
                        let lang = v.Language().map(|s| s.to_string()).unwrap_or_default();
                        if !want.is_empty() && name == want {
                            chosen = Some(v);
                            break;
                        }
                        if want.is_empty() && chosen.is_none() && lang.to_lowercase().starts_with("zh") {
                            chosen = Some(v);
                        }
                    }
                }
                if let Some(v) = chosen {
                    let _ = synth.SetVoice(&v);
                }
            }
            if let Ok(opts) = synth.Options() {
                let _ = opts.SetSpeakingRate((rate as f64).clamp(0.5, 3.0));
            }
            let stream = synth
                .SynthesizeTextToStreamAsync(&HSTRING::from(text.as_str()))
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|e| e.to_string())?;
            let size = stream.Size().map_err(|e| e.to_string())? as u32;
            let reader = DataReader::CreateDataReader(&stream).map_err(|e| e.to_string())?;
            reader.LoadAsync(size).map_err(|e| e.to_string())?.join().map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; size as usize];
            reader.ReadBytes(&mut buf).map_err(|e| e.to_string())?;
            Ok(buf)
        })
        .unwrap_or_else(|| Err("系统语音超时".into()))
}

// ───────────────────────── 模拟输入 ─────────────────────────

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

pub fn move_cursor(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// 在屏幕坐标点击。button: "left" / "right" / "double"
pub fn click_at(x: i32, y: i32, button: &str) {
    move_cursor(x, y);
    std::thread::sleep(Duration::from_millis(30));
    match button {
        "right" => send(&[mouse(MOUSEEVENTF_RIGHTDOWN, 0), mouse(MOUSEEVENTF_RIGHTUP, 0)]),
        "double" => {
            send(&[mouse(MOUSEEVENTF_LEFTDOWN, 0), mouse(MOUSEEVENTF_LEFTUP, 0)]);
            std::thread::sleep(Duration::from_millis(60));
            send(&[mouse(MOUSEEVENTF_LEFTDOWN, 0), mouse(MOUSEEVENTF_LEFTUP, 0)]);
        }
        _ => send(&[mouse(MOUSEEVENTF_LEFTDOWN, 0), mouse(MOUSEEVENTF_LEFTUP, 0)]),
    }
}

pub fn scroll_at(x: i32, y: i32, notches: i32) {
    move_cursor(x, y);
    std::thread::sleep(Duration::from_millis(20));
    send(&[mouse(MOUSEEVENTF_WHEEL, notches * 120)]);
}

/// 打字（支持中文，直接发 Unicode 字符）
pub fn type_text(text: &str) {
    for ch in text.chars() {
        if ch == '\n' {
            send(&[key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)), key(VK_RETURN, 0, KEYEVENTF_KEYUP)]);
        } else {
            let mut units = [0u16; 2];
            for u in ch.encode_utf16(&mut units).iter() {
                send(&[key(VIRTUAL_KEY(0), *u, KEYEVENTF_UNICODE), key(VIRTUAL_KEY(0), *u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)]);
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn vk_of(name: &str) -> Option<VIRTUAL_KEY> {
    let n = name.trim().to_lowercase();
    Some(match n.as_str() {
        "ctrl" | "control" => VK_CONTROL,
        "alt" => VK_MENU,
        "shift" => VK_SHIFT,
        "win" | "meta" | "super" | "cmd" => VK_LWIN,
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "esc" | "escape" => VK_ESCAPE,
        "space" => VK_SPACE,
        "backspace" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        _ => {
            if let Some(num) = n.strip_prefix('f').and_then(|s| s.parse::<u16>().ok()) {
                if (1..=24).contains(&num) {
                    return Some(VIRTUAL_KEY(VK_F1.0 + num - 1));
                }
            }
            let mut cs = n.chars();
            let c = cs.next()?;
            if cs.next().is_some() {
                return None;
            }
            if c.is_ascii_alphanumeric() {
                return Some(VIRTUAL_KEY(c.to_ascii_uppercase() as u16));
            }
            let r = unsafe { VkKeyScanW(c as u16) };
            if r == -1 {
                return None;
            }
            VIRTUAL_KEY((r & 0xff) as u16)
        }
    })
}

/// 按组合键，比如 "ctrl+s"、"alt+f4"、"enter"
pub fn press_keys(combo: &str) -> bool {
    let parts: Vec<&str> = combo.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let mut vks = Vec::new();
    for p in &parts {
        match vk_of(p) {
            Some(v) => vks.push(v),
            None => return false,
        }
    }
    if vks.is_empty() {
        return false;
    }
    let mut inputs = Vec::new();
    for v in &vks {
        inputs.push(key(*v, 0, KEYBD_EVENT_FLAGS(0)));
    }
    for v in vks.iter().rev() {
        inputs.push(key(*v, 0, KEYEVENTF_KEYUP));
    }
    send(&inputs);
    true
}

// ───────────────────────── 诊断 ─────────────────────────

/// 所有进程（pid、父 pid、程序名）
pub fn process_list() -> Vec<ProcInfo> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut v = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return v };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            v.push(ProcInfo { pid: e.th32ProcessID, ppid: e.th32ParentProcessID, name: String::from_utf16_lossy(&e.szExeFile[..n]) });
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    v
}

/// (累计 CPU 时间，单位 100 纳秒；工作集内存字节)
pub fn process_usage(pid: u32) -> Option<(u64, u64)> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{GetProcessTimes, PROCESS_VM_READ};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid)
            .or_else(|_| OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid))
            .ok()?;
        let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let times = GetProcessTimes(h, &mut c, &mut x, &mut k, &mut u).ok();
        let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        let mut m = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
        let mem = if K32GetProcessMemoryInfo(h, &mut m, m.cb).as_bool() { m.WorkingSetSize as u64 } else { 0 };
        let _ = CloseHandle(h);
        times.map(|_| (ft(k) + ft(u), mem))
    }
}

/// 宠物是不是跑在某个 MSIX 包（比如商店版 Claude 桌面版）的容器里：是的话返回包名
pub fn package_name() -> Option<String> {
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    unsafe {
        let mut len = 0u32;
        if GetCurrentPackageFullName(&mut len, None) != ERROR_INSUFFICIENT_BUFFER || len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if GetCurrentPackageFullName(&mut len, Some(PWSTR(buf.as_mut_ptr()))).0 != 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

/// 等某个进程退出，最多等 timeout；进程已经不在了也算退出
pub fn wait_process_exit(pid: u32, timeout: std::time::Duration) -> bool {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Threading::{WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return true };
        let r = WaitForSingleObject(h, timeout.as_millis() as u32);
        let _ = CloseHandle(h);
        r == WAIT_OBJECT_0
    }
}

/// 是否在某个作业对象里（被别的程序“管着”，那个程序退出时可能把宠物一起关掉）
pub fn in_job() -> Option<bool> {
    use windows::Win32::System::JobObjects::IsProcessInJob;
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut r = BOOL(0);
        IsProcessInJob(GetCurrentProcess(), None, &mut r).ok()?;
        Some(r.as_bool())
    }
}

pub fn os_version() -> String {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let get = |name: windows::core::PCWSTR| -> String {
        unsafe {
            let mut buf = vec![0u16; 128];
            let mut len = (buf.len() * 2) as u32;
            let r = RegGetValueW(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"), name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len));
            if r.0 != 0 {
                return String::new();
            }
            let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..n])
        }
    };
    let build = get(w!("CurrentBuild"));
    let disp = get(w!("DisplayVersion"));
    let name = if build.parse::<u32>().unwrap_or(0) >= 22000 { "Windows 11" } else { "Windows 10" };
    format!("{name} {disp}（内部版本 {build}）")
}
