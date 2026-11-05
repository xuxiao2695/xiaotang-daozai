//! 小工具联动的岛上表现：语音输入实时活动、亮度 HUD、提醒卡片与 Claude / 专注提示。
//!
//! 全部复用通知队列（`Alert` + `draw_notice`）：语音会话期间它就是当前通知，听写时插到最前面，
//! 其它通知排到它后面；结束后队列照常继续。
use crate::anim::{self, smoothstep};
use crate::app::{App, HUD_TRACK_W, HUD_TRACK_X, HUD_W};
use crate::autoboost::Mode;
use crate::bar_layout as bar;
use crate::extras::{Alert, AlertSource};
use crate::integrations::{Event, Indicator, Show, VoicePhase};
use crate::paint::{self, Card, H};
use crate::theme::{self, W};
use eframe::egui::Color32;
use std::f32::consts::{PI, TAU};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToolNotice {
    Voice,
    Brightness,
    Reminder(String),
    Away,
    Claude { session: String, approval: bool },
    ClaudeDone(String),
    FocusEnded,
    /// AutoBoost 切换：true = 开启睿频，false = 回到省电。
    Boost(bool),
}
impl ToolNotice {
    pub(crate) fn is_hud(&self) -> bool { matches!(self, Self::Voice | Self::Brightness | Self::Boost(_)) }
}

pub(crate) const VOICE_TITLE: &str = "语音输入";
const BRIGHTNESS_TITLE: &str = "屏幕亮度";
const BOOST_TITLE: &str = "睿频";
const ORANGE: Color32 = Color32::from_rgb(255, 140, 40);
const MIC_RED: Color32 = Color32::from_rgb(255, 69, 58);
const AMBER: Color32 = Color32::from_rgb(255, 176, 46);
const PURPLE: Color32 = Color32::from_rgb(168, 124, 255);
const TEAL: Color32 = Color32::from_rgb(52, 199, 170);
const VOICE_H: f32 = 38.0;
const VOICE_MAX_W: f32 = 340.0;

/// 估算文字宽度（布局时还拿不到字体）：汉字按一个字号，ASCII 约半个。
fn approx_w(text: &str, size: f32) -> f32 { text.chars().map(|c| if c.is_ascii() { 0.56 } else { 1.0 }).sum::<f32>() * size }

/// 睿频 HUD 随原因长短伸缩。
pub(crate) fn boost_width(body: &str) -> f32 { (62.0 + approx_w(body, 11.0)).clamp(196.0, VOICE_MAX_W) }

fn listening_label(command: bool) -> &'static str { if command { "说出命令" } else { "正在听" } }

impl App {
    /// 外壳每帧把联动端口收到的事件交进来。
    pub fn integration_event(&mut self, event: Event) {
        let now = chrono::Utc::now().timestamp();
        if let Event::Voice(v) = event {
            if self.settings.tool_voice { self.integrations.voice_event(v, self.t); }
            return;
        }
        for show in self.integrations.apply(event, self.t, now) { self.integration_show(show); }
    }

    /// 四个开关（语音、亮度、DeskHub/Claude、睿频）此刻是否会真的显示在岛上；外壳写给联动端口用来回复 `display`。
    pub fn integration_flags(&self) -> [bool; 4] {
        let n = self.settings.notifications;
        [self.settings.tool_voice, self.settings.tool_brightness && n, self.settings.tool_claude && n, self.settings.tool_boost && n]
    }

    /// 常驻指示（按优先级，最多由常驻栏取前两个）。
    pub(crate) fn resident_indicators(&self) -> Vec<Indicator> {
        self.integrations.indicators(chrono::Utc::now().timestamp(), self.settings.tool_claude, self.settings.tool_boost)
    }
    /// 常驻栏上实际要排布的指示（最多两个）。
    pub(crate) fn shown_indicators(&self) -> Vec<Indicator> {
        let mut list = self.resident_indicators();
        list.truncate(crate::bar_layout::TOOL_SLOTS);
        list
    }
    /// Claude 等待时琥珀色小点呼吸，需要持续重绘。
    pub(crate) fn indicators_animate(&self) -> bool {
        bar::animations_enabled() && self.shown_indicators().iter().any(|i| matches!(i, Indicator::Claude { .. } | Indicator::ClaudeWorking { .. }))
    }

    /// 收起的常驻栏上画小工具指示；点一下打开小工具页。
    pub(crate) fn draw_tool_indicators(&mut self, c: &Card, cx: f32, layout: &bar::Layout) {
        let now = chrono::Utc::now().timestamp();
        let mut open = false;
        for (i, indicator) in self.shown_indicators().iter().enumerate() {
            let slot = bar::TOOLS + i;
            let Some(x) = layout.x[slot] else { continue };
            let alpha = layout.pet_clearance(slot);
            let faded = c.faded(alpha);
            let c = if alpha < 0.8 { faded.ghost() } else { faded };
            let x = cx + x;
            indicator_glyph(&c, x, 15.0, indicator, self.t as f32, now);
            if crate::quota_ui::bar_tooltip(c.hit(x - 11.0, 3.0, 22.0, 25.0, ("tool-indicator", i)), format!("{}\n点击查看小工具", indicator_tip(indicator, now))).clicked() { open = true; }
        }
        if open { self.show_page(crate::app::TOOLS_PAGE); }
    }

    /// 每 2 秒读一次 AutoBoost 状态：切换时弹 HUD，睿频期间交给常驻指示。
    fn tick_boost(&mut self) {
        self.autoboost.poll(self.t);
        let after = self.autoboost.live().map(|b| (b.mode, b.reason.clone()));
        let before = std::mem::replace(&mut self.autoboost.seen, after.as_ref().map(|(mode, _)| *mode));
        self.integrations.boost = after.as_ref().filter(|(mode, _)| *mode == Mode::Boost).map(|(_, reason)| reason.clone());
        let Some((mode, reason)) = after else { return };
        // 刚启动（或主脚本刚开始运行）时只记下当前模式，不弹提示。
        if !(self.settings.tool_boost && self.settings.notifications) || before.is_none() || before == Some(mode) { return; }
        let body = match mode {
            Mode::Boost => if reason.is_empty() { "已开启睿频".to_string() } else { format!("已开启睿频 · {reason}") },
            Mode::Eco if before == Some(Mode::Boost) => "已回到省电".into(),
            _ => return,
        };
        self.drop_tool(|t| matches!(t, ToolNotice::Boost(_)));
        self.tool_alert(ToolNotice::Boost(mode == Mode::Boost), BOOST_TITLE, body, 2.5);
    }

    fn tool_alert(&mut self, tool: ToolNotice, title: impl Into<String>, body: impl Into<String>, duration: f64) {
        let a = Alert { source: AlertSource::Internal, title: title.into(), body: body.into(), page: usize::MAX, at: self.t, duration, bluetooth: None, meeting: None, tool: Some(tool) };
        self.enqueue_alert(a, false);
    }
    fn drop_tool(&mut self, matches: impl Fn(&ToolNotice) -> bool) {
        self.alerts.retain(|a| !a.tool.as_ref().is_some_and(&matches));
        if self.notice.as_ref().is_some_and(|a| a.tool.as_ref().is_some_and(&matches)) { self.notice = None; }
    }

    fn integration_show(&mut self, show: Show) {
        let (brightness, deskhub) = (self.settings.tool_brightness, self.settings.tool_claude);
        match show {
            Show::BrightnessHud { percent, .. } if brightness => self.tool_alert(ToolNotice::Brightness, BRIGHTNESS_TITLE, format!("{percent}%"), 2.5),
            Show::Reminder { kind, title, body } if brightness => self.tool_alert(ToolNotice::Reminder(kind), title, body, 8.0),
            Show::AwayCountdown(seconds) if brightness => {
                self.drop_tool(|t| *t == ToolNotice::Away);
                self.tool_alert(ToolNotice::Away, "即将黑屏", "动一下鼠标取消", seconds as f64 + 1.0);
            }
            Show::AwayEnd => self.drop_tool(|t| *t == ToolNotice::Away),
            Show::ClaudeWaiting { session, title, approval } if deskhub => {
                let s = session.clone();
                self.drop_tool(|t| matches!(t, ToolNotice::Claude { session, .. } if *session == s));
                self.tool_alert(ToolNotice::Claude { session, approval }, title, if approval { "Claude 要你批准" } else { "Claude 在等你回复" }, 10.0);
            }
            Show::ClaudeDone { session, title } if deskhub => self.tool_alert(ToolNotice::ClaudeDone(session), title, "Claude 做完了", 6.0),
            Show::ClaudeEnd { session } => self.drop_tool(|t| matches!(t, ToolNotice::Claude { session: s, .. } if *s == session)),
            Show::FocusEnded { missed } if deskhub => {
                let body = match missed { Some(n) if n > 0 => format!("期间错过 {n} 条"), _ => "期间没有错过消息".into() };
                self.tool_alert(ToolNotice::FocusEnded, "专注结束", body, 8.0);
            }
            _ => {}
        }
    }

    /// 每帧在通知队列推进之前调用：超时、语音会话接管 / 交还当前通知。
    pub(crate) fn tick_integrations(&mut self) {
        let now = chrono::Utc::now().timestamp();
        for show in self.integrations.tick(self.t, now) { self.integration_show(show); }
        self.tick_boost();
        if !self.settings.tool_boost { self.drop_tool(|t| matches!(t, ToolNotice::Boost(_))); }
        if !self.settings.tool_voice { self.integrations.voice = None; }
        if !self.settings.tool_brightness { self.drop_tool(|t| matches!(t, ToolNotice::Brightness | ToolNotice::Reminder(_) | ToolNotice::Away)); }
        if !self.settings.tool_claude { self.drop_tool(|t| matches!(t, ToolNotice::Claude { .. } | ToolNotice::ClaudeDone(_) | ToolNotice::FocusEnded)); }
        let is_voice = |a: &Alert| a.tool == Some(ToolNotice::Voice);
        self.alerts.retain(|a| !is_voice(a));
        let Some(session) = self.integrations.voice.clone() else {
            if self.notice.as_ref().is_some_and(is_voice) { self.notice = None; }
            self.voice_level = 0.0;
            return;
        };
        if self.notice.as_ref().is_some_and(|a| !is_voice(a)) {
            // 听写一开始就接管岛：正在显示的通知退回队首，稍后重新完整显示。
            if !session.live() { return; }
            if let Some(other) = self.notice.take() { self.alerts.push_front(other); }
        }
        let body = if session.text.is_empty() { session.mic.clone() } else { session.text.clone() };
        match &mut self.notice {
            Some(a) => a.body = body,
            None => self.notice = Some(Alert { source: AlertSource::Internal, title: VOICE_TITLE.into(), body, page: usize::MAX, at: session.started, duration: f64::INFINITY, bluetooth: None, meeting: None, tool: Some(ToolNotice::Voice) }),
        }
    }

    pub(crate) fn voice_showing(&self) -> bool { self.integrations.voice.is_some() && self.notice.as_ref().is_some_and(|a| a.tool == Some(ToolNotice::Voice)) }

    /// 动画中的帧（语音电平、识别闪光、亮度条缓动）需要 60 帧刷新。
    pub(crate) fn integration_animating(&self) -> bool {
        self.voice_showing() || self.integrations.brightness_hud.is_some_and(|(_, _, at)| self.t - at < 0.6)
    }

    pub(crate) fn tool_notice_size(&self, tool: &ToolNotice) -> (f32, f32) {
        match tool {
            ToolNotice::Voice => {
                let Some(s) = &self.integrations.voice else { return (HUD_W, VOICE_H) };
                let width = match s.phase {
                    VoicePhase::Listening if !s.text.is_empty() => VOICE_MAX_W - 40.0,
                    VoicePhase::Listening => 52.0 + approx_w(listening_label(s.command), 11.0) + if s.command { 36.0 } else { 0.0 } + if s.mic.is_empty() { 0.0 } else { 10.0 + approx_w(&s.mic, 10.5).min(120.0) } + 50.0,
                    VoicePhase::Recognizing => 150.0 + if s.command { 36.0 } else { 0.0 },
                    _ => 66.0 + approx_w(if s.text.is_empty() { "没有识别到内容" } else { &s.text }, 11.5),
                };
                (width.clamp(196.0, VOICE_MAX_W), VOICE_H)
            }
            ToolNotice::Brightness => (HUD_W, 38.0),
            _ => (334.0, 44.0),
        }
    }

    pub(crate) fn draw_tool_notice(&mut self, nc: &Card, tool: &ToolNotice, a: &Alert) {
        match tool {
            ToolNotice::Voice => self.draw_voice(nc),
            ToolNotice::Brightness => self.draw_brightness_hud(nc),
            ToolNotice::Boost(on) => {
                let since = (self.t - a.at) as f32;
                let pop = 1.0 + 0.16 * (1.0 - anim::ease_out_cubic((since / 0.3).clamp(0.0, 1.0)));
                let ic = nc.scaled(22.0, 19.0, pop);
                if *on { ic.glow(22.0, 19.0, 16.0, paint::alpha(ORANGE, 0.35)); }
                ic.circle(22.0, 19.0, 11.0, if *on { ORANGE } else { Color32::from_rgb(52, 168, 83) });
                crate::tools::bolt(&ic, 22.0, 19.0, 0.82, theme::WHITE);
                let width = boost_width(&a.body);
                let (head, tail) = a.body.split_once(" · ").map_or((a.body.as_str(), ""), |(h, t)| (h, t));
                let w = nc.text(42.0, 23.0, head, 11.0, W::Semibold, theme::WHITE, H::Left);
                if !tail.is_empty() {
                    let tail = nc.fit(tail, 10.5, W::Regular, (width - 42.0 - w - 28.0).max(20.0));
                    nc.text(42.0 + w + 6.0, 23.0, &format!("· {tail}"), 10.5, W::Regular, theme::GRAY, H::Left);
                }
            }
            _ => {
                tool_icon(nc, 22.0, 22.0, tool, self.t - a.at, self.integrations.away.map(|(s, at)| (s, self.t - at)));
                let (title, body, accent) = match tool {
                    ToolNotice::Away => {
                        let left = self.integrations.away.map_or(0.0, |(s, at)| (s as f64 - (self.t - at)).max(0.0)).ceil() as u32;
                        (format!("{left} 秒后黑屏"), a.body.clone(), theme::GRAY)
                    }
                    ToolNotice::Claude { .. } => (a.title.clone(), a.body.clone(), AMBER),
                    ToolNotice::ClaudeDone(_) => (a.title.clone(), a.body.clone(), theme::GREEN),
                    _ => (a.title.clone(), a.body.clone(), theme::GRAY),
                };
                let button = !matches!(tool, ToolNotice::Away);
                let max = if button { 200.0 } else { 270.0 };
                nc.centered_text(43.0, 14.0, &nc.fit(&title, 11.0, W::Semibold, max), 11.0, W::Semibold, theme::WHITE, H::Left);
                nc.centered_text(43.0, 30.0, &nc.fit(&body, 10.5, W::Regular, max), 10.5, W::Regular, accent, H::Left);
                if button {
                    let (r, b) = paint::capsule(nc, 257.0, 12.0, 60.0, 20.0, "tool-notice-ok", Color32::from_gray(35), theme::BTN_HOVER);
                    b.centered_text(287.0, 22.0, "知道了", 10.5, W::Semibold, theme::WHITE, H::Center);
                    if r.clicked() { self.notice = None; }
                }
            }
        }
    }

    fn draw_brightness_hud(&mut self, nc: &Card) {
        sun(nc, 24.0, 19.0, 1.0, theme::YELLOW);
        let (from, to, at) = self.integrations.brightness_hud.unwrap_or((0.0, 0.0, self.t));
        let e = anim::ease_out_cubic(((self.t - at) as f32 / 0.35).clamp(0.0, 1.0));
        let v = (from + (to - from) * e).clamp(0.0, 100.0) / 100.0;
        let (h, y) = (5.0, 16.5);
        nc.fill(HUD_TRACK_X, y, HUD_TRACK_W, h, h / 2.0, Color32::from_white_alpha(34));
        if v > 0.0 {
            let w = (HUD_TRACK_W * v).max(h);
            nc.fill(HUD_TRACK_X, y, w, h, h / 2.0, theme::WHITE);
            nc.fill(HUD_TRACK_X, y + 0.8, (w - 1.2).max(0.0), h * 0.4, h * 0.2, Color32::from_white_alpha(64));
        }
        nc.centered_text(HUD_W - 26.0, 19.0, &format!("{}%", to.round() as i32), 10.5, W::Regular, theme::WHITE, H::Center);
    }

    fn draw_voice(&mut self, nc: &Card) {
        let Some(s) = self.integrations.voice.clone() else { return };
        let (width, _) = self.tool_notice_size(&ToolNotice::Voice);
        // 电平：新值快速跟上，没有新值时缓慢回落，避免条形一跳一跳。
        let fresh = (self.t - s.level_at) < 0.3;
        let target = if s.phase == VoicePhase::Listening { if fresh { s.level } else { s.level * (1.0 - ((self.t - s.level_at - 0.3) as f32 / 0.5).clamp(0.0, 1.0)) } } else { 0.0 };
        self.voice_level = anim::approach(self.voice_level, target, if target > self.voice_level { 26.0 } else { 7.0 }, self.dt);
        let since = (self.t - s.phase_at) as f32;
        let c = nc.faded(smoothstep(0.0, 0.16, since).max(if s.phase == VoicePhase::Listening { 1.0 } else { 0.0 }));
        let (x, y) = (22.0, 19.0);
        let t = self.t as f32;
        match s.phase {
            VoicePhase::Listening => {
                let level = self.voice_level;
                c.glow(x, y, 15.0 + 7.0 * level, paint::alpha(MIC_RED, 0.30 + 0.35 * level));
                let pulse = 0.5 + 0.5 * (t * 3.2).sin();
                c.circle(x, y, 11.0 + 0.6 * pulse * (1.0 - level), MIC_RED);
                mic(&c, x, y, theme::WHITE);
                let mut left = 42.0;
                if s.text.is_empty() {
                    left += c.text(left, 23.0, listening_label(s.command), 11.0, W::Semibold, theme::WHITE, H::Left);
                } else {
                    let tail = tail_fit(&c, &s.text, 11.0, width - 42.0 - 62.0);
                    left += c.text(left, 23.0, &tail, 11.0, W::Semibold, theme::WHITE, H::Left);
                }
                if s.command && s.text.is_empty() {
                    left += 7.0;
                    command_badge(&c, left, y);
                    left += 30.0;
                }
                let bars_x = width - 30.0;
                if s.text.is_empty() && !s.mic.is_empty() {
                    left += 9.0;
                    c.centered_text(left, y, &c.fit(&s.mic, 10.5, W::Regular, (bars_x - 22.0 - left).max(20.0)), 10.5, W::Regular, theme::GRAY, H::Left);
                }
                level_bars(&c, bars_x, y, level, t);
            }
            VoicePhase::Recognizing => {
                c.circle(x, y, 11.0, Color32::from_gray(40));
                let spin = t * 5.2;
                c.arc(x, y, 9.0, spin, spin + 4.2, 1.8, theme::CYAN);
                mic(&c.scaled(x, y, 0.82), x, y, theme::WHITE);
                let mut left = 42.0;
                if s.command { command_badge(&c, left, y); left += 37.0; }
                shimmer(&c, left, 23.0, "识别中…", 11.0, t);
            }
            phase => {
                let (fill, ok) = match phase { VoicePhase::Done => (theme::GREEN, true), VoicePhase::Result(ok) => (if ok { theme::GREEN } else { AMBER }, ok), _ => (MIC_RED, false) };
                let empty = s.text.is_empty();
                let pop = 1.0 + 0.18 * (1.0 - anim::ease_out_cubic((since / 0.28).clamp(0.0, 1.0)));
                let ic = c.scaled(x, y, pop);
                ic.circle(x, y, 11.0, if empty { Color32::from_gray(64) } else { fill });
                if empty { ic.seg((x - 4.0, y), (x + 4.0, y), 2.0, theme::WHITE); }
                else if ok { paint::check(&ic, x, y + 0.5, 11.0, (since / 0.32).clamp(0.0, 1.0), 2.0, theme::WHITE); }
                else { ic.seg((x, y - 5.0), (x, y + 1.5), 2.2, theme::WHITE); ic.circle(x, y + 5.0, 1.3, theme::WHITE); }
                let text = if empty { "没有识别到内容" } else { s.text.as_str() };
                let ink = if empty { theme::GRAY } else { theme::WHITE };
                c.text(42.0, 23.5, &c.fit(text, 11.5, W::Semibold, width - 42.0 - 16.0), 11.5, W::Semibold, ink, H::Left);
            }
        }
    }
}

/// 末尾放得下多少就显示多少，前面用省略号（边说边打字时只看最新的部分）。
fn tail_fit(c: &Card, text: &str, size: f32, max_w: f32) -> String {
    if c.text_w(text, size, W::Semibold) <= max_w { return text.to_owned(); }
    let chars: Vec<char> = text.chars().collect();
    let mut start = chars.len();
    while start > 0 {
        let candidate: String = std::iter::once('…').chain(chars[start - 1..].iter().copied()).collect();
        if c.text_w(&candidate, size, W::Semibold) > max_w { break; }
        start -= 1;
    }
    std::iter::once('…').chain(chars[start..].iter().copied()).collect()
}

fn command_badge(c: &Card, x: f32, cy: f32) {
    c.fill(x, cy - 8.0, 30.0, 16.0, 8.0, paint::alpha(PURPLE, 0.28));
    c.centered_text(x + 15.0, cy, "命令", 9.5, W::Semibold, Color32::from_rgb(214, 196, 255), H::Center);
}

/// 五根电平条，中间最高；静音时保持轻微呼吸。
fn level_bars(c: &Card, cx: f32, cy: f32, level: f32, t: f32) {
    const PROFILE: [f32; 5] = [0.55, 0.82, 1.0, 0.82, 0.55];
    for (i, k) in PROFILE.iter().enumerate() {
        // 左右对称起伏，中间一根最活跃，读起来像一个声波而不是随机跳动。
        let d = (i as f32 - 2.0).abs();
        let wobble = 0.70 + 0.30 * (t * (9.5 - d * 1.8) + d * 1.1).sin();
        let idle = 0.12 + 0.06 * (t * 2.6 - d * 0.9).sin();
        let v = (level.sqrt() * k * wobble).max(idle * k);
        let h = 3.0 + 16.0 * v.clamp(0.0, 1.0);
        let x = cx - 12.0 + i as f32 * 6.0;
        c.fill(x - 1.5, cy - h / 2.0, 3.0, h, 1.5, theme::WHITE);
    }
}

/// 文字上扫过一道高光。
fn shimmer(c: &Card, x: f32, base: f32, text: &str, size: f32, t: f32) {
    let total = c.text_w(text, size, W::Semibold);
    let sweep = ((t * 70.0) % (total + 70.0)) - 35.0;
    let mut cursor = x;
    for ch in text.chars() {
        let s = ch.to_string();
        let w = c.text_w(&s, size, W::Semibold);
        let d = (cursor - x + w / 2.0 - sweep) / 18.0;
        let k = (-d * d).exp();
        c.text(cursor, base, &s, size, W::Semibold, paint::mix(Color32::from_gray(150), theme::WHITE, k), H::Left);
        cursor += w;
    }
}

fn mic(c: &Card, x: f32, y: f32, color: Color32) {
    c.fill(x - 2.6, y - 7.0, 5.2, 9.0, 2.6, color);
    c.arc(x, y - 1.6, 5.0, 0.25, PI - 0.25, 1.5, color);
    c.seg((x, y + 3.4), (x, y + 6.0), 1.5, color);
    c.seg((x - 2.6, y + 6.2), (x + 2.6, y + 6.2), 1.5, color);
}

pub(crate) fn sun(c: &Card, x: f32, y: f32, k: f32, color: Color32) {
    c.circle(x, y, 4.0 * k, color);
    for i in 0..8 {
        let a = i as f32 * TAU / 8.0;
        let (s, co) = a.sin_cos();
        c.seg((x + co * 6.6 * k, y + s * 6.6 * k), (x + co * 8.8 * k, y + s * 8.8 * k), 1.6 * k, color);
    }
}

fn claude_mark(c: &Card, x: f32, y: f32, r: f32, color: Color32) {
    for i in 0..6 {
        let a = i as f32 * PI / 6.0 - PI / 2.0;
        let (s, co) = a.sin_cos();
        c.seg((x - co * r, y - s * r), (x + co * r, y + s * r), 2.0, color);
    }
}

/// 卡片左侧的圆形图标；`away` 为黑屏倒计时（总秒数, 已过秒数）。
pub(crate) fn tool_icon(c: &Card, x: f32, y: f32, tool: &ToolNotice, elapsed: f64, away: Option<(u32, f64)>) {
    match tool {
        ToolNotice::Voice => { c.circle(x, y, 11.0, MIC_RED); mic(c, x, y, theme::WHITE); }
        ToolNotice::Brightness => sun(c, x, y, 1.0, theme::YELLOW),
        ToolNotice::Reminder(kind) => {
            c.circle(x, y, 13.0, TEAL);
            match kind.as_str() {
                "eye" => {
                    c.arc(x, y + 5.0, 9.0, -PI + 0.62, -0.62, 1.6, theme::WHITE);
                    c.arc(x, y - 5.0, 9.0, 0.62, PI - 0.62, 1.6, theme::WHITE);
                    c.circle(x, y, 2.6, theme::WHITE);
                }
                _ => {
                    // 站起来活动一下的小人
                    c.circle(x + 0.5, y - 6.0, 2.2, theme::WHITE);
                    c.path(&[(x + 0.5, y - 3.0), (x - 0.5, y + 2.5)], 2.0, theme::WHITE);
                    c.path(&[(x - 4.5, y - 0.5), (x, y - 2.2), (x + 4.0, y + 0.5)], 1.7, theme::WHITE);
                    c.path(&[(x - 4.0, y + 7.5), (x - 0.5, y + 2.5), (x + 3.5, y + 7.5)], 1.8, theme::WHITE);
                }
            }
        }
        ToolNotice::Away => {
            let (total, gone) = away.unwrap_or((1, 0.0));
            let left = (1.0 - gone as f32 / total.max(1) as f32).clamp(0.0, 1.0);
            c.circle(x, y, 13.0, Color32::from_gray(34));
            c.ring(x, y, 10.5, 2.0, Color32::from_gray(64));
            if left > 0.0 { c.arc(x, y, 10.5, -PI / 2.0, -PI / 2.0 + TAU * left, 2.0, theme::YELLOW); }
            let n = (total as f64 - gone).max(0.0).ceil() as u32;
            c.centered_text(x, y, &n.to_string(), 10.0, W::Semibold, theme::WHITE, H::Center);
        }
        ToolNotice::Claude { .. } => {
            let pulse = 0.5 + 0.5 * ((elapsed as f32) * 4.0).sin() * (1.0 - smoothstep(1.5, 2.5, elapsed as f32));
            c.glow(x, y, 17.0, paint::alpha(AMBER, 0.25 * pulse));
            c.circle(x, y, 13.0, AMBER);
            claude_mark(c, x, y, 6.5, Color32::from_rgb(60, 34, 4));
        }
        ToolNotice::ClaudeDone(_) => { c.circle(x, y, 13.0, theme::GREEN); paint::check(c, x, y + 0.5, 12.0, ((elapsed as f32) / 0.35).clamp(0.0, 1.0), 2.2, theme::WHITE); }
        ToolNotice::FocusEnded => {
            c.circle(x, y, 13.0, PURPLE);
            paint::stopwatch(&c.scaled(x, y, 0.55), x, y - 1.0, 8.0, 0.0, theme::WHITE);
        }
        ToolNotice::Boost(on) => { c.circle(x, y, 13.0, if *on { ORANGE } else { theme::GREEN }); crate::tools::bolt(c, x, y, 0.9, theme::WHITE); }
    }
}

/// 常驻栏上的小工具指示（约 16 px）：Claude 等待 = 呼吸的琥珀色星芒，会议 = 红点，
/// 专注 = 紫色圆环 + 剩余分钟，睿频 = 橙色小闪电。
pub(crate) fn indicator_glyph(c: &Card, x: f32, y: f32, indicator: &Indicator, t: f32, now: i64) {
    match indicator {
        Indicator::Claude { count, approval, .. } => {
            let pulse = 0.5 + 0.5 * (t * 3.0).sin();
            c.glow(x, y, 9.0 + 3.0 * pulse, paint::alpha(AMBER, 0.20 + 0.25 * pulse));
            claude_mark(&c.scaled(x, y, 0.92 + 0.08 * pulse), x, y, 6.6, AMBER);
            c.circle(x, y, 1.6, AMBER);
            if *approval { c.circle(x + 6.0, y - 5.5, 2.4, theme::WHITE); c.circle(x + 6.0, y - 5.5, 1.5, MIC_RED); }
            if *count > 1 {
                c.circle(x + 6.5, y + 6.0, 4.2, Color32::from_gray(232));
                c.centered_text(x + 6.5, y + 6.0, &count.min(&9).to_string(), 6.5, W::Bold, Color32::BLACK, H::Center);
            }
        }
        Indicator::Meeting => {
            c.glow(x, y, 9.0, paint::alpha(MIC_RED, 0.30));
            c.circle(x, y, 4.6, MIC_RED);
            c.ring(x, y, 7.2, 1.3, paint::alpha(MIC_RED, 0.45));
        }
        Indicator::Focus { ends_at } => {
            let minutes = ((ends_at - now).max(0) + 59) / 60;
            c.circle(x, y, 8.0, Color32::from_rgb(38, 28, 58));
            c.ring(x, y, 7.2, 1.6, paint::alpha(PURPLE, 0.35));
            // 最后 25 分钟按比例走圈，更长的专注先保持满圈。
            let k = (minutes as f32 / 25.0).clamp(0.04, 1.0);
            c.arc(x, y, 7.2, -PI / 2.0, -PI / 2.0 + TAU * k, 1.6, PURPLE);
            let text = if minutes > 99 { "99+".to_string() } else { minutes.to_string() };
            c.centered_text(x, y, &text, if text.len() > 2 { 5.5 } else { 7.5 }, W::Bold, theme::WHITE, H::Center);
        }
        Indicator::Boost { .. } => {
            c.glow(x, y, 9.0, paint::alpha(ORANGE, 0.28));
            crate::tools::bolt(c, x, y, 0.95, ORANGE);
        }
        Indicator::ClaudeWorking { count, .. } => {
            // 安静的灰白星芒慢慢转，表示 Claude 在干活、不需要你
            let spin = 0.85 + 0.15 * (t * 1.5).sin();
            claude_mark(&c.scaled(x, y, spin), x, y, 6.2, Color32::from_gray(190));
            c.circle(x, y, 1.4, Color32::from_gray(190));
            if *count > 1 {
                c.circle(x + 6.5, y + 6.0, 4.2, Color32::from_gray(120));
                c.centered_text(x + 6.5, y + 6.0, &count.min(&9).to_string(), 6.5, W::Bold, theme::WHITE, H::Center);
            }
        }
    }
}

pub(crate) fn indicator_tip(indicator: &Indicator, now: i64) -> String {
    match indicator {
        Indicator::Claude { count: 1, title, approval } => format!("{title} · {}", if *approval { "Claude 要你批准" } else { "Claude 在等你回复" }),
        Indicator::Claude { count, title, .. } => format!("{count} 个 Claude 会话在等你（最早：{title}）"),
        Indicator::Meeting => "会议中 · DeskHub 已静音提醒".into(),
        Indicator::Focus { ends_at } => format!("专注中 · 还剩 {} 分钟", ((ends_at - now).max(0) + 59) / 60),
        Indicator::Boost { reason } if reason.is_empty() => "AutoBoost 睿频中".into(),
        Indicator::Boost { reason } => format!("AutoBoost 睿频中 · {reason}"),
        Indicator::ClaudeWorking { count: 1, title } => format!("{title} · Claude 正在干活"),
        Indicator::ClaudeWorking { count, title } => format!("{count} 个 Claude 会话正在干活（最早：{title}）"),
    }
}
