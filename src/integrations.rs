//! 小工具联动：VoiceInput、AutoBrightness、DeskHub 把实时状态推给岛仔（协议 v1）。
//!
//! - 只监听 `127.0.0.1:47820`，非回环来源直接断开；端口被占用时岛仔照常运行，只是没有联动。
//! - `POST /event` 一次一个 JSON 对象（≤ 64 KiB），`GET /ping` 探活。
//! - 服务线程只解析、转交，界面线程每帧取走；回复里的 `display` 由界面每帧写入的开关决定。
//! - 各工具推送失败（岛仔没开）时保持原来的提示方式，所以这里任何错误都不影响工具本身。
//!
//! 状态模型（[`State`]）与界面无关：语音会话、亮度快照、DeskHub 的 Claude 会话 / 会议 / 专注，
//! 以及常驻指示的优先级列表，均可脱离窗口单独测试。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

pub const PORT: u16 = 47820;
pub const MAX_BODY: usize = 64 * 1024;
const MAX_HEAD: usize = 8 * 1024;
/// 听写 / 识别阶段这么久没有任何事件，视为会话已结束（工具崩溃或漏发 idle）。
pub const VOICE_STALE: f64 = 20.0;
/// AutoBrightness 快照 180 秒、DeskHub 快照 90 秒没更新就作废。
pub const BRIGHTNESS_TTL: f64 = 180.0;
pub const DESKHUB_TTL: f64 = 90.0;
const TEXT_MAX: usize = 200;

// ---------------- 事件 ----------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceState { Listening, Level, Partial, Recognizing, Done, CommandResult, Idle, Error }

#[derive(Clone, Debug, PartialEq)]
pub struct Voice { pub state: VoiceState, pub command: bool, pub mic: String, pub level: f32, pub text: String, pub ok: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence { Present, Away, Blackout }

#[derive(Clone, Debug, PartialEq)]
pub struct BrightnessStatus { pub auto: bool, pub brightness: Option<u8>, pub presence: Presence, pub next_measure_s: Option<u32> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Away { Countdown(u32), Blackout, Cancel, Present }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeState { Approval, Waiting, Working, Done, Clear }
impl ClaudeState {
    pub fn waiting(self) -> bool { matches!(self, Self::Approval | Self::Waiting) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Claude { pub session: String, pub title: String, pub state: ClaudeState }

#[derive(Clone, Debug, PartialEq)]
pub struct HubStatus { pub meeting: bool, pub focus_ends_at: Option<i64>, pub speak: bool, pub claude: Vec<Claude> }

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Voice(Voice),
    Brightness { percent: u8, from: Option<u8>, manual: bool, note: String },
    Reminder { kind: String, title: String, body: String },
    Away(Away),
    BrightnessStatus(BrightnessStatus),
    Claude(Claude),
    Meeting(bool),
    /// `ends_at` 为 Unix 秒；没有给出时按 `minutes` 推算。
    Focus { active: bool, ends_at: Option<i64>, minutes: Option<u32>, missed: Option<u32> },
    HubStatus(HubStatus),
}

/// 设置里「小工具联动」的四个开关，各管一类事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature { Voice, Brightness, Claude, Boost }

impl Event {
    pub fn feature(&self) -> Feature {
        match self {
            Event::Voice(_) => Feature::Voice,
            Event::Brightness { .. } | Event::Reminder { .. } | Event::Away(_) | Event::BrightnessStatus(_) => Feature::Brightness,
            _ => Feature::Claude,
        }
    }
}

fn clean(text: &str) -> String {
    let text: String = text.trim().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if text.chars().count() > TEXT_MAX { text.chars().take(TEXT_MAX).collect::<String>() + "…" } else { text }
}

/// ISO 8601 → Unix 秒；不带时区的按本地时间。
pub fn iso(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(text) { return Some(t.timestamp()); }
    let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").or_else(|_| chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M")).ok()?;
    naive.and_local_timezone(chrono::Local).earliest().map(|t| t.timestamp())
}

/// `Err` = 不是合法的事件对象（回复 `ok:false`）；`Ok(None)` = 不认识的来源 / 类型 / 状态（回复 `display:false`）。
pub fn parse(body: &[u8]) -> Result<Option<Event>, ()> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| ())?;
    let o = value.as_object().ok_or(())?;
    let text = |k: &str| o.get(k).and_then(|v| v.as_str()).map(clean).unwrap_or_default();
    let flag = |k: &str| o.get(k).and_then(|v| v.as_bool());
    let number = |k: &str| o.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite());
    let percent = |k: &str| number(k).map(|v| v.round().clamp(0.0, 100.0) as u8);
    let (Some(source), Some(kind)) = (o.get("source").and_then(|v| v.as_str()), o.get("kind").and_then(|v| v.as_str())) else { return Err(()) };
    let claude = |o: &serde_json::Map<String, serde_json::Value>| -> Option<Claude> {
        let state = match o.get("state")?.as_str()? { "approval" => ClaudeState::Approval, "waiting" => ClaudeState::Waiting, "working" => ClaudeState::Working, "done" => ClaudeState::Done, "clear" => ClaudeState::Clear, _ => return None };
        let session = clean(o.get("session").and_then(|v| v.as_str()).unwrap_or(""));
        let title = clean(o.get("title").and_then(|v| v.as_str()).unwrap_or(""));
        Some(Claude { title: if title.is_empty() { "Claude".into() } else { title }, session, state })
    };
    Ok(Some(match (source.trim().to_ascii_lowercase().as_str(), kind.trim()) {
        ("voiceinput", "voice") => {
            let state = match o.get("state").and_then(|v| v.as_str()).ok_or(())? {
                "listening" => VoiceState::Listening, "level" => VoiceState::Level, "partial" => VoiceState::Partial, "recognizing" => VoiceState::Recognizing,
                "done" => VoiceState::Done, "command_result" => VoiceState::CommandResult, "idle" => VoiceState::Idle, "error" => VoiceState::Error, _ => return Ok(None),
            };
            Event::Voice(Voice { state, command: o.get("mode").and_then(|v| v.as_str()) == Some("command"), mic: text("mic"), level: number("level").unwrap_or(0.0).clamp(0.0, 1.0) as f32, text: text("text"), ok: flag("ok").unwrap_or(true) })
        }
        ("autobrightness", "brightness") => Event::Brightness { percent: percent("percent").ok_or(())?, from: percent("from"), manual: o.get("reason").and_then(|v| v.as_str()) == Some("manual"), note: text("note") },
        ("autobrightness", "reminder") => Event::Reminder { kind: text("type"), title: text("title"), body: text("body") },
        ("autobrightness", "away") => Event::Away(match o.get("state").and_then(|v| v.as_str()).ok_or(())? {
            "countdown" => Away::Countdown(number("seconds").unwrap_or(10.0).clamp(0.0, 3600.0) as u32), "blackout" => Away::Blackout, "cancel" => Away::Cancel, "present" => Away::Present, _ => return Ok(None),
        }),
        ("autobrightness", "status") => Event::BrightnessStatus(BrightnessStatus {
            auto: flag("auto").unwrap_or(true), brightness: percent("brightness"),
            presence: match o.get("presence").and_then(|v| v.as_str()) { Some("away") => Presence::Away, Some("blackout") => Presence::Blackout, _ => Presence::Present },
            next_measure_s: number("next_measure_s").map(|v| v.max(0.0) as u32),
        }),
        ("deskhub", "claude") => match claude(o) { Some(c) => Event::Claude(c), None => return if o.contains_key("state") { Ok(None) } else { Err(()) } },
        ("deskhub", "meeting") => Event::Meeting(flag("active").ok_or(())?),
        ("deskhub", "focus") => Event::Focus { active: flag("active").ok_or(())?, ends_at: o.get("ends_at").and_then(|v| v.as_str()).and_then(iso), minutes: number("minutes").map(|v| v.max(0.0) as u32), missed: number("missed").map(|v| v.max(0.0) as u32) },
        ("deskhub", "status") => Event::HubStatus(HubStatus {
            meeting: flag("meeting").unwrap_or(false), focus_ends_at: o.get("focus_ends_at").and_then(|v| v.as_str()).and_then(iso), speak: flag("speak").unwrap_or(true),
            claude: o.get("claude").and_then(|v| v.as_array()).map(|list| list.iter().filter_map(|v| v.as_object().and_then(claude)).collect()).unwrap_or_default(),
        }),
        _ => return Ok(None),
    }))
}

// ---------------- 回环服务 ----------------

/// 界面每帧写入：各功能开关，以及岛此刻是否可见（全屏隐藏时为 false）。
#[derive(Default)]
pub struct Gate { flags: [AtomicBool; 4], visible: AtomicBool }
impl Gate {
    pub fn set(&self, flags: [bool; 4], visible: bool) {
        for (slot, on) in self.flags.iter().zip(flags) { slot.store(on, Ordering::Relaxed); }
        self.visible.store(visible, Ordering::Relaxed);
    }
    pub fn display(&self, feature: Feature) -> bool { self.visible.load(Ordering::Relaxed) && self.flags[feature as usize].load(Ordering::Relaxed) }
}

pub struct Server { pub port: u16, pub gate: Arc<Gate>, rx: mpsc::Receiver<Event> }

impl Server {
    /// `port` 为 0 时绑定随机端口（测试用）。`wake` 在每个事件入队后调用，用来唤醒界面重绘。
    pub fn start(port: u16, wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        let gate = Arc::new(Gate::default());
        let (tx, rx) = mpsc::channel();
        let shared = gate.clone();
        std::thread::Builder::new().name("daozai-integrations".into()).spawn(move || {
            // 逐个处理：请求很小，顺序处理还能保证同一会话的事件按发送顺序到达界面。
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if let Some(event) = serve(stream, &shared) {
                    if tx.send(event).is_err() { break; }
                    wake();
                }
            }
        })?;
        Ok(Self { port, gate, rx })
    }
    pub fn try_recv(&self) -> Option<Event> { self.rx.try_recv().ok() }
}

/// 端口启动失败时记一行日志，岛仔继续运行。
pub fn log_failure(error: &std::io::Error) -> String {
    let message = format!("小工具联动端口 127.0.0.1:{PORT} 无法使用（{error}），岛上不显示小工具状态");
    let directory = crate::model::config_dir().join("diagnostics");
    if std::fs::create_dir_all(&directory).is_ok() { let _ = std::fs::write(directory.join("integrations.log"), format!("{message}\n")); }
    message
}

fn respond(stream: &mut TcpStream, code: u16, body: &str) {
    let reason = match code { 200 => "OK", 404 => "Not Found", 405 => "Method Not Allowed", 413 => "Payload Too Large", _ => "Bad Request" };
    let _ = write!(stream, "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.flush();
}

fn serve(mut stream: TcpStream, gate: &Gate) -> Option<Event> {
    if !stream.peer_addr().is_ok_and(|a| a.ip().is_loopback()) { return None; }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_nodelay(true);
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break i + 4; }
        if buf.len() > MAX_HEAD { respond(&mut stream, 413, r#"{"ok":false}"#); return None; }
        match stream.read(&mut chunk) { Ok(0) | Err(_) => return None, Ok(n) => buf.extend_from_slice(&chunk[..n]) }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut request = head.lines().next().unwrap_or("").split_whitespace();
    let (method, path) = (request.next().unwrap_or(""), request.next().unwrap_or(""));
    let path = path.split('?').next().unwrap_or("");
    let header = |name: &str| head.split("\r\n").skip(1).find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
    match (method, path) {
        ("GET", "/ping") => { respond(&mut stream, 200, &format!(r#"{{"ok":true,"app":"daozai","version":"{}"}}"#, env!("CARGO_PKG_VERSION"))); None }
        ("POST", "/event") => {
            let Some(length) = header("content-length").and_then(|v| v.parse::<usize>().ok()) else { respond(&mut stream, 400, r#"{"ok":false}"#); return None };
            if length > MAX_BODY { respond(&mut stream, 413, r#"{"ok":false}"#); return None; }
            if header("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue")) { let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n"); }
            let mut body = buf[head_end..].to_vec();
            while body.len() < length {
                match stream.read(&mut chunk) { Ok(0) | Err(_) => return None, Ok(n) => body.extend_from_slice(&chunk[..n]) }
            }
            body.truncate(length);
            match parse(&body) {
                Err(()) => { respond(&mut stream, 200, r#"{"ok":false}"#); None }
                Ok(None) => { respond(&mut stream, 200, r#"{"ok":true,"display":false}"#); None }
                Ok(Some(event)) => { respond(&mut stream, 200, &format!(r#"{{"ok":true,"display":{}}}"#, gate.display(event.feature()))); Some(event) }
            }
        }
        (_, "/event" | "/ping") => { respond(&mut stream, 405, r#"{"ok":false}"#); None }
        _ => { respond(&mut stream, 404, r#"{"ok":false}"#); None }
    }
}

// ---------------- 状态模型 ----------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoicePhase { Listening, Recognizing, Done, Result(bool), Error }

#[derive(Clone, Debug, PartialEq)]
pub struct VoiceSession {
    pub phase: VoicePhase,
    pub command: bool,
    pub mic: String,
    /// 听写中已经打出的字（流式键入时）或最终结果 / 命令回复。
    pub text: String,
    pub level: f32,
    pub level_at: f64,
    pub started: f64,
    pub phase_at: f64,
    pub last_event: f64,
}
impl VoiceSession {
    /// 结束阶段停留多久：结果 2 秒，命令回复 / 出错 3 秒；命令模式的识别结果等回复最多 8 秒。
    pub fn linger(&self) -> Option<f64> {
        match self.phase {
            VoicePhase::Done if self.command && !self.text.is_empty() => Some(8.0),
            VoicePhase::Done if self.text.is_empty() => Some(1.5),
            VoicePhase::Done => Some(2.0),
            VoicePhase::Result(_) | VoicePhase::Error => Some(3.0),
            _ => None,
        }
    }
    pub fn live(&self) -> bool { matches!(self.phase, VoicePhase::Listening | VoicePhase::Recognizing) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeSession { pub session: String, pub title: String, pub state: ClaudeState, pub since: f64 }

/// 岛上要弹出的提示（由界面层决定样式，并按设置开关过滤）。
#[derive(Clone, Debug, PartialEq)]
pub enum Show {
    BrightnessHud { percent: u8, from: Option<u8> },
    Reminder { kind: String, title: String, body: String },
    AwayCountdown(u32),
    AwayEnd,
    ClaudeWaiting { session: String, title: String, approval: bool },
    ClaudeDone { session: String, title: String },
    /// 该会话不再等待：撤下还没看的提醒。
    ClaudeEnd { session: String },
    FocusEnded { missed: Option<u32> },
}

/// 常驻指示，按优先级排列：Claude 等待 > 会议 > 专注 > 睿频。
#[derive(Clone, Debug, PartialEq)]
pub enum Indicator {
    Claude { count: usize, title: String, approval: bool },
    Meeting,
    Focus { ends_at: i64 },
    Boost { reason: String },
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub voice: Option<VoiceSession>,
    pub voice_seen: Option<f64>,
    pub brightness: Option<BrightnessStatus>,
    /// 最近一次亮度快照的时间（过期判断用）。
    pub brightness_status_at: Option<f64>,
    /// 亮度 HUD 的动画起止（from, to, 时间）。
    pub brightness_hud: Option<(f32, f32, f64)>,
    /// 黑屏倒计时：剩余秒数与收到时间。
    pub away: Option<(u32, f64)>,
    pub brightness_seen: Option<f64>,
    pub claude: Vec<ClaudeSession>,
    pub meeting: bool,
    pub focus_ends_at: Option<i64>,
    pub speak: Option<bool>,
    pub deskhub_seen: Option<f64>,
    /// AutoBoost 处于睿频时的原因；由读取 AutoBoost 状态文件的一侧填写，None 表示省电或未运行。
    pub boost: Option<String>,
}

impl State {
    pub fn voice_event(&mut self, v: Voice, t: f64) {
        self.voice_seen = Some(t);
        let fresh = |phase| VoiceSession { phase, command: v.command, mic: v.mic.clone(), text: String::new(), level: 0.0, level_at: t, started: t, phase_at: t, last_event: t };
        match v.state {
            VoiceState::Listening => self.voice = Some(fresh(VoicePhase::Listening)),
            VoiceState::Idle => self.voice = None,
            VoiceState::Level | VoiceState::Partial => {
                let Some(s) = self.voice.as_mut().filter(|s| s.phase == VoicePhase::Listening) else { return };
                s.last_event = t;
                if v.state == VoiceState::Level { s.level = v.level; s.level_at = t; } else { s.text = v.text; }
            }
            state => {
                let s = self.voice.get_or_insert_with(|| fresh(VoicePhase::Listening));
                let phase = match state { VoiceState::Recognizing => VoicePhase::Recognizing, VoiceState::Done => VoicePhase::Done, VoiceState::CommandResult => VoicePhase::Result(v.ok), _ => VoicePhase::Error };
                if phase == VoicePhase::Recognizing && s.phase != VoicePhase::Listening && s.phase != VoicePhase::Recognizing { return; }
                if s.phase != phase { s.phase_at = t; }
                if !v.mic.is_empty() { s.mic = v.mic; }
                s.command |= v.command;
                s.phase = phase;
                s.last_event = t;
                s.level = 0.0;
                if phase != VoicePhase::Recognizing { s.text = v.text; }
            }
        }
    }

    /// 非语音事件：更新状态并返回要弹出的提示。`now` 为 Unix 秒。
    pub fn apply(&mut self, event: Event, t: f64, now: i64) -> Vec<Show> {
        let mut out = Vec::new();
        match event {
            Event::Voice(v) => self.voice_event(v, t),
            Event::Brightness { percent, from, .. } => {
                self.brightness_seen = Some(t);
                if let Some(status) = &mut self.brightness { status.brightness = Some(percent); }
                let start = self.brightness_hud.filter(|(_, _, at)| t - at < 2.5).map(|(a, b, at)| a + (b - a) * ((t - at) as f32 / 0.35).clamp(0.0, 1.0))
                    .or(from.map(|v| v as f32)).unwrap_or(percent as f32);
                self.brightness_hud = Some((start, percent as f32, t));
                out.push(Show::BrightnessHud { percent, from });
            }
            Event::Reminder { kind, title, body } => {
                self.brightness_seen = Some(t);
                let title = if title.is_empty() { match kind.as_str() { "eye" => "护眼提醒", "posture" => "坐姿提醒", _ => "久坐提醒" }.into() } else { title };
                out.push(Show::Reminder { kind, title, body });
            }
            Event::Away(away) => {
                self.brightness_seen = Some(t);
                match away {
                    Away::Countdown(seconds) => { self.away = Some((seconds, t)); out.push(Show::AwayCountdown(seconds)); }
                    _ => { if self.away.take().is_some() { out.push(Show::AwayEnd); } }
                }
                if let Some(status) = &mut self.brightness {
                    status.presence = match away { Away::Blackout => Presence::Blackout, Away::Present | Away::Cancel => Presence::Present, Away::Countdown(_) => Presence::Away };
                }
            }
            Event::BrightnessStatus(status) => { self.brightness_seen = Some(t); self.brightness_status_at = Some(t); self.brightness = Some(status); }
            Event::Claude(c) => {
                self.deskhub_seen = Some(t);
                let index = self.claude.iter().position(|s| s.session == c.session);
                match c.state {
                    ClaudeState::Approval | ClaudeState::Waiting => {
                        let changed = index.map_or(true, |i| self.claude[i].state != c.state);
                        let since = index.filter(|i| self.claude[*i].state.waiting()).map_or(t, |i| self.claude[i].since);
                        let session = ClaudeSession { session: c.session.clone(), title: c.title.clone(), state: c.state, since };
                        match index { Some(i) => self.claude[i] = session, None => self.claude.push(session) }
                        if changed { out.push(Show::ClaudeWaiting { session: c.session, title: c.title, approval: c.state == ClaudeState::Approval }); }
                    }
                    ClaudeState::Working => {
                        match index { Some(i) => { self.claude[i].state = ClaudeState::Working; self.claude[i].since = t; } None => self.claude.push(ClaudeSession { session: c.session.clone(), title: c.title, state: c.state, since: t }) }
                        out.push(Show::ClaudeEnd { session: c.session });
                    }
                    ClaudeState::Done | ClaudeState::Clear => {
                        if let Some(i) = index { self.claude.remove(i); }
                        out.push(Show::ClaudeEnd { session: c.session.clone() });
                        if c.state == ClaudeState::Done { out.push(Show::ClaudeDone { session: c.session, title: c.title }); }
                    }
                }
            }
            Event::Meeting(active) => { self.deskhub_seen = Some(t); self.meeting = active; }
            Event::Focus { active, ends_at, minutes, missed } => {
                self.deskhub_seen = Some(t);
                if active {
                    self.focus_ends_at = ends_at.or(minutes.map(|m| now + m as i64 * 60));
                } else {
                    self.focus_ends_at = None;
                    out.push(Show::FocusEnded { missed });
                }
            }
            Event::HubStatus(status) => {
                self.deskhub_seen = Some(t);
                self.meeting = status.meeting;
                self.focus_ends_at = status.focus_ends_at;
                self.speak = Some(status.speak);
                let old = std::mem::take(&mut self.claude);
                for o in &old {
                    if !status.claude.iter().any(|c| c.session == o.session && c.state.waiting()) && o.state.waiting() { out.push(Show::ClaudeEnd { session: o.session.clone() }); }
                }
                self.claude = status.claude.into_iter().filter(|c| !matches!(c.state, ClaudeState::Done | ClaudeState::Clear)).map(|c| {
                    let since = old.iter().find(|o| o.session == c.session && o.state == c.state).map_or(t, |o| o.since);
                    ClaudeSession { session: c.session, title: c.title, state: c.state, since }
                }).collect();
            }
        }
        out
    }

    /// 每帧推进：语音超时、快照过期、倒计时结束。
    pub fn tick(&mut self, t: f64, now: i64) -> Vec<Show> {
        let mut out = Vec::new();
        if let Some(s) = &self.voice {
            let over = match s.linger() { Some(linger) => t - s.phase_at > linger, None => t - s.last_event > VOICE_STALE };
            if over { self.voice = None; }
        }
        if self.brightness_status_at.is_some_and(|at| t - at > BRIGHTNESS_TTL) { self.brightness = None; self.brightness_status_at = None; }
        if self.brightness_hud.is_some_and(|(_, _, at)| t - at > 3.0) { self.brightness_hud = None; }
        if self.away.is_some_and(|(seconds, at)| t - at > seconds as f64 + 2.0) { self.away = None; out.push(Show::AwayEnd); }
        if self.deskhub_seen.is_some_and(|at| t - at > DESKHUB_TTL) {
            for s in self.claude.drain(..) { out.push(Show::ClaudeEnd { session: s.session }); }
            self.meeting = false;
            self.focus_ends_at = None;
            self.speak = None;
            self.deskhub_seen = None;
        }
        if self.focus_ends_at.is_some_and(|end| end <= now) { self.focus_ends_at = None; }
        out
    }

    /// 常驻指示（已按优先级排序）；`deskhub` / `boost` 为设置里「Claude 提醒」「睿频提示」开关。
    pub fn indicators(&self, now: i64, deskhub: bool, boost: bool) -> Vec<Indicator> {
        let mut out = Vec::new();
        let waiting: Vec<_> = self.claude.iter().filter(|s| s.state.waiting()).collect();
        if deskhub {
            if let Some(first) = waiting.iter().min_by(|a, b| a.since.total_cmp(&b.since)) {
                out.push(Indicator::Claude { count: waiting.len(), title: first.title.clone(), approval: waiting.iter().any(|s| s.state == ClaudeState::Approval) });
            }
        }
        if deskhub && self.meeting { out.push(Indicator::Meeting); }
        if let Some(ends_at) = self.focus_ends_at.filter(|end| deskhub && *end > now) { out.push(Indicator::Focus { ends_at }); }
        if boost { if let Some(reason) = &self.boost { out.push(Indicator::Boost { reason: reason.clone() }); } }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(json: &str) -> Event { parse(json.as_bytes()).expect("合法事件").expect("已知事件") }

    #[test]
    fn parses_every_voice_state() {
        for (name, state) in [("listening", VoiceState::Listening), ("level", VoiceState::Level), ("partial", VoiceState::Partial), ("recognizing", VoiceState::Recognizing), ("done", VoiceState::Done), ("command_result", VoiceState::CommandResult), ("idle", VoiceState::Idle), ("error", VoiceState::Error)] {
            let Event::Voice(v) = ev(&format!(r#"{{"source":"voiceinput","kind":"voice","state":"{name}"}}"#)) else { panic!("{name}") };
            assert_eq!(v.state, state);
        }
        let Event::Voice(v) = ev(r#"{"source":"voiceinput","kind":"voice","state":"listening","mode":"command","mic":"  AJAZZ 鼠标 ","extra":1}"#) else { panic!() };
        assert!(v.command); assert_eq!(v.mic, "AJAZZ 鼠标");
        let Event::Voice(v) = ev(r#"{"source":"voiceinput","kind":"voice","state":"level","level":1.7}"#) else { panic!() };
        assert_eq!(v.level, 1.0);
        let Event::Voice(v) = ev(r#"{"source":"voiceinput","kind":"voice","state":"command_result","text":"已开始专注","ok":false}"#) else { panic!() };
        assert!(!v.ok && v.text == "已开始专注" && !v.command);
        let long = "字".repeat(500);
        let Event::Voice(v) = ev(&format!(r#"{{"source":"voiceinput","kind":"voice","state":"done","text":"{long}"}}"#)) else { panic!() };
        assert_eq!(v.text.chars().count(), TEXT_MAX + 1);
    }

    #[test]
    fn parses_autobrightness_and_deskhub_kinds() {
        assert_eq!(ev(r#"{"source":"autobrightness","kind":"brightness","percent":45,"from":38,"reason":"auto","note":"环境变暗"}"#), Event::Brightness { percent: 45, from: Some(38), manual: false, note: "环境变暗".into() });
        assert!(matches!(ev(r#"{"source":"autobrightness","kind":"brightness","percent":120,"reason":"manual"}"#), Event::Brightness { percent: 100, from: None, manual: true, .. }));
        assert_eq!(ev(r#"{"source":"autobrightness","kind":"reminder","type":"sit","title":"久坐提醒","body":"已经坐了 50 分钟"}"#), Event::Reminder { kind: "sit".into(), title: "久坐提醒".into(), body: "已经坐了 50 分钟".into() });
        for (state, away) in [(r#""countdown","seconds":10"#, Away::Countdown(10)), (r#""blackout""#, Away::Blackout), (r#""cancel""#, Away::Cancel), (r#""present""#, Away::Present)] {
            assert_eq!(ev(&format!(r#"{{"source":"autobrightness","kind":"away","state":{state}}}"#)), Event::Away(away));
        }
        for (presence, value) in [("present", Presence::Present), ("away", Presence::Away), ("blackout", Presence::Blackout)] {
            assert_eq!(ev(&format!(r#"{{"source":"autobrightness","kind":"status","auto":false,"brightness":45,"presence":"{presence}","next_measure_s":120}}"#)), Event::BrightnessStatus(BrightnessStatus { auto: false, brightness: Some(45), presence: value, next_measure_s: Some(120) }));
        }
        for (name, state) in [("approval", ClaudeState::Approval), ("waiting", ClaudeState::Waiting), ("working", ClaudeState::Working), ("done", ClaudeState::Done), ("clear", ClaudeState::Clear)] {
            assert_eq!(ev(&format!(r#"{{"source":"deskhub","kind":"claude","session":"s1","title":"daozai","state":"{name}","at":"2026-11-05T09:00:00+08:00"}}"#)), Event::Claude(Claude { session: "s1".into(), title: "daozai".into(), state }));
        }
        assert_eq!(ev(r#"{"source":"deskhub","kind":"meeting","active":true}"#), Event::Meeting(true));
        assert_eq!(ev(r#"{"source":"deskhub","kind":"focus","active":true,"ends_at":"2026-11-05T10:00:00Z","minutes":25}"#), Event::Focus { active: true, ends_at: Some(1793872800), minutes: Some(25), missed: None });
        assert_eq!(ev(r#"{"source":"deskhub","kind":"focus","active":false,"missed":3}"#), Event::Focus { active: false, ends_at: None, minutes: None, missed: Some(3) });
        let Event::HubStatus(s) = ev(r#"{"source":"deskhub","kind":"status","meeting":true,"focus_ends_at":null,"speak":false,"claude":[{"session":"a","title":"A","state":"waiting"},{"session":"b","state":"bogus"}]}"#) else { panic!() };
        assert!(s.meeting && !s.speak && s.focus_ends_at.is_none());
        assert_eq!(s.claude, vec![Claude { session: "a".into(), title: "A".into(), state: ClaudeState::Waiting }]);
    }

    #[test]
    fn malformed_and_unknown_events_are_told_apart() {
        for bad in ["", "not json", "[]", r#"{"kind":"voice"}"#, r#"{"source":"voiceinput","kind":"voice"}"#, r#"{"source":"autobrightness","kind":"brightness"}"#, r#"{"source":"deskhub","kind":"meeting"}"#] {
            assert_eq!(parse(bad.as_bytes()), Err(()), "{bad}");
        }
        for unknown in [r#"{"source":"voiceinput","kind":"weather"}"#, r#"{"source":"other","kind":"voice","state":"listening"}"#, r#"{"source":"voiceinput","kind":"voice","state":"dancing"}"#, r#"{"source":"deskhub","kind":"claude","state":"sleeping"}"#] {
            assert_eq!(parse(unknown.as_bytes()), Ok(None), "{unknown}");
        }
        assert_eq!(iso("2026-11-05T10:00:00.1234567+08:00"), Some(1793844000));
        assert!(iso("明天").is_none());
    }

    fn post(port: u16, request: &[u8]) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(request).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }
    fn event_request(body: &str) -> Vec<u8> { format!("POST /event HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes() }

    #[test]
    fn server_round_trip_reports_display_flag() {
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let server = Server::start(0, move || { counter.fetch_add(1, Ordering::SeqCst); }).unwrap();
        let port = server.port;
        assert_ne!(port, PORT);
        let ping = post(port, b"GET /ping HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(ping.starts_with("HTTP/1.1 200") && ping.contains(r#""app":"daozai""#) && ping.contains(env!("CARGO_PKG_VERSION")), "{ping}");
        let listening = r#"{"source":"voiceinput","kind":"voice","state":"listening","mic":"C925e"}"#;
        assert!(post(port, &event_request(listening)).ends_with(r#"{"ok":true,"display":false}"#), "开关没打开时不显示");
        server.gate.set([true, false, false, false], true);
        assert!(post(port, &event_request(listening)).ends_with(r#"{"ok":true,"display":true}"#));
        assert!(post(port, &event_request(r#"{"source":"autobrightness","kind":"brightness","percent":40}"#)).ends_with(r#""display":false}"#), "亮度开关关闭");
        server.gate.set([true, true, true, true], false);
        assert!(post(port, &event_request(listening)).ends_with(r#""display":false}"#), "全屏隐藏时不显示");
        assert!(post(port, &event_request("{oops")).ends_with(r#"{"ok":false}"#));
        assert!(post(port, &event_request(r#"{"source":"voiceinput","kind":"unknown"}"#)).ends_with(r#"{"ok":true,"display":false}"#));
        let mut received = Vec::new();
        for _ in 0..100 { while let Some(e) = server.try_recv() { received.push(e); } if received.len() >= 4 { break; } std::thread::sleep(Duration::from_millis(10)); }
        assert_eq!(received.len(), 4, "合法事件都应转交界面");
        assert!(matches!(&received[0], Event::Voice(v) if v.mic == "C925e"));
        assert_eq!(woke.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn server_rejects_wrong_method_path_and_oversize_body() {
        let server = Server::start(0, || {}).unwrap();
        let port = server.port;
        assert!(post(port, b"GET /event HTTP/1.1\r\nHost: x\r\n\r\n").starts_with("HTTP/1.1 405"));
        assert!(post(port, b"PUT /ping HTTP/1.1\r\nContent-Length: 0\r\n\r\n").starts_with("HTTP/1.1 405"));
        assert!(post(port, b"POST /other HTTP/1.1\r\nContent-Length: 0\r\n\r\n").starts_with("HTTP/1.1 404"));
        assert!(post(port, b"POST /event HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 400"), "缺少长度");
        let big = format!(r#"{{"source":"voiceinput","kind":"voice","state":"done","text":"{}"}}"#, "x".repeat(MAX_BODY));
        let reply = post(port, &event_request(&big));
        assert!(reply.starts_with("HTTP/1.1 413"), "{reply}");
        // 恰好 64 KiB 以内的仍然接收（Expect: 100-continue 也支持）。
        let fits = format!(r#"{{"source":"voiceinput","kind":"voice","state":"done","text":"{}"}}"#, "x".repeat(MAX_BODY - 80));
        let request = format!("POST /event HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n{fits}", fits.len());
        let reply = post(port, request.as_bytes());
        assert!(reply.starts_with("HTTP/1.1 100") && reply.ends_with(r#"{"ok":true,"display":false}"#), "{reply}");
        std::thread::sleep(Duration::from_millis(50));
        assert!(matches!(server.try_recv(), Some(Event::Voice(_))));
        assert!(server.try_recv().is_none(), "被拒绝的请求不能进入界面");
        // 端口被占用时返回错误，由外壳记录后继续运行。
        assert!(Server::start(port, || {}).is_err());
    }

    fn voice(state: VoiceState) -> Voice { Voice { state, command: false, mic: String::new(), level: 0.0, text: String::new(), ok: true } }

    #[test]
    fn voice_session_state_machine_and_stale_timeout() {
        let mut s = State::default();
        s.voice_event(Voice { mic: "C925e".into(), command: true, ..voice(VoiceState::Listening) }, 1.0);
        s.voice_event(Voice { level: 0.6, ..voice(VoiceState::Level) }, 1.1);
        let v = s.voice.clone().unwrap();
        assert_eq!((v.phase, v.level, v.mic.as_str(), v.command), (VoicePhase::Listening, 0.6, "C925e", true));
        s.voice_event(Voice { text: "你好".into(), ..voice(VoiceState::Partial) }, 1.5);
        assert_eq!(s.voice.as_ref().unwrap().text, "你好");
        s.voice_event(voice(VoiceState::Recognizing), 2.0);
        assert_eq!(s.voice.as_ref().unwrap().phase, VoicePhase::Recognizing);
        s.voice_event(Voice { level: 0.9, ..voice(VoiceState::Level) }, 2.1);
        assert_eq!(s.voice.as_ref().unwrap().level, 0.0, "识别中不再跟随音量");
        s.voice_event(Voice { text: "开始专注".into(), ..voice(VoiceState::Done) }, 3.0);
        s.voice_event(Voice { text: "已开始 25 分钟专注".into(), ok: true, ..voice(VoiceState::CommandResult) }, 4.0);
        let v = s.voice.clone().unwrap();
        assert_eq!((v.phase, v.text.as_str(), v.command), (VoicePhase::Result(true), "已开始 25 分钟专注", true));
        s.tick(6.9, 0); assert!(s.voice.is_some());
        s.tick(7.1, 0); assert!(s.voice.is_none(), "命令回复 3 秒后收起");

        // 听写结果约 2 秒后收起；迟到的 recognizing 不会把结果拉回识别中。
        s.voice_event(voice(VoiceState::Listening), 10.0);
        s.voice_event(Voice { text: "好的".into(), ..voice(VoiceState::Done) }, 11.0);
        s.voice_event(voice(VoiceState::Recognizing), 11.2);
        assert_eq!(s.voice.as_ref().unwrap().phase, VoicePhase::Done);
        s.tick(12.9, 0); assert!(s.voice.is_some());
        s.tick(13.1, 0); assert!(s.voice.is_none());

        // 卡在听写或识别中，20 秒无事件视为结束；中途的音量事件会续期。
        s.voice_event(voice(VoiceState::Listening), 20.0);
        s.voice_event(Voice { level: 0.2, ..voice(VoiceState::Level) }, 30.0);
        s.tick(49.0, 0); assert!(s.voice.is_some());
        s.tick(50.5, 0); assert!(s.voice.is_none());
        s.voice_event(voice(VoiceState::Recognizing), 60.0);
        assert_eq!(s.voice.as_ref().unwrap().phase, VoicePhase::Recognizing, "漏掉 listening 也能显示识别中");
        s.tick(80.5, 0); assert!(s.voice.is_none());

        // idle 立即结束；没有会话时 level 不会凭空创建会话。
        s.voice_event(voice(VoiceState::Listening), 90.0);
        s.voice_event(voice(VoiceState::Idle), 90.5);
        assert!(s.voice.is_none());
        s.voice_event(Voice { level: 0.5, ..voice(VoiceState::Level) }, 91.0);
        assert!(s.voice.is_none());
        s.voice_event(Voice { text: "麦克风不可用".into(), ..voice(VoiceState::Error) }, 92.0);
        assert_eq!(s.voice.as_ref().unwrap().phase, VoicePhase::Error);
        s.tick(95.5, 0); assert!(s.voice.is_none());
    }

    fn claude(session: &str, state: ClaudeState) -> Event { Event::Claude(Claude { session: session.into(), title: format!("项目 {session}"), state }) }

    #[test]
    fn claude_sessions_drive_indicators_and_notices() {
        let mut s = State::default();
        assert!(s.indicators(0, true, true).is_empty());
        assert_eq!(s.apply(claude("a", ClaudeState::Waiting), 1.0, 0), vec![Show::ClaudeWaiting { session: "a".into(), title: "项目 a".into(), approval: false }]);
        assert!(s.apply(claude("a", ClaudeState::Waiting), 2.0, 0).is_empty(), "重复推送不再弹提醒");
        s.apply(claude("b", ClaudeState::Approval), 3.0, 0);
        assert_eq!(s.indicators(0, true, true), vec![Indicator::Claude { count: 2, title: "项目 a".into(), approval: true }]);
        assert!(s.indicators(0, false, true).is_empty(), "关闭 Claude 提醒后不常驻");
        assert_eq!(s.apply(claude("a", ClaudeState::Working), 4.0, 0), vec![Show::ClaudeEnd { session: "a".into() }]);
        assert_eq!(s.indicators(0, true, true), vec![Indicator::Claude { count: 1, title: "项目 b".into(), approval: true }]);
        assert_eq!(s.apply(claude("b", ClaudeState::Done), 5.0, 0), vec![Show::ClaudeEnd { session: "b".into() }, Show::ClaudeDone { session: "b".into(), title: "项目 b".into() }]);
        assert!(s.indicators(0, true, true).is_empty());
        s.apply(claude("c", ClaudeState::Waiting), 6.0, 0);
        s.apply(claude("c", ClaudeState::Clear), 7.0, 0);
        assert_eq!(s.claude.iter().map(|c| (c.session.as_str(), c.state)).collect::<Vec<_>>(), vec![("a", ClaudeState::Working)]);

        // 优先级：Claude 等待 > 会议 > 专注 > 睿频。
        s.boost = Some("编译中".into());
        s.apply(Event::Focus { active: true, ends_at: None, minutes: Some(25), missed: None }, 8.0, 1000);
        s.apply(Event::Meeting(true), 8.0, 1000);
        s.apply(claude("d", ClaudeState::Waiting), 8.0, 1000);
        let list = s.indicators(1000, true, true);
        assert!(matches!(list.as_slice(), [Indicator::Claude { .. }, Indicator::Meeting, Indicator::Focus { ends_at: 2500 }, Indicator::Boost { .. }]), "{list:?}");
        assert_eq!(s.indicators(1000, true, false).len(), 3);
        assert_eq!(s.indicators(2600, true, true).len(), 3, "专注到点后不再显示");
        assert_eq!(s.apply(Event::Focus { active: false, ends_at: None, minutes: None, missed: Some(3) }, 9.0, 1000), vec![Show::FocusEnded { missed: Some(3) }]);
    }

    #[test]
    fn snapshots_reconcile_and_expire() {
        let mut s = State::default();
        s.apply(claude("a", ClaudeState::Waiting), 1.0, 0);
        s.apply(claude("b", ClaudeState::Approval), 1.0, 0);
        s.apply(Event::Meeting(true), 1.0, 0);
        let snapshot = |claude: Vec<Claude>| Event::HubStatus(HubStatus { meeting: false, focus_ends_at: Some(5000), speak: true, claude });
        let shows = s.apply(snapshot(vec![Claude { session: "b".into(), title: "B".into(), state: ClaudeState::Approval }, Claude { session: "c".into(), title: "C".into(), state: ClaudeState::Waiting }]), 10.0, 0);
        assert_eq!(shows, vec![Show::ClaudeEnd { session: "a".into() }], "快照里没有的会话撤下，新出现的静默加入");
        assert_eq!(s.claude.iter().map(|c| (c.session.as_str(), c.since)).collect::<Vec<_>>(), vec![("b", 1.0), ("c", 10.0)]);
        assert!(!s.meeting && s.focus_ends_at == Some(5000));
        s.tick(99.0, 0); assert_eq!(s.claude.len(), 2);
        s.apply(snapshot(vec![Claude { session: "b".into(), title: "B".into(), state: ClaudeState::Approval }]), 60.0, 0);
        s.tick(149.0, 0); assert_eq!(s.claude.len(), 1, "新快照续期");
        let shows = s.tick(151.0, 0);
        assert!(s.claude.is_empty() && s.focus_ends_at.is_none() && s.indicators(0, true, true).is_empty(), "90 秒没有快照，DeskHub 状态全部作废");
        assert_eq!(shows, vec![Show::ClaudeEnd { session: "b".into() }]);

        s.apply(Event::BrightnessStatus(BrightnessStatus { auto: true, brightness: Some(40), presence: Presence::Present, next_measure_s: Some(60) }), 200.0, 0);
        s.apply(Event::Brightness { percent: 55, from: Some(40), manual: false, note: String::new() }, 210.0, 0);
        assert_eq!(s.brightness.as_ref().unwrap().brightness, Some(55));
        s.tick(379.0, 0); assert!(s.brightness.is_some(), "亮度变化不续期快照，但仍在 180 秒内");
        s.tick(381.0, 0); assert!(s.brightness.is_none());

        assert_eq!(s.apply(Event::Away(Away::Countdown(10)), 400.0, 0), vec![Show::AwayCountdown(10)]);
        assert_eq!(s.apply(Event::Away(Away::Cancel), 403.0, 0), vec![Show::AwayEnd]);
        assert!(s.apply(Event::Away(Away::Present), 404.0, 0).is_empty());
        s.apply(Event::Away(Away::Countdown(5)), 410.0, 0);
        assert_eq!(s.tick(417.5, 0), vec![Show::AwayEnd], "倒计时结束后自动撤下");
    }
}
