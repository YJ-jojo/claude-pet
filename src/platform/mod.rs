//! 平台层：窗口枚举、截图、无障碍树、OCR、模拟输入、系统语音。
//! 只有 Windows 有真实实现，其它系统给出空实现，方便在别的机器上编译调试界面。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WinInfo {
    pub hwnd: i64,
    pub title: String,
    pub process: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub minimized: bool,
}

/// 进程表里的一项（诊断日志用：谁启动了宠物、网页进程占多少）
#[derive(Debug, Clone, Default)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
}

/// 无障碍树里的一个可操作元素
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Element {
    pub id: usize,
    /// 中文类型名：按钮、链接、输入框……
    pub kind: String,
    pub name: String,
    pub value: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub enabled: bool,
    pub focused: bool,
}

impl Element {
    #[allow(dead_code)]
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiSnapshot {
    /// 按从上到下、从左到右排好的可见文字
    pub texts: Vec<String>,
    /// 可操作元素
    pub elements: Vec<Element>,
    /// 当前有键盘焦点的元素
    pub focused: Option<String>,
    /// 生成这次快照花的毫秒数
    pub took_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OcrLine {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// BGRA 像素图
#[derive(Clone)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

impl Bitmap {
    /// 缩成 cols×rows 的灰度小图，用来判断画面有没有变化
    pub fn thumb(&self, cols: u32, rows: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((cols * rows) as usize);
        if self.w == 0 || self.h == 0 {
            return vec![0; (cols * rows) as usize];
        }
        for r in 0..rows {
            for c in 0..cols {
                // 每格取 4 个采样点平均，抗噪
                let mut sum = 0u32;
                for (dx, dy) in [(1u32, 1u32), (3, 1), (1, 3), (3, 3)] {
                    let x = ((c * 4 + dx) * self.w / (cols * 4)).min(self.w - 1);
                    let y = ((r * 4 + dy) * self.h / (rows * 4)).min(self.h - 1);
                    let i = ((y * self.w + x) * 4) as usize;
                    let (b, g, rr) = (self.bgra[i] as u32, self.bgra[i + 1] as u32, self.bgra[i + 2] as u32);
                    sum += (rr * 30 + g * 59 + b * 11) / 100;
                }
                out.push((sum / 4) as u8);
            }
        }
        out
    }

    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Bitmap {
        let x = x.min(self.w);
        let y = y.min(self.h);
        let w = w.min(self.w - x);
        let h = h.min(self.h - y);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let s = ((row * self.w + x) * 4) as usize;
            out.extend_from_slice(&self.bgra[s..s + (w * 4) as usize]);
        }
        Bitmap { w, h, bgra: out }
    }

    /// 编码成 JPEG（最长边不超过 max_side），返回字节
    pub fn to_jpeg(&self, max_side: u32, quality: u8) -> Vec<u8> {
        use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage, RgbImage};
        let mut rgb = Vec::with_capacity((self.w * self.h * 3) as usize);
        for px in self.bgra.chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        let Some(img) = RgbImage::from_raw(self.w, self.h, rgb) else { return vec![] };
        let mut img = DynamicImage::ImageRgb8(img);
        let longest = self.w.max(self.h);
        if longest > max_side {
            let scale = max_side as f32 / longest as f32;
            img = img.resize(
                ((self.w as f32 * scale) as u32).max(1),
                ((self.h as f32 * scale) as u32).max(1),
                FilterType::Triangle,
            );
        }
        let mut buf = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut buf, quality);
        if enc.encode_image(&img).is_err() {
            return vec![];
        }
        buf
    }

    /// 编码后图片的尺寸（按 to_jpeg 的缩放规则）
    pub fn scaled_size(&self, max_side: u32) -> (u32, u32) {
        let longest = self.w.max(self.h);
        if longest <= max_side {
            return (self.w, self.h);
        }
        let scale = max_side as f32 / longest as f32;
        (((self.w as f32 * scale) as u32).max(1), ((self.h as f32 * scale) as u32).max(1))
    }
}

/// 两张灰度小图的平均差（0-255）
#[allow(dead_code)]
pub fn thumb_diff(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 255.0;
    }
    let sum: u64 = a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum();
    sum as f32 / a.len() as f32
}

/// 有明显变化的格子占比（0-1），比平均差更能抓住“局部出现一行新字”这种变化
pub fn thumb_changed_ratio(a: &[u8], b: &[u8], cell_threshold: u8) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    let n = a.iter().zip(b).filter(|(x, y)| (**x as i32 - **y as i32).unsigned_abs() as u8 > cell_threshold).count();
    n as f32 / a.len() as f32
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct RectI {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub use stub::*;

#[cfg_attr(not(windows), allow(dead_code))]
/// 浏览器里开着的宠物对话框（标签页标题见 webui::PAGE_TITLE）：看屏幕、选窗口时当成宠物自己的窗口跳过
pub fn is_own_page(title: &str) -> bool {
    title.contains(crate::webui::PAGE_TITLE)
}
