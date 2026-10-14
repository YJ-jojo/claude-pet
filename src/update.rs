//! 自动更新：从 GitHub Releases（YJ-jojo/claude-pet）拿最新版本，下载新的 ClaudePet.exe，
//! 验证签名后换掉自己再重启。
//!
//! 签名：用户在自己电脑上点“生成更新钥匙”，宠物生成一对 Ed25519 钥匙，公钥存在本机
//! （%APPDATA%\com.claudepet.app\update-key.pub），私钥复制到剪贴板，用户粘贴到 GitHub 仓库的
//! Actions 保密设置 UPDATE_SIGNING_KEY。GitHub 编译时用私钥签 “claude-pet 版本 sha256”，
//! 宠物用本机公钥验证，对不上就不装。没生成过钥匙的电脑不会自动更新。
//!
//! 换文件：Windows 不能覆盖正在运行的 exe，但能改它的名字。先把自己改名成 ClaudePet.old.exe，
//! 新文件改名成 ClaudePet.exe，启动新的（带 --wait-pid），自己退出。新进程等旧进程退出后才往下走
//! （不然单实例检查会把它当成第二个实例关掉），下次启动时删掉 .old.exe。
//!
//! 下载用 PowerShell：它默认走系统代理设置，开着代理软件也能连上 GitHub。

use crate::plog;
use crate::state::app;
use crate::talk;
use base64::Engine;
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

pub const REPO: &str = "YJ-jojo/claude-pet";
const EXE_NAME: &str = "ClaudePet.exe";
const OLD_NAME: &str = "ClaudePet.old.exe";
const NEW_NAME: &str = "ClaudePet.exe.new";
const KEY_FILE: &str = "update-key.pub";
/// 启动后多久第一次检查、之后多久查一次
const FIRST_CHECK: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(3 * 3600);

static RUNNING: AtomicBool = AtomicBool::new(false);
/// 已经有钥匙时，第一次点“生成”的时间（30 秒内再点一次才真的换）
static REGEN_ASKED: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Latest {
    pub version: String,
    pub sha256: String,
    /// 对 signed_message(version, sha256) 的 Ed25519 签名（base64）；没配钥匙时编出来的版本没有
    #[serde(default)]
    pub sig: String,
}

/// 签名覆盖的内容：版本号和文件指纹绑在一起，旧版本的签名不能拿来冒充新版本
pub fn signed_message(version: &str, sha256: &str) -> String {
    format!("claude-pet {} {}", version.trim(), sha256.trim().to_ascii_lowercase())
}

/// 用公钥验证 latest.json 里的签名
pub fn verify(latest: &Latest, pubkey_b64: &str) -> Result<(), String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let pk: [u8; 32] = b64.decode(pubkey_b64.trim()).ok().and_then(|v| v.try_into().ok()).ok_or("本机的更新公钥坏了")?;
    let pk = VerifyingKey::from_bytes(&pk).map_err(|_| "本机的更新公钥坏了")?;
    if latest.sig.trim().is_empty() {
        return Err("这个版本没有签名（GitHub 上还没设置 UPDATE_SIGNING_KEY）".into());
    }
    let sig = b64.decode(latest.sig.trim()).map_err(|_| "签名格式不对")?;
    let sig = Signature::from_slice(&sig).map_err(|_| "签名格式不对")?;
    pk.verify_strict(signed_message(&latest.version, &latest.sha256).as_bytes(), &sig)
        .map_err(|_| "签名对不上（不是用你的钥匙签的），没有更新".into())
}

/// Ed25519 私钥 → PKCS#8 PEM（GitHub 上用 openssl 签名要这个格式）
pub fn pkcs8_pem(seed: &[u8; 32]) -> String {
    let mut der = vec![0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20];
    der.extend_from_slice(seed);
    let b = base64::engine::general_purpose::STANDARD.encode(&der);
    format!("-----BEGIN PRIVATE KEY-----\n{b}\n-----END PRIVATE KEY-----\n")
}

/// "0.3.1" → [0, 3, 1]；带 v 前缀或后缀（-beta）也能比
fn parse_ver(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches('v')
        .split(|c: char| c == '.' || c == '-' || c == '+')
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    let (mut r, mut l) = (parse_ver(remote), parse_ver(local));
    r.resize(3, 0);
    l.resize(3, 0);
    r > l
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

fn key_path() -> PathBuf {
    app().data_dir.join(KEY_FILE)
}

/// 程序启动时最先调用：刚更新完的话等旧进程退出，再清掉旧文件
pub fn on_start(args: &[String]) {
    if let Some(i) = args.iter().position(|a| a == "--wait-pid") {
        if let Some(pid) = args.get(i + 1).and_then(|p| p.parse::<u32>().ok()) {
            let done = crate::platform::wait_process_exit(pid, Duration::from_secs(15));
            plog!("更新：等旧版本（进程 {pid}）退出{}", if done { "完毕" } else { "超时，继续启动" });
        }
    }
    if let Some(dir) = exe_dir() {
        let old = dir.join(OLD_NAME);
        if old.exists() {
            // Claude 桌面版启动的桥进程可能还在用旧文件，删不掉就下次再删
            match std::fs::remove_file(&old) {
                Ok(()) => plog!("更新：删掉了旧版本文件"),
                Err(e) => plog!("更新：旧版本文件暂时删不掉（{e}），下次启动再删"),
            }
        }
        let _ = std::fs::remove_file(dir.join(NEW_NAME));
    }
}

/// 刚更新完（带 --updated 启动）：宠物出来后说一声
pub fn announce_if_updated(args: &[String]) {
    if !args.iter().any(|a| a == "--updated") {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(8));
        talk::say(&format!("更新好了，现在是 {}。", env!("CARGO_PKG_VERSION")), "alert");
    });
}

/// 托盘菜单“生成更新钥匙”：公钥存本机，私钥放进剪贴板，打开 GitHub 的保密设置页
pub fn gen_key() {
    let path = key_path();
    if path.exists() {
        let now = chrono::Local::now().timestamp() as u64;
        let asked = REGEN_ASKED.swap(now, Ordering::SeqCst);
        if now.saturating_sub(asked) > 30 {
            talk::say("已经有更新钥匙了。真要换新的，30 秒内再点一次；换了以后要把新钥匙重新粘到 GitHub。", "talk");
            return;
        }
        REGEN_ASKED.store(0, Ordering::SeqCst);
    }
    let mut seed = [0u8; 32];
    if let Err(e) = getrandom::fill(&mut seed) {
        talk::say(&format!("生成钥匙失败：{e}"), "error");
        return;
    }
    let pk = SigningKey::from_bytes(&seed).verifying_key();
    let pem = pkcs8_pem(&seed);
    seed.fill(0);
    let pk_b64 = base64::engine::general_purpose::STANDARD.encode(pk.as_bytes());
    if let Err(e) = std::fs::write(&path, &pk_b64) {
        talk::say(&format!("钥匙存不下来：{e}"), "error");
        return;
    }
    plog!("更新：生成了新的更新钥匙，公钥 {pk_b64}");
    if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(pem.clone())) {
        talk::say(&format!("钥匙复制到剪贴板失败：{e}"), "error");
        return;
    }
    open_secret_page();
    talk::say(
        "钥匙已经复制好了。在打开的 GitHub 网页里：Name 填 UPDATE_SIGNING_KEY，Secret 里粘贴，点 Add secret。两分钟后我会把剪贴板清掉。",
        "alert",
    );
    // 两分钟后剪贴板里还是这把钥匙就清掉
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(120));
        if let Ok(mut c) = arboard::Clipboard::new() {
            if c.get_text().map(|t| t == pem).unwrap_or(false) {
                let _ = c.set_text(String::new());
            }
        }
    });
}

fn open_secret_page() {
    let url = format!("https://github.com/{REPO}/settings/secrets/actions/new");
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &url]).spawn();
    }
    #[cfg(not(windows))]
    plog!("打开网址：{url}");
}

/// 后台定时检查
pub fn spawn_checker() {
    std::thread::Builder::new()
        .name("update".into())
        .spawn(|| {
            std::thread::sleep(FIRST_CHECK);
            loop {
                check_and_apply(false);
                std::thread::sleep(EVERY);
            }
        })
        .ok();
}

/// 托盘菜单“检查更新”
pub fn check_now() {
    std::thread::spawn(|| check_and_apply(true));
}

fn check_and_apply(manual: bool) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        if manual {
            talk::say("正在检查更新，稍等。", "talk");
        }
        return;
    }
    let r = run(manual);
    RUNNING.store(false, Ordering::SeqCst);
    if let Err(e) = r {
        plog!("更新：{e}");
        if manual {
            talk::say(&format!("没有更新：{e}"), "error");
        }
    }
}

fn run(manual: bool) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("只有 Windows 版能自动更新".into());
    }
    let local = env!("CARGO_PKG_VERSION");
    let pubkey = std::fs::read_to_string(key_path()).map_err(|_| "这台电脑还没生成更新钥匙（托盘图标右键 → 生成更新钥匙）".to_string())?;
    let dir = exe_dir().ok_or("找不到程序所在的文件夹")?;
    let tmp = std::env::temp_dir().join(format!("claudepet-latest-{}.json", std::process::id()));
    let url = format!("https://github.com/{REPO}/releases/latest/download/latest.json");
    download(&url, &tmp).map_err(|e| format!("连不上 GitHub（{e}）"))?;
    let text = std::fs::read_to_string(&tmp).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    let latest: Latest = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("版本信息看不懂：{e}"))?;
    if !is_newer(&latest.version, local) {
        plog!("更新：已经是最新版（本机 {local}，GitHub 上 {}）", latest.version);
        if manual {
            talk::say(&format!("已经是最新版 {local}。"), "talk");
        }
        return Ok(());
    }
    // 先验签名再下载：签名不对就不用浪费流量
    verify(&latest, &pubkey)?;
    plog!("更新：发现新版本 {}（本机 {local}），签名对得上，开始下载", latest.version);
    if manual {
        talk::say(&format!("发现新版本 {}，正在下载。", latest.version), "talk");
    }
    let new_path = dir.join(NEW_NAME);
    let exe_url = format!("https://github.com/{REPO}/releases/download/v{}/{EXE_NAME}", latest.version);
    download(&exe_url, &new_path).map_err(|e| format!("新版本下载失败（{e}）"))?;
    let bytes = std::fs::read(&new_path).map_err(|e| e.to_string())?;
    let got = sha256_hex(&bytes);
    if got != latest.sha256.trim().to_ascii_lowercase() {
        let _ = std::fs::remove_file(&new_path);
        return Err(format!("下载的文件和签名的不是同一个（{got}），没有更新"));
    }
    plog!("更新：下载好了 {} 字节，和签名的文件一致", bytes.len());
    // 宠物正在帮你点东西、在想或在说话时先不换，最多等 10 分钟
    let a = app();
    for _ in 0..600 {
        let busy = a.operating.load(Ordering::SeqCst) || a.thinking.load(Ordering::SeqCst) || a.tts.speaking.load(Ordering::SeqCst);
        if !busy {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    talk::say(&format!("我要更新到 {} 了，几秒后回来。", latest.version), "alert");
    std::thread::sleep(Duration::from_secs(3));
    swap_and_restart(&dir)
}

fn swap_and_restart(dir: &Path) -> Result<(), String> {
    let cur = dir.join(EXE_NAME);
    let old = dir.join(OLD_NAME);
    let new = dir.join(NEW_NAME);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&cur, &old).map_err(|e| format!("换文件失败（改旧文件名：{e}）"))?;
    if let Err(e) = std::fs::rename(&new, &cur) {
        // 换不过去：把旧的改回来，什么都不变
        let _ = std::fs::rename(&old, &cur);
        return Err(format!("换文件失败（放新文件：{e}）"));
    }
    let me = std::process::id().to_string();
    let mut cmd = std::process::Command::new(&cur);
    cmd.args(["--wait-pid", &me, "--updated"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    const DETACHED: u32 = 0x0000_0008 | 0x0000_0200; // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED | 0x0100_0000); // 再加 CREATE_BREAKAWAY_FROM_JOB
    }
    #[allow(unused_mut)]
    let mut spawned = cmd.spawn();
    #[cfg(windows)]
    if spawned.is_err() {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED);
        spawned = cmd.spawn();
    }
    match spawned {
        Ok(c) => {
            plog!("更新：新版本已启动（进程 {}），旧版本退出", c.id());
            crate::quit();
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&cur, &new);
            let _ = std::fs::rename(&old, &cur);
            Err(format!("新版本启动不了（{e}），已换回旧版本"))
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// 用 PowerShell 下载（走系统代理），最多 5 分钟
fn download(url: &str, to: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(to);
    let q = |s: &str| s.replace('\'', "''");
    let script = format!(
        "$ProgressPreference='SilentlyContinue'; [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
         Invoke-WebRequest -UseBasicParsing -Uri '{}' -OutFile '{}' -TimeoutSec 300",
        q(url),
        q(&to.display().to_string())
    );
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| format!("启动 PowerShell 失败：{e}"))?;
    if !out.status.success() || !to.exists() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("没有说明");
        return Err(first.chars().take(200).collect());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.3.0", "0.2.2"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        assert!(is_newer("0.3.1", "0.3"));
    }

    #[test]
    fn verifies_signatures() {
        let b64 = base64::engine::general_purpose::STANDARD;
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = b64.encode(sk.verifying_key().as_bytes());
        let sha = "ab".repeat(32);
        let sig = b64.encode(sk.sign(signed_message("0.3.1", &sha).as_bytes()).to_bytes());
        let ok = Latest { version: "0.3.1".into(), sha256: sha.clone(), sig: sig.clone() };
        assert!(verify(&ok, &pk).is_ok());
        // 签名不能挪给别的版本号或别的文件
        assert!(verify(&Latest { version: "9.9.9".into(), ..ok.clone() }, &pk).is_err());
        assert!(verify(&Latest { sha256: "cd".repeat(32), ..ok.clone() }, &pk).is_err());
        assert!(verify(&Latest { sig: String::new(), ..ok.clone() }, &pk).is_err());
        let other = b64.encode(SigningKey::from_bytes(&[9u8; 32]).verifying_key().as_bytes());
        assert!(verify(&ok, &other).is_err());
    }

    #[test]
    fn makes_pkcs8_pem() {
        let pem = pkcs8_pem(&[1u8; 32]);
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEI"));
        assert!(pem.ends_with("-----END PRIVATE KEY-----\n"));
    }
}
