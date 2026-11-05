//! 小工具页：AutoBrightness、VoiceInput、DeskHub、AutoBoost 四张小卡片（2×2）。
//!
//! 每张卡片：运行状态点、一行实时状态、一个主操作。
//!   1. 是否在运行：各自的单实例互斥体，或最近收到过它推来的事件（见 `integrations`）；
//!   2. 实时状态优先用推送的事件 / 快照（AutoBoost 读 `state.json`），没有时退回 exe 同目录的 `config.json`；
//!   3. 主操作：打开 / 启动 / 获取；DeskHub 为专注 25 分钟、清除提醒（经 127.0.0.1:47800），
//!      AutoBoost 为睿频 1 小时、恢复、取消强制（写 `control.json`，右键暂停 / 恢复自动切换）。
//!
//! 安装位置默认 `%USERPROFILE%\<文件夹>\<exe>`（各自 README 里的发布位置），
//! 也可以在 `%APPDATA%\Daozai\tools.txt` 里逐行写 `autobrightness=D:\路径\AutoBrightness.exe` 覆盖
//! （键为 autobrightness / voiceinput / deskhub / autoboost，值可以是 exe 或它所在的文件夹）。

use crate::anim::lerp;
use crate::app::{App, Cmd};
use crate::paint::{capsule, Card, H};
use crate::theme::{self, size, W};
use eframe::egui::Color32;
use std::path::{Path, PathBuf};

pub struct Spec {
    pub id: &'static str,
    pub name: &'static str,
    pub folder: &'static str,
    pub exe: &'static str,
    /// 工具启动时创建的命名互斥体（Windows 下用来判断是否在运行）。
    pub mutex: &'static str,
    pub repo: &'static str,
    pub accent: (Color32, Color32),
}

pub const SPECS: [Spec; 4] = [
    Spec {
        id: "autobrightness",
        name: "自动亮度",
        folder: "AutoBrightness",
        exe: "AutoBrightness.exe",
        mutex: "AutoBrightness.SingleInstance",
        repo: "https://github.com/xuxiao2695/AutoBrightness",
        accent: (Color32::from_rgb(255, 205, 84), Color32::from_rgb(255, 133, 52)),
    },
    Spec {
        id: "voiceinput",
        name: "语音输入",
        folder: "VoiceInput",
        exe: "VoiceInput.exe",
        mutex: "VoiceInput.SingleInstance",
        repo: "https://github.com/xuxiao2695/VoiceInput",
        accent: (Color32::from_rgb(98, 190, 255), Color32::from_rgb(116, 86, 255)),
    },
    Spec {
        id: "deskhub",
        name: "DeskHub",
        folder: "DeskHub",
        exe: "DeskHub.exe",
        mutex: "DeskHub.SingleInstance",
        repo: "https://github.com/xuxiao2695/deskhub",
        accent: (Color32::from_rgb(255, 128, 178), Color32::from_rgb(170, 92, 255)),
    },
    Spec {
        id: "autoboost",
        name: "自动睿频",
        folder: "AutoBoost",
        exe: "AutoBoost.ps1",
        mutex: crate::autoboost::MUTEX,
        repo: "https://github.com/xuxiao2695/autoboost",
        accent: (Color32::from_rgb(255, 184, 64), Color32::from_rgb(255, 106, 36)),
    },
];
pub const AUTOBRIGHTNESS: usize = 0;
pub const VOICEINPUT: usize = 1;
pub const DESKHUB: usize = 2;
pub const AUTOBOOST: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Stopped,
    Missing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub status: Status,
    pub exe: Option<PathBuf>,
    pub lines: [String; 3],
}

impl Info {
    fn empty() -> Self {
        Self { status: Status::Missing, exe: None, lines: Default::default() }
    }
}

/// 每隔几秒重新探测一次，页面不显示时不探测。
pub struct Cache {
    infos: Vec<Info>,
    at: f64,
    #[cfg(test)]
    pinned: bool,
}

impl Default for Cache {
    fn default() -> Self {
        Self { infos: SPECS.iter().map(|_| Info::empty()).collect(), at: f64::NEG_INFINITY, #[cfg(test)] pinned: false }
    }
}

fn key_name(vk: i64) -> String {
    match vk {
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0xA2 => "左 Ctrl".into(),
        0xA3 => "右 Ctrl".into(),
        0x14 => "Caps Lock".into(),
        _ => format!("键码 {vk:#x}"),
    }
}

fn duration_text(seconds: i64) -> String {
    if seconds >= 60 && seconds % 60 == 0 { format!("{} 分钟", seconds / 60) } else { format!("{seconds} 秒") }
}

fn on_off(value: Option<bool>, default: bool) -> &'static str {
    if value.unwrap_or(default) { "开" } else { "关" }
}

/// 读不到配置（还没生成、或被占用）时用各工具自己的默认值。
pub fn describe(id: &str, config: &serde_json::Value) -> [String; 3] {
    let flag = |key: &str| config.get(key).and_then(|v| v.as_bool());
    let number = |key: &str| config.get(key).and_then(|v| v.as_i64());
    match id {
        "autobrightness" => [
            format!("自动亮度 {}", on_off(flag("AutoEnabled"), true)),
            format!("每 {}测一次光", duration_text(number("IntervalSeconds").filter(|v| *v > 0).unwrap_or(300))),
            if flag("AwayEnabled").unwrap_or(true) {
                format!("离座 {}后黑屏", duration_text(number("AwaySeconds").filter(|v| *v > 0).unwrap_or(60)))
            } else {
                "离座黑屏 关".into()
            },
        ],
        "voiceinput" => {
            let trigger = config.get("Trigger");
            let trigger_vk = trigger.and_then(|t| t.get("Vk")).and_then(|v| v.as_i64()).unwrap_or(0x7C);
            let trigger_is_key = trigger.and_then(|t| t.get("Kind")).and_then(|v| v.as_str()).map_or(true, |k| k == "Key");
            let command = number("CommandVk").unwrap_or(0x7D);
            let alt = number("AltVk").unwrap_or(0x7E);
            let mut keys = Vec::new();
            if command > 0 { keys.push(format!("{} 指令", key_name(command))); }
            if alt > 0 { keys.push(format!("{} 副麦", key_name(alt))); }
            [
                if trigger_is_key { format!("按住 {} 听写", key_name(trigger_vk)) } else { "按住鼠标键听写".into() },
                if keys.is_empty() { "仅听写".into() } else { keys.join(" · ") },
                format!("{} 识别 · 整理 {}", if flag("UseGpuAsr").unwrap_or(true) { "GPU" } else { "CPU" }, on_off(flag("Polish"), true)),
            ]
        }
        "deskhub" => [
            format!("语音播报 {}", on_off(flag("Speak"), true)),
            format!("岛仔联动 {}", on_off(flag("IslandEnabled"), true)),
            String::new(),
        ],
        "autoboost" => ["按负载自动开关睿频".into(), String::new(), String::new()],
        _ => Default::default(),
    }
}

pub(crate) fn overrides(config_dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(text) = std::fs::read_to_string(config_dir.join("tools.txt")) else { return Vec::new() };
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), PathBuf::from(v.trim())))
        .filter(|(_, path)| !path.as_os_str().is_empty())
        .collect()
}

/// 探测一个工具。`running` 由调用方提供（Windows 查互斥体），便于测试。
pub fn probe(spec: &Spec, home: Option<&Path>, config_dir: &Path, running: impl Fn(&str) -> bool) -> Info {
    let custom = overrides(config_dir).into_iter().find(|(k, _)| k == spec.id).map(|(_, path)| if path.is_dir() { path.join(spec.exe) } else { path });
    let exe = custom
        .into_iter()
        .chain(home.map(|home| home.join(spec.folder).join(spec.exe)))
        .find(|path| path.is_file());
    let is_running = running(spec.mutex);
    let Some(exe) = exe else {
        return Info {
            status: if is_running { Status::Running } else { Status::Missing },
            exe: None,
            lines: [
                "没找到安装位置".into(),
                format!("默认 用户目录\\{}", spec.folder),
                "或写进 Daozai\\tools.txt".into(),
            ],
        };
    };
    let config = exe
        .parent()
        .and_then(|dir| std::fs::read_to_string(dir.join("config.json")).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or(serde_json::Value::Null);
    Info {
        status: if is_running { Status::Running } else { Status::Stopped },
        lines: describe(spec.id, &config),
        exe: Some(exe),
    }
}

#[cfg(windows)]
pub(crate) fn mutex_exists(name: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(wide.as_ptr())) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn mutex_exists(_name: &str) -> bool {
    false
}

pub(crate) fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

impl Cache {
    fn refresh(&mut self, now: f64) {
        #[cfg(test)] if self.pinned { return; }
        if now - self.at < 2.0 && now >= self.at { return; }
        self.at = now;
        let home = user_home();
        let dir = crate::model::config_dir();
        self.infos = SPECS.iter().map(|spec| probe(spec, home.as_deref(), &dir, mutex_exists)).collect();
    }
}

#[cfg(test)]
impl Cache {
    /// 预览／测试用：直接放入探测结果，之后不再探测本机。
    pub(crate) fn set(&mut self, infos: Vec<Info>, now: f64) {
        self.infos = infos;
        self.at = now;
        self.pinned = true;
    }
}

// ---------- 实时状态与操作 ----------

/// 卡片的主操作。
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Open(PathBuf),
    Url(&'static str),
    /// POST 到 DeskHub 的控制口（路径含查询串）。
    Hub(&'static str),
    /// 写 AutoBoost 的 control.json。
    Boost { paused: bool, force_until: Option<i64> },
    StartBoost,
}

const AMBER: Color32 = Color32::from_rgb(255, 176, 46);
const ORANGE: Color32 = Color32::from_rgb(255, 140, 40);
const PURPLE: Color32 = Color32::from_rgb(178, 140, 255);
const MIC_RED: Color32 = Color32::from_rgb(255, 92, 82);

fn clock(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0).map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default()
}

/// 「 · 」分隔的几段：整段放得下才放，第一段放不下时截断；末尾还有不少空间时把下一段截断放进去。
fn fit_parts(c: &Card, line: &str, max_w: f32) -> String {
    let mut parts = line.split(" · ");
    let mut out = c.fit(parts.next().unwrap_or(""), size::TINY, W::Regular, max_w);
    for part in parts {
        let candidate = format!("{out} · {part}");
        if c.text_w(&candidate, size::TINY, W::Regular) <= max_w { out = candidate; continue; }
        if max_w - c.text_w(&out, size::TINY, W::Regular) >= 56.0 { out = c.fit(&candidate, size::TINY, W::Regular, max_w); }
        break;
    }
    out
}

fn joined(lines: &[String]) -> String { lines.iter().filter(|l| !l.is_empty()).cloned().collect::<Vec<_>>().join(" · ") }

impl App {
    /// 合并探测结果与推送来的事件：最近有事件说明它在运行（非 Windows 或互斥体名变了时也成立）。
    pub(crate) fn tool_status(&self, i: usize, info: &Info) -> Status {
        let s = &self.integrations;
        let alive = match i {
            AUTOBRIGHTNESS => s.brightness.is_some(),
            VOICEINPUT => s.voice.is_some() || s.voice_seen.is_some_and(|at| self.t - at < 90.0),
            DESKHUB => s.deskhub_seen.is_some(),
            _ => self.autoboost.running,
        };
        if alive { Status::Running } else { info.status }
    }

    /// 卡片第二行：一行实时状态与它的颜色。
    pub(crate) fn tool_live(&self, i: usize, info: &Info) -> (String, Color32) {
        let status = self.tool_status(i, info);
        let s = &self.integrations;
        let now = chrono::Utc::now().timestamp();
        if status == Status::Missing { return (format!("未找到 用户目录\\{}", SPECS[i].folder), theme::DIM); }
        // 没有实时数据：用 config.json 的关键设置（自动亮度开着时不必再说一遍）。
        let lines = if i == AUTOBRIGHTNESS && info.lines[0].ends_with('开') { &info.lines[1..] } else { &info.lines[..] };
        let fallback = (joined(lines), theme::GRAY);
        match i {
            AUTOBRIGHTNESS => {
                let Some(b) = s.brightness.as_ref().filter(|_| status == Status::Running) else { return fallback };
                let mut parts = vec![match b.brightness { Some(v) => format!("亮度 {v}%"), None => "亮度 --".into() }];
                match b.presence {
                    crate::integrations::Presence::Away => parts.push("离座中".into()),
                    crate::integrations::Presence::Blackout => parts.push("已黑屏".into()),
                    _ if !b.auto => parts.push("手动".into()),
                    _ => parts.push(match b.next_measure_s { Some(n) if n >= 60 => format!("{} 分钟后测光", n.div_ceil(60)), Some(n) => format!("{n} 秒后测光"), None => "自动".into() }),
                }
                (parts.join(" · "), theme::GRAY)
            }
            VOICEINPUT => match s.voice.as_ref().map(|v| v.phase) {
                Some(crate::integrations::VoicePhase::Listening) => {
                    let v = s.voice.as_ref().unwrap();
                    (if v.mic.is_empty() { "正在听…".into() } else { format!("正在听 · {}", v.mic) }, MIC_RED)
                }
                Some(crate::integrations::VoicePhase::Recognizing) => ("识别中…".into(), theme::CYAN),
                _ => fallback,
            },
            DESKHUB => {
                if s.deskhub_seen.is_none() { return fallback; }
                let waiting: Vec<_> = s.claude.iter().filter(|c| c.state.waiting()).collect();
                if let [one] = waiting.as_slice() {
                    return (format!("{} · {}", if one.state == crate::integrations::ClaudeState::Approval { "Claude 等你批准" } else { "Claude 等你回复" }, one.title), AMBER);
                }
                if waiting.len() > 1 { return (format!("{} 个 Claude 会话在等你", waiting.len()), AMBER); }
                if let Some(end) = s.focus_ends_at.filter(|end| *end > now) { return (format!("专注中 · 还剩 {} 分钟", (end - now + 59) / 60), PURPLE); }
                if s.meeting { return ("会议中 · 提醒已静音".into(), MIC_RED); }
                let working = s.claude.iter().filter(|c| c.state == crate::integrations::ClaudeState::Working).count();
                let speak = match s.speak { Some(true) => " · 播报 开", Some(false) => " · 播报 关", None => "" };
                (if working > 0 { format!("Claude 工作中 {working} 个{speak}") } else { format!("空闲{speak}") }, theme::GRAY)
            }
            _ => {
                let Some(b) = self.autoboost.live() else { return ("主脚本没在运行".into(), theme::GRAY) };
                use crate::autoboost::Mode;
                if b.forcing(now) { (format!("强制睿频到 {}", clock(b.force_until.unwrap())), ORANGE) }
                else if b.paused { (format!("已暂停 · 当前{}", match b.mode { Mode::Boost => "睿频", Mode::Eco => "省电", Mode::Other => "其他计划" }), theme::GRAY) }
                else if b.manual() { ("未接管 · 手动选了电源计划".into(), theme::GRAY) }
                else if b.mode == Mode::Boost { (if b.reason.is_empty() { "睿频中".into() } else { format!("睿频中 · {}", b.reason) }, ORANGE) }
                else { (match b.since { Some(t) => format!("省电 · {} 起", clock(t)), None => "省电".into() }, theme::GREEN) }
            }
        }
    }

    /// 主操作：按钮文字、动作、是否强调、悬停说明。
    pub(crate) fn tool_action(&self, i: usize, info: &Info) -> Option<(&'static str, Action, bool, String)> {
        let spec = &SPECS[i];
        let status = self.tool_status(i, info);
        let get = || Some(("获取", Action::Url(spec.repo), false, format!("打开 {} 主页", spec.repo)));
        match i {
            DESKHUB if status == Status::Running => {
                let s = &self.integrations;
                Some(if s.focus_ends_at.is_some() { ("结束专注", Action::Hub("/focus?minutes=0"), false, "让 DeskHub 结束专注".into()) }
                else if s.claude.iter().any(|c| c.state.waiting()) { ("清除提醒", Action::Hub("/clear"), true, "清除 DeskHub 的 Claude 提醒灯（同单击托盘图标）".into()) }
                else { ("专注 25 分钟", Action::Hub("/focus?minutes=25"), true, "让 DeskHub 开始 25 分钟专注，期间消息静音".into()) })
            }
            AUTOBOOST => match status {
                Status::Missing => get(),
                Status::Stopped => Some(("启动", Action::StartBoost, true, format!("运行计划任务「{}」（不弹窗口）", crate::autoboost::TASK))),
                Status::Running => {
                    let now = chrono::Utc::now().timestamp();
                    let (paused, forcing) = self.autoboost.state.as_ref().map_or((false, false), |b| (b.paused, b.forcing(now)));
                    let force = self.autoboost.state.as_ref().and_then(|b| b.force_until);
                    let tip = if paused { "右键：恢复自动切换" } else { "右键：暂停自动切换" };
                    Some(if forcing { ("取消强制", Action::Boost { paused, force_until: None }, false, format!("回到自动切换\n{tip}")) }
                    else if paused { ("恢复", Action::Boost { paused: false, force_until: force }, true, "恢复自动切换".into()) }
                    else { ("睿频 1 小时", Action::Boost { paused, force_until: Some(now + 3600) }, false, format!("强制睿频到 {}\n{tip}", clock(now + 3600))) })
                }
            },
            _ => match (&info.exe, status) {
                (Some(path), Status::Running) => Some(("打开", Action::Open(path.clone()), false, format!("{}（再次打开会把窗口带到前台）", path.display()))),
                (Some(path), _) => Some(("启动", Action::Open(path.clone()), true, path.display().to_string())),
                (None, Status::Running) => None,
                (None, _) => get(),
            },
        }
    }

    pub(crate) fn run_tool_action(&mut self, action: Action) {
        match action {
            Action::Open(path) => self.pending.push(Cmd::Open(path)),
            Action::Url(url) => self.pending.push(Cmd::OpenUrl(url.into())),
            Action::Hub(path) => self.pending.push(Cmd::DeskHub(path.into())),
            Action::StartBoost => { self.pending.push(Cmd::RunHidden("schtasks".into(), vec!["/Run".into(), "/TN".into(), crate::autoboost::TASK.into()])); self.autoboost.soon(self.t + 1.5); }
            Action::Boost { paused, force_until } => {
                let Some(folder) = self.autoboost.folder.clone() else { return };
                match crate::autoboost::write_control(&folder, paused, force_until) {
                    Ok(()) => {
                        // 先在界面上反映，主脚本下一次采样后 state.json 会跟上。
                        if let Some(b) = &mut self.autoboost.state { b.paused = paused; b.force_until = force_until; }
                        self.autoboost.soon(self.t);
                    }
                    Err(e) => self.say(format!("无法写入 AutoBoost 指令：{e}")),
                }
            }
        }
        self.tools.at = self.t - 1.0;
    }
}

// ---------- 绘制 ----------

pub(crate) const GRID: (f32, f32, f32, f32) = (40.0, 33.0, 166.0, 45.0);
const GAP: (f32, f32) = (7.0, 5.0);

fn glyph_sun(c: &Card, cx: f32, cy: f32) {
    c.circle(cx, cy, 3.6, theme::WHITE);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let (s, k) = a.sin_cos();
        c.seg((cx + k * 6.0, cy + s * 6.0), (cx + k * 8.2, cy + s * 8.2), 1.5, theme::WHITE);
    }
}

fn glyph_mic(c: &Card, cx: f32, cy: f32) {
    c.fill(cx - 3.0, cy - 8.0, 6.0, 11.0, 3.0, theme::WHITE);
    c.path(&[(cx - 6.0, cy - 1.0), (cx - 6.0, cy + 2.0), (cx - 3.5, cy + 5.0), (cx, cy + 5.8), (cx + 3.5, cy + 5.0), (cx + 6.0, cy + 2.0), (cx + 6.0, cy - 1.0)], 1.4, theme::WHITE);
    c.seg((cx, cy + 5.8), (cx, cy + 8.5), 1.4, theme::WHITE);
}

/// 闪电：两个凸三角拼成，`k` 为缩放。
pub(crate) fn bolt(c: &Card, x: f32, y: f32, k: f32, color: Color32) {
    c.poly(&[(x + 2.6 * k, y - 8.0 * k), (x - 4.6 * k, y + 1.2 * k), (x + 0.9 * k, y + 1.2 * k)], color);
    c.poly(&[(x - 0.9 * k, y - 1.2 * k), (x + 4.6 * k, y - 1.2 * k), (x - 2.6 * k, y + 8.0 * k)], color);
}

fn glyph(c: &Card, i: usize, cx: f32, cy: f32) {
    match i {
        AUTOBRIGHTNESS => glyph_sun(&c.scaled(cx, cy, 0.78), cx, cy),
        VOICEINPUT => glyph_mic(&c.scaled(cx, cy, 0.78), cx, cy),
        DESKHUB => crate::paint::bell(&c.scaled(cx, cy, 0.85), cx, cy + 0.5, theme::WHITE, 0.0),
        _ => bolt(c, cx, cy, 0.85, theme::WHITE),
    }
}

impl App {
    pub(crate) fn page_tools(&mut self, c: &Card) {
        self.tools.refresh(self.t);
        c.ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
        let (left, top, width, height) = GRID;
        let mut chosen = None;
        for (i, spec) in SPECS.iter().enumerate() {
            let info = self.tools.infos.get(i).cloned().unwrap_or_else(Info::empty);
            let status = self.tool_status(i, &info);
            let x = left + (i % 2) as f32 * (width + GAP.0);
            let y = top + (i / 2) as f32 * (height + GAP.1);
            c.fill(x, y, width, height, 10.0, theme::CARD);
            // 卡片整体的悬停说明：关键设置（来自 config.json）或安装位置提示。
            let detail = joined(&info.lines);
            if !detail.is_empty() { c.hit(x, y, width, height, ("tool-card", spec.id)).on_hover_text(info.lines.iter().filter(|l| !l.is_empty()).cloned().collect::<Vec<_>>().join("\n")); }
            // 图标块
            let tile = c.faded(if status == Status::Missing { 0.45 } else { 1.0 });
            tile.vgrad(x + 8.0, y + 12.5, 20.0, 20.0, 6.0, spec.accent.0, spec.accent.1);
            glyph(&tile, i, x + 18.0, y + 22.5);
            // 名称与状态点
            let name_w = c.text(x + 35.0, y + 19.5, spec.name, 11.5, W::Bold, theme::WHITE, H::Left);
            let dot = match status { Status::Running => theme::GREEN, Status::Stopped => theme::GRAY, Status::Missing => Color32::from_gray(90) };
            let (dx, dy) = (x + 35.0 + name_w + 6.5, y + 15.5);
            if status == Status::Running {
                let pulse = 0.5 + 0.5 * (self.t as f32 * 2.4 + i as f32).sin();
                c.glow(dx, dy, lerp(4.5, 7.0, pulse), dot.gamma_multiply(0.5));
            }
            c.circle(dx, dy, 2.4, dot);
            // 实时状态
            let (line, color) = self.tool_live(i, &info);
            c.text(x + 35.0, y + 35.5, &fit_parts(c, &line, width - 35.0 - 9.0), size::TINY, W::Regular, color, H::Left);
            // 主操作
            if let Some((label, action, accent, tip)) = self.tool_action(i, &info) {
                let bw = (c.text_w(label, 10.0, W::Semibold) + 14.0).max(36.0);
                let bx = x + width - 7.0 - bw;
                let (r, b) = capsule(c, bx, y + 6.0, bw, 18.0, &format!("tool-{}", spec.id), if accent { spec.accent.1.gamma_multiply(0.85) } else { theme::BTN }, if accent { spec.accent.1 } else { theme::BTN_HOVER });
                b.centered_text(bx + bw / 2.0, y + 15.0, label, 10.0, W::Semibold, theme::WHITE, H::Center);
                let r = r.on_hover_text(tip);
                if r.clicked() { chosen = Some(action); }
                else if i == AUTOBOOST && r.secondary_clicked() && status == Status::Running {
                    let force = self.autoboost.state.as_ref().and_then(|b| b.force_until);
                    let paused = self.autoboost.state.as_ref().is_some_and(|b| b.paused);
                    chosen = Some(Action::Boost { paused: !paused, force_until: force });
                }
            }
        }
        if let Some(action) = chosen { self.run_tool_action(action); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("daozai-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn autobrightness_summary_reads_config_and_falls_back_to_defaults() {
        let lines = describe("autobrightness", &json!({"AutoEnabled": false, "IntervalSeconds": 600, "AwayEnabled": true, "AwaySeconds": 90}));
        assert_eq!(lines, ["自动亮度 关", "每 10 分钟测一次光", "离座 90 秒后黑屏"].map(String::from));
        let lines = describe("autobrightness", &serde_json::Value::Null);
        assert_eq!(lines, ["自动亮度 开", "每 5 分钟测一次光", "离座 1 分钟后黑屏"].map(String::from));
        assert_eq!(describe("autobrightness", &json!({"AwayEnabled": false}))[2], "离座黑屏 关");
    }

    #[test]
    fn voiceinput_summary_names_function_keys_and_skips_disabled_ones() {
        let lines = describe("voiceinput", &json!({"Trigger": {"Kind": "Key", "Vk": 124}, "CommandVk": 125, "AltVk": 0, "UseGpuAsr": false, "Polish": false}));
        assert_eq!(lines, ["按住 F13 听写", "F14 指令", "CPU 识别 · 整理 关"].map(String::from));
        let lines = describe("voiceinput", &json!({"Trigger": {"Kind": "Mouse", "Vk": 2}, "CommandVk": 0, "AltVk": 0}));
        assert_eq!(lines[0], "按住鼠标键听写");
        assert_eq!(lines[1], "仅听写");
        assert_eq!(describe("voiceinput", &serde_json::Value::Null)[1], "F14 指令 · F15 副麦");
    }

    #[test]
    fn probe_distinguishes_missing_stopped_and_running() {
        let home = scratch("home");
        let data = scratch("data");
        let spec = &SPECS[0];
        // 没装
        let info = probe(spec, Some(&home), &data, |_| false);
        assert_eq!((info.status, info.exe.is_none()), (Status::Missing, true));
        // 装了没运行，并读到 config.json
        let folder = home.join(spec.folder);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(spec.exe), b"").unwrap();
        std::fs::write(folder.join("config.json"), "\u{feff}{\"IntervalSeconds\": 120}").unwrap();
        let info = probe(spec, Some(&home), &data, |_| false);
        assert_eq!(info.status, Status::Stopped);
        assert_eq!(info.lines[1], "每 2 分钟测一次光");
        // 互斥体存在 = 在运行；只会用自己的互斥体名
        let info = probe(spec, Some(&home), &data, |name| name == spec.mutex);
        assert_eq!(info.status, Status::Running);
        assert_eq!(probe(&SPECS[1], Some(&home), &data, |name| name == spec.mutex).status, Status::Missing);
        let _ = std::fs::remove_dir_all(home);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn tools_txt_overrides_the_default_location() {
        let home = scratch("override-home");
        let data = scratch("override-data");
        let elsewhere = scratch("elsewhere");
        let exe = elsewhere.join("VoiceInput.exe");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(data.join("tools.txt"), format!("VoiceInput = {}\n", exe.display())).unwrap();
        let info = probe(&SPECS[1], Some(&home), &data, |_| false);
        assert_eq!((info.status, info.exe), (Status::Stopped, Some(exe)));
        for dir in [home, data, elsewhere] { let _ = std::fs::remove_dir_all(dir); }
    }
}
