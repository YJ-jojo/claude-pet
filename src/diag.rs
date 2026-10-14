//! 诊断日志：启动每一步花了多久、谁把宠物启动的、崩溃在哪，以及运行时每 30 秒一行占用情况。
//! 全写进 pet.log，用户把日志发过来就能看出卡在哪、为什么打不开。

use crate::platform;
use crate::plog;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();

/// 窗口移动、穿透切换、页面调后端的次数（每 30 秒清零一次）
pub static WIN_MOVES: AtomicU64 = AtomicU64::new(0);
pub static HIT_TOGGLES: AtomicU64 = AtomicU64::new(0);
/// 最慢的一次穿透切换（微秒），它要等主线程
pub static HIT_TOGGLE_MAX_US: AtomicU64 = AtomicU64::new(0);

pub fn start_clock() {
    let _ = START.set(Instant::now());
}

pub fn since_start_ms() -> u128 {
    START.get().map(|t| t.elapsed().as_millis()).unwrap_or(0)
}

/// 启动过程中的一个节点：“启动 +1234ms 窗口建好”
pub fn mark(what: &str) {
    plog!("启动 +{}ms {what}", since_start_ms());
}

/// 崩溃时把原因和位置写进日志（不然窗口一闪就没了，什么都看不到）
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("未命名").to_string();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "（没有说明）".into());
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        plog!("!!! 崩溃：线程 {thread}，{msg}，位置 {at}");
        prev(info);
    }));
}

fn proc_name(list: &[platform::ProcInfo], pid: u32) -> String {
    list.iter().find(|p| p.pid == pid).map(|p| p.name.clone()).unwrap_or_else(|| "（已退出）".into())
}

/// 启动时记一遍环境：谁启动的、是否在别的程序的容器里、系统版本等
pub fn log_environment(role: &str) {
    let me = std::process::id();
    let list = platform::process_list();
    let parent = list.iter().find(|p| p.pid == me).map(|p| p.ppid).unwrap_or(0);
    let grand = list.iter().find(|p| p.pid == parent).map(|p| p.ppid).unwrap_or(0);
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    plog!(
        "===== Claude 宠物 {} {role} 启动 =====  程序 {exe}  参数 {:?}",
        env!("CARGO_PKG_VERSION"),
        args
    );
    plog!(
        "由 {}({parent}) 启动，它的上级是 {}({grand})；系统 {}；CPU {} 核",
        proc_name(&list, parent),
        proc_name(&list, grand),
        platform::os_version(),
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
    );
    match platform::package_name() {
        Some(p) => plog!("注意：宠物跑在应用包 {p} 的容器里（被商店版程序启动时会这样），文件读写可能被重定向"),
        None => {}
    }
    if platform::in_job() == Some(true) {
        plog!("注意：宠物在一个作业对象里，启动它的程序退出时可能把宠物一起关掉");
    }
    if let Some(p) = crate::logger::log_path() {
        plog!("日志文件 {}", p.display());
    }
}

static STARTUP_DONE: AtomicBool = AtomicBool::new(false);

/// 宠物页面加载好了
pub fn startup_done() {
    if !STARTUP_DONE.swap(true, Ordering::SeqCst) {
        mark("启动完成");
    }
}

/// 启动没完成时每 5 秒记一行卡在哪（0.2.0 有两次一直卡在建窗口，日志里什么都没有）。最多记 3 分钟。
pub fn spawn_startup_watch() {
    std::thread::Builder::new()
        .name("startup-watch".into())
        .spawn(|| {
            let me = std::process::id();
            let cores = std::thread::available_parallelism().map(|n| n.get() as f64).unwrap_or(1.0);
            let mut last_cpu: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
            let mut last_t = Instant::now();
            for _ in 0..36 {
                std::thread::sleep(Duration::from_secs(5));
                let shown = crate::stage::first_shown();
                if shown && STARTUP_DONE.load(Ordering::SeqCst) {
                    return;
                }
                let list = platform::process_list();
                let tree = tree_of(&list, me);
                let wall = last_t.elapsed().as_secs_f64().max(0.1);
                last_t = Instant::now();
                let (mut n, mut cpu) = (0, 0f64);
                let mut now_cpu = std::collections::HashMap::new();
                for pid in tree.iter().filter(|p| **p != me) {
                    n += 1;
                    if let Some((c, _)) = platform::process_usage(*pid) {
                        if let Some(prev) = last_cpu.get(pid) {
                            cpu += c.saturating_sub(*prev) as f64 / 1e7 / wall / cores * 100.0;
                        }
                        now_cpu.insert(*pid, c);
                    }
                }
                last_cpu = now_cpu;
                // 上级进程已经不在了的网页进程：多半是以前没关干净的宠物留下的
                let orphans = list
                    .iter()
                    .filter(|p| p.name.eq_ignore_ascii_case("msedgewebview2.exe") && !tree.contains(&p.pid) && !list.iter().any(|q| q.pid == p.ppid))
                    .count();
                let built: Vec<&str> = match crate::state::try_app() {
                    Some(a) => {
                        use tauri::Manager;
                        ["stage"].into_iter().filter(|l| a.handle.get_webview_window(l).is_some()).collect()
                    }
                    None => vec![],
                };
                plog!(
                    "启动 +{}s 还没完成：宠物{}；建好的窗口 {:?}；加载好的页面 {:?}；网页进程 {n} 个 CPU {cpu:.1}%；系统里没有上级的网页进程 {orphans} 个",
                    since_start_ms() / 1000,
                    if shown { "已显示" } else { "还没显示" },
                    built,
                    crate::commands::pages_ready()
                );
            }
        })
        .ok();
}

/// 我们自己和所有子孙进程（WebView2 的 msedgewebview2.exe 都挂在宠物下面）
fn tree_of(list: &[platform::ProcInfo], root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let p = out[i];
        for c in list.iter().filter(|c| c.ppid == p && c.pid != p) {
            if !out.contains(&c.pid) {
                out.push(c.pid);
            }
        }
        i += 1;
    }
    out
}

/// 后台每 30 秒记一行：宠物本体和网页进程各占多少 CPU / 内存、主线程有没有被卡住、窗口挪了多少次。
/// 每秒往主线程丢一个空任务量它多久才被执行，量出来的就是“界面卡不卡”。
pub fn spawn_perf_sampler(handle: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("perf".into())
        .spawn(move || {
            let me = std::process::id();
            let cores = std::thread::available_parallelism().map(|n| n.get() as f64).unwrap_or(1.0);
            let mut last_cpu: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
            let mut last_t = Instant::now();
            let (mut lat_sum, mut lat_n, mut lat_max, mut lat_slow) = (0u128, 0u32, 0u128, 0u32);
            let mut tick = 0u32;
            // 先取一次基准
            for pid in tree_of(&platform::process_list(), me) {
                if let Some((c, _)) = platform::process_usage(pid) {
                    last_cpu.insert(pid, c);
                }
            }
            loop {
                std::thread::sleep(Duration::from_secs(1));
                tick += 1;
                // 主线程响应
                let (tx, rx) = std::sync::mpsc::channel();
                let sent = Instant::now();
                if handle.run_on_main_thread(move || {
                    let _ = tx.send(());
                }).is_ok()
                {
                    let ms = match rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(()) => sent.elapsed().as_millis(),
                        Err(_) => {
                            plog!("!!! 主线程 10 秒没响应（界面卡死）");
                            10_000
                        }
                    };
                    lat_sum += ms;
                    lat_n += 1;
                    lat_max = lat_max.max(ms);
                    if ms > 100 {
                        lat_slow += 1;
                    }
                }
                if tick % 30 != 0 {
                    continue;
                }
                let wall = last_t.elapsed().as_secs_f64();
                last_t = Instant::now();
                let list = platform::process_list();
                let tree = tree_of(&list, me);
                let (mut own, mut own_mem, mut web, mut web_mem, mut web_n) = (0f64, 0u64, 0f64, 0u64, 0);
                let mut now_cpu = std::collections::HashMap::new();
                for pid in &tree {
                    let Some((c, mem)) = platform::process_usage(*pid) else { continue };
                    let delta = c.saturating_sub(*last_cpu.get(pid).unwrap_or(&c));
                    now_cpu.insert(*pid, c);
                    // 100 纳秒 → 秒，再除以墙钟时间和核数 = 任务管理器里那个百分比
                    let pct = delta as f64 / 1e7 / wall / cores * 100.0;
                    if *pid == me {
                        own = pct;
                        own_mem = mem;
                    } else {
                        web += pct;
                        web_mem += mem;
                        web_n += 1;
                    }
                }
                last_cpu = now_cpu;
                let a = crate::state::try_app();
                let breathing = a.map(|a| a.breathing.load(Ordering::SeqCst)).unwrap_or(false);
                plog!(
                    "占用（{:.0} 秒）：宠物 CPU {own:.1}% 内存 {}MB；网页进程 {web_n} 个 CPU {web:.1}% 内存 {}MB；主线程响应 平均 {}ms 最慢 {lat_max}ms 超过 0.1 秒 {lat_slow} 次；窗口移动 {} 次，穿透切换 {} 次（最慢 {}ms）；呼吸模式 {}",
                    wall,
                    own_mem / 1_048_576,
                    web_mem / 1_048_576,
                    if lat_n > 0 { lat_sum / lat_n as u128 } else { 0 },
                    WIN_MOVES.swap(0, Ordering::Relaxed),
                    HIT_TOGGLES.swap(0, Ordering::Relaxed),
                    HIT_TOGGLE_MAX_US.swap(0, Ordering::Relaxed) / 1000,
                    if breathing { "开" } else { "关" }
                );
                lat_sum = 0;
                lat_n = 0;
                lat_max = 0;
                lat_slow = 0;
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_process_tree() {
        let p = |pid, ppid| platform::ProcInfo { pid, ppid, name: String::new() };
        let list = vec![p(1, 0), p(10, 1), p(11, 10), p(12, 11), p(20, 1), p(13, 10)];
        let mut t = tree_of(&list, 10);
        t.sort();
        assert_eq!(t, vec![10, 11, 12, 13]);
    }
}
