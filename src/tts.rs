//! 语音：微软在线自然音色（Edge 朗读同款）为主，Windows 自带语音兜底。
//! 合成和播放是两条流水线：播第一句的同时合成第二句，尽量做到不停顿。

use crate::plog;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc as smpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

#[derive(Clone, Debug)]
pub struct VoiceSettings {
    pub engine: String,
    pub edge_voice: String,
    pub system_voice: String,
    pub rate: f32,
    pub enabled: bool,
}

struct SayItem {
    gen: u64,
    text: String,
    voice: VoiceSettings,
}

struct Audio {
    gen: u64,
    text: String,
    bytes: Vec<u8>,
}

pub enum PlayEvent {
    Start(String),
    Idle,
}

pub struct Tts {
    tx: mpsc::UnboundedSender<SayItem>,
    play_tx: smpsc::Sender<PlayCmd>,
    gen: Arc<AtomicU64>,
    pub voice: Mutex<VoiceSettings>,
    cache: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    edge_down_until: Arc<Mutex<Option<Instant>>>,
    pub speaking: Arc<AtomicBool>,
}

enum PlayCmd {
    Audio(Audio),
    Chime,
    Stop,
}

impl Tts {
    pub fn start(voice: VoiceSettings, on_event: impl Fn(PlayEvent) + Send + 'static) -> Arc<Tts> {
        let (tx, mut rx) = mpsc::unbounded_channel::<SayItem>();
        let (play_tx, play_rx) = smpsc::channel::<PlayCmd>();
        let gen = Arc::new(AtomicU64::new(1));
        let speaking = Arc::new(AtomicBool::new(false));
        let tts = Arc::new(Tts {
            tx,
            play_tx: play_tx.clone(),
            gen: gen.clone(),
            voice: Mutex::new(voice),
            cache: Arc::new(Mutex::new(HashMap::new())),
            edge_down_until: Arc::new(Mutex::new(None)),
            speaking: speaking.clone(),
        });

        // 播放线程（音频设备对象只能留在一个线程里）
        let gen_p = gen.clone();
        std::thread::Builder::new()
            .name("tts-player".into())
            .spawn(move || player_loop(play_rx, gen_p, speaking, on_event))
            .ok();

        // 合成任务
        let t2 = tts.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(item) = rx.recv().await {
                if item.gen != t2.gen.load(Ordering::SeqCst) {
                    continue;
                }
                let bytes = t2.synth(&item.text, &item.voice).await;
                if item.gen != t2.gen.load(Ordering::SeqCst) {
                    continue;
                }
                match bytes {
                    Ok(b) => {
                        let _ = t2.play_tx.send(PlayCmd::Audio(Audio { gen: item.gen, text: item.text, bytes: b }));
                    }
                    Err(e) => plog!("语音合成失败：{e}"),
                }
            }
        });
        tts
    }

    pub fn set_voice(&self, v: VoiceSettings) {
        *self.voice.lock().unwrap() = v;
    }

    /// 排队念一句
    pub fn say(&self, text: &str) {
        let v = self.voice.lock().unwrap().clone();
        if !v.enabled {
            return;
        }
        let text = clean_for_speech(text);
        if text.trim().is_empty() {
            return;
        }
        let _ = self.tx.send(SayItem { gen: self.gen.load(Ordering::SeqCst), text, voice: v });
    }

    /// 打断：清空队列并停止正在念的
    pub fn stop(&self) {
        self.gen.fetch_add(1, Ordering::SeqCst);
        let _ = self.play_tx.send(PlayCmd::Stop);
    }

    /// 叮一声（不排队，立刻响）
    pub fn chime(&self) {
        let _ = self.play_tx.send(PlayCmd::Chime);
    }

    /// 马上要说话了（Claude 正在想、或者呼吸模式看到画面变了）：先把在线语音的连接建好
    pub fn prewarm(&self) {
        let v = self.voice.lock().unwrap().clone();
        let edge_ok = v.enabled && v.engine == "edge" && self.edge_down_until.lock().unwrap().map(|t| Instant::now() > t).unwrap_or(true);
        if edge_ok {
            edge_prewarm();
        }
    }

    /// 提前合成常用短句，之后念它们零延迟
    pub fn warm(self: &Arc<Self>, phrases: Vec<String>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let v = me.voice.lock().unwrap().clone();
            for p in phrases {
                let _ = me.synth(&clean_for_speech(&p), &v).await;
            }
        });
    }

    async fn synth(&self, text: &str, v: &VoiceSettings) -> Result<Vec<u8>, String> {
        let key = format!("{}|{}|{}|{}|{}", v.engine, v.edge_voice, v.system_voice, v.rate, text);
        if let Some(b) = self.cache.lock().unwrap().get(&key) {
            return Ok(b.clone());
        }
        let edge_ok = v.engine == "edge" && self.edge_down_until.lock().unwrap().map(|t| Instant::now() > t).unwrap_or(true);
        let result = if edge_ok {
            match tokio::time::timeout(Duration::from_secs(6), edge_tts(text, &v.edge_voice, v.rate)).await {
                Ok(Ok(b)) => Ok(b),
                other => {
                    let msg = match other {
                        Ok(Err(e)) => e,
                        _ => "超时".into(),
                    };
                    plog!("在线语音不可用（{msg}），改用系统语音");
                    *self.edge_down_until.lock().unwrap() = Some(Instant::now() + Duration::from_secs(120));
                    system_synth(text, v).await
                }
            }
        } else {
            system_synth(text, v).await
        };
        if let Ok(b) = &result {
            if text.chars().count() <= 40 {
                let mut c = self.cache.lock().unwrap();
                if c.len() > 300 {
                    c.clear();
                }
                c.insert(key, b.clone());
            }
        }
        result
    }
}

async fn system_synth(text: &str, v: &VoiceSettings) -> Result<Vec<u8>, String> {
    let (t, voice, rate) = (text.to_string(), v.system_voice.clone(), v.rate);
    tokio::task::spawn_blocking(move || crate::platform::system_tts(t, voice, rate))
        .await
        .map_err(|e| e.to_string())?
}

fn player_loop(rx: smpsc::Receiver<PlayCmd>, gen: Arc<AtomicU64>, speaking: Arc<AtomicBool>, on_event: impl Fn(PlayEvent)) {
    let mut stream = match rodio::OutputStreamBuilder::open_default_stream() {
        Ok(s) => s,
        Err(e) => {
            plog!("打不开声音设备：{e}");
            // 没声卡也要把消息消费掉
            while rx.recv().is_ok() {}
            return;
        }
    };
    stream.log_on_drop(false);
    let mut sink = rodio::Sink::connect_new(stream.mixer());
    let fx = rodio::Sink::connect_new(stream.mixer());
    let chime = chime_samples();
    let mut playing = false;
    loop {
        let cmd = if playing { rx.recv_timeout(Duration::from_millis(15)) } else { rx.recv().map_err(|_| smpsc::RecvTimeoutError::Disconnected) };
        match cmd {
            Ok(PlayCmd::Audio(a)) => {
                if a.gen != gen.load(Ordering::SeqCst) {
                    continue;
                }
                match rodio::Decoder::new(Cursor::new(a.bytes)) {
                    Ok(src) => {
                        // 等前一句念完再通知界面换字幕
                        while !sink.empty() {
                            if gen.load(Ordering::SeqCst) != a.gen {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        if gen.load(Ordering::SeqCst) != a.gen {
                            continue;
                        }
                        speaking.store(true, Ordering::SeqCst);
                        on_event(PlayEvent::Start(a.text));
                        sink.append(src);
                        playing = true;
                    }
                    Err(e) => plog!("解码语音失败：{e}"),
                }
            }
            Ok(PlayCmd::Chime) => {
                fx.append(rodio::buffer::SamplesBuffer::new(1, 24000, chime.clone()));
            }
            Ok(PlayCmd::Stop) => {
                sink.stop();
                sink = rodio::Sink::connect_new(stream.mixer());
                if playing {
                    playing = false;
                    speaking.store(false, Ordering::SeqCst);
                    on_event(PlayEvent::Idle);
                }
            }
            Err(smpsc::RecvTimeoutError::Timeout) => {
                if playing && sink.empty() {
                    playing = false;
                    speaking.store(false, Ordering::SeqCst);
                    on_event(PlayEvent::Idle);
                }
            }
            Err(smpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// 两声短促的提示音
fn chime_samples() -> Vec<f32> {
    let sr = 24000.0;
    let mut v = Vec::new();
    for (freq, ms) in [(988.0f32, 70.0f32), (1319.0, 110.0)] {
        let n = (sr * ms / 1000.0) as usize;
        for i in 0..n {
            let t = i as f32 / sr;
            let env = (1.0 - i as f32 / n as f32).powf(1.5) * (i as f32 / 120.0).min(1.0);
            v.push((t * freq * std::f32::consts::TAU).sin() * 0.25 * env);
        }
    }
    v
}

/// 去掉 markdown 符号、表情、链接，免得念出奇怪的东西
pub fn clean_for_speech(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' | '#' | '`' | '_' | '~' | '>' | '|' => {}
            c if (c as u32) >= 0x1F000 => {} // 表情
            '\n' | '\r' | '\t' => out.push('，'),
            _ => out.push(c),
        }
    }
    let out = out.replace("[静默]", "");
    out.trim_matches(|c: char| c == '，' || c.is_whitespace()).to_string()
}

// ───────────────────────── 微软在线语音 ─────────────────────────

const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const CHROMIUM_FULL_VERSION: &str = "143.0.3650.75";

fn sec_ms_gec() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut ticks = now + 11_644_473_600;
    ticks -= ticks % 300;
    let ticks = ticks as u128 * 10_000_000;
    let mut h = Sha256::new();
    h.update(format!("{ticks}{TRUSTED_CLIENT_TOKEN}").as_bytes());
    h.finalize().iter().map(|b| format!("{b:02X}")).collect()
}

fn js_date() -> String {
    chrono::Utc::now().format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)").to_string()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn hex_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

type EdgeWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 提前连好的一条连接和它建好的时间。握手（DNS + TLS + WebSocket）要几百毫秒，
/// 在 Claude 想的时候先连上，第一句话就能直接发。每条连接只合成一句。
static EDGE_SPARE: Mutex<Option<(EdgeWs, Instant)>> = Mutex::new(None);
static EDGE_CONNECTING: AtomicBool = AtomicBool::new(false);
/// 预连的连接用失败过一次就不再预连（没在真实网络上验证过服务器是否接受“先连后发”，失败就退回老办法）
static EDGE_SPARE_OFF: AtomicBool = AtomicBool::new(false);
/// 备用连接放久了服务器可能已经断开，超过这个时间就不用它
const EDGE_SPARE_MAX_AGE: Duration = Duration::from_secs(20);

/// 后台连一条备用连接（已经有新鲜的、或者正在连，就什么都不做）
pub fn edge_prewarm() {
    if EDGE_SPARE_OFF.load(Ordering::SeqCst) {
        return;
    }
    if let Some((_, t)) = EDGE_SPARE.lock().unwrap().as_ref() {
        if t.elapsed() < EDGE_SPARE_MAX_AGE / 2 {
            return;
        }
    }
    if EDGE_CONNECTING.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        if let Ok(Ok(ws)) = tokio::time::timeout(Duration::from_secs(5), edge_connect()).await {
            *EDGE_SPARE.lock().unwrap() = Some((ws, Instant::now()));
        }
        EDGE_CONNECTING.store(false, Ordering::SeqCst);
    });
}

fn take_spare() -> Option<EdgeWs> {
    let spare = EDGE_SPARE.lock().unwrap().take();
    if EDGE_SPARE_OFF.load(Ordering::SeqCst) {
        return None;
    }
    spare.filter(|(_, t)| t.elapsed() < EDGE_SPARE_MAX_AGE).map(|(ws, _)| ws)
}

pub async fn edge_tts(text: &str, voice: &str, rate: f32) -> Result<Vec<u8>, String> {
    let spare = take_spare();
    // 顺手给下一句也连一条（一段话通常有好几句）
    edge_prewarm();
    if let Some(ws) = spare {
        match edge_speak(ws, text, voice, rate, Duration::from_millis(1500)).await {
            Ok(a) => return Ok(a),
            Err(e) => {
                plog!("预先连好的语音连接用不了（{e}），以后不再预连");
                EDGE_SPARE_OFF.store(true, Ordering::SeqCst);
                drop(EDGE_SPARE.lock().unwrap().take());
            }
        }
    }
    let ws = edge_connect().await?;
    edge_speak(ws, text, voice, rate, Duration::from_secs(5)).await
}

async fn edge_connect() -> Result<EdgeWs, String> {
    let major = CHROMIUM_FULL_VERSION.split('.').next().unwrap_or("143");
    let url = format!(
        "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&ConnectionId={}&Sec-MS-GEC={}&Sec-MS-GEC-Version=1-{CHROMIUM_FULL_VERSION}",
        hex_id(),
        sec_ms_gec()
    );
    let mut req = url.into_client_request().map_err(|e| e.to_string())?;
    let h = req.headers_mut();
    let ua = format!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36 Edg/{major}.0.0.0");
    for (k, v) in [
        ("Pragma", "no-cache".to_string()),
        ("Cache-Control", "no-cache".to_string()),
        ("Origin", "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold".to_string()),
        ("User-Agent", ua),
        ("Accept-Encoding", "gzip, deflate, br, zstd".to_string()),
        ("Accept-Language", "en-US,en;q=0.9".to_string()),
        ("Cookie", format!("muid={};", hex_id().to_uppercase())),
    ] {
        if let Ok(val) = v.parse() {
            h.insert(k, val);
        }
    }
    let (ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| e.to_string())?;
    Ok(ws)
}

/// 在一条连好的连接上合成一句。first_wait：发出去以后多久没收到任何回音就算这条连接坏了
async fn edge_speak(mut ws: EdgeWs, text: &str, voice: &str, rate: f32, first_wait: Duration) -> Result<Vec<u8>, String> {
    let cfg = format!(
        "X-Timestamp:{}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}}}}}\r\n",
        js_date()
    );
    ws.send(Message::Text(cfg.into())).await.map_err(|e| e.to_string())?;
    let pct = ((rate - 1.0) * 100.0).round() as i32;
    let rate_s = format!("{}{}%", if pct >= 0 { "+" } else { "" }, pct);
    let ssml = format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'><voice name='{}'><prosody pitch='+0Hz' rate='{}' volume='+0%'>{}</prosody></voice></speak>",
        xml_escape(voice),
        rate_s,
        xml_escape(text)
    );
    let msg = format!("X-RequestId:{}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{}Z\r\nPath:ssml\r\n\r\n{}", hex_id(), js_date(), ssml);
    ws.send(Message::Text(msg.into())).await.map_err(|e| e.to_string())?;

    let mut audio = Vec::new();
    let mut first = true;
    loop {
        let wait = if first { first_wait } else { Duration::from_secs(5) };
        let m = match tokio::time::timeout(wait, ws.next()).await {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(_) => return Err(if first { "服务器没回应".into() } else { "音频中途断了".into() }),
        };
        first = false;
        match m.map_err(|e| e.to_string())? {
            Message::Text(t) => {
                if t.contains("Path:turn.end") {
                    break;
                }
            }
            Message::Binary(b) => {
                if b.len() < 2 {
                    continue;
                }
                let hl = u16::from_be_bytes([b[0], b[1]]) as usize;
                if b.len() < 2 + hl {
                    continue;
                }
                let head = String::from_utf8_lossy(&b[2..2 + hl]);
                if head.contains("Path:audio") && head.contains("Content-Type:audio/mpeg") {
                    audio.extend_from_slice(&b[2 + hl..]);
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = ws.close(None).await;
    if audio.is_empty() {
        return Err("没有收到音频".into());
    }
    Ok(audio)
}

pub fn edge_voices() -> Vec<(&'static str, &'static str)> {
    vec![
        ("zh-CN-XiaoyiNeural", "晓伊（活泼女声，推荐）"),
        ("zh-CN-XiaoxiaoNeural", "晓晓（温柔女声）"),
        ("zh-CN-YunxiaNeural", "云夏（男童声）"),
        ("zh-CN-YunxiNeural", "云希（少年男声）"),
        ("zh-CN-YunjianNeural", "云健（成熟男声）"),
        ("zh-CN-liaoning-XiaobeiNeural", "晓北（东北话）"),
        ("zh-CN-shaanxi-XiaoniNeural", "晓妮（陕西话）"),
        ("zh-TW-HsiaoChenNeural", "曉臻（台湾）"),
        ("zh-HK-HiuGaaiNeural", "曉佳（粤语）"),
        ("ja-JP-NanamiNeural", "七海（日语）"),
    ]
}

/// 把 base64 音频解出来（给界面“试听”用）
#[allow(dead_code)]
pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ───────────────────────── 按句切分 ─────────────────────────

/// 流式文字按句切开，第一句尽量短，好让宠物尽早开口
pub struct SentenceSplitter {
    buf: String,
    emitted: usize,
}

impl SentenceSplitter {
    pub fn new() -> Self {
        Self { buf: String::new(), emitted: 0 }
    }

    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        loop {
            let chars: Vec<char> = self.buf.chars().collect();
            let mut cut = None;
            let first = self.emitted == 0;
            for (i, c) in chars.iter().enumerate() {
                let hard = matches!(c, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n' | '…');
                let soft = matches!(c, '，' | ',' | '、' | '：' | ':');
                // 英文句号后面跟空格才算句末，避免把 3.14 切开
                let dot = *c == '.' && chars.get(i + 1).map(|n| n.is_whitespace()).unwrap_or(false);
                if hard || dot {
                    if i >= 1 {
                        cut = Some(i + 1);
                        break;
                    }
                } else if soft && ((first && i >= 6) || i >= 28) {
                    cut = Some(i + 1);
                    break;
                }
                if i >= 60 {
                    cut = Some(i + 1);
                    break;
                }
            }
            match cut {
                Some(n) => {
                    let s: String = chars[..n].iter().collect();
                    self.buf = chars[n..].iter().collect();
                    let s = s.trim().to_string();
                    if !s.is_empty() {
                        self.emitted += 1;
                        out.push(s);
                    }
                }
                None => break,
            }
        }
        out
    }

    pub fn flush(&mut self) -> Option<String> {
        let s = std::mem::take(&mut self.buf).trim().to_string();
        if s.is_empty() {
            None
        } else {
            self.emitted += 1;
            Some(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sentences_early() {
        let mut s = SentenceSplitter::new();
        let mut got = Vec::new();
        for piece in ["你好呀，我看到", "你的终端报错了。原因是", "少了一个分号！", "改完再跑"] {
            got.extend(s.push(piece));
        }
        got.extend(s.flush());
        assert_eq!(got, vec!["你好呀，我看到你的终端报错了。", "原因是少了一个分号！", "改完再跑"]);
    }

    #[test]
    fn gec_is_hex64() {
        let g = sec_ms_gec();
        assert_eq!(g.len(), 64);
        assert!(g.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
    }

    #[test]
    fn cleans_markdown() {
        assert_eq!(clean_for_speech("**注意**：`npm` 报错\n"), "注意：npm 报错");
    }
}
