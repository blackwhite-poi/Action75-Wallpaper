mod preview;
mod tutorial;

use crate::calibration_geometry::CalibrationGeometry;
use action75_manager::{self as core, Calibration, Point};
use eframe::egui::{self, Color32, FontId, Rect, RichText, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

const BOARD_FRAME_FILL: Color32 = Color32::from_rgb(29, 39, 46);
const BOARD_FRAME_SELECTED: Color32 = Color32::from_rgb(155, 207, 249);
const DISPLAY_CHECK_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Starting,
    Active,
    Saving,
    Ending,
}

#[derive(Clone, Copy)]
enum Action {
    Start,
    Save,
    SaveEnd,
    Undo,
    Entry,
    Defaults,
    RecoverDefaults,
    Fit,
    SaveFit,
    UndoFit,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CalibrationDisplay {
    device: String,
    identity: String,
    serial: String,
    bounds: [i32; 4],
}

impl CalibrationDisplay {
    fn selected(monitors: &Value) -> Option<Self> {
        let monitor = monitors
            .as_array()?
            .iter()
            .find(|monitor| monitor["selected"] == true && monitor["primary"] != true)?;
        let identity = monitor["monitorIdentity"].as_str()?.to_owned();
        let device = monitor["device"].as_str()?.to_owned();
        if identity.is_empty() || device.is_empty() {
            return None;
        }
        Some(Self {
            device,
            identity,
            serial: monitor["monitorSerial"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            bounds: monitor_bounds(monitor)?,
        })
    }

    fn same_display(&self, other: &Self) -> bool {
        if !self.serial.is_empty() && !other.serial.is_empty() {
            self.serial == other.serial
        } else {
            self.identity.eq_ignore_ascii_case(&other.identity)
        }
    }

    fn confirm(&self, current: Self) -> Result<Self, String> {
        if self.same_display(&current) {
            Ok(current)
        } else {
            Err("键盘副屏已更换，请重新连接原副屏。校准草稿仍保留。".into())
        }
    }
}

struct Session {
    model: Calibration,
    entered: BTreeMap<String, Point>,
    defaults: BTreeMap<String, Point>,
    undo: Vec<BTreeMap<String, Point>>,
    selected: Option<String>,
    drag: Option<(String, [f64; 2])>,
    display: CalibrationDisplay,
    display_issue: Option<String>,
    display_check_pending: bool,
    next_display_check: Instant,
    placement: Placement,
}
impl Session {
    fn new(prepared: Prepared) -> Self {
        Self {
            entered: prepared.model.points.clone(),
            model: prepared.model,
            defaults: prepared.defaults,
            undo: vec![],
            selected: None,
            drag: None,
            display: prepared.display,
            display_issue: None,
            display_check_pending: false,
            next_display_check: Instant::now() + DISPLAY_CHECK_INTERVAL,
            placement: Placement::default(),
        }
    }

    fn update_display(&mut self, result: Result<CalibrationDisplay, String>) -> bool {
        match result {
            Ok(display) => {
                let changed = self.display != display || self.display_issue.is_some();
                self.display = display;
                self.display_issue = None;
                if changed {
                    self.placement = Placement::default();
                    self.drag = None;
                }
                changed
            }
            Err(error) => {
                self.display_issue = Some(error);
                self.placement = Placement::default();
                self.drag = None;
                true
            }
        }
    }
    fn replace_points(&mut self, points: BTreeMap<String, Point>) {
        if self.model.points != points {
            self.undo.push(self.model.points.clone());
            self.model.points = points;
        }
    }
    fn undo(&mut self) -> bool {
        if let Some(points) = self.undo.pop() {
            self.model.points = points;
            true
        } else {
            false
        }
    }
}
struct State {
    phase: Phase,
    generation: u64,
    session: Option<Session>,
    saved: Result<Calibration, String>,
    fit_preview: Option<Calibration>,
    close_requested: bool,
    end_pending: bool,
    close_after_save: bool,
    close_cancelled: bool,
    error: String,
    message: String,
}
impl State {
    fn new(saved: Result<Calibration, String>) -> Self {
        Self {
            phase: Phase::Idle,
            generation: 0,
            session: None,
            saved,
            fit_preview: None,
            close_requested: false,
            end_pending: false,
            close_after_save: false,
            close_cancelled: false,
            error: String::new(),
            message: String::new(),
        }
    }

    fn preview_model(&self) -> Option<&Calibration> {
        self.session
            .as_ref()
            .map(|session| &session.model)
            .or(self.fit_preview.as_ref())
            .or(self.saved.as_ref().ok())
    }
}
struct Prepared {
    model: Calibration,
    defaults: BTreeMap<String, Point>,
    display: CalibrationDisplay,
}
enum Work {
    Begin(u64),
    CheckDisplay(u64),
    Save(u64, Calibration),
    End(u64),
    Shutdown(mpsc::Sender<()>),
}
enum Reply {
    Begun(u64, Result<Prepared, String>),
    DisplayChecked(u64, Result<CalibrationDisplay, String>),
    Saved(u64, Result<Calibration, String>),
    Ended(u64, Result<(), String>),
}

trait Backend: Send + 'static {
    fn begin(&mut self) -> Result<Prepared, String>;
    fn check_display(&self) -> Result<CalibrationDisplay, String>;
    fn save(&mut self, model: &Calibration) -> Result<Calibration, String>;
    fn end(&mut self) -> Result<(), String>;
}
struct Host {
    root: PathBuf,
    paused: Option<PausedPlayer>,
    display: Option<CalibrationDisplay>,
}

struct PausedPlayer {
    pid: u32,
    process: OwnedHandle,
}
impl PausedPlayer {
    fn watch(status: &Value) -> Result<Self, String> {
        let pid = status["playerPid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|&pid| pid != 0)
            .ok_or("无法确定当前播放器进程")?;
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }
            .map_err(|error| format!("无法监视播放器进程：{error}"))?;
        // OpenProcess returns a handle owned by this caller.
        let process = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        Ok(Self { pid, process })
    }

    fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(HANDLE(self.process.as_raw_handle()), 0) == WAIT_OBJECT_0 }
    }

    fn request(&self, action: &str) -> Value {
        json!({"action":action,"temporary":true,
               "ownerPid":std::process::id(),"expectedPlayerPid":self.pid})
    }

    fn confirm(&self, status: &Value) -> Result<(), String> {
        if status["playerPid"].as_u64() != Some(u64::from(self.pid)) {
            return Err("播放器实例已变化，请重新开始校准".into());
        }
        Ok(())
    }
}

fn pause_owned(
    paused: &mut Option<PausedPlayer>,
    player: PausedPlayer,
    request: impl FnOnce(Value) -> Result<Value, String>,
) -> Result<(), String> {
    if player.exited() {
        return Err("播放器已退出，请重新连接后开始校准".into());
    }
    // Retain cleanup responsibility before sending in case the reply is lost.
    let player = paused.insert(player);
    let status = request(player.request("pause"))?;
    player.confirm(&status)
}

fn resume_owned(
    paused: &mut Option<PausedPlayer>,
    request: impl FnOnce(Value) -> Result<Value, String>,
) -> Result<(), String> {
    let Some(player) = paused.as_ref() else {
        return Ok(());
    };
    if !player.exited() {
        let reply = request(player.request("resume"));
        if !player.exited() {
            player.confirm(&reply?)?;
        }
    }
    *paused = None;
    Ok(())
}

fn read_calibration_display(
    root: &std::path::Path,
    player: &PausedPlayer,
) -> Result<CalibrationDisplay, String> {
    if player.exited() {
        return Err("播放器已退出，校准草稿仍保留。".into());
    }
    let monitors = core::command(
        root,
        json!({"action":"listMonitors", "expectedPlayerPid":player.pid}),
    )?;
    CalibrationDisplay::selected(&monitors)
        .ok_or_else(|| "请连接原键盘副屏，校准草稿仍保留。".into())
}

impl Backend for Host {
    fn begin(&mut self) -> Result<Prepared, String> {
        let model = Calibration::load(&self.root)?;
        let defaults = Calibration::default_points(&self.root)?;
        let status = core::command(&self.root, json!({"action":"status"}))?;
        let player = PausedPlayer::watch(&status)?;
        let display = read_calibration_display(&self.root, &player)?;
        if status["monitorConnected"] != true || status["monitorDevice"] != display.device {
            return Err("请先选择已连接的键盘副屏".into());
        }
        let pid = player.pid;
        if let Err(error) = pause_owned(&mut self.paused, player, |request| {
            core::command(&self.root, request)
        }) {
            let _ = self.end();
            return Err(error);
        }
        if let Err(error) = core::confirm_calibration_access(&self.root, pid) {
            let _ = self.end();
            return Err(error);
        }
        self.display = Some(display);
        let display = match self.check_display() {
            Ok(display) => display,
            Err(error) => {
                let _ = self.end();
                return Err(error);
            }
        };
        Ok(Prepared {
            model,
            defaults,
            display,
        })
    }

    fn check_display(&self) -> Result<CalibrationDisplay, String> {
        let player = self.paused.as_ref().ok_or("校准会话已结束")?;
        let original = self.display.as_ref().ok_or("无法确认原键盘副屏")?;
        original.confirm(read_calibration_display(&self.root, player)?)
    }

    fn save(&mut self, model: &Calibration) -> Result<Calibration, String> {
        if let Some(player) = &self.paused {
            self.check_display()?;
            core::save_calibration_for_player(&self.root, model, player.pid)
        } else {
            core::save_calibration_and_reload(&self.root, model)
        }
    }
    fn end(&mut self) -> Result<(), String> {
        resume_owned(&mut self.paused, |request| {
            core::command(&self.root, request)
        })?;
        self.display = None;
        Ok(())
    }
}
fn worker<B: Backend>(
    mut backend: B,
    jobs: mpsc::Receiver<Work>,
    replies: mpsc::Sender<Reply>,
    ctx: egui::Context,
) {
    while let Ok(job) = jobs.recv() {
        let reply = match job {
            Work::Begin(g) => Reply::Begun(g, backend.begin()),
            Work::CheckDisplay(g) => Reply::DisplayChecked(g, backend.check_display()),
            Work::Save(g, model) => Reply::Saved(g, backend.save(&model)),
            Work::End(g) => Reply::Ended(g, backend.end()),
            Work::Shutdown(done) => {
                let _ = backend.end();
                let _ = done.send(());
                break;
            }
        };
        let _ = replies.send(reply);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
    let _ = backend.end();
}

pub struct Controller {
    fit_confirmation: Option<Phase>,
    observed_revision: Option<(String, bool)>,
    root: PathBuf,
    state: Arc<Mutex<State>>,
    tx: Option<mpsc::Sender<Work>>,
    rx: Option<mpsc::Receiver<Reply>>,
    #[cfg(test)]
    controls: BTreeMap<&'static str, Rect>,
}
impl Controller {
    #[cfg(test)]
    pub fn test_set_active(&mut self, active: bool) {
        let mut s = self.state.lock().unwrap();
        s.phase = if active { Phase::Active } else { Phase::Idle };
        s.session = active.then(|| Session::new(tests::prepared()));
    }
    pub fn new(root: PathBuf) -> Self {
        let saved = Calibration::load(&root);
        Self {
            fit_confirmation: None,
            observed_revision: None,
            root,
            state: Arc::new(Mutex::new(State::new(saved))),
            tx: None,
            rx: None,
            #[cfg(test)]
            controls: BTreeMap::new(),
        }
    }
    fn ensure_worker(&mut self, ctx: &egui::Context) {
        if self.tx.is_some() {
            return;
        }
        let (tx, jobs) = mpsc::channel();
        let (replies, rx) = mpsc::channel();
        let host = Host {
            root: self.root.clone(),
            paused: None,
            display: None,
        };
        let ctx = ctx.clone();
        thread::spawn(move || worker(host, jobs, replies, ctx));
        self.tx = Some(tx);
        self.rx = Some(rx);
    }
    pub fn has_session(&self) -> bool {
        self.state.lock().unwrap().phase != Phase::Idle
    }
    pub fn take_close_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.state.lock().unwrap().close_cancelled)
    }
    pub fn start(&mut self, ctx: &egui::Context) {
        if self.has_session() {
            return;
        }
        self.ensure_worker(ctx);
        let mut s = self.state.lock().unwrap();
        s.generation = s.generation.wrapping_add(1);
        s.phase = Phase::Starting;
        s.error.clear();
        s.message.clear();
        s.close_requested = false;
        s.close_after_save = false;
        s.close_cancelled = false;
        if self
            .tx
            .as_ref()
            .unwrap()
            .send(Work::Begin(s.generation))
            .is_err()
        {
            s.phase = Phase::Idle;
            s.error = "校准控制线程已退出".into();
        }
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
    pub fn end(&mut self, ctx: &egui::Context) {
        let mut s = self.state.lock().unwrap();
        if s.phase == Phase::Idle || s.end_pending {
            return;
        }
        if s.phase == Phase::Saving {
            s.close_after_save = true;
            s.close_requested = false;
            if s.session.is_some() {
                ctx.send_viewport_cmd_to(
                    viewport_id(s.generation),
                    egui::ViewportCommand::CancelClose,
                );
            }
            return;
        }
        s.phase = Phase::Ending;
        s.end_pending = true;
        s.close_requested = false;
        // Close commands are deferred by the backend. Hide synchronously before
        // allowing the native guard to resume and move ordinary visible windows.
        if let Err(error) = native::hide(&board_title(s.generation)) {
            s.end_pending = false;
            s.error = error;
            return;
        }
        ctx.send_viewport_cmd_to(viewport_id(s.generation), egui::ViewportCommand::Close);
        if self
            .tx
            .as_ref()
            .is_none_or(|tx| tx.send(Work::End(s.generation)).is_err())
        {
            s.end_pending = false;
            s.error = "校准控制线程已退出".into();
        }
    }
    fn save(&mut self, ctx: &egui::Context, action: Action) {
        self.ensure_worker(ctx);
        let mut s = self.state.lock().unwrap();
        let model = match (action, s.phase) {
            (Action::Save | Action::SaveEnd, Phase::Active) => s
                .session
                .as_ref()
                .filter(|session| session.display_issue.is_none())
                .map(|session| &session.model),
            (Action::SaveFit, Phase::Idle) => s.fit_preview.as_ref(),
            (Action::RecoverDefaults, Phase::Idle) => s
                .saved
                .as_ref()
                .ok()
                .filter(|model| model.recovery_notice().is_some()),
            _ => None,
        };
        let Some(model) = model.cloned() else { return };
        let previous_phase = s.phase;
        if previous_phase == Phase::Idle {
            s.generation = s.generation.wrapping_add(1);
        }
        s.phase = Phase::Saving;
        s.close_after_save = matches!(action, Action::SaveEnd);
        s.error.clear();
        if self
            .tx
            .as_ref()
            .is_none_or(|tx| tx.send(Work::Save(s.generation, model)).is_err())
        {
            s.phase = previous_phase;
            s.error = "校准控制线程已退出".into();
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut end = false;
        if let Some(rx) = &self.rx {
            while let Ok(reply) = rx.try_recv() {
                let mut s = self.state.lock().unwrap();
                match reply {
                    Reply::Begun(g, result) if g == s.generation && s.phase == Phase::Starting => {
                        match result {
                            Ok(prepared) => {
                                s.saved = Ok(prepared.model.clone());
                                let mut session = Session::new(prepared);
                                if let Some(draft) = s.fit_preview.take() {
                                    if draft.original == session.model.points {
                                        session.replace_points(draft.points);
                                    } else {
                                        s.message = "已载入更新后的校准，可重新拟合".into();
                                    }
                                }
                                s.session = Some(session);
                                s.phase = Phase::Active;
                            }
                            Err(error) => {
                                s.error = error;
                                end = true;
                            }
                        }
                    }
                    Reply::DisplayChecked(g, result) if g == s.generation => {
                        if let Some(session) = s.session.as_mut() {
                            session.display_check_pending = false;
                            session.next_display_check = Instant::now() + DISPLAY_CHECK_INTERVAL;
                            if session.update_display(result) {
                                let _ = native::hide(&board_title(g));
                            }
                        }
                    }
                    Reply::Saved(g, result) if g == s.generation && s.phase == Phase::Saving => {
                        s.phase = if s.session.is_some() {
                            Phase::Active
                        } else {
                            Phase::Idle
                        };
                        let close_after_save = std::mem::take(&mut s.close_after_save);
                        match result {
                            Ok(model) => {
                                let restored_defaults = s.saved.as_ref().is_ok_and(|previous| {
                                    previous.recovery_notice().is_some()
                                        && previous.points == model.points
                                });
                                s.saved = Ok(model.clone());
                                if let Some(session) = s.session.as_mut() {
                                    session.model = model;
                                    s.message = "校准已保存".into();
                                    end |= close_after_save;
                                } else {
                                    s.fit_preview = None;
                                    s.message = if restored_defaults {
                                        "默认键位已恢复"
                                    } else {
                                        "拟合已保存"
                                    }
                                    .into();
                                }
                            }
                            Err(error) => {
                                s.error = error;
                                s.close_cancelled = close_after_save;
                                if let Some(session) = s.session.as_mut() {
                                    session.next_display_check = Instant::now();
                                }
                            }
                        }
                    }
                    Reply::Ended(g, result) if g == s.generation && s.phase == Phase::Ending => {
                        s.end_pending = false;
                        match result {
                            Ok(()) => {
                                s.phase = Phase::Idle;
                                if let Some(session) = s.session.take() {
                                    s.error.clear();
                                    if session.model.changed() {
                                        s.message = "校准已结束，未保存的改动已丢弃".into();
                                    }
                                }
                            }
                            Err(error) => s.error = error,
                        }
                    }
                    _ => {}
                }
            }
        }
        end |= self.state.lock().unwrap().close_requested;
        if end {
            self.end(ctx);
        } else {
            let mut s = self.state.lock().unwrap();
            if s.phase != Phase::Active {
                return;
            }
            let generation = s.generation;
            let Some(session) = s.session.as_mut() else {
                return;
            };
            if session.display_check_pending {
                return;
            }
            let remaining = session
                .next_display_check
                .saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                ctx.request_repaint_after(remaining);
            } else if self
                .tx
                .as_ref()
                .is_some_and(|tx| tx.send(Work::CheckDisplay(generation)).is_ok())
            {
                session.display_check_pending = true;
            } else {
                session.update_display(Err("校准控制线程已退出，草稿仍保留。".into()));
                let _ = native::hide(&board_title(generation));
            }
        }
    }
    fn observe_revision(&mut self, revision: Option<&str>, using_fallback: bool) {
        let Some(revision) = revision else { return };
        if self
            .observed_revision
            .as_ref()
            .is_some_and(|(previous, fallback)| previous == revision && *fallback == using_fallback)
        {
            return;
        }
        let mut s = self.state.lock().unwrap();
        if s.phase == Phase::Idle {
            self.observed_revision = Some((revision.to_owned(), using_fallback));
            if !s.saved.as_ref().is_ok_and(|model| {
                model.document["revision"].as_str().unwrap_or("") == revision
                    && model.recovery_notice().is_some() == using_fallback
            }) {
                s.saved = Calibration::load(&self.root);
                s.message.clear();
            }
        }
    }

    pub fn show_page(&mut self, ui: &mut egui::Ui, status: &Value, monitors: &[Value]) {
        let running = status["running"] == true;
        let connected = running && status["monitorConnected"] == true;
        self.observe_revision(
            status["calibrationRevision"].as_str(),
            status["calibrationError"].is_string(),
        );

        ui.set_max_width(ui.available_width().min(1240.));

        #[cfg(test)]
        self.controls.clear();
        let mut action = None;
        let s = self.state.lock().unwrap();
        let model = s.preview_model();
        let bounds = s
            .session
            .as_ref()
            .map(|session| session.display.bounds)
            .or_else(|| {
                let device = connected
                    .then(|| status["monitorDevice"].as_str())
                    .flatten()?;
                monitors
                    .iter()
                    .find(|monitor| monitor["device"] == device && monitor["primary"] != true)
                    .and_then(monitor_bounds)
            });
        let selected = s
            .session
            .as_ref()
            .and_then(|session| session.selected.as_deref());
        let show_preview = |ui: &mut egui::Ui| {
            let caption = if s.phase == Phase::Saving {
                "正在保存…"
            } else if s.fit_preview.is_some() {
                "拟合预览 · 待保存"
            } else if model.is_some_and(Calibration::changed) {
                "未保存的校准"
            } else if !s.message.is_empty() {
                &s.message
            } else if model.is_some_and(|model| model.recovery_notice().is_some()) {
                "默认键位 · 待保存"
            } else if model.is_some() {
                "已保存的校准"
            } else {
                "无法读取已保存的校准"
            };
            preview::show(ui, bounds, model, selected, caption);
            if matches!(s.phase, Phase::Starting | Phase::Active)
                || (s.phase == Phase::Saving && s.session.is_some())
            {
                tutorial::show(ui);
            }
        };
        let mut show_controls = |ui: &mut egui::Ui| {
            let mut button = |ui: &mut egui::Ui, label: &'static str, command, allowed| {
                let primary = matches!(command, Action::Start | Action::SaveEnd | Action::SaveFit);
                let width = if primary || matches!(command, Action::Fit) {
                    112.
                } else {
                    0.
                };
                let response = ui.add_enabled(
                    allowed,
                    egui::Button::new(label)
                        .min_size(vec2(width, 34.))
                        .selected(primary),
                );
                #[cfg(test)]
                self.controls.insert(label, response.rect);
                if response.clicked() {
                    action = Some(command);
                }
                response
            };

            show_manual_controls(ui, &s, running, connected, &mut button);
            ui.add_space(9.);
            ui.separator();
            ui.add_space(8.);
            show_fit_controls(ui, &s, running, &mut button);
            if !s.error.is_empty() {
                ui.add_space(6.);
                ui.colored_label(ui.visuals().error_fg_color, &s.error);
            }
        };

        let width = ui.available_width();
        if width >= 740. {
            const CONTROLS_WIDTH: f32 = 252.;
            const GAP: f32 = 34.;
            ui.horizontal_top(|ui| {
                let spacing = ui.spacing().item_spacing.x;
                let diagram = ui.vertical(|ui| {
                    ui.set_width(width - CONTROLS_WIDTH - GAP);
                    show_preview(ui);
                });
                ui.add_space(GAP - spacing);
                let panel = ui.vertical(|ui| {
                    ui.set_width(CONTROLS_WIDTH);
                    show_controls(ui);
                });
                ui.painter().vline(
                    panel.response.rect.left() - GAP * 0.5,
                    diagram.response.rect.top()
                        ..=panel
                            .response
                            .rect
                            .bottom()
                            .max(diagram.response.rect.bottom()),
                    Stroke::new(
                        1_f32,
                        ui.visuals()
                            .widgets
                            .noninteractive
                            .bg_stroke
                            .color
                            .gamma_multiply(0.55),
                    ),
                );
            });
        } else {
            show_preview(ui);
            ui.add_space(18.);
            show_controls(ui);
        }
        drop(s);

        match action {
            Some(Action::Start) => self.start(ui.ctx()),
            Some(
                action @ (Action::Save
                | Action::SaveFit
                | Action::SaveEnd
                | Action::RecoverDefaults),
            ) => {
                self.save(ui.ctx(), action);
            }
            Some(Action::End) => self.end(ui.ctx()),
            Some(Action::Fit) => self.fit_confirmation = Some(self.state.lock().unwrap().phase),
            Some(Action::UndoFit) => {
                let mut s = self.state.lock().unwrap();
                s.fit_preview = None;
                s.message.clear();
                s.error.clear();
            }
            Some(action) => {
                let mut s = self.state.lock().unwrap();
                if let Some(session) = &mut s.session {
                    match action {
                        Action::Undo => {
                            session.undo();
                        }
                        Action::Entry => session.replace_points(session.entered.clone()),
                        Action::Defaults => session.replace_points(session.defaults.clone()),
                        _ => {}
                    }
                    s.message.clear();
                }
                ui.ctx().request_repaint_of(viewport_id(s.generation));
            }
            None => {}
        }
        self.show_fit_confirmation(ui.ctx());
    }

    fn show_fit_confirmation(&mut self, ctx: &egui::Context) {
        let Some(phase) = self.fit_confirmation else {
            return;
        };
        if self.state.lock().unwrap().phase != phase {
            self.fit_confirmation = None;
            return;
        }
        let response =
            egui::Modal::new(egui::Id::new("fit-calibration-confirmation")).show(ctx, |ui| {
                ui.set_width(358.);
                ui.label(RichText::new("根据现有数据拟合键位？").size(20.));
                ui.add_space(4.);
                ui.label("按 Action75 的键帽排列微调当前校准点。");
                ui.label(RichText::new("部分点位可能移动。可预览、撤销，保存后生效。").weak());
                ui.add_space(10.);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let fit = ui
                        .add(egui::Button::new("继续拟合").selected(true))
                        .clicked();
                    let dismiss = ui.button("取消").clicked();
                    (fit, dismiss)
                })
                .inner
            });
        let (fit, dismiss) = response.inner;
        if response.should_close() || dismiss || fit {
            self.fit_confirmation = None;
        }
        if fit {
            self.fit(ctx);
        }
    }

    fn fit(&mut self, ctx: &egui::Context) {
        let mut s = self.state.lock().unwrap();
        let Some(model) = s.preview_model() else {
            return;
        };
        match core::calibration_fit::fit(&model.points, &preview::fit_reference()) {
            Ok(points) => {
                s.error.clear();
                if let Some(session) = &mut s.session {
                    session.replace_points(points);
                    ctx.request_repaint_of(viewport_id(s.generation));
                } else {
                    let mut draft = s.saved.as_ref().unwrap().clone();
                    if draft.points != points {
                        draft.points = points;
                        s.fit_preview = Some(draft);
                    } else {
                        s.message = "当前键位已对齐".into();
                    }
                }
            }
            Err(error) => s.error = error,
        }
        ctx.request_repaint();
    }
    pub fn show_viewport(&self, ctx: &egui::Context) {
        let s = self.state.lock().unwrap();
        if !matches!(s.phase, Phase::Active | Phase::Saving) {
            return;
        }
        let Some(session) = &s.session else { return };
        if session.display_issue.is_some() {
            return;
        }
        let generation = s.generation;
        let bounds = session.display.bounds;
        drop(s);
        let title = board_title(generation);
        let state = self.state.clone();
        ctx.show_viewport_deferred(
            viewport_id(generation),
            egui::ViewportBuilder::default()
                .with_title(&title)
                .with_visible(false)
                .with_decorations(false)
                .with_resizable(false)
                .with_inner_size([bounds[2] as f32, bounds[3] as f32])
                .with_active(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop),
            move |ctx, class| {
                let mut s = state.lock().unwrap();
                if s.generation != generation
                    || s.session
                        .as_ref()
                        .is_none_or(|session| session.display_issue.is_some())
                    || !matches!(s.phase, Phase::Active | Phase::Saving)
                {
                    return;
                }
                if class == egui::ViewportClass::Embedded {
                    s.error = "当前窗口后端不支持副屏校准".into();
                    s.close_requested = true;
                } else if board_close_requested(ctx) {
                    s.close_requested = true;
                } else {
                    let active = s.phase == Phase::Active;
                    let session = s.session.as_mut().unwrap();
                    match session.placement.prepare(ctx, &session.display, &title) {
                        Ok(true) => {
                            draw_canvas(ctx, session, active);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            session.update_display(Err(error));
                            session.next_display_check = Instant::now();
                            let _ = native::hide(&title);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                            ctx.request_repaint_of(egui::ViewportId::ROOT);
                        }
                    }
                }
                if s.close_requested {
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            },
        );
    }
    pub fn shutdown(&mut self) {
        let _ = native::hide(&board_title(self.state.lock().unwrap().generation));
        if let Some(tx) = self.tx.take() {
            let (done, result) = mpsc::channel();
            let _ = tx.send(Work::Shutdown(done));
            let _ = result.recv_timeout(Duration::from_secs(9));
        }
    }
}

fn show_manual_controls(
    ui: &mut egui::Ui,
    s: &State,
    running: bool,
    connected: bool,
    button: &mut impl FnMut(&mut egui::Ui, &'static str, Action, bool) -> egui::Response,
) {
    ui.label(RichText::new("手动校准").size(17.));
    let issue = s
        .session
        .as_ref()
        .and_then(|session| session.display_issue.as_deref());
    let hint = if let Some(issue) = issue {
        issue
    } else if connected || s.session.is_some() {
        "在键盘屏拖动键位，用方向键微调。"
    } else {
        "在「设备与输出」中连接键盘屏。"
    };
    ui.label(RichText::new(hint).size(13.).weak());
    if let Some(notice) = s.preview_model().and_then(Calibration::recovery_notice) {
        ui.label(RichText::new(notice).size(13.).weak());
    }
    ui.add_space(4.);

    match s.phase {
        Phase::Starting => {
            ui.label("正在打开副屏校准…");
        }
        Phase::Ending if s.end_pending => {
            ui.label("正在结束校准…");
        }
        Phase::Ending => {
            button(ui, "重试结束", Action::End, true);
        }
        _ => {
            if let Some(session) = &s.session {
                let active = s.phase == Phase::Active;
                let can_save = active && session.display_issue.is_none();
                ui.horizontal_wrapped(|ui| {
                    button(ui, "保存并结束", Action::SaveEnd, can_save);
                    button(
                        ui,
                        "保存",
                        Action::Save,
                        can_save
                            && (session.model.changed()
                                || session.model.recovery_notice().is_some()),
                    );
                    button(ui, "撤销", Action::Undo, active && !session.undo.is_empty());
                });
                ui.horizontal_wrapped(|ui| {
                    button(
                        ui,
                        "恢复进入前",
                        Action::Entry,
                        active && session.model.points != session.entered,
                    );
                    button(
                        ui,
                        "恢复默认",
                        Action::Defaults,
                        active && session.model.points != session.defaults,
                    );
                    button(ui, "结束", Action::End, true)
                        .on_hover_text("结束校准并丢弃本次未保存的改动");
                });
            } else {
                button(
                    ui,
                    "开始校准",
                    Action::Start,
                    connected && s.phase == Phase::Idle,
                );
                if s.saved
                    .as_ref()
                    .is_ok_and(|model| model.recovery_notice().is_some())
                {
                    button(
                        ui,
                        "恢复默认",
                        Action::RecoverDefaults,
                        running && s.phase == Phase::Idle,
                    );
                }
            }
        }
    }
}

fn show_fit_controls(
    ui: &mut egui::Ui,
    s: &State,
    running: bool,
    button: &mut impl FnMut(&mut egui::Ui, &'static str, Action, bool) -> egui::Response,
) {
    ui.label(RichText::new("键位拟合").size(17.));
    if s.fit_preview.is_some() && s.session.is_none() {
        let idle = s.phase == Phase::Idle;
        ui.label(
            RichText::new(if running {
                "确认预览中的点位后保存。"
            } else {
                "连接播放器后可保存拟合。"
            })
            .size(13.)
            .weak(),
        );
        ui.add_space(4.);
        ui.horizontal_wrapped(|ui| {
            button(ui, "保存拟合", Action::SaveFit, idle && running);
            button(ui, "撤销拟合", Action::UndoFit, idle);
        });
    } else {
        ui.label(
            RichText::new("参考 Action75 的键帽布局，微调键距和排列。")
                .size(13.)
                .weak(),
        );
        ui.add_space(4.);
        let allowed = matches!(s.phase, Phase::Idle | Phase::Active) && s.preview_model().is_some();
        button(ui, "拟合键位", Action::Fit, allowed);
    }
}

fn monitor_bounds(monitor: &Value) -> Option<[i32; 4]> {
    let mut bounds = [0; 4];
    for (value, field) in bounds.iter_mut().zip(["x", "y", "width", "height"]) {
        *value = i32::try_from(monitor[field].as_i64()?).ok()?;
    }
    (bounds[2] > 0 && bounds[3] > 0).then_some(bounds)
}

fn viewport_id(generation: u64) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("action75-calibration", generation))
}
fn board_title(generation: u64) -> String {
    format!(
        "{} · 键位校准 {} {generation}",
        core::APP_NAME,
        std::process::id()
    )
}

fn board_close_requested(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape))
}

#[derive(Default)]
struct Placement {
    started: Option<Instant>,
    scale: Option<f32>,
    stable: u8,
    last_frame: Option<u64>,
    visible: bool,
}
impl Placement {
    fn prepare(
        &mut self,
        ctx: &egui::Context,
        display: &CalibrationDisplay,
        title: &str,
    ) -> Result<bool, String> {
        let started = *self.started.get_or_insert_with(Instant::now);
        let canvas = ctx.viewport_rect();
        let geometry = CalibrationGeometry::new(
            display.bounds,
            [
                canvas.left() as f64,
                canvas.top() as f64,
                canvas.width() as f64,
                canvas.height() as f64,
            ],
        )
        .ok_or("校准画布尺寸无效")?;
        let scale = ctx.pixels_per_point();
        let client = native::client(title, display)?;
        let scale_changed = self.scale != Some(scale);
        let matches = client.is_some_and(|rect| geometry.client_matches_target(rect, 0))
            && geometry.canvas_matches_target(scale as f64, 1.);
        if self.visible && matches {
            return Ok(true);
        }
        if started.elapsed() > Duration::from_secs(3) {
            return Err("副屏校准窗口未能匹配显示器尺寸".into());
        }
        if scale_changed || !matches {
            self.scale = Some(scale);
            self.stable = 0;
            self.last_frame = None;
            let size = geometry.logical_size(scale as f64).ok_or("副屏缩放无效")?;
            let position = geometry.logical_position(scale as f64).unwrap();
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(
                size[0] as f32,
                size[1] as f32,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2(
                position[0] as f32,
                position[1] as f32,
            )));
            native::place(title, display)?;
        } else {
            if self.record_match(ctx.cumulative_frame_nr()) {
                self.visible = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                return Ok(true);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(16));
        Ok(false)
    }
    fn record_match(&mut self, frame: u64) -> bool {
        if self.last_frame != Some(frame) {
            self.last_frame = Some(frame);
            self.stable = self.stable.saturating_add(1);
        }
        self.stable >= 2
    }
}

fn draw_canvas(ctx: &egui::Context, session: &mut Session, editable: bool) -> Rect {
    let canvas = ctx.viewport_rect();
    let geometry = CalibrationGeometry::new(
        session.display.bounds,
        [
            canvas.left() as f64,
            canvas.top() as f64,
            canvas.width() as f64,
            canvas.height() as f64,
        ],
    )
    .unwrap();
    if editable {
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) && session.undo() {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        if let Some(code) = session.selected.clone() {
            let delta = ctx.input(|i| {
                let step = if i.modifiers.shift { 10. } else { 1. };
                [
                    if i.key_pressed(egui::Key::ArrowRight) {
                        step
                    } else if i.key_pressed(egui::Key::ArrowLeft) {
                        -step
                    } else {
                        0.
                    },
                    if i.key_pressed(egui::Key::ArrowDown) {
                        step
                    } else if i.key_pressed(egui::Key::ArrowUp) {
                        -step
                    } else {
                        0.
                    },
                ]
            });
            if delta != [0., 0.] {
                session.undo.push(session.model.points.clone());
                let delta = geometry.normalized_delta_for_physical_pixels(delta);
                let point = session.model.points[&code];
                session
                    .model
                    .move_point(&code, point.x + delta[0], point.y + delta[1]);
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
        }
    }
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(Color32::from_rgb(15, 20, 25))
                .inner_margin(0),
        )
        .show(ctx, |ui| {
            for code in session.model.order.clone() {
                let point = session.model.points[&code];
                let center = geometry.normalized_to_canvas([point.x, point.y]);
                let center = pos2(center[0] as f32, center[1] as f32);
                let label = key_label(&code);
                let rect = Rect::from_center_size(
                    center,
                    vec2((label.len() as f32 * 7. + 18.).clamp(40., 82.), 30.),
                );
                let response = ui.interact(
                    rect,
                    ui.id().with(&code),
                    if editable {
                        Sense::click_and_drag()
                    } else {
                        Sense::hover()
                    },
                );
                let selected = session.selected.as_deref() == Some(code.as_str());
                let color = if selected {
                    BOARD_FRAME_SELECTED
                } else {
                    Color32::from_rgb(177, 191, 203)
                };
                ui.painter().rect_filled(rect, 5, BOARD_FRAME_FILL);
                ui.painter().rect_stroke(
                    rect,
                    5,
                    Stroke::new(if selected { 1.5_f32 } else { 0.7_f32 }, color),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(12.),
                    color,
                );
                ui.painter()
                    .circle_stroke(center, 3., Stroke::new(0.8_f32, color));
                if response.clicked() {
                    session.selected = Some(code.clone());
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                if response.drag_started() {
                    session.selected = Some(code.clone());
                    session.undo.push(session.model.points.clone());
                    let origin = ctx.input(|i| i.pointer.press_origin()).unwrap_or(center);
                    session.drag = Some((
                        code.clone(),
                        [(origin.x - center.x) as f64, (origin.y - center.y) as f64],
                    ));
                }
                if response.dragged() && editable {
                    if let (Some(pointer), Some((dragged, offset))) =
                        (response.interact_pointer_pos(), session.drag.as_ref())
                    {
                        if dragged == &code {
                            let normalized = geometry.canvas_to_normalized([
                                pointer.x as f64 - offset[0],
                                pointer.y as f64 - offset[1],
                            ]);
                            session
                                .model
                                .move_point(&code, normalized[0], normalized[1]);
                        }
                    }
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                if response.drag_stopped() {
                    session.drag = None;
                }
            }
        });
    canvas
}
fn key_label(code: &str) -> &str {
    code.strip_prefix("Key")
        .or_else(|| code.strip_prefix("Digit"))
        .unwrap_or(match code {
            "ControlLeft" | "ControlRight" => "Ctrl",
            "ShiftLeft" | "ShiftRight" => "Shift",
            "AltLeft" | "AltRight" => "Alt",
            "MetaLeft" => "Win",
            // Legacy calibration files store the physical ScrLk position as Insert.
            "Insert" | "ScrollLock" => "ScrLk",
            "ArrowUp" => "↑",
            "ArrowDown" => "↓",
            "ArrowLeft" => "←",
            "ArrowRight" => "→",
            "Backquote" => "`",
            "Minus" => "-",
            "Equal" => "=",
            "BracketLeft" => "[",
            "BracketRight" => "]",
            "Backslash" => "\\",
            "Semicolon" => ";",
            "Quote" => "'",
            "Comma" => ",",
            "Period" => ".",
            "Slash" => "/",
            _ => code,
        })
}

#[cfg(windows)]
mod native {
    use super::CalibrationDisplay;
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct MonitorInfo {
        size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
        device: [u16; 32],
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn FindWindowW(class: *const u16, name: *const u16) -> isize;
        fn GetWindowThreadProcessId(window: isize, process: *mut u32) -> u32;
        fn GetClientRect(window: isize, rect: *mut Rect) -> i32;
        fn ClientToScreen(window: isize, point: *mut Point) -> i32;
        fn SetWindowPos(
            window: isize,
            after: isize,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> i32;
        fn SetThreadDpiAwarenessContext(context: isize) -> isize;
        fn MonitorFromRect(rect: *const Rect, flags: u32) -> isize;
        fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
        fn ShowWindow(window: isize, command: i32) -> i32;
        fn IsWindowVisible(window: isize) -> i32;
    }
    fn owned(title: &str) -> Result<Option<isize>, String> {
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if hwnd == 0 {
            return Ok(None);
        }
        let mut process = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut process);
        }
        if process != std::process::id() {
            return Err("校准窗口不属于当前程序".into());
        }
        Ok(Some(hwnd))
    }
    struct Dpi(isize);
    impl Dpi {
        fn enter() -> Result<Self, String> {
            let previous = unsafe { SetThreadDpiAwarenessContext(-4) };
            if previous == 0 {
                return Err(format!(
                    "无法使用副屏物理坐标：{}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self(previous))
        }
    }
    impl Drop for Dpi {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    SetThreadDpiAwarenessContext(self.0);
                }
            }
        }
    }
    pub fn client(title: &str, display: &CalibrationDisplay) -> Result<Option<[i32; 4]>, String> {
        let Some(hwnd) = owned(title)? else {
            return Ok(None);
        };
        let _dpi = Dpi::enter()?;
        verify_target(display)?;
        let mut r = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut p = Point { x: 0, y: 0 };
        if unsafe { GetClientRect(hwnd, &mut r) } == 0
            || unsafe { ClientToScreen(hwnd, &mut p) } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Some([p.x, p.y, r.right - r.left, r.bottom - r.top]))
    }
    pub fn hide(title: &str) -> Result<(), String> {
        if let Some(hwnd) = owned(title)? {
            unsafe {
                ShowWindow(hwnd, 0);
            }
            if unsafe { IsWindowVisible(hwnd) } != 0 {
                return Err("校准窗口尚未关闭".into());
            }
        }
        Ok(())
    }
    fn verify_target(display: &CalibrationDisplay) -> Result<(), String> {
        let bounds = display.bounds;
        let rect = Rect {
            left: bounds[0],
            top: bounds[1],
            right: bounds[0].saturating_add(bounds[2]),
            bottom: bounds[1].saturating_add(bounds[3]),
        };
        let monitor = unsafe { MonitorFromRect(&rect, 0) };
        let mut info = MonitorInfo {
            size: std::mem::size_of::<MonitorInfo>() as u32,
            monitor: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            work: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            flags: 0,
            device: [0; 32],
        };
        if monitor == 0
            || unsafe { GetMonitorInfoW(monitor, &mut info) } == 0
            || info.flags & 1 != 0
            || [
                info.monitor.left,
                info.monitor.top,
                info.monitor.right - info.monitor.left,
                info.monitor.bottom - info.monitor.top,
            ] != bounds
        {
            return Err("键盘副屏已变化，校准草稿仍保留。".into());
        }
        let end = info
            .device
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.device.len());
        if !String::from_utf16_lossy(&info.device[..end]).eq_ignore_ascii_case(&display.device) {
            return Err("键盘副屏已变化，校准草稿仍保留。".into());
        }
        Ok(())
    }
    pub fn place(title: &str, display: &CalibrationDisplay) -> Result<(), String> {
        let Some(hwnd) = owned(title)? else {
            return Ok(());
        };
        let _dpi = Dpi::enter()?;
        verify_target(display)?;
        let bounds = display.bounds;
        if unsafe {
            SetWindowPos(
                hwnd,
                -1,
                bounds[0],
                bounds[1],
                bounds[2],
                bounds[3],
                0x0010 | 0x0200,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}
#[cfg(not(windows))]
mod native {
    use super::CalibrationDisplay;
    pub fn hide(_: &str) -> Result<(), String> {
        Ok(())
    }
    pub fn client(_: &str, _: &CalibrationDisplay) -> Result<Option<[i32; 4]>, String> {
        Err("校准需要 Windows".into())
    }
    pub fn place(_: &str, _: &CalibrationDisplay) -> Result<(), String> {
        Err("校准需要 Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput};

    pub(super) fn prepared() -> Prepared {
        let mut points: BTreeMap<String, Point> = (0..82)
            .map(|i| {
                (
                    format!("Key{i}"),
                    Point {
                        x: (i as f64 + 0.5) / 83.,
                        y: 0.08,
                    },
                )
            })
            .collect();
        points.insert("KeyA".into(), Point { x: 0.5, y: 0.5 });
        let order = points.keys().cloned().collect();
        let model = Calibration::from_points(json!({"revision":"before"}), points, order);
        let mut defaults = model.points.clone();
        defaults.insert("KeyA".into(), Point { x: 0.7, y: 0.6 });
        Prepared {
            model,
            defaults,
            display: CalibrationDisplay {
                device: "DISPLAY2".into(),
                identity: "original-port".into(),
                serial: "serial-1".into(),
                bounds: [-1920, 0, 1920, 720],
            },
        }
    }
    fn input(size: egui::Vec2, scale: f32, events: Vec<Event>) -> RawInput {
        let mut input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        input
    }
    fn board_frame(
        ctx: &egui::Context,
        session: &mut Session,
        scale: f32,
        events: Vec<Event>,
    ) -> (Rect, egui::FullOutput) {
        let mut canvas = Rect::NOTHING;
        let output = ctx.run(
            input(vec2(1920. / scale, 720. / scale), scale, events),
            |ctx| {
                canvas = draw_canvas(ctx, session, true);
            },
        );
        (canvas, output)
    }
    fn pointer(p: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn actual_board_is_full_viewport_and_drag_uses_shared_model_at_each_dpi() {
        for scale in [1., 1.25, 1.5] {
            let ctx = egui::Context::default();
            let state = Arc::new(Mutex::new(Session::new(prepared())));
            let board = state.clone();
            let mut s = board.lock().unwrap();
            let (canvas, output) = board_frame(&ctx, &mut s, scale, vec![]);
            assert_eq!(
                canvas,
                Rect::from_min_size(Pos2::ZERO, vec2(1920. / scale, 720. / scale))
            );
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| {
                    if let egui::Shape::Text(t) = &s.shape {
                        Some(t.galley.job.text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(text.len(), 83);
            assert!(
                !text
                    .iter()
                    .any(|t| t.contains("保存") || t.contains("恢复") || t.contains("校准"))
            );
            let origin = canvas.center();
            board_frame(&ctx, &mut s, scale, vec![Event::PointerMoved(origin)]);
            board_frame(&ctx, &mut s, scale, vec![pointer(origin, true)]);
            board_frame(
                &ctx,
                &mut s,
                scale,
                vec![Event::PointerMoved(origin + vec2(3. / scale, 0.))],
            );
            let target = origin + vec2(20. / scale, 12. / scale);
            board_frame(&ctx, &mut s, scale, vec![Event::PointerMoved(target)]);
            board_frame(&ctx, &mut s, scale, vec![pointer(target, false)]);
            assert_eq!(s.undo.len(), 1);
            drop(s);
            let shared = state.lock().unwrap();
            let point = shared.model.points["KeyA"];
            assert!((point.x - 0.5 - 20. / 1920.).abs() < 1e-7);
            assert!((point.y - 0.5 - 12. / 720.).abs() < 1e-7);
            assert_eq!(shared.entered["KeyA"], Point { x: 0.5, y: 0.5 });
        }
    }
    #[test]
    fn actual_board_arrow_moves_one_physical_pixel_at_each_dpi() {
        for scale in [1., 1.25, 1.5] {
            let ctx = egui::Context::default();
            let mut session = Session::new(prepared());
            session.selected = Some("KeyA".into());
            board_frame(&ctx, &mut session, scale, vec![]);
            board_frame(
                &ctx,
                &mut session,
                scale,
                vec![Event::Key {
                    key: Key::ArrowRight,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
            );
            assert!((session.model.points["KeyA"].x - 0.5 - 1. / 1920.).abs() < 1e-10);
        }
    }
    fn fixture() -> (Controller, mpsc::Receiver<Work>, mpsc::Sender<Reply>) {
        let (tx, jobs) = mpsc::channel();
        let (replies, rx) = mpsc::channel();
        let mut controller = Controller::new(PathBuf::new());
        controller.tx = Some(tx);
        controller.rx = Some(rx);
        {
            let mut s = controller.state.lock().unwrap();
            s.phase = Phase::Active;
            s.generation = 7;
            s.session = Some(Session::new(prepared()));
        }
        (controller, jobs, replies)
    }

    #[test]
    fn display_replacement_at_the_same_bounds_is_rejected() {
        let original = prepared().display;
        let mut replacement = original.clone();
        replacement.serial = "serial-2".into();
        assert!(original.confirm(replacement).is_err());

        let mut reconnected = original.clone();
        reconnected.device = "DISPLAY3".into();
        reconnected.identity = "other-port".into();
        reconnected.bounds = [0, 1080, 1600, 600];
        assert_eq!(
            original.confirm(reconnected.clone()),
            Ok(reconnected.clone())
        );
        reconnected.serial.clear();
        assert!(original.confirm(reconnected).is_err());
    }

    #[test]
    fn missing_display_preserves_draft_and_undo_until_the_original_returns() {
        let ctx = egui::Context::default();
        let (mut controller, jobs, replies) = fixture();
        let (generation, original, edited) = {
            let mut state = controller.state.lock().unwrap();
            let generation = state.generation;
            let session = state.session.as_mut().unwrap();
            let mut edited = session.model.points.clone();
            edited.insert("KeyA".into(), Point { x: 0.61, y: 0.57 });
            session.replace_points(edited.clone());
            session.placement.visible = true;
            session.next_display_check = Instant::now();
            (generation, session.display.clone(), edited)
        };

        controller.poll(&ctx);
        assert!(matches!(jobs.try_recv().unwrap(), Work::CheckDisplay(g) if g == generation));
        replies
            .send(Reply::DisplayChecked(generation, Err("设备已断开".into())))
            .unwrap();
        controller.poll(&ctx);
        controller.save(&ctx, Action::Save);
        assert!(jobs.try_recv().is_err());
        {
            let mut state = controller.state.lock().unwrap();
            assert_eq!(state.phase, Phase::Active);
            let session = state.session.as_mut().unwrap();
            assert!(!session.placement.visible);
            assert_eq!(session.model.points, edited);
            assert_eq!(session.undo.len(), 1);
            session.next_display_check = Instant::now();
        }

        controller.poll(&ctx);
        assert!(matches!(jobs.try_recv().unwrap(), Work::CheckDisplay(g) if g == generation));
        let mut reconnected = original.clone();
        reconnected.bounds = [0, 1080, 1600, 600];
        replies
            .send(Reply::DisplayChecked(
                generation,
                original.confirm(reconnected.clone()),
            ))
            .unwrap();
        controller.poll(&ctx);
        {
            let state = controller.state.lock().unwrap();
            let session = state.session.as_ref().unwrap();
            assert_eq!(session.display, reconnected);
            assert!(session.display_issue.is_none());
            assert_eq!(session.undo.len(), 1);
        }
        controller.save(&ctx, Action::Save);
        let Work::Save(g, model) = jobs.try_recv().unwrap() else {
            panic!("a draft can be saved after the original display returns")
        };
        assert_eq!(g, generation);
        assert_eq!(model.points, edited);
    }

    fn control_frame(
        ctx: &egui::Context,
        c: &mut Controller,
        events: Vec<Event>,
    ) -> egui::FullOutput {
        ctx.run(input(vec2(950., 300.), 1., events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                c.show_page(ui, &json!({"running":true,"monitorConnected":true}), &[]);
            });
        })
    }
    fn click_control(ctx: &egui::Context, c: &mut Controller, label: &str) {
        control_frame(ctx, c, vec![]);
        let p = c.controls[label].center();
        control_frame(ctx, c, vec![Event::PointerMoved(p)]);
        control_frame(ctx, c, vec![pointer(p, true)]);
        control_frame(ctx, c, vec![pointer(p, false)]);
    }

    #[test]
    fn starting_manual_calibration_shows_the_guide_below_the_preview_until_end() {
        let ctx = egui::Context::default();
        let (mut controller, jobs, replies) = fixture();
        controller.test_set_active(false);
        let guide_position = |controller: &mut Controller| {
            let output = ctx.run(input(vec2(950., 650.), 1., vec![]), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    controller.show_page(ui, &json!({"running":true,"monitorConnected":true}), &[]);
                });
            });
            let label_y = |label: &str| {
                output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => Some(text.pos.y),
                    _ => None,
                })
            };
            label_y("校准教程").map(|y| (y, label_y("有效显示区").unwrap()))
        };

        assert!(guide_position(&mut controller).is_none());
        click_control(&ctx, &mut controller, "开始校准");
        let Work::Begin(generation) = jobs.try_recv().unwrap() else {
            panic!("manual calibration starts")
        };
        let (guide_y, legend_y) = guide_position(&mut controller).unwrap();
        assert!(guide_y > legend_y);

        replies
            .send(Reply::Begun(generation, Ok(prepared())))
            .unwrap();
        controller.poll(&ctx);
        assert!(guide_position(&mut controller).is_some());

        click_control(&ctx, &mut controller, "保存并结束");
        let Work::Save(saved_generation, model) = jobs.try_recv().unwrap() else {
            panic!("manual calibration saves")
        };
        assert_eq!(generation, saved_generation);
        assert!(guide_position(&mut controller).is_some());
        replies.send(Reply::Saved(generation, Ok(model))).unwrap();
        controller.poll(&ctx);
        assert!(matches!(jobs.try_recv().unwrap(), Work::End(id) if id == generation));
        replies.send(Reply::Ended(generation, Ok(()))).unwrap();
        controller.poll(&ctx);
        assert!(guide_position(&mut controller).is_none());

        controller.state.lock().unwrap().phase = Phase::Saving;
        assert!(guide_position(&mut controller).is_none());
    }

    #[test]
    fn actual_main_controls_save_ack_restore_entry_defaults_and_undo_one_model() {
        let (mut c, jobs, replies) = fixture();
        let ctx = egui::Context::default();
        c.state
            .lock()
            .unwrap()
            .session
            .as_mut()
            .unwrap()
            .model
            .move_point("KeyA", 0.55, 0.52);
        click_control(&ctx, &mut c, "保存");
        let Work::Save(g, mut model) = jobs.try_recv().unwrap() else {
            panic!("save command required")
        };
        assert_eq!(g, 7);
        assert_eq!(model.points["KeyA"], Point { x: 0.55, y: 0.52 });
        model.original = model.points.clone();
        model.document["revision"] = json!("saved");
        replies.send(Reply::Saved(g, Ok(model))).unwrap();
        c.poll(&ctx);
        click_control(&ctx, &mut c, "恢复进入前");
        {
            let s = c.state.lock().unwrap();
            let model = &s.session.as_ref().unwrap().model;
            assert_eq!(model.points["KeyA"], Point { x: 0.5, y: 0.5 });
            assert_eq!(model.original["KeyA"], Point { x: 0.55, y: 0.52 });
            assert_eq!(model.document["revision"], "saved");
        }
        click_control(&ctx, &mut c, "撤销");
        assert_eq!(
            c.state
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .model
                .points["KeyA"],
            Point { x: 0.55, y: 0.52 }
        );
        click_control(&ctx, &mut c, "恢复默认");
        let s = c.state.lock().unwrap();
        let session = s.session.as_ref().unwrap();
        assert_eq!(session.model.points["KeyA"], Point { x: 0.7, y: 0.6 });
        assert_eq!(session.entered["KeyA"], Point { x: 0.5, y: 0.5 });
        assert!(jobs.try_recv().is_err());
    }
    #[test]
    fn save_and_end_uses_ack_then_resume_and_old_replies_cannot_mutate_new_session() {
        let (mut c, jobs, replies) = fixture();
        let ctx = egui::Context::default();
        click_control(&ctx, &mut c, "保存并结束");
        let Work::Save(g, model) = jobs.try_recv().unwrap() else {
            panic!()
        };
        replies.send(Reply::Saved(g, Ok(model))).unwrap();
        c.poll(&ctx);
        assert!(matches!(jobs.try_recv().unwrap(), Work::End(7)));
        replies.send(Reply::Ended(7, Ok(()))).unwrap();
        c.poll(&ctx);
        assert!(!c.has_session());
        {
            let mut s = c.state.lock().unwrap();
            s.phase = Phase::Active;
            s.generation = 8;
            s.session = Some(Session::new(prepared()));
        }
        replies.send(Reply::Saved(7, Ok(prepared().model))).unwrap();
        replies.send(Reply::Ended(7, Ok(()))).unwrap();
        replies.send(Reply::Begun(7, Ok(prepared()))).unwrap();
        c.poll(&ctx);
        let s = c.state.lock().unwrap();
        assert_eq!(s.phase, Phase::Active);
        assert_eq!(s.generation, 8);
        assert_eq!(
            s.session.as_ref().unwrap().entered["KeyA"],
            Point { x: 0.5, y: 0.5 }
        );
    }
    #[test]
    fn end_while_starting_waits_for_begin_then_resumes_without_opening_board() {
        let (mut c, jobs, replies) = fixture();
        let ctx = egui::Context::default();
        {
            let mut s = c.state.lock().unwrap();
            s.phase = Phase::Starting;
            s.session = None;
        }
        c.end(&ctx);
        assert!(matches!(jobs.try_recv().unwrap(), Work::End(7)));
        replies.send(Reply::Begun(7, Ok(prepared()))).unwrap();
        c.poll(&ctx);
        assert!(c.state.lock().unwrap().session.is_none());
        replies.send(Reply::Ended(7, Ok(()))).unwrap();
        c.poll(&ctx);
        assert!(!c.has_session());
    }
    #[test]
    fn board_escape_and_end_close_only_child_and_resume_errors_remain_retryable() {
        let (mut c, jobs, replies) = fixture();
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut close = false;
        let output = ctx.run(
            input(
                vec2(400., 200.),
                1.,
                vec![Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
            ),
            |ctx| {
                ctx.show_viewport_deferred(
                    viewport_id(7),
                    egui::ViewportBuilder::default(),
                    |_, _| {},
                );
                close = board_close_requested(ctx);
                if close {
                    c.end(ctx);
                }
            },
        );
        assert!(close);
        assert!(matches!(jobs.try_recv().unwrap(), Work::End(7)));
        assert!(
            !output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::Close))
        );
        assert!(
            output.viewport_output[&viewport_id(7)]
                .commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::Close))
        );
        replies
            .send(Reply::Ended(7, Err("resume failed".into())))
            .unwrap();
        c.poll(&ctx);
        assert!(c.has_session());
        c.end(&ctx);
        assert!(matches!(jobs.try_recv().unwrap(), Work::End(7)));
    }
    #[test]
    fn placement_requires_two_distinct_frames_not_two_layout_passes() {
        let mut p = Placement::default();
        assert!(!p.record_match(10));
        assert!(!p.record_match(10));
        assert!(p.record_match(11));
        p.stable = 0;
        p.last_frame = None;
        assert!(!p.record_match(12));
        assert!(p.record_match(13));
    }
    struct FakeHost {
        trace: Arc<Mutex<Vec<String>>>,
        lease: u64,
        paused: bool,
    }
    impl Backend for FakeHost {
        fn begin(&mut self) -> Result<Prepared, String> {
            self.lease += 1;
            self.paused = true;
            self.trace
                .lock()
                .unwrap()
                .push(format!("pause{}", self.lease));
            Ok(prepared())
        }
        fn check_display(&self) -> Result<CalibrationDisplay, String> {
            Ok(prepared().display)
        }
        fn save(&mut self, model: &Calibration) -> Result<Calibration, String> {
            self.trace
                .lock()
                .unwrap()
                .push(format!("save{}", self.lease));
            Ok(model.clone())
        }
        fn end(&mut self) -> Result<(), String> {
            if self.paused {
                self.trace
                    .lock()
                    .unwrap()
                    .push(format!("resume{}", self.lease));
                self.paused = false;
            }
            Ok(())
        }
    }
    #[test]
    fn fifo_worker_finishes_save_before_resume_and_never_resumes_a_new_lease() {
        let trace = Arc::new(Mutex::new(vec![]));
        let backend = FakeHost {
            trace: trace.clone(),
            lease: 0,
            paused: false,
        };
        let (tx, jobs) = mpsc::channel();
        let (replies, _rx) = mpsc::channel();
        let (done, result) = mpsc::channel();
        let thread =
            thread::spawn(move || worker(backend, jobs, replies, egui::Context::default()));
        for job in [
            Work::Begin(1),
            Work::Save(1, prepared().model),
            Work::End(1),
            Work::Begin(2),
            Work::End(2),
            Work::Shutdown(done),
        ] {
            tx.send(job).unwrap();
        }
        result.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        assert_eq!(
            *trace.lock().unwrap(),
            vec!["pause1", "save1", "resume1", "pause2", "resume2"]
        );
    }
    #[test]
    fn disconnected_controller_still_releases_owned_pause() {
        let trace = Arc::new(Mutex::new(vec![]));
        let backend = FakeHost {
            trace: trace.clone(),
            lease: 0,
            paused: false,
        };
        let (tx, jobs) = mpsc::channel();
        let (replies, rx) = mpsc::channel();
        let thread =
            thread::spawn(move || worker(backend, jobs, replies, egui::Context::default()));
        tx.send(Work::Begin(1)).unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(tx);
        thread.join().unwrap();
        assert_eq!(*trace.lock().unwrap(), vec!["pause1", "resume1"]);
    }
    #[test]
    fn ambiguous_pause_ack_keeps_cleanup_responsibility_until_resume_ack() {
        let applied = std::cell::Cell::new(false);
        let pid = std::process::id();
        let player = PausedPlayer::watch(&json!({"playerPid":pid})).unwrap();
        let mut paused = None;
        let result = pause_owned(&mut paused, player, |request| {
            assert_eq!(request["action"], "pause");
            assert_eq!(request["temporary"], true);
            assert_eq!(request["expectedPlayerPid"], pid);
            applied.set(true);
            Err("pause ACK lost".into())
        });
        assert!(result.is_err());
        assert!(paused.is_some());
        assert!(applied.get());
        resume_owned(&mut paused, |request| {
            assert_eq!(
                request,
                json!({"action":"resume","temporary":true,
                                      "expectedPlayerPid":pid,"ownerPid":std::process::id()})
            );
            applied.set(false);
            Ok(json!({"playerPid":pid}))
        })
        .unwrap();
        assert!(paused.is_none());
        assert!(!applied.get());
    }
    #[test]
    fn ending_after_the_original_player_exits_does_not_contact_a_replacement() {
        use windows::{Win32::System::Threading::CreateEventW, core::PCWSTR};

        let signaled = unsafe { CreateEventW(None, true, true, PCWSTR::null()) }.unwrap();
        let mut paused = Some(PausedPlayer {
            pid: 123,
            process: unsafe { OwnedHandle::from_raw_handle(signaled.0) },
        });
        resume_owned(&mut paused, |_| panic!("the original player has exited")).unwrap();
        assert!(paused.is_none());
    }
    #[test]
    fn losing_the_pipe_while_the_original_player_lives_keeps_the_pause() {
        let pid = std::process::id();
        let mut paused = Some(PausedPlayer::watch(&json!({"playerPid":pid})).unwrap());
        assert!(resume_owned(&mut paused, |_| Err("pipe unavailable".into())).is_err());
        assert!(resume_owned(&mut paused, |_| Ok(json!({"playerPid":0}))).is_err());
        assert_eq!(paused.as_ref().unwrap().pid, pid);
        resume_owned(&mut paused, |_| Ok(json!({"playerPid":pid}))).unwrap();
        assert!(paused.is_none());
    }
    #[test]
    fn close_during_save_waits_for_ack_and_failure_preserves_edit_and_cancels_main_close() {
        let (mut c, jobs, replies) = fixture();
        let ctx = egui::Context::default();
        c.state
            .lock()
            .unwrap()
            .session
            .as_mut()
            .unwrap()
            .model
            .move_point("KeyA", 0.61, 0.53);
        click_control(&ctx, &mut c, "保存");
        assert!(matches!(jobs.try_recv().unwrap(), Work::Save(7, _)));
        c.end(&ctx);
        assert!(jobs.try_recv().is_err());
        assert_eq!(c.state.lock().unwrap().phase, Phase::Saving);
        replies
            .send(Reply::Saved(7, Err("disk full".into())))
            .unwrap();
        c.poll(&ctx);
        assert!(c.take_close_cancelled());
        assert!(!c.take_close_cancelled());
        assert!(matches!(jobs.try_recv().unwrap(), Work::CheckDisplay(7)));
        assert!(jobs.try_recv().is_err());
        let s = c.state.lock().unwrap();
        assert_eq!(s.phase, Phase::Active);
        assert_eq!(s.error, "disk full");
        assert_eq!(
            s.session.as_ref().unwrap().model.points["KeyA"],
            Point { x: 0.61, y: 0.53 }
        );
    }
    fn fit_fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "a75-fit-ui-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("existing-resources/finalmouse")).unwrap();
        std::fs::write(
            root.join("existing-resources/finalmouse/action75-ue-keymap.json"),
            include_bytes!("../../../existing-resources/finalmouse/action75-ue-keymap.json"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/fm-calibration.json"), b"original-on-disk").unwrap();
        root
    }

    fn fit_session() -> Session {
        let mut prepared = prepared();
        let points = preview::fit_reference();
        let order = points.keys().cloned().collect();
        prepared.model = Calibration::from_points(json!({"revision":"before"}), points, order);
        prepared.defaults = prepared.model.points.clone();
        Session::new(prepared)
    }

    #[test]
    fn action75_fit_preserves_the_calibrated_navigation_key_positions() {
        let document: Value =
            serde_json::from_str(include_str!("../../../assets/defaults/fm-calibration.json"))
                .unwrap();
        let current: BTreeMap<String, Point> =
            serde_json::from_value(document["points"].clone()).unwrap();
        let reference = preview::fit_reference();
        let fitted = core::calibration_fit::fit(&current, &reference).unwrap();
        assert_eq!(fitted.len(), 83);
        for (code, point) in &fitted {
            let original = current[code];
            let dx = (point.x - original.x) * 1920.;
            let dy = (point.y - original.y) * 720.;
            assert!(dx.hypot(dy) < 10., "{code} moved ({dx}, {dy}) pixels");
        }
        assert_eq!(key_label("Insert"), "ScrLk");
    }

    fn modal_click(ctx: &egui::Context, c: &mut Controller, label: &str) {
        let output = control_frame(ctx, c, vec![]);
        let point = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label => {
                    Some(t.pos + t.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .expect("modal button");
        control_frame(ctx, c, vec![Event::PointerMoved(point)]);
        control_frame(ctx, c, vec![pointer(point, true)]);
        control_frame(ctx, c, vec![pointer(point, false)]);
    }

    #[test]
    fn fit_requires_confirmation_is_preview_only_and_undoable() {
        let root = fit_fixture();
        let mut controller = Controller::new(root.clone());
        {
            let mut state = controller.state.lock().unwrap();
            let mut session = fit_session();
            session.model.points.get_mut("KeyA").unwrap().y += 0.03;
            state.phase = Phase::Active;
            state.session = Some(session);
        }
        let before = controller
            .state
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .model
            .points
            .clone();
        let ctx = egui::Context::default();
        click_control(&ctx, &mut controller, "拟合键位");
        assert!(controller.fit_confirmation.is_some());
        modal_click(&ctx, &mut controller, "取消");
        assert!(controller.fit_confirmation.is_none());
        assert_eq!(
            controller
                .state
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .model
                .points,
            before
        );
        click_control(&ctx, &mut controller, "拟合键位");
        modal_click(&ctx, &mut controller, "继续拟合");
        {
            let state = controller.state.lock().unwrap();
            assert!(state.error.is_empty(), "{}", state.error);
            assert_ne!(state.session.as_ref().unwrap().model.points, before);
        }
        assert_eq!(
            std::fs::read(root.join("data/fm-calibration.json")).unwrap(),
            b"original-on-disk"
        );
        click_control(&ctx, &mut controller, "撤销");
        assert_eq!(
            controller
                .state
                .lock()
                .unwrap()
                .session
                .as_ref()
                .unwrap()
                .model
                .points,
            before
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fit_after_save_and_end_uses_the_saved_points_without_reopening_the_board() {
        let root = fit_fixture();
        let (mut c, jobs, replies) = fixture();
        c.root = root.clone();
        {
            let mut session = fit_session();
            session.model.points.get_mut("KeyA").unwrap().y += 0.03;
            c.state.lock().unwrap().session = Some(session);
        }

        let ctx = egui::Context::default();
        click_control(&ctx, &mut c, "保存并结束");
        let Work::Save(g, mut manual) = jobs.try_recv().unwrap() else {
            panic!("manual save")
        };
        manual.original = manual.points.clone();
        replies.send(Reply::Saved(g, Ok(manual.clone()))).unwrap();
        c.poll(&ctx);
        assert!(matches!(jobs.try_recv().unwrap(), Work::End(generation) if generation == g));
        replies.send(Reply::Ended(g, Ok(()))).unwrap();
        c.poll(&ctx);
        assert!(!c.has_session());

        let reference = preview::fit_reference();
        let expected = core::calibration_fit::fit(&manual.points, &reference).unwrap();
        assert_ne!(expected, manual.points);
        click_control(&ctx, &mut c, "拟合键位");
        modal_click(&ctx, &mut c, "继续拟合");
        assert!(!c.has_session());
        assert!(jobs.try_recv().is_err());
        assert_eq!(
            c.state.lock().unwrap().preview_model().unwrap().points,
            expected
        );
        assert_eq!(
            std::fs::read(root.join("data/fm-calibration.json")).unwrap(),
            b"original-on-disk"
        );
        click_control(&ctx, &mut c, "撤销拟合");
        assert_eq!(
            c.state.lock().unwrap().preview_model().unwrap().points,
            manual.points
        );

        click_control(&ctx, &mut c, "拟合键位");
        modal_click(&ctx, &mut c, "继续拟合");
        click_control(&ctx, &mut c, "保存拟合");
        let Work::Save(generation, mut fitted) = jobs.try_recv().unwrap() else {
            panic!("fit save")
        };
        assert_eq!(fitted.original, manual.points);
        assert_eq!(fitted.points, expected);
        replies
            .send(Reply::Saved(generation, Err("no ACK".into())))
            .unwrap();
        c.poll(&ctx);
        assert!(!c.has_session());
        assert_eq!(
            c.state.lock().unwrap().preview_model().unwrap().points,
            expected
        );

        click_control(&ctx, &mut c, "保存拟合");
        let Work::Save(retry_generation, _) = jobs.try_recv().unwrap() else {
            panic!("fit retry")
        };
        assert_ne!(retry_generation, generation);
        fitted.original = fitted.points.clone();
        replies
            .send(Reply::Saved(retry_generation, Ok(fitted)))
            .unwrap();
        c.poll(&ctx);
        assert!(!c.has_session());
        assert!(jobs.try_recv().is_err());
        let s = c.state.lock().unwrap();
        assert!(s.fit_preview.is_none());
        assert_eq!(s.preview_model().unwrap().points, expected);
        std::fs::remove_dir_all(root).unwrap();
    }
}
