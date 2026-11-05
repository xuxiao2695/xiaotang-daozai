//! AutoBoost（按负载切换睿频的 PowerShell 脚本）：岛仔不改它的代码，只读写它目录里的两个文件。
//!
//! - `state.json`：主脚本写出的当前状态 `{"Mode":"eco"|"boost"|"other","Reason":…,"Since":…,"Pid":…,"Paused":…,"ForceUntil":…}`（UTF-8，可能带 BOM）；
//! - `control.json`：托盘程序发给主脚本的指令 `{"Paused":bool,"ForceUntil":ISO|null}`，岛仔写的格式与 AutoBoostTray.ps1 完全一致；
//! - 是否在运行看命名互斥体 `Local\AutoBoost`；没在运行时用 `schtasks /Run /TN AutoBoost` 启动（不弹窗口）。
//!
//! 安装位置默认 `%USERPROFILE%\AutoBoost`，可在 `%APPDATA%\Daozai\tools.txt` 写 `autoboost=<文件夹或 AutoBoost.ps1>` 覆盖。
use std::path::{Path, PathBuf};

pub const MUTEX: &str = "Local\\AutoBoost";
pub const TASK: &str = "AutoBoost";
const POLL: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Eco, Boost, Other }

#[derive(Clone, Debug, PartialEq)]
pub struct BoostState { pub mode: Mode, pub reason: String, pub since: Option<i64>, pub paused: bool, pub force_until: Option<i64> }
impl BoostState {
    pub fn forcing(&self, now: i64) -> bool { self.force_until.is_some_and(|t| t > now) }
    /// 手动选了 Balanced 或其他电源计划：脚本不接管（与托盘图标的「未接管」判断一致）。
    pub fn manual(&self) -> bool { self.mode == Mode::Other || self.reason.starts_with("手动选择了") }
}

pub fn parse_state(text: &str) -> Option<BoostState> {
    let value: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}').trim()).ok()?;
    let o = value.as_object()?;
    let text = |k: &str| o.get(k).and_then(|v| v.as_str()).map(|s| s.trim().to_string());
    let mode = match text("Mode")?.to_ascii_lowercase().as_str() { "eco" => Mode::Eco, "boost" => Mode::Boost, _ => Mode::Other };
    Some(BoostState {
        mode, reason: text("Reason").unwrap_or_default(), since: text("Since").and_then(|s| crate::integrations::iso(&s)),
        paused: o.get("Paused").and_then(|v| v.as_bool()).unwrap_or(false), force_until: text("ForceUntil").and_then(|s| crate::integrations::iso(&s)),
    })
}

/// 与托盘程序写的一样：`ForceUntil` 用带时区的本地时间，PowerShell 的 `[datetime]` 能直接解析。
pub fn control_json(paused: bool, force_until: Option<i64>) -> String {
    let until = force_until.and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|t| t.with_timezone(&chrono::Local).to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
    serde_json::json!({"Paused": paused, "ForceUntil": until}).to_string()
}

pub fn write_control(folder: &Path, paused: bool, force_until: Option<i64>) -> std::io::Result<()> {
    let temp = folder.join("control.json.daozai");
    std::fs::write(&temp, control_json(paused, force_until))?;
    std::fs::rename(&temp, folder.join("control.json")).inspect_err(|_| { let _ = std::fs::remove_file(&temp); })
}

/// tools.txt 里的路径可以是文件夹，也可以直接指向 AutoBoost.ps1。
pub fn folder(home: Option<&Path>, config_dir: &Path) -> Option<PathBuf> {
    let custom = crate::tools::overrides(config_dir).into_iter().find(|(k, _)| k == "autoboost").map(|(_, p)| if p.is_file() { p.parent().map(Path::to_path_buf).unwrap_or(p) } else { p });
    custom.into_iter().chain(home.map(|h| h.join("AutoBoost"))).find(|dir| dir.join("AutoBoost.ps1").is_file() || dir.join("state.json").is_file())
}

/// 每 2 秒读一次状态文件；岛上的睿频提示、常驻小闪电和小工具页都用它。
pub struct Watch {
    pub folder: Option<PathBuf>,
    pub state: Option<BoostState>,
    pub running: bool,
    /// 上一帧看到的模式（运行中才有），用来判断是否刚刚切换。
    pub seen: Option<Mode>,
    at: f64,
    #[cfg(test)]
    pinned: bool,
}
impl Default for Watch {
    fn default() -> Self { Self { folder: None, state: None, running: false, seen: None, at: f64::NEG_INFINITY, #[cfg(test)] pinned: false } }
}
impl Watch {
    /// 主脚本在运行时的模式；没运行（状态文件可能是旧的）为 None。
    pub fn live(&self) -> Option<&BoostState> { self.state.as_ref().filter(|_| self.running) }
    /// 返回 true 表示这次重新读取了。
    pub fn poll(&mut self, t: f64) -> bool {
        #[cfg(test)] if self.pinned { return false; }
        if t - self.at < POLL && t >= self.at { return false; }
        self.at = t;
        self.folder = folder(crate::tools::user_home().as_deref(), &crate::model::config_dir());
        self.state = self.folder.as_ref().and_then(|dir| std::fs::read_to_string(dir.join("state.json")).ok()).and_then(|text| parse_state(&text));
        self.running = crate::tools::mutex_exists(MUTEX);
        true
    }
    /// 写完指令后尽快重新读状态。
    pub fn soon(&mut self, t: f64) { self.at = t - POLL + 0.4; }
}
#[cfg(test)]
impl Watch {
    pub(crate) fn set(&mut self, folder: Option<PathBuf>, state: Option<BoostState>, running: bool) { *self = Self { folder, state, running, seen: self.seen, at: 0.0, pinned: true }; }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_state_written_by_powershell_with_bom() {
        let s = parse_state("\u{feff}{\"Mode\":\"boost\",\"Reason\":\"cl.exe 占用 3.2 核\",\"Since\":\"2026-11-05T09:30:00.1234567+08:00\",\"Pid\":4242,\"Paused\":false,\"ForceUntil\":null}\r\n").unwrap();
        assert_eq!(s, BoostState { mode: Mode::Boost, reason: "cl.exe 占用 3.2 核".into(), since: Some(1793842200), paused: false, force_until: None });
        let s = parse_state(r#"{"Mode":"other","Reason":"手动选了其他计划","Paused":true,"ForceUntil":"2026-11-05T10:00:00+08:00"}"#).unwrap();
        assert!(s.manual() && s.paused && s.forcing(1793842200) && !s.forcing(1793844000));
        assert_eq!(parse_state(r#"{"Mode":"eco"}"#).unwrap().mode, Mode::Eco);
        assert!(parse_state("not json").is_none() && parse_state(r#"{"Reason":"x"}"#).is_none());
    }

    #[test]
    fn control_matches_the_tray_format_and_round_trips() {
        let value: serde_json::Value = serde_json::from_str(&control_json(true, None)).unwrap();
        assert_eq!(value, serde_json::json!({"Paused": true, "ForceUntil": null}));
        let value: serde_json::Value = serde_json::from_str(&control_json(false, Some(1793844000))).unwrap();
        assert_eq!(value["Paused"], false);
        assert_eq!(crate::integrations::iso(value["ForceUntil"].as_str().unwrap()), Some(1793844000));
        let dir = std::env::temp_dir().join(format!("daozai-autoboost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_control(&dir, false, Some(1793844000)).unwrap();
        write_control(&dir, true, None).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("control.json")).unwrap(), control_json(true, None));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "不留临时文件");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn folder_defaults_to_home_and_accepts_a_script_path_override() {
        let root = std::env::temp_dir().join(format!("daozai-autoboost-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, data, other) = (root.join("home"), root.join("data"), root.join("D").join("Tools"));
        for dir in [&home, &data, &other] { std::fs::create_dir_all(dir).unwrap(); }
        assert_eq!(folder(Some(&home), &data), None);
        std::fs::create_dir_all(home.join("AutoBoost")).unwrap();
        std::fs::write(home.join("AutoBoost").join("AutoBoost.ps1"), "").unwrap();
        assert_eq!(folder(Some(&home), &data), Some(home.join("AutoBoost")));
        std::fs::write(other.join("AutoBoost.ps1"), "").unwrap();
        std::fs::write(data.join("tools.txt"), format!("\u{feff}AutoBoost = {}\n", other.join("AutoBoost.ps1").display())).unwrap();
        assert_eq!(folder(Some(&home), &data), Some(other.clone()));
        std::fs::write(data.join("tools.txt"), format!("autoboost={}\n", other.display())).unwrap();
        assert_eq!(folder(None, &data), Some(other));
        let _ = std::fs::remove_dir_all(root);
    }
}
