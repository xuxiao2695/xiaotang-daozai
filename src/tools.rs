//! 小工具页：把本机自己做的 AutoBrightness、VoiceInput 放进岛里。
//!
//! 两个工具都是独立的托盘程序，没有对外接口，所以岛仔只做三件事，且全部是只读／启动：
//!   1. 用它们的单实例互斥体判断是否正在运行；
//!   2. 读 exe 同目录的 `config.json`，显示几条关键设置；
//!   3. 点按钮启动（已在运行时再次启动只会把它的窗口带到前台）。
//!
//! 安装位置默认 `%USERPROFILE%\<文件夹>\<exe>`（各自 README 里的发布位置），
//! 也可以在 `%APPDATA%\Daozai\tools.txt` 里逐行写 `autobrightness=D:\路径\AutoBrightness.exe` 覆盖。

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

pub const SPECS: [Spec; 2] = [
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
];

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
}

impl Default for Cache {
    fn default() -> Self {
        Self { infos: SPECS.iter().map(|_| Info::empty()).collect(), at: f64::NEG_INFINITY }
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
        _ => Default::default(),
    }
}

fn overrides(config_dir: &Path) -> Vec<(String, PathBuf)> {
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
    let custom = overrides(config_dir).into_iter().find(|(k, _)| k == spec.id).map(|(_, path)| path);
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
fn mutex_exists(name: &str) -> bool {
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
fn mutex_exists(_name: &str) -> bool {
    false
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

impl Cache {
    fn refresh(&mut self, now: f64) {
        if now - self.at < 2.0 && now >= self.at { return; }
        self.at = now;
        let home = user_home();
        let dir = crate::model::config_dir();
        self.infos = SPECS.iter().map(|spec| probe(spec, home.as_deref(), &dir, mutex_exists)).collect();
    }
}

#[cfg(test)]
impl Cache {
    /// 预览／测试用：直接放入探测结果，并在一段时间内不重新探测。
    pub(crate) fn set(&mut self, infos: Vec<Info>, now: f64) {
        self.infos = infos;
        self.at = now;
    }
}

// ---------- 绘制 ----------

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

impl App {
    pub(crate) fn page_tools(&mut self, c: &Card) {
        self.tools.refresh(self.t);
        c.ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
        let count = SPECS.len() as f32;
        let (gap, width) = (9.0, 165.0);
        let left = 209.5 - (count * width + (count - 1.0) * gap) / 2.0;
        for (i, spec) in SPECS.iter().enumerate() {
            let info = self.tools.infos[i].clone();
            let x = left + i as f32 * (width + gap);
            let y = 33.0;
            c.fill(x, y, width, 93.0, 12.0, theme::CARD);
            // 图标块
            let dim = if info.status == Status::Missing { 0.45 } else { 1.0 };
            let tile = c.faded(dim);
            tile.vgrad(x + 10.0, y + 10.0, 28.0, 28.0, 8.0, spec.accent.0, spec.accent.1);
            if spec.id == "autobrightness" { glyph_sun(&tile, x + 24.0, y + 24.0); } else { glyph_mic(&tile, x + 24.0, y + 24.0); }
            // 名称与状态
            c.text(x + 46.0, y + 22.0, spec.name, 12.5, W::Bold, theme::WHITE, H::Left);
            let (label, color) = match info.status {
                Status::Running => ("运行中", theme::GREEN),
                Status::Stopped => ("未运行", theme::GRAY),
                Status::Missing => ("未安装", theme::DIM),
            };
            if info.status == Status::Running {
                let pulse = 0.5 + 0.5 * (self.t as f32 * 2.4).sin();
                c.glow(x + 50.0, y + 33.5, lerp(5.0, 8.0, pulse), color.gamma_multiply(0.55));
            }
            c.circle(x + 50.0, y + 33.5, 2.5, color);
            c.text(x + 57.0, y + 37.0, label, size::TINY, W::Regular, color, H::Left);
            // 按钮
            let (text, accent) = match info.status {
                Status::Running => ("打开", false),
                Status::Stopped => ("启动", true),
                Status::Missing => ("获取", false),
            };
            let (r, b) = capsule(c, x + width - 50.0, y + 12.0, 40.0, 20.0, &format!("tool-{}", spec.id), if accent { spec.accent.1.gamma_multiply(0.85) } else { theme::BTN }, if accent { spec.accent.1 } else { theme::BTN_HOVER });
            b.text(x + width - 30.0, y + 26.0, text, size::SMALL, W::Semibold, theme::WHITE, H::Center);
            let r = r.on_hover_text(match (&info.exe, info.status) {
                (Some(path), Status::Running) => format!("{}（再次打开会把窗口带到前台）", path.display()),
                (Some(path), _) => path.display().to_string(),
                (None, _) => format!("打开 {} 主页", spec.repo),
            });
            if r.clicked() {
                match (&info.exe, info.status) {
                    (Some(path), _) => self.pending.push(Cmd::Open(path.clone())),
                    (None, _) => self.pending.push(Cmd::OpenUrl(spec.repo.into())),
                }
                // 启动后很快会有新状态，尽早重新探测。
                self.tools.at = self.t - 1.0;
            }
            // 关键设置
            c.fill(x + 10.0, y + 46.0, width - 20.0, 1.0, 0.5, Color32::from_white_alpha(14));
            for (line, text) in info.lines.iter().enumerate() {
                let text = c.fit(text, size::SMALL, W::Regular, width - 22.0);
                c.text(x + 11.0, y + 61.0 + line as f32 * 13.0, &text, size::SMALL, W::Regular, if info.status == Status::Missing { theme::DIM } else { theme::GRAY }, H::Left);
            }
        }
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
