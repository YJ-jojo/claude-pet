//! 用户设置：保存在 %APPDATA%\com.claudepet.app\config.json

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const CFG_REV: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 配置版本：老配置里的旧默认值在载入时换成新默认值（缺省按 0 算）
    #[serde(default)]
    pub cfg_rev: u32,
    /// 宠物名字（Claude 讲话时的自称）
    pub pet_name: String,
    /// 形象包名字（sprites 目录下的文件夹名），内置占位形象叫 "placeholder"
    pub sprite_pack: String,
    /// 宠物显示高度（逻辑像素）
    pub pet_height: u32,

    /// claude 命令的完整路径，留空则自动查找
    pub claude_path: String,
    /// 呼吸模式讲解用的模型（越小越快）
    pub breath_model: String,
    /// 看一眼 / 聊天用的模型
    pub chat_model: String,
    /// 帮你操作时用的模型
    pub operate_model: String,
    /// 翻译用的模型
    pub translate_model: String,
    /// 关闭思考以加快首字速度
    pub disable_thinking: bool,

    /// 语音引擎："edge"（微软在线自然音色）或 "system"（Windows 自带离线音色）
    pub tts_engine: String,
    pub edge_voice: String,
    /// 语速倍率，1.0 为正常
    pub tts_rate: f32,
    pub system_voice: String,
    pub tts_enabled: bool,
    /// 讲话时同时显示气泡
    pub bubble_enabled: bool,

    /// 本地服务端口（Claude Code 钩子和 MCP 工具都连这里）
    pub port: u16,
    /// Claude Code 申请权限时立刻提醒（否则等它自己的约 6 秒通知）
    pub alert_permission_immediately: bool,
    /// Claude Code 空闲等你输入时提醒
    pub alert_idle: bool,
    /// Claude Code 回答完成时播报
    pub announce_stop: bool,

    /// 呼吸模式："quiet" 只说要紧事 / "normal" / "detailed"
    pub breath_mode: String,
    /// 呼吸模式检查画面的间隔（毫秒）
    pub breath_interval_ms: u64,
    /// 画面停止变化多久后再讲（毫秒）
    pub breath_settle_ms: u64,
    /// 两次开口之间至少隔多久（毫秒）
    pub breath_min_gap_ms: u64,
    /// 观察目标（重启后按进程名和标题重新匹配）
    pub targets: Vec<SavedTarget>,
    /// 永远不看的程序（进程名或窗口标题包含这些字就跳过）
    pub blacklist: Vec<String>,

    /// 翻译目标语言
    pub translate_target: String,
    /// 翻译后让宠物念出来
    pub translate_speak: bool,

    /// 帮你操作前是否每一步都先问
    pub operate_confirm_all: bool,
    /// 先点后演：先把按钮点了，宠物再跑过去做按的动作（点击零等待）
    pub operate_click_first: bool,
    /// 宠物跑动速度（像素/秒）
    pub run_speed: f64,
    /// 空闲时自己到处走走
    pub wander: bool,
    /// 鼠标离宠物远时让宠物窗口不穿透（更省；万一宠物附近的东西点不到，关掉它）
    pub hit_relax: bool,
    /// 截图、录屏、共享屏幕时不拍进宠物（也让呼吸模式不把宠物自己的动作当成画面变化）
    pub hide_from_capture: bool,

    pub hotkey_translate: String,
    pub hotkey_look: String,
    pub hotkey_breath: String,
    pub hotkey_watch: String,
    pub hotkey_stop: String,
    pub hotkey_chat: String,

    /// 自动更新的下载源，按顺序试，失败就换下一个。"" 是直接连 GitHub，其它是 GitHub 镜像，
    /// 地址拼在 https://github.com/... 前面。下载的东西都要过签名和指纹检查，镜像改不了内容
    pub update_sources: Vec<String>,

    /// 上次宠物的位置
    pub pet_x: Option<i32>,
    pub pet_y: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedTarget {
    pub process: String,
    pub title: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cfg_rev: CFG_REV,
            pet_name: "小光".into(),
            sprite_pack: "叶瞬光".into(),
            pet_height: 240,
            claude_path: String::new(),
            breath_model: "haiku".into(),
            chat_model: "sonnet".into(),
            operate_model: "sonnet".into(),
            translate_model: "haiku".into(),
            disable_thinking: true,
            tts_engine: "edge".into(),
            edge_voice: "zh-CN-XiaoyiNeural".into(),
            tts_rate: 1.25,
            system_voice: String::new(),
            tts_enabled: true,
            bubble_enabled: true,
            port: 47321,
            alert_permission_immediately: true,
            alert_idle: true,
            announce_stop: false,
            breath_mode: "normal".into(),
            breath_interval_ms: 700,
            breath_settle_ms: 200,
            breath_min_gap_ms: 2500,
            targets: vec![],
            blacklist: vec![
                "密码".into(),
                "KeePass".into(),
                "1Password".into(),
                "Bitwarden".into(),
                "银行".into(),
                "支付宝".into(),
            ],
            translate_target: "zh".into(),
            translate_speak: false,
            operate_confirm_all: false,
            operate_click_first: false,
            run_speed: 1800.0,
            wander: false,
            hit_relax: true,
            hide_from_capture: true,
            hotkey_translate: "CommandOrControl+Alt+T".into(),
            hotkey_look: "CommandOrControl+Alt+E".into(),
            hotkey_breath: "CommandOrControl+Alt+B".into(),
            hotkey_watch: "CommandOrControl+Alt+W".into(),
            hotkey_stop: "CommandOrControl+Alt+Q".into(),
            hotkey_chat: "CommandOrControl+Alt+Space".into(),
            update_sources: vec!["".into(), "https://ghfast.top/".into(), "https://gh-proxy.com/".into()],
            pet_x: None,
            pet_y: None,
        }
    }
}

impl Config {
    pub fn load(path: &PathBuf) -> Self {
        let mut c = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                crate::logger::log(&format!("配置文件解析失败，使用默认值：{e}"));
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        c.migrate();
        c
    }

    /// 0.2 及以前的配置：没被用户改过的旧默认值换成新默认值
    fn migrate(&mut self) {
        if self.cfg_rev < 1 {
            if self.breath_settle_ms == 350 {
                self.breath_settle_ms = 200;
            }
            // 有了正式形象包：还在用占位形象或试用包的，换成正式包（找不到时界面会自动退回占位形象）
            if self.sprite_pack == "placeholder" || self.sprite_pack == "叶瞬光-试用" {
                self.sprite_pack = "叶瞬光".into();
            }
        }
        self.cfg_rev = CFG_REV;
    }

    pub fn save(&self, path: &PathBuf) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(s) => {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, s).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
            Err(e) => crate::logger::log(&format!("保存配置失败：{e}")),
        }
    }

    pub fn is_blacklisted(&self, process: &str, title: &str) -> bool {
        let p = process.to_lowercase();
        let t = title.to_lowercase();
        self.blacklist.iter().any(|b| {
            let b = b.trim().to_lowercase();
            !b.is_empty() && (p.contains(&b) || t.contains(&b))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_old_breath_defaults() {
        let mut old: Config = serde_json::from_str(r#"{"breath_interval_ms":700,"breath_settle_ms":350,"sprite_pack":"placeholder"}"#).unwrap();
        old.migrate();
        assert_eq!((old.breath_interval_ms, old.breath_settle_ms, old.cfg_rev), (700, 200, CFG_REV));
        assert_eq!(old.sprite_pack, "叶瞬光");
        // 用户自己改过的值不动
        let mut custom: Config = serde_json::from_str(r#"{"breath_interval_ms":1000,"breath_settle_ms":500,"sprite_pack":"别的包"}"#).unwrap();
        custom.migrate();
        assert_eq!((custom.breath_interval_ms, custom.breath_settle_ms), (1000, 500));
        assert_eq!(custom.sprite_pack, "别的包");
        // 新版存的配置不再迁移
        let mut new: Config = serde_json::from_str(r#"{"cfg_rev":1,"breath_settle_ms":350,"sprite_pack":"placeholder"}"#).unwrap();
        new.migrate();
        assert_eq!((new.breath_settle_ms, new.sprite_pack.as_str()), (350, "placeholder"));
    }
}
