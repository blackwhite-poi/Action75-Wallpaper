use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod branding;
pub use branding::APP_NAME;
pub mod calibration_fit;
pub mod game_compat;
pub mod graphics_adapters;
pub mod pack_model;
pub mod pipe_client;
mod player_control;
mod player_start;
pub mod runtime_identity;
pub mod software_update;
pub mod startup;
pub mod storage;
pub mod thumbnail;
pub mod update_install;
pub mod wallpaper_import;
mod wallpaper_library;
pub use player_control::command;
use player_control::command_timeout;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub fps: u32,
    #[serde(rename = "effectBrightness")]
    pub brightness: f32,
    #[serde(rename = "effectScale")]
    pub scale: f32,
    #[serde(rename = "effectIntensity")]
    pub intensity: f32,
    #[serde(rename = "effectColor")]
    pub color: [u8; 3],
    #[serde(rename = "effectSaturation")]
    pub saturation: f32,
    #[serde(rename = "effectHue")]
    pub hue: f32,
    #[serde(rename = "effectPalette")]
    pub palette: String,
    #[serde(rename = "effectPaletteColors")]
    pub palette_colors: Vec<[u8; 3]>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 60,
            brightness: 1.,
            scale: 1.,
            intensity: 1.,
            color: [255; 3],
            saturation: 1.,
            hue: 0.,
            palette: "original".into(),
            palette_colors: vec![[255, 110, 155], [77, 208, 225], [178, 147, 255]],
        }
    }
}
impl Settings {
    pub fn from_status(value: &Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.fps != 60
            || !self.brightness.is_finite()
            || !(0.0..=4.).contains(&self.brightness)
            || !self.scale.is_finite()
            || !(0.5..=3.).contains(&self.scale)
            || !self.intensity.is_finite()
            || !(0.0..=3.).contains(&self.intensity)
            || !self.saturation.is_finite()
            || !(0.0..=2.5).contains(&self.saturation)
            || !self.hue.is_finite()
            || !(-180.0..=180.0).contains(&self.hue)
            || self.palette.is_empty()
            || self.palette.len() > 32
            || !self
                .palette
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || !(1..=9).contains(&self.palette_colors.len())
        {
            return Err("特效参数超出允许范围".into());
        }
        Ok(())
    }
    pub fn request(&self) -> Value {
        json!({"action":"setSettings","settings":self})
    }
    pub fn reset_color_grade(&mut self) {
        self.color = [255; 3];
        self.saturation = 1.;
        self.hue = 0.;
        self.palette = "original".into();
    }
    pub fn add_palette_color(&mut self, color: [u8; 3]) -> bool {
        if self.palette_colors.len() >= 9 {
            return false;
        }
        self.palette_colors.push(color);
        true
    }
    pub fn remove_palette_color(&mut self, index: usize) -> bool {
        if self.palette_colors.len() <= 1 || index >= self.palette_colors.len() {
            return false;
        }
        self.palette_colors.remove(index);
        true
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PercentScale {
    Brightness,
    Saturation,
    Intensity,
}
impl PercentScale {
    pub fn factor(self, percent: f32) -> f32 {
        let p = percent.clamp(0., 100.);
        match self {
            Self::Brightness => 4. * (p / 100.).powi(2),
            Self::Saturation => {
                if p <= 50. {
                    p / 50.
                } else {
                    1. + (p - 50.) * 1.5 / 50.
                }
            }
            Self::Intensity => {
                if p <= 50. {
                    p / 50.
                } else {
                    1. + (p - 50.) * 2. / 50.
                }
            }
        }
    }
    pub fn percent(self, factor: f32) -> f32 {
        let p = match self {
            Self::Brightness => (factor.max(0.) / 4.).sqrt() * 100.,
            Self::Saturation => {
                if factor <= 1. {
                    factor * 50.
                } else {
                    50. + (factor - 1.) / 1.5 * 50.
                }
            }
            Self::Intensity => {
                if factor <= 1. {
                    factor * 50.
                } else {
                    50. + (factor - 1.) / 2. * 50.
                }
            }
        };
        p.clamp(0., 100.)
    }
}
pub fn single_color_hue_visible(settings: &Settings) -> bool {
    settings.palette == "custom" && settings.palette_colors.len() == 1
}

pub fn player(root: &Path) -> PathBuf {
    root.join("dist/Action75Native/Action75Native.exe")
}
pub fn default_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        for _ in 0..6 {
            if let Some(p) = dir {
                if p.join("existing-resources/finalmouse/action75-ue-keymap.json")
                    .is_file()
                {
                    return p.to_owned();
                }
                dir = p.parent();
            } else {
                break;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
pub fn confirm_calibration_access(root: &Path, player_pid: u32) -> Result<(), String> {
    let start = Instant::now();
    confirm_calibration_access_with(
        |request, timeout| {
            let status = command_timeout(root, request, timeout)?;
            if status["playerPid"].as_u64() != Some(u64::from(player_pid)) {
                return Err("播放器实例已变化，请重新开始校准".into());
            }
            if status["paused"] != true
                || status
                    .get("calibrationPauseOwnerPid")
                    .is_some_and(|owner| owner.as_u64() != Some(u64::from(std::process::id())))
            {
                return Err("播放器未保留本次校准暂停".into());
            }
            Ok(status)
        },
        thread::sleep,
        || start.elapsed(),
        Duration::from_secs(2),
    )
}
fn confirm_calibration_access_with<R, W, N>(
    mut request: R,
    mut wait: W,
    mut now: N,
    timeout: Duration,
) -> Result<(), String>
where
    R: FnMut(Value, Duration) -> Result<Value, String>,
    W: FnMut(Duration),
    N: FnMut() -> Duration,
{
    let start = now();
    loop {
        let remaining = timeout.saturating_sub(now().saturating_sub(start));
        if remaining.is_zero() {
            break Err("副屏鼠标保护尚未释放，校准未打开".to_owned());
        }
        let status = match request(json!({"action":"status"}), remaining) {
            Ok(status) => status,
            Err(error) => break Err(error),
        };
        if let Some(error) = status["displayGuardError"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            break Err(error.to_owned());
        }
        if let Some(mode) = status.get("displayGuardMode") {
            if status["displayGuardActive"] != true
                && matches!(mode.as_str(), Some("suspended" | "disabled" | "headless"))
            {
                break Ok(());
            }
        } else if status["exclusiveDisplay"] != true || status["displayGuardActive"] != true {
            break Ok(());
        }
        let remaining = timeout.saturating_sub(now().saturating_sub(start));
        wait(remaining.min(Duration::from_millis(75)));
    }
}
pub fn start_player(root: &Path) -> Result<(), String> {
    player_start::start(root)
}

pub enum Job {
    SetExclusive {
        generation: u64,
        request: Value,
    },
    FlushAndQuit {
        write: pack_model::FinalWrite,
        done: mpsc::Sender<Result<(), String>>,
    },
    PrepareRestart(pack_model::FinalWrite),
    Full,
    ReadDevices,
    RefreshDevices,
    PreviewPack {
        id: String,
        selection: u64,
        values: Map<String, Value>,
    },
    Migrate(storage::Preferences),
    SaveInterface(storage::InterfacePreferences),
    Startup(startup::StartupAction),
    DetectGraphicsAdapters,
    SaveGpuPreference(storage::GpuPreference),
    Setup {
        monitor: String,
        monitor_identity: Option<String>,
        device: String,
    },
    DeletePack {
        id: String,
        path: PathBuf,
    },
    SelectPack {
        id: String,
        selection: u64,
    },
    LoadPack {
        id: String,
        selection: u64,
    },
    RecoverPack(String),
    WritePack {
        id: String,
        selection: u64,
        edit: u64,
        values: Map<String, Value>,
        revision: Option<Value>,
    },
    ResetPack {
        id: String,
        selection: u64,
        edit: u64,
        revision: Option<Value>,
    },
    Status,
    Request(Value),
    Settings {
        generation: u64,
        settings: Settings,
    },
    Start,
    Save(Calibration),
    Quit,
}
pub enum Event {
    DevicesRefreshed(Result<(Vec<Value>, Vec<Value>), String>),
    PackDeleted {
        id: String,
        result: Result<(), String>,
    },
    Previewed {
        id: String,
        selection: u64,
        error: Option<String>,
    },
    ExclusiveAck {
        generation: u64,
        status: Value,
    },
    ExclusiveFailed {
        generation: u64,
        error: String,
    },
    ConnectionFailed(String),
    Full {
        connection_error: Option<String>,
        catalog_error: Option<String>,
        status: Value,
        packs: Vec<Value>,
        monitors: Vec<Value>,
        devices: Vec<Value>,
        palettes: Vec<Value>,
    },
    Status(Value),
    PackSelected {
        id: String,
        selection: u64,
        status: Value,
        bundle: pack_model::Bundle,
    },
    PackLoaded {
        id: String,
        selection: u64,
        bundle: pack_model::Bundle,
    },
    PackRecovered {
        id: String,
        result: Result<(pack_model::Bundle, bool), String>,
    },
    PackSaved {
        id: String,
        selection: u64,
        edit: u64,
        bundle: pack_model::Bundle,
    },
    PackFailed {
        id: String,
        selection: u64,
        edit: Option<u64>,
        error: String,
    },
    SettingsAck {
        generation: u64,
        status: Value,
    },
    SettingsFailed {
        generation: u64,
        error: String,
    },
    Saved(String),
    InterfaceSaveFailed(String),
    Startup(Result<startup::StartupState, String>),
    GraphicsAdapters(Result<graphics_adapters::GraphicsAdapters, String>),
    GpuPreferenceSaved(Result<storage::GpuPreference, String>),
    RestartPrepared(Result<(), String>),
    RequestFailed(String),
    SetupFinished(Result<(), String>),
    MigrationFinished(Result<(), String>),
    Error(String),
}
pub fn worker(
    root: PathBuf,
    repaint: eframe::egui::Context,
) -> (mpsc::Sender<Job>, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let (reply, events) = mpsc::channel();
    thread::spawn(move || {
        let mut preview = PackPreview::default();
        let mut catalog = None::<mpsc::Receiver<LoadedCatalog>>;
        loop {
            if let Some(load) = &catalog {
                let ready = match load.try_recv() {
                    Ok(loaded) => Some(loaded),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(LoadedCatalog {
                        player_pid: None,
                        packs: local_packs(&root),
                        error: Some("壁纸目录读取任务已退出".into()),
                    }),
                };
                if let Some(loaded) = ready {
                    catalog = None;
                    // Take status on the command queue after the scan so an
                    // earlier catalog request cannot undo a newer selection.
                    let event =
                        full_with_catalog(&root, |request| command(&root, request), Some(loaded));
                    let _ = reply.send(event.unwrap_or_else(Event::Error));
                    repaint.request_repaint();
                }
            }
            let job = if catalog.is_some() {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                let Ok(job) = rx.recv() else { break };
                job
            };
            if matches!(&job, Job::Start | Job::Setup { .. } | Job::Migrate(_)) {
                catalog = None;
            }
            let event = match job {
                Job::FlushAndQuit { write, done } => {
                    let result = preview
                        .finish(&root, write)
                        .inspect_err(|error| storage::log(&root, "ERROR", error));
                    let _ = done.send(result);
                    break;
                }
                Job::PrepareRestart(write) => Ok(Event::RestartPrepared(
                    preview
                        .save(&root, write)
                        .and_then(|_| game_compat::stop_player(&root))
                        .inspect_err(|error| storage::log(&root, "ERROR", error)),
                )),
                Job::SetExclusive {
                    generation,
                    request,
                } => Ok(match command(&root, request) {
                    Ok(status) => Event::ExclusiveAck { generation, status },
                    Err(error) => Event::ExclusiveFailed { generation, error },
                }),
                Job::DeletePack { id, path } => {
                    let result = wallpaper_import::delete_pack(&root, &id, &path);
                    if result.is_ok() {
                        preview.settled(&id);
                    }
                    Ok(Event::PackDeleted { id, result })
                }
                Job::Quit => break,
                Job::Status => Ok(match command(&root, json!({"action":"status"})) {
                    Ok(status) => Event::Status(status),
                    Err(error) => Event::ConnectionFailed(error),
                }),
                Job::Request(v) => Ok(
                    match command(&root, v)
                        .and_then(|v| {
                            if v.is_object() && v.get("running").is_some() {
                                Ok(v)
                            } else {
                                command(&root, json!({"action":"status"}))
                            }
                        })
                        .inspect_err(|error| storage::log(&root, "ERROR", error))
                    {
                        Ok(status) => Event::Status(status),
                        Err(error) => Event::RequestFailed(error),
                    },
                ),
                Job::Settings {
                    generation,
                    settings,
                } => Ok(match command(&root, settings.request()) {
                    Ok(status) => Event::SettingsAck { generation, status },
                    Err(error) => Event::SettingsFailed { generation, error },
                }),
                Job::SelectPack { id, selection } => {
                    Ok(match player_control::select_pack(&root, &id) {
                        Ok((status, bundle)) => {
                            preview.pack = None;
                            Event::PackSelected {
                                id,
                                selection,
                                status,
                                bundle,
                            }
                        }
                        Err(failure) => {
                            if let Some(status) = failure.observed {
                                let _ = reply.send(Event::Status(status));
                            }
                            Event::PackFailed {
                                id,
                                selection,
                                edit: None,
                                error: failure.error,
                            }
                        }
                    })
                }
                Job::LoadPack { id, selection } => Ok(
                    match load_pack(&root, &id).inspect(|_| preview.settled(&id)) {
                        Ok(bundle) => Event::PackLoaded {
                            id,
                            selection,
                            bundle,
                        },
                        Err(error) => Event::PackFailed {
                            id,
                            selection,
                            edit: None,
                            error,
                        },
                    },
                ),
                Job::RecoverPack(id) => {
                    let result = recover_profile_with(&id, |request| command(&root, request));
                    if result.is_ok() {
                        preview.settled(&id);
                    }
                    Ok(Event::PackRecovered { id, result })
                }
                Job::WritePack {
                    id,
                    selection,
                    edit,
                    values,
                    revision,
                } => Ok(
                    match pack_request(&root, "setPackSettings", &id, Some(values), revision)
                        .inspect(|_| preview.settled(&id))
                    {
                        Ok(bundle) => Event::PackSaved {
                            id,
                            selection,
                            edit,
                            bundle,
                        },
                        Err(error) => Event::PackFailed {
                            id,
                            selection,
                            edit: Some(edit),
                            error,
                        },
                    },
                ),
                Job::ResetPack {
                    id,
                    selection,
                    edit,
                    revision,
                } => Ok(
                    match pack_request(&root, "resetPackSettings", &id, None, revision)
                        .inspect(|_| preview.settled(&id))
                    {
                        Ok(bundle) => Event::PackSaved {
                            id,
                            selection,
                            edit,
                            bundle,
                        },
                        Err(error) => Event::PackFailed {
                            id,
                            selection,
                            edit: Some(edit),
                            error,
                        },
                    },
                ),
                Job::Start | Job::Full => {
                    if matches!(job, Job::Start)
                        && let Err(error) = start_player(&root)
                    {
                        full_with(&root, |_| Err(error.clone()))
                    } else {
                        if catalog.is_none() {
                            let (completed, pending) = mpsc::channel();
                            let root = root.clone();
                            thread::spawn(move || {
                                let _ = completed.send(load_catalog(&root));
                            });
                            catalog = Some(pending);
                        }
                        continue;
                    }
                }
                Job::ReadDevices => Ok(Event::DevicesRefreshed(read_devices_with(|request| {
                    command(&root, request)
                }))),
                Job::RefreshDevices => Ok(refresh_devices_with(|request| command(&root, request))),
                Job::PreviewPack {
                    id,
                    selection,
                    values,
                } => {
                    let error = preview.send(&root, &id, values).err();
                    Ok(Event::Previewed {
                        id,
                        selection,
                        error,
                    })
                }
                Job::Setup {
                    monitor,
                    monitor_identity,
                    device,
                } => {
                    let result = (|| {
                        let mut preferences = storage::Preferences::load_checked(&root)?;
                        start_player(&root)?;
                        configure_first_run(
                            &monitor,
                            monitor_identity.as_deref(),
                            &device,
                            |request| command(&root, request),
                        )?;
                        preferences.setup_complete = true;
                        preferences.save(&root)?;
                        storage::log(&root, "INFO", "首次设置完成");
                        Ok::<(), String>(())
                    })()
                    .inspect_err(|error| storage::log(&root, "ERROR", error));
                    let _ = reply.send(Event::SetupFinished(result));
                    repaint.request_repaint();
                    // Setup canceled any pending scan; complete its replacement
                    // even when setup failed, without acknowledging another job.
                    full(&root)
                }
                Job::SaveInterface(interface) => {
                    let saved =
                        storage::Preferences::load_checked(&root).and_then(|mut preferences| {
                            preferences.interface = interface;
                            preferences.save(&root)
                        });
                    match saved {
                        Ok(()) => continue,
                        Err(error) => Ok(Event::InterfaceSaveFailed(error)),
                    }
                }
                Job::Startup(action) => Ok(Event::Startup(startup::update(&root, action))),
                Job::DetectGraphicsAdapters => Ok(Event::GraphicsAdapters(
                    graphics_adapters::GraphicsAdapters::detect()
                        .map_err(|error| error.to_string()),
                )),
                Job::SaveGpuPreference(gpu) => Ok(Event::GpuPreferenceSaved(
                    storage::Preferences::load_checked(&root).and_then(|mut preferences| {
                        preferences.gpu_preference = gpu;
                        preferences.save(&root).map(|_| gpu)
                    }),
                )),
                Job::Migrate(next) => {
                    let result =
                        migrate_preferences_with(&root, next, |request| command(&root, request))
                            .inspect_err(|error| storage::log(&root, "ERROR", error));
                    let _ = reply.send(Event::MigrationFinished(result));
                    repaint.request_repaint();
                    full(&root)
                }
                Job::Save(model) => save_calibration(&root, &model).map(Event::Saved),
            };
            if let Err(error) = &event {
                storage::log(&root, "ERROR", error);
            }
            if let Ok(event) = &event {
                preview.record_write(event);
            }
            let _ = reply.send(event.unwrap_or_else(Event::Error));
            repaint.request_repaint();
        }
    });
    (tx, events)
}

struct WriteReceipt {
    id: String,
    selection: u64,
    edit: u64,
    result: Result<Value, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MigrationSession {
    Offline,
    Online(u32),
}

fn migration_session(
    request: &mut impl FnMut(Value) -> Result<Value, String>,
) -> Result<MigrationSession, String> {
    match request(json!({"action":"status"})) {
        Ok(status) => status["playerPid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid != 0)
            .map(MigrationSession::Online)
            .ok_or_else(|| "播放器未提供有效实例身份，未迁移目录".into()),
        Err(error) if error == "播放器未启动" => Ok(MigrationSession::Offline),
        Err(error) => Err(format!("无法确认播放器状态，目录未提交：{error}")),
    }
}

fn migration_request(
    request: &mut impl FnMut(Value) -> Result<Value, String>,
    action: &str,
    pid: u32,
) -> Result<(), String> {
    let status = request(json!({"action":action,"expectedPlayerPid":pid}))?;
    if status["playerPid"].as_u64() != Some(u64::from(pid)) {
        return Err("目录迁移期间播放器实例已变化".into());
    }
    Ok(())
}

// Both forward activation and rollback cross the same confirmed session seam.
fn migrate_preferences_with(
    root: &Path,
    next: storage::Preferences,
    mut request: impl FnMut(Value) -> Result<Value, String>,
) -> Result<(), String> {
    let mut preferences = storage::Preferences::load_checked(root)?;
    let old_wallpapers = preferences.wallpapers(root);
    preferences.wallpaper_dir = next.wallpaper_dir;
    preferences.log_dir = next.log_dir;
    let reload = old_wallpapers != preferences.wallpapers(root);
    let session = migration_session(&mut request)?;
    if let MigrationSession::Online(pid) = session {
        if let Err(error) = migration_request(&mut request, "suspendDiagnostics", pid) {
            let _ = migration_request(&mut request, "resumeDiagnostics", pid);
            return Err(format!("无法暂停诊断日志，未迁移目录：{error}"));
        }
    }
    let result = storage::migrate(root, &preferences, || {
        if migration_session(&mut request)? != session {
            return Err("目录迁移期间播放器实例或运行状态已变化，未清理源文件".into());
        }
        if reload && let MigrationSession::Online(pid) = session {
            migration_request(&mut request, "reloadDirectories", pid)?;
        }
        Ok(())
    });
    let resumed = match session {
        MigrationSession::Online(pid) => migration_request(&mut request, "resumeDiagnostics", pid),
        MigrationSession::Offline => Ok(()),
    };
    match (result, resumed) {
        (Err(error), Err(resume)) => Err(format!("{error}；恢复诊断日志失败：{resume}")),
        (Err(error), _) => Err(error),
        (_, Err(resume)) => Err(format!("目录已迁移，恢复诊断日志失败：{resume}")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod migration_transaction_tests {
    use super::*;

    fn fixture() -> (PathBuf, storage::Preferences) {
        let root = std::env::temp_dir().join(format!(
            "action75-migration-session-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("wallpapers-native/a")).unwrap();
        fs::write(root.join("wallpapers-native/a/manifest.json"), b"{}").unwrap();
        fs::write(root.join("wallpapers-native/a/asset"), b"original").unwrap();
        storage::Preferences::default().save(&root).unwrap();
        let next = storage::Preferences {
            wallpaper_dir: root.join("new-packs").display().to_string(),
            log_dir: root.join("logs").display().to_string(),
            ..Default::default()
        };
        (root, next)
    }

    #[test]
    fn migration_unknown_ipc_or_invalid_identity_never_creates_destinations() {
        for response in [
            Err("播放器权限不匹配（5）".into()),
            Err("读取超时".into()),
            Err("运行目录不同".into()),
            Ok(json!({"playerPid":0})),
            Ok(json!({})),
        ] {
            let (root, next) = fixture();
            let before = fs::read(root.join("data/app-settings.json")).unwrap();
            let mut calls = 0;
            let result = migrate_preferences_with(&root, next, |request| {
                calls += 1;
                assert_eq!(request["action"], "status");
                response.clone()
            });
            assert!(result.is_err());
            assert_eq!(calls, 1);
            assert!(!root.join("new-packs").exists());
            assert!(root.join("wallpapers-native/a/asset").exists());
            assert_eq!(
                fs::read(root.join("data/app-settings.json")).unwrap(),
                before
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn migration_known_offline_is_reconfirmed_before_commit() {
        let (root, next) = fixture();
        let mut calls = 0;
        migrate_preferences_with(&root, next, |request| {
            calls += 1;
            assert_eq!(request["action"], "status");
            Err("播放器未启动".into())
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(
            fs::read(root.join("new-packs/a/asset")).unwrap(),
            b"original"
        );
        assert!(!root.join("wallpapers-native/a/asset").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_online_commands_keep_the_confirmed_player_identity() {
        let (root, next) = fixture();
        let mut actions = Vec::new();
        migrate_preferences_with(&root, next, |request| {
            let action = request["action"].as_str().unwrap().to_owned();
            if action != "status" {
                assert_eq!(request["expectedPlayerPid"], 42);
            }
            actions.push(action);
            Ok(json!({"playerPid":42}))
        })
        .unwrap();
        assert_eq!(
            actions,
            [
                "status",
                "suspendDiagnostics",
                "status",
                "reloadDirectories",
                "resumeDiagnostics"
            ]
        );
        assert!(!root.join("wallpapers-native/a/asset").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_session_change_or_unknown_midflight_preserves_sources_and_copies() {
        for initially_online in [false, true] {
            for unknown in [false, true] {
                let (root, next) = fixture();
                let mut queries = 0;
                let result = migrate_preferences_with(&root, next, |request| {
                    if request["action"] == "status" {
                        queries += 1;
                        if queries == 1 {
                            return if initially_online {
                                Ok(json!({"playerPid":42}))
                            } else {
                                Err("播放器未启动".into())
                            };
                        }
                        return if unknown {
                            Err("读取超时".into())
                        } else {
                            Ok(json!({"playerPid":43}))
                        };
                    }
                    assert_ne!(request["action"], "reloadDirectories");
                    assert_eq!(request["expectedPlayerPid"], 42);
                    Ok(json!({"playerPid":42}))
                });
                assert!(result.is_err());
                for prefix in ["wallpapers-native", "new-packs"] {
                    assert_eq!(
                        fs::read(root.join(prefix).join("a/asset")).unwrap(),
                        b"original"
                    );
                }
                assert!(
                    storage::Preferences::load_checked(&root)
                        .unwrap()
                        .wallpaper_dir
                        .is_empty()
                );
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn migration_lost_reload_ack_reactivates_old_paths_and_resumes_logs() {
        let (root, next) = fixture();
        let mut loaded = Vec::new();
        let mut resumed = false;
        let result = migrate_preferences_with(&root, next, |request| {
            if request["action"] != "status" {
                assert_eq!(request["expectedPlayerPid"], 42);
            }
            if request["action"] == "reloadDirectories" {
                loaded.push(
                    storage::Preferences::load_checked(&root)
                        .unwrap()
                        .wallpapers(&root),
                );
                if loaded.len() == 1 {
                    return Err("ACK lost after applying".into());
                }
            }
            if request["action"] == "resumeDiagnostics" {
                resumed = true;
            }
            Ok(json!({"playerPid":42}))
        });
        assert!(result.is_err());
        assert_eq!(
            loaded,
            [root.join("new-packs"), root.join("wallpapers-native")]
        );
        assert!(resumed);
        assert!(root.join("wallpapers-native/a/asset").exists());
        assert!(!root.join("new-packs/a/asset").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_failed_suspend_attempts_resume_without_touching_files() {
        let (root, next) = fixture();
        let mut resumed = false;
        let result = migrate_preferences_with(&root, next, |request| {
            if request["action"] == "suspendDiagnostics" {
                return Err("suspend ACK lost".into());
            }
            if request["action"] == "resumeDiagnostics" {
                resumed = true;
            }
            Ok(json!({"playerPid":42}))
        });
        assert!(result.is_err());
        assert!(resumed);
        assert!(!root.join("new-packs").exists());
        assert!(root.join("wallpapers-native/a/asset").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[derive(Default)]
struct PackPreview {
    pack: Option<String>,
    last_write: Option<WriteReceipt>,
}

impl PackPreview {
    fn send(&mut self, root: &Path, id: &str, values: Map<String, Value>) -> Result<(), String> {
        if values.is_empty() {
            if self.pack.as_deref() == Some(id) {
                self.restore(root)?;
            }
            return Ok(());
        }
        // A transport timeout can happen after the player applied the preview.
        self.pack = Some(id.to_owned());
        command(
            root,
            json!({"action":"previewPackSettings","id":id,"values":values}),
        )
        .map(|_| ())
    }

    fn settled(&mut self, id: &str) {
        if self.pack.as_deref() == Some(id) {
            self.pack = None;
        }
    }

    fn restore(&mut self, root: &Path) -> Result<(), String> {
        if let Some(id) = self.pack.as_deref() {
            // The player's read applies the stored profile if this pack is active.
            load_pack(root, id)?;
            self.pack = None;
        }
        Ok(())
    }

    fn record_write(&mut self, event: &Event) {
        let (id, selection, edit, result) = match event {
            Event::PackSaved {
                id,
                selection,
                edit,
                bundle,
            } => (id, *selection, *edit, Ok(bundle.revision.clone())),
            Event::PackFailed {
                id,
                selection,
                edit: Some(edit),
                error,
            } => (id, *selection, *edit, Err(error.clone())),
            _ => return,
        };
        self.last_write = Some(WriteReceipt {
            id: id.clone(),
            selection,
            edit,
            result,
        });
    }

    fn final_revision(&self, write: &pack_model::FinalWrite) -> Result<Option<Value>, String> {
        if let Some((selection, edit)) = write.after {
            let receipt = self
                .last_write
                .as_ref()
                .filter(|receipt| {
                    receipt.id == write.id && receipt.selection == selection && receipt.edit == edit
                })
                .ok_or("上一笔壁纸参数保存未确认")?;
            receipt.result.clone().map(Some)
        } else {
            Ok(write.revision.clone())
        }
    }

    fn save(&mut self, root: &Path, write: pack_model::FinalWrite) -> Result<(), String> {
        let revision = self.final_revision(&write)?;
        if !write.values.is_empty() {
            pack_request(
                root,
                "setPackSettings",
                &write.id,
                Some(write.values),
                revision,
            )?;
            self.settled(&write.id);
        }
        Ok(())
    }

    fn finish(&mut self, root: &Path, write: pack_model::FinalWrite) -> Result<(), String> {
        let saved = self.save(root, write);
        let restored = self.restore(root);
        saved.and(restored)
    }
}

fn load_pack(root: &Path, id: &str) -> Result<pack_model::Bundle, String> {
    serde_json::from_value(command(root, json!({"action":"getPackSettings","id":id}))?)
        .map_err(|e| format!("壁纸配置格式错误：{e}"))
}

pub fn damaged_profile_error(error: &str) -> bool {
    [
        "Invalid wallpaper settings profile",
        "Invalid wallpaper profile revision",
        "Wallpaper settings profile exceeds 64 KiB",
        "Wallpaper settings nesting exceeds 24 levels",
        "[json.exception.parse_error.",
        "[json.exception.type_error.",
        "[json.exception.out_of_range.",
    ]
    .iter()
    .any(|prefix| error.starts_with(prefix))
}

fn recover_profile_with(
    id: &str,
    mut request: impl FnMut(Value) -> Result<Value, String>,
) -> Result<(pack_model::Bundle, bool), String> {
    let (reply, recovered) = match request(json!({"action":"getPackSettings","id":id})) {
        Ok(bundle) => (bundle, false),
        Err(error) if damaged_profile_error(&error) => (
            request(json!({"action":"resetPackSettings","id":id}))?,
            true,
        ),
        Err(error) => return Err(error),
    };
    let bundle: pack_model::Bundle = serde_json::from_value(reply).map_err(|e| e.to_string())?;
    if bundle.pack_id != id {
        return Err("播放器返回了其他壁纸的参数".into());
    }
    Ok((bundle, recovered))
}

fn pack_request(
    root: &Path,
    action: &str,
    id: &str,
    values: Option<Map<String, Value>>,
    revision: Option<Value>,
) -> Result<pack_model::Bundle, String> {
    let mut req = json!({"action":action,"id":id});
    if let Some(values) = values {
        req["values"] = Value::Object(values);
    }
    if let Some(revision) = revision {
        if !revision.is_null() {
            req["expectedRevision"] = revision;
        }
    }
    serde_json::from_value(command(root, req)?).map_err(|e| e.to_string())
}
fn local_packs(root: &Path) -> Vec<Value> {
    let mut packs = BTreeMap::<String, (Value, bool)>::new();
    let configured = storage::Preferences::load(root).wallpapers(root);
    let entries = wallpaper_library::sources(root, configured)
        .into_iter()
        .flat_map(|(folder, bundled)| {
            fs::read_dir(folder)
                .into_iter()
                .flatten()
                .flatten()
                .map(move |entry| (entry, bundled))
        });
    for (entry, bundled) in entries {
        let dir = entry.path();
        let folder_name = entry.file_name();
        let Some(folder_name) = folder_name.to_str() else {
            continue;
        };
        if bundled && !wallpaper_library::bundled_id(folder_name) {
            continue;
        }
        let file = dir.join("manifest.json");
        if !dir.is_dir()
            || fs::metadata(&file)
                .map(|m| m.len() > 1024 * 1024)
                .unwrap_or(true)
        {
            continue;
        }
        let Some(manifest) = fs::read(file)
            .ok()
            .and_then(|v| serde_json::from_slice::<Value>(&v).ok())
        else {
            continue;
        };
        let Some(id) = manifest["id"].as_str() else {
            continue;
        };
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || !folder_name.eq_ignore_ascii_case(id)
        {
            continue;
        }
        let kind = manifest["kind"].as_str().unwrap_or("");
        let media = matches!(kind, "image" | "video");
        // Published media packs keep assets under a revision directory.
        let media_file = |key: &str| {
            manifest[key].as_str().is_some_and(|name| {
                let path = Path::new(name);
                !name.is_empty()
                    && path
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                    && dir.join(path).is_file()
            })
        };
        let native_available =
            kind == "native" && wallpaper_import::native_catalog_available(&dir, &manifest);
        let supported = matches!(
            kind,
            "field"
                | "keycap"
                | "spriteBurst"
                | "proceduralBurst"
                | "effekseer"
                | "blackHole"
                | "waterSurface"
                | "fireworks"
                | "glassShatter"
        ) || ((native_available || (media && media_file("background")))
            && manifest["enabled"] != false);
        if bundled
            && (!supported
                || manifest["enabled"] == false
                || !wallpaper_library::bundled_manifest(&dir, &manifest))
        {
            continue;
        }
        let managed = manifest[wallpaper_library::REVISION]
            .as_str()
            .is_some_and(wallpaper_library::valid_revision);
        let row = |id: &str| {
            let schema_available = if kind == "native" {
                native_available
            } else if media {
                media_file("settings")
            } else {
                dir.join("settings.json").is_file()
            };
            json!({"id":id,"name":manifest["name"].as_str().unwrap_or(id),
            "path":dir,"kind":kind,"supported":supported,"schemaAvailable":schema_available})
        };
        match packs.entry(id.to_ascii_lowercase()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert((row(id), managed));
            }
            std::collections::btree_map::Entry::Occupied(mut slot) if bundled && !slot.get().1 => {
                let stored_id = slot.get().0["id"].as_str().unwrap_or(id);
                let bundled_row = row(stored_id);
                slot.insert((bundled_row, false));
            }
            _ => {}
        }
    }
    let mut packs: Vec<_> = packs.into_values().map(|(row, _)| row).collect();
    packs.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    packs
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogPage {
    packs: Vec<Value>,
    catalog_revision: u64,
    next_offset: Option<usize>,
}

fn player_catalog(
    request: &mut impl FnMut(Value) -> Result<Value, String>,
) -> Result<Vec<Value>, String> {
    // A competing refresh invalidates the continuation cursor. Retry from
    // zero once so the caller always receives a complete catalog.
    const STALE: &str = "Wallpaper catalog changed during pagination";
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut refresh = None;
    for _ in 0..2 {
        let mut packs = Vec::new();
        let mut revision = None;
        loop {
            let offset = packs.len();
            let mut command = json!({"action":"listPacks","offset":offset});
            if offset == 0 {
                command["asyncRefresh"] = json!(true);
                if let Some(ticket) = refresh {
                    command["catalogRefreshRevision"] = json!(ticket);
                }
            }
            if let Some(revision) = revision {
                command["catalogRevision"] = json!(revision);
            }
            let reply = match request(command) {
                Ok(Value::Array(packs)) if offset == 0 => return Ok(packs), // Older player
                Ok(reply) => reply,
                Err(error) if offset > 0 && error == STALE => break,
                Err(error) => return Err(error),
            };
            if let Some(pending) = reply.get("pendingCatalogRevision") {
                let ticket = pending.as_u64().filter(|ticket| *ticket > 0);
                if offset != 0 || ticket.is_none() || refresh.is_some() && refresh != ticket {
                    return Err("壁纸目录刷新版本无效".into());
                }
                refresh = ticket;
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("壁纸目录刷新超时".into());
                }
                thread::sleep(remaining.min(Duration::from_millis(40)));
                continue;
            }
            let page: CatalogPage = serde_json::from_value(reply)
                .map_err(|error| format!("壁纸列表响应格式错误，{error}"))?;
            if page.catalog_revision == 0
                || refresh.is_some_and(|ticket| page.catalog_revision < ticket)
                || revision.is_some_and(|r| r != page.catalog_revision)
            {
                return Err("壁纸列表分页版本无效".into());
            }
            packs.extend(page.packs);
            match page.next_offset {
                None => return Ok(packs),
                Some(next) if next == packs.len() && next > offset => {
                    revision = Some(page.catalog_revision);
                }
                _ => return Err("壁纸列表分页位置无效".into()),
            }
        }
    }
    Err("读取时壁纸目录发生变化".into())
}

fn configure_first_run(
    monitor: &str,
    monitor_identity: Option<&str>,
    device: &str,
    mut request: impl FnMut(Value) -> Result<Value, String>,
) -> Result<(), String> {
    let mut settings = json!({"monitorDevice":monitor,"deviceGroup":device,"fps":60});
    if let Some(identity) = monitor_identity {
        settings["monitorIdentity"] = json!(identity);
    }
    let applied = request(json!({
        "action":"setSettings",
        "settings":settings,
        "requireConnectedKeyboard":true
    }))?;
    if applied["selectedKeyboardDevices"].as_u64() == Some(0) {
        return Err("所选键盘已断开，请连接后刷新设备，或选择其他键盘。".into());
    }
    if applied["packId"].as_str().is_none_or(str::is_empty)
        && request(json!({"action":"selectPack","id":"paintball-native"})).is_err()
    {
        // A missing default wallpaper must still allow access to the library.
        let _ = request(json!({"action":"selectPack","id":"black-hole"}));
    }
    request(json!({"action":"show"}))?;
    Ok(())
}

fn refresh_devices_with(request: impl FnOnce(Value) -> Result<Value, String>) -> Event {
    #[derive(Deserialize)]
    struct DeviceLists {
        monitors: Vec<Value>,
        devices: Vec<Value>,
    }
    let result = request(json!({"action":"refreshDevices"})).and_then(|reply| {
        let lists: DeviceLists = serde_json::from_value(reply)
            .map_err(|_| "播放器返回的显示器或键盘列表无效".to_string())?;
        Ok((lists.monitors, lists.devices))
    });
    Event::DevicesRefreshed(result)
}

fn read_devices_with(
    mut request: impl FnMut(Value) -> Result<Value, String>,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut read = |action| {
        serde_json::from_value(request(json!({"action":action}))?)
            .map_err(|_| "播放器返回的显示器或键盘列表无效".to_string())
    };
    Ok((read("listMonitors")?, read("listDevices")?))
}

fn full(root: &Path) -> Result<Event, String> {
    full_with(root, |request| command(root, request))
}
fn full_with<F: FnMut(Value) -> Result<Value, String>>(
    root: &Path,
    request: F,
) -> Result<Event, String> {
    full_with_catalog(root, request, None)
}

struct LoadedCatalog {
    player_pid: Option<u64>,
    packs: Vec<Value>,
    error: Option<String>,
}

fn load_catalog(root: &Path) -> LoadedCatalog {
    let mut request = |request| command(root, request);
    let status = request(json!({"action":"status"}));
    let player_pid = status
        .as_ref()
        .ok()
        .and_then(|status| status["playerPid"].as_u64());
    let (packs, error) = if status.is_ok() {
        match player_catalog(&mut request) {
            Ok(packs) => (packs, None),
            Err(error) => (local_packs(root), Some(error)),
        }
    } else {
        (local_packs(root), None)
    };
    LoadedCatalog {
        player_pid,
        packs,
        error,
    }
}

fn full_with_catalog<F: FnMut(Value) -> Result<Value, String>>(
    root: &Path,
    mut request: F,
    loaded: Option<LoadedCatalog>,
) -> Result<Event, String> {
    let (status, connection_error) = match request(json!({"action":"status"})) {
        Ok(status) => (status, None),
        Err(error) => (json!({"running":false}), Some(error)),
    };
    let (packs, catalog_error) = if let Some(loaded) = loaded {
        if loaded.player_pid == status["playerPid"].as_u64() {
            (loaded.packs, loaded.error)
        } else {
            let error = connection_error
                .is_none()
                .then(|| "播放器已重启，请重新刷新壁纸目录".into());
            (local_packs(root), error)
        }
    } else if connection_error.is_none() {
        match player_catalog(&mut request) {
            Ok(packs) => (packs, None),
            Err(error) => (local_packs(root), Some(error)),
        }
    } else {
        (local_packs(root), None)
    };
    let (monitors, devices, palettes) = if connection_error.is_none() {
        let mut list = |action| match request(json!({"action":action})) {
            Ok(Value::Array(entries)) => entries,
            _ => Vec::new(),
        };
        (
            list("listMonitors"),
            list("listDevices"),
            list("listPalettes"),
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    Ok(Event::Full {
        connection_error,
        catalog_error,
        status,
        packs,
        monitors,
        devices,
        palettes,
    })
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
#[derive(Clone, Debug)]
pub struct Calibration {
    pub document: Value,
    pub original: BTreeMap<String, Point>,
    pub points: BTreeMap<String, Point>,
    pub order: Vec<String>,
    recovery: Option<CalibrationRecovery>,
}

#[derive(Clone, Debug)]
enum CalibrationRecovery {
    Missing,
    Damaged(Vec<u8>),
    Unreadable(String),
}

impl Calibration {
    pub fn from_points(
        document: Value,
        points: BTreeMap<String, Point>,
        order: Vec<String>,
    ) -> Self {
        Self {
            document,
            original: points.clone(),
            points,
            order,
            recovery: None,
        }
    }

    pub fn default_points(root: &Path) -> Result<BTreeMap<String, Point>, String> {
        let (layout, order) = Self::layout(root)?;
        Self::preset_points(root, &layout, &order)
    }

    fn preset_points(
        root: &Path,
        layout: &BTreeMap<String, Point>,
        order: &[String],
    ) -> Result<BTreeMap<String, Point>, String> {
        let file = root.join("assets/defaults/fm-calibration.json");
        match fs::read(file) {
            Ok(bytes) => calibration_points(&calibration_document(&bytes)?, layout, order),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(layout.clone()),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn layout_points(root: &Path) -> Result<BTreeMap<String, Point>, String> {
        Self::layout(root).map(|(points, _)| points)
    }

    // The reference coordinates use the author's 1920×555 space.
    fn layout(root: &Path) -> Result<(BTreeMap<String, Point>, Vec<String>), String> {
        let bytes = fs::read(root.join("existing-resources/finalmouse/action75-ue-keymap.json"))
            .map_err(|e| e.to_string())?;
        let document = calibration_document(&bytes)?;
        if document["coordinateSpace"] != json!([1920, 555]) {
            return Err("默认键位坐标空间不正确".into());
        }
        let keys = document["keys"].as_array().ok_or("默认键位格式错误")?;
        let mut points = BTreeMap::new();
        let mut order = Vec::new();
        for key in keys {
            let code = key["code"].as_str().ok_or("默认键位缺少编号")?;
            let x = key["x"].as_f64().ok_or("默认键位横坐标错误")?;
            let y = key["y"].as_f64().ok_or("默认键位纵坐标错误")?;
            if !x.is_finite()
                || !y.is_finite()
                || !(0.0..=1920.0).contains(&x)
                || !(0.0..=555.0).contains(&y)
            {
                return Err("默认键位坐标越界".into());
            }
            if points
                .insert(
                    code.to_owned(),
                    Point {
                        x: x / 1920.0,
                        y: y / 555.0,
                    },
                )
                .is_some()
            {
                return Err("默认键位编号重复".into());
            }
            order.push(code.to_owned());
        }
        validate_points(&points, &order)?;
        Ok((points, order))
    }

    pub fn load(root: &Path) -> Result<Self, String> {
        let (layout, order) = Self::layout(root)?;
        let recovery = match fs::read(root.join("data/fm-calibration.json")) {
            Ok(bytes) => {
                if let Ok(document) = calibration_document(&bytes)
                    && let Ok(points) = calibration_points(&document, &layout, &order)
                {
                    return Ok(Self::from_points(document, points, order));
                }
                CalibrationRecovery::Damaged(bytes)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => CalibrationRecovery::Missing,
            Err(error) => CalibrationRecovery::Unreadable(error.to_string()),
        };
        let points = Self::preset_points(root, &layout, &order)?;
        let mut model = Self::from_points(json!({"version":1,"revision":""}), points, order);
        model.recovery = Some(recovery);
        Ok(model)
    }

    pub fn recovery_notice(&self) -> Option<&'static str> {
        self.recovery.as_ref().map(|state| match state {
            CalibrationRecovery::Missing => "未找到校准文件，已载入默认键位。",
            CalibrationRecovery::Damaged(_) => "校准文件损坏，已载入默认键位。保存时会备份原文件。",
            CalibrationRecovery::Unreadable(_) => {
                "无法读取校准文件，已载入默认键位。请检查文件访问权限。"
            }
        })
    }
    pub fn changed(&self) -> bool {
        self.original != self.points
    }
    pub fn move_point(&mut self, code: &str, x: f64, y: f64) {
        if let Some(p) = self.points.get_mut(code) {
            p.x = x.clamp(0., 1.);
            p.y = y.clamp(0., 1.);
        }
    }
}

fn calibration_document(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
        .map_err(|error| error.to_string())
}

fn calibration_points(
    document: &Value,
    layout: &BTreeMap<String, Point>,
    order: &[String],
) -> Result<BTreeMap<String, Point>, String> {
    if document["version"].as_f64() != Some(1.0) {
        return Err("校准文件版本无效".into());
    }
    let overrides: BTreeMap<String, Point> =
        serde_json::from_value(document["points"].clone()).map_err(|e| e.to_string())?;
    let mut points = layout.clone();
    points.extend(overrides);
    validate_points(&points, order)?;
    Ok(points)
}

pub fn validate_points(points: &BTreeMap<String, Point>, order: &[String]) -> Result<(), String> {
    if points.len() != 83 || order.len() != 83 {
        return Err("校准必须包含原来的 83 个键位".into());
    }
    for code in order {
        let p = points.get(code).ok_or_else(|| format!("缺少键位 {code}"))?;
        if !p.x.is_finite()
            || !p.y.is_finite()
            || !(0.0..=1.0).contains(&p.x)
            || !(0.0..=1.0).contains(&p.y)
        {
            return Err(format!("键位 {code} 的坐标越界"));
        }
    }
    Ok(())
}
pub fn save_calibration(root: &Path, model: &Calibration) -> Result<String, String> {
    save_calibration_with_ack(root, model, || {
        command(root, json!({"action":"reloadCalibration"}))
    })
}
fn save_calibration_with_ack<F>(
    root: &Path,
    model: &Calibration,
    mut reload: F,
) -> Result<String, String>
where
    F: FnMut() -> Result<Value, String>,
{
    validate_points(&model.points, &model.order)?;
    if !model.changed() && model.recovery.is_none() {
        return Ok("校准未改动，保留原文件".into());
    }
    let file = root.join("data/fm-calibration.json");
    let previous = match fs::read(&file) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(format!("无法读取和备份校准文件，{error}")),
    };
    let matches_entry = match &model.recovery {
        Some(CalibrationRecovery::Missing) => previous.is_none(),
        Some(CalibrationRecovery::Damaged(bytes)) => previous.as_ref() == Some(bytes),
        Some(CalibrationRecovery::Unreadable(error)) => {
            return Err(format!(
                "打开页面时无法读取校准文件，请重新打开校准，{error}"
            ));
        }
        None => {
            let (layout, order) = Calibration::layout(root)?;
            // Compare coordinates so equivalent numeric representations agree.
            previous.as_deref().is_some_and(|bytes| {
                calibration_document(bytes)
                    .and_then(|doc| calibration_points(&doc, &layout, &order))
                    .is_ok_and(|points| points == model.original)
            })
        }
    };
    if !matches_entry {
        return Err("校准已被其它程序修改，请重新打开校准板".into());
    }
    let mut document = model.document.clone();
    document["points"] = serde_json::to_value(&model.points).map_err(|e| e.to_string())?;
    let data = serde_json::to_vec(&model.points).map_err(|e| e.to_string())?;
    let mut hash = 2166136261u32;
    for v in data {
        hash = (hash ^ v as u32).wrapping_mul(16777619);
    }
    let revision = format!("{hash:08X}");
    document["revision"] = json!(revision);
    document["savedBy"] = json!("Action75 Rust Manager");
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    if let Some(bytes) = &previous {
        let backup = root.join("data/backups");
        fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(backup.join(format!("calibration-before-rust-{time}.json")))
            .and_then(|mut backup| {
                backup.write_all(bytes)?;
                backup.sync_all()
            })
            .map_err(|e| e.to_string())?;
    } else {
        fs::create_dir_all(root.join("data")).map_err(|e| e.to_string())?;
    }
    let temp = root.join(format!(
        "data/fm-calibration-rust-{}-{time}.tmp",
        std::process::id()
    ));
    let written = serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?;
    fs::write(&temp, &written).map_err(|e| e.to_string())?;
    atomic_replace(&temp, &file)?;
    match reload() {
        Ok(status) if status["calibrationRevision"] == revision => {
            Ok(format!("83 键校准已保存 · {revision}"))
        }
        result => {
            if fs::read(&file).map_err(|e| e.to_string())? != written {
                return Err("保存后校准文件又被更新，已保留最新文件，没有用旧版本覆盖".into());
            }
            let restored = if let Some(bytes) = &previous {
                fs::write(&temp, bytes).map_err(|e| e.to_string())?;
                atomic_replace(&temp, &file)?;
                "已恢复原文件"
            } else {
                fs::remove_file(&file).map_err(|e| e.to_string())?;
                "已撤销新建文件"
            };
            let _ = reload();
            Err(format!(
                "播放器未确认新校准，{restored}：{}",
                result.err().unwrap_or_else(|| "版本不一致".into())
            ))
        }
    }
}
/// Return the actual acknowledged document so another save in the same session
/// checks against the newly saved points rather than the old entrance snapshot.
pub fn save_calibration_and_reload(
    root: &Path,
    model: &Calibration,
) -> Result<Calibration, String> {
    save_calibration_and_reload_with_ack(root, model, || {
        command(root, json!({"action":"reloadCalibration"}))
    })
}
pub fn save_calibration_for_player(
    root: &Path,
    model: &Calibration,
    player_pid: u32,
) -> Result<Calibration, String> {
    save_calibration_and_reload_with_ack(root, model, || {
        let acknowledgement = command(
            root,
            json!({"action":"reloadCalibration","expectedPlayerPid":player_pid}),
        )?;
        if acknowledgement["playerPid"].as_u64() != Some(u64::from(player_pid)) {
            return Err("播放器实例已变化，请重新开始校准".into());
        }
        Ok(acknowledgement)
    })
}
fn save_calibration_and_reload_with_ack<F>(
    root: &Path,
    model: &Calibration,
    reload: F,
) -> Result<Calibration, String>
where
    F: FnMut() -> Result<Value, String>,
{
    save_calibration_with_ack(root, model, reload)?;
    let saved = Calibration::load(root)?;
    if saved.recovery.is_some() || saved.points != model.points {
        return Err("保存后校准数据又发生变化，请重新打开校准".into());
    }
    Ok(saved)
}
pub fn atomic_replace(source: &Path, target: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(a: *const u16, b: *const u16, flags: u32) -> i32;
        }
        let a: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let b: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 1 | 8) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, target).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_preserves_playback_and_can_finish_without_default_wallpapers() {
        for active in ["black-hole", ""] {
            let mut actions = Vec::new();
            configure_first_run("display", Some("identity"), "keyboard", |request| {
                let action = request["action"].as_str().unwrap().to_owned();
                let result = match action.as_str() {
                    "setSettings" => Ok(json!({"selectedKeyboardDevices": 1, "packId": active})),
                    "selectPack" => Err("missing bundled wallpaper".into()),
                    "show" => Ok(Value::Null),
                    _ => panic!("unexpected request"),
                };
                actions.push(action);
                result
            })
            .unwrap();
            let expected: &[&str] = if active.is_empty() {
                &["setSettings", "selectPack", "selectPack", "show"]
            } else {
                &["setSettings", "show"]
            };
            assert_eq!(actions, expected);
        }
    }

    #[test]
    fn failed_ordinary_worker_request_is_not_an_unrelated_operation_completion() {
        let root = tempfile::tempdir().unwrap();
        let (jobs, events) = worker(root.path().to_owned(), eframe::egui::Context::default());
        jobs.send(Job::Request(json!({"action":"pause"}))).unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::RequestFailed(_)
        ));
        jobs.send(Job::Quit).unwrap();
    }

    #[test]
    fn failed_setup_and_migration_complete_their_own_job_then_replace_the_catalog() {
        for setup in [false, true] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("data")).unwrap();
            fs::write(root.path().join("data/app-settings.json"), b"broken").unwrap();
            let (jobs, events) = worker(root.path().to_owned(), eframe::egui::Context::default());
            let job = if setup {
                Job::Setup {
                    monitor: "unused".into(),
                    monitor_identity: None,
                    device: "unused".into(),
                }
            } else {
                Job::Migrate(storage::Preferences::default())
            };
            jobs.send(job).unwrap();
            match events.recv_timeout(Duration::from_secs(2)).unwrap() {
                Event::SetupFinished(Err(_)) if setup => {}
                Event::MigrationFinished(Err(_)) if !setup => {}
                _ => panic!("failure must complete only the job that produced it"),
            }
            assert!(matches!(
                events.recv_timeout(Duration::from_secs(2)).unwrap(),
                Event::Full { .. }
            ));
            jobs.send(Job::Quit).unwrap();
        }
    }

    #[test]
    fn failed_start_still_loads_the_local_wallpaper_catalog() {
        let root = tempfile::tempdir().unwrap();
        let pack = root.path().join("wallpapers-native/local");
        fs::create_dir_all(&pack).unwrap();
        fs::write(
            pack.join("manifest.json"),
            br#"{"version":1,"id":"local","name":"Local","kind":"fireworks"}"#,
        )
        .unwrap();
        let (jobs, events) = worker(root.path().to_owned(), eframe::egui::Context::default());
        jobs.send(Job::Start).unwrap();
        let Event::Full {
            connection_error,
            packs,
            status,
            ..
        } = events.recv_timeout(Duration::from_secs(3)).unwrap()
        else {
            panic!("expected the offline catalog as well as the startup error");
        };
        assert!(connection_error.unwrap().contains("无法启动播放器"));
        assert_eq!(status["running"], false);
        assert_eq!(packs[0]["id"], "local");
    }

    #[test]
    fn queued_close_uses_the_matching_write_ack_and_never_replays_a_rejected_draft() {
        let mut preview = PackPreview::default();
        let write = pack_model::FinalWrite {
            id: "a".into(),
            values: Map::new(),
            revision: Some(json!(20)),
            after: Some((7, 8)),
        };
        assert!(preview.final_revision(&write).is_err());
        preview.record_write(&Event::PackFailed {
            id: "a".into(),
            selection: 7,
            edit: Some(8),
            error: "stale".into(),
        });
        assert_eq!(preview.final_revision(&write), Err("stale".into()));
        preview.record_write(&Event::PackSaved {
            id: "a".into(),
            selection: 7,
            edit: 8,
            bundle: pack_model::Bundle {
                pack_id: "a".into(),
                schema: Value::Null,
                values: Map::new(),
                revision: json!(21),
            },
        });
        assert_eq!(preview.final_revision(&write), Ok(Some(json!(21))));
    }

    #[test]
    fn recovery_rechecks_the_profile_before_sending_a_revisionless_reset() {
        let response = json!({"packId":"a","schema":null,"values":{},"revision":1});
        for (error, must_reset) in [
            (None, false),
            (Some("Invalid wallpaper profile revision"), true),
            (Some("播放器未启动"), false),
        ] {
            let mut requests = Vec::new();
            let result = recover_profile_with("a", |request| {
                requests.push(request.clone());
                if request["action"] == "getPackSettings"
                    && let Some(error) = error
                {
                    return Err(error.into());
                }
                Ok(response.clone())
            });
            assert_eq!(requests[0], json!({"action":"getPackSettings","id":"a"}));
            if must_reset {
                assert_eq!(requests[1], json!({"action":"resetPackSettings","id":"a"}));
                assert!(result.unwrap().1);
            } else {
                assert_eq!(requests.len(), 1);
                assert_eq!(result.is_err(), error.is_some());
            }
        }
    }
    #[test]
    fn gallery_loads_local_packs_when_player_denies_connection() {
        let f = CalibrationFixture::new();
        let dir = f.root.join("wallpapers-native/snow");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            br#"{"id":"snow","name":"Snow","kind":"proceduralBurst","preview":"preview.png"}"#,
        )
        .unwrap();
        let mut calls = 0;
        let result = full_with(&f.root, |_| {
            calls += 1;
            Err("播放器权限不匹配（5）".into())
        })
        .unwrap();
        let Event::Full {
            status,
            packs,
            connection_error,
            catalog_error,
            ..
        } = result
        else {
            panic!("Expected catalog")
        };
        assert_eq!(calls, 1);
        assert_eq!(status["running"], false);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0]["supported"], true);
        assert_eq!(Path::new(packs[0]["path"].as_str().unwrap()), dir);
        assert!(connection_error.is_some());
        assert!(catalog_error.is_none());
    }
    #[test]
    fn local_media_catalog_follows_published_asset_paths() {
        let fixture = CalibrationFixture::new();
        for (kind, extension) in [("image", "png"), ("video", "mp4")] {
            let dir = fixture.root.join("wallpapers-native").join(kind);
            let rev = "v-0123456789abcdef0123456789abcdef01234567";
            let prefix = format!(".action75-versions/{rev}");
            let asset = format!("{prefix}/media.{extension}");
            let schema = format!("{prefix}/settings.json");
            fs::create_dir_all(dir.join(&prefix)).unwrap();
            fs::write(dir.join(&asset), []).unwrap();
            fs::write(dir.join(&schema), "{}").unwrap();
            let manifest = json!({"id":kind,"name":kind,"version":1,"kind":kind,
                                  "settings":schema,"background":asset,"_action75Revision":rev});
            fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        }
        let rows = local_packs(&fixture.root);
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row["kind"], row["id"]);
            assert_eq!(row["supported"], true);
            assert_eq!(row["schemaAvailable"], true);
        }
    }

    #[test]
    fn wallpaper_library_regression_discovery_after_migration_and_upgrade() {
        let fixture = CalibrationFixture::new();
        let installed = fixture.root.join("wallpapers-native");
        let custom = fixture.root.join("custom-library");
        let write_pack = |base: &Path, id: &str, name: &str| {
            let directory = base.join(id);
            fs::create_dir_all(&directory).unwrap();
            let manifest = json!({"version":1,"id":id,"name":name,"kind":"spriteBurst"});
            fs::write(
                directory.join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
        };
        write_pack(&installed, "afterimage", "Old stock");
        write_pack(&installed, "author-preserved", "User author");
        let mut prefs = storage::Preferences {
            wallpaper_dir: custom.to_str().unwrap().to_owned(),
            ..Default::default()
        };
        storage::migrate(&fixture.root, &prefs, || Ok(())).unwrap();
        write_pack(&installed, "afterimage", "New stock");
        write_pack(&installed, "glass-shatter", "New bundled pack");
        write_pack(&installed, "private-unlisted", "Not in the custom library");
        let migrated = fs::read(custom.join("afterimage/manifest.json")).unwrap();
        let packs = local_packs(&fixture.root);
        assert_eq!(packs.len(), 3);
        let upgrade = packs
            .iter()
            .find(|pack| pack["id"] == "afterimage")
            .unwrap();
        assert_eq!(upgrade["name"], "New stock");
        assert_eq!(
            Path::new(upgrade["path"].as_str().unwrap()),
            installed.join("afterimage")
        );
        assert!(packs.iter().any(|pack| pack["id"] == "author-preserved"));
        assert!(packs.iter().any(|pack| pack["id"] == "glass-shatter"));
        assert_eq!(
            fs::read(custom.join("afterimage/manifest.json")).unwrap(),
            migrated
        );

        let invalid = fixture.root.join("custom-is-a-file");
        fs::write(&invalid, b"regular file").unwrap();
        prefs.wallpaper_dir = invalid.to_str().unwrap().to_owned();
        prefs.save(&fixture.root).unwrap();
        let ids: Vec<_> = local_packs(&fixture.root)
            .into_iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, ["afterimage", "glass-shatter"]);
        assert_eq!(fs::read(invalid).unwrap(), b"regular file");
    }

    #[test]
    fn refreshing_devices_returns_one_live_snapshot_or_an_explicit_error() {
        let event = refresh_devices_with(|request| {
            assert_eq!(request, json!({"action":"refreshDevices"}));
            Ok(json!({"monitors":[{"device":"secondary","selected":true}],"devices":[]}))
        });
        let Event::DevicesRefreshed(Ok((monitors, keyboards))) = event else {
            panic!("expected live device lists");
        };
        assert_eq!(monitors[0]["device"], "secondary");
        assert!(keyboards.is_empty());

        let empty = refresh_devices_with(|_| Ok(json!({"monitors":[], "devices":[]})));
        assert!(
            matches!(empty, Event::DevicesRefreshed(Ok((m,k))) if m.is_empty() && k.is_empty())
        );
        for failed in [Err("disconnected".to_string()), Ok(json!({"monitors":[]}))] {
            assert!(matches!(
                refresh_devices_with(|_| failed),
                Event::DevicesRefreshed(Err(_))
            ));
        }
    }

    #[test]
    fn hardware_query_failure_does_not_discard_online_wallpaper_catalog() {
        let f = CalibrationFixture::new();
        let result = full_with(&f.root, |v| match v["action"].as_str().unwrap() {
            "status" => Ok(json!({"running":true})),
            "listPacks" => Ok(json!([{"id":"snow"}])),
            _ => Err("Unavailable".into()),
        })
        .unwrap();
        let Event::Full {
            status,
            packs,
            connection_error,
            ..
        } = result
        else {
            panic!("Expected catalog")
        };
        assert_eq!(status["running"], true);
        assert_eq!(packs.len(), 1);
        assert!(connection_error.is_none());
    }
    #[test]
    fn catalog_failure_keeps_online_controls_and_uses_local_packs() {
        let f = CalibrationFixture::new();
        let dir = f.root.join("data/native-wallpapers/snow");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            br#"{"id":"snow","name":"Snow","kind":"fireworks"}"#,
        )
        .unwrap();
        let mut calls = Vec::new();
        let event = full_with(&f.root, |request| {
            let action = request["action"].as_str().unwrap();
            calls.push(action.to_owned());
            match action {
                "status" => Ok(json!({"running":true,"packId":"snow"})),
                "listPacks" => Err("播放器响应过大".into()),
                "listMonitors" | "listDevices" | "listPalettes" => Ok(json!([{"id":action}])),
                _ => panic!("Unexpected request {request}"),
            }
        })
        .unwrap();
        let Event::Full {
            status,
            packs,
            monitors,
            devices,
            palettes,
            connection_error,
            catalog_error,
        } = event
        else {
            panic!("Expected catalog")
        };
        assert_eq!(
            calls,
            [
                "status",
                "listPacks",
                "listMonitors",
                "listDevices",
                "listPalettes"
            ]
        );
        assert_eq!(status["running"], true);
        assert_eq!(packs[0]["id"], "snow");
        assert_eq!([monitors.len(), devices.len(), palettes.len()], [1, 1, 1]);
        assert!(connection_error.is_none() && catalog_error.is_some());
    }
    #[test]
    fn catalog_retries_one_stale_snapshot_and_consumes_ordered_pages() {
        let mut requests = Vec::new();
        let values = player_catalog(&mut |request| {
            requests.push((
                request["offset"].clone(),
                request["catalogRevision"].clone(),
            ));
            match requests.len() {
                1 => Ok(json!({"packs":[{"id":"stale"}],"catalogRevision":9,"nextOffset":1})),
                2 => Err("Wallpaper catalog changed during pagination".into()),
                3 => {
                    Ok(json!({"packs":[{"id":"a"},{"id":"b"}],"catalogRevision":10,"nextOffset":2}))
                }
                4 => Ok(json!({"packs":[{"id":"c"}],"catalogRevision":10,"nextOffset":null})),
                _ => panic!("Unexpected request {request}"),
            }
        })
        .unwrap();
        assert_eq!(
            requests,
            [
                (json!(0), Value::Null),
                (json!(1), json!(9)),
                (json!(0), Value::Null),
                (json!(2), json!(10)),
            ]
        );
        assert_eq!(
            values,
            [json!({"id":"a"}), json!({"id":"b"}), json!({"id":"c"})]
        );
    }
    #[test]
    fn catalog_rejects_a_nonadvancing_cursor() {
        let mut calls = 0;
        let result = player_catalog(&mut |_| {
            calls += 1;
            Ok(json!({"packs":[],"catalogRevision":1,"nextOffset":0}))
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
    #[test]
    fn catalog_waits_for_one_refresh_ticket_and_reuses_it_after_stale_pages() {
        let mut calls = 0;
        let packs = player_catalog(&mut |request| {
            calls += 1;
            match calls {
                1 => {
                    assert_eq!(
                        request,
                        json!({"action":"listPacks","offset":0,"asyncRefresh":true})
                    );
                    Ok(json!({"pendingCatalogRevision": 13}))
                }
                2 => {
                    assert_eq!(request["catalogRefreshRevision"], 13);
                    Ok(json!({"packs":[{"id":"old"}],"catalogRevision":13,"nextOffset":1}))
                }
                3 => {
                    assert_eq!(request["catalogRevision"], 13);
                    Err("Wallpaper catalog changed during pagination".into())
                }
                4 => {
                    assert_eq!(request["catalogRefreshRevision"], 13);
                    Ok(json!({"packs":[{"id":"new"}],"catalogRevision":14,"nextOffset":null}))
                }
                _ => panic!("Unexpected request {request}"),
            }
        })
        .unwrap();
        assert_eq!(packs, [json!({"id":"new"})]);
    }
    struct CalibrationFixture {
        root: PathBuf,
    }
    impl CalibrationFixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "action75-calibration-data-{}-{unique}",
                std::process::id()
            ));
            let defaults = root.join("existing-resources/finalmouse");
            fs::create_dir_all(&defaults).unwrap();
            fs::create_dir(root.join("data")).unwrap();
            let keys = (0..83).map(|i|json!({"code":format!("Key{i}"),"x":1920.0*i as f64/82.0,"y":555.0*i as f64/82.0})).collect::<Vec<_>>();
            fs::write(
                defaults.join("action75-ue-keymap.json"),
                serde_json::to_vec(&json!({"coordinateSpace":[1920,555],"keys":keys})).unwrap(),
            )
            .unwrap();
            let points = Calibration::default_points(&root).unwrap();
            fs::write(root.join("data/fm-calibration.json"),serde_json::to_vec(&json!({"version":1,"revision":"ENTRY","points":points,"customMetadata":"preserved"})).unwrap()).unwrap();
            Self { root }
        }
        fn file(&self) -> PathBuf {
            self.root.join("data/fm-calibration.json")
        }
    }
    impl Drop for CalibrationFixture {
        fn drop(&mut self) {
            if let (Ok(root), Ok(temp)) = (
                self.root.canonicalize(),
                std::env::temp_dir().canonicalize(),
            ) {
                if root.parent() == Some(temp.as_path())
                    && root
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("action75-calibration-data-"))
                {
                    let _ = fs::remove_dir_all(root);
                }
            }
        }
    }
    #[test]
    fn calibration_default_coordinates_match_native_1920_by_555_space() {
        let fixture = CalibrationFixture::new();
        let points = Calibration::default_points(&fixture.root).unwrap();
        assert_eq!(points.len(), 83);
        assert_eq!(points["Key0"], Point { x: 0., y: 0. });
        assert_eq!(points["Key41"], Point { x: 0.5, y: 0.5 });
        assert_eq!(points["Key82"], Point { x: 1., y: 1. });
        let file = fixture
            .root
            .join("existing-resources/finalmouse/action75-ue-keymap.json");
        let mut bad: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        bad["coordinateSpace"] = json!([1920, 720]);
        fs::write(&file, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(Calibration::default_points(&fixture.root).is_err());
    }
    #[test]
    fn calibration_defaults_reject_duplicate_codes_without_touching_user_points() {
        let fixture = CalibrationFixture::new();
        let before = fs::read(fixture.file()).unwrap();
        let file = fixture
            .root
            .join("existing-resources/finalmouse/action75-ue-keymap.json");
        let mut bad: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        bad["keys"][82]["code"] = json!("Key0");
        fs::write(&file, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(Calibration::default_points(&fixture.root).is_err());
        assert_eq!(fs::read(fixture.file()).unwrap(), before);
    }

    #[test]
    fn damaged_calibration_uses_packaged_defaults_and_recovers_with_an_exact_backup() {
        let fixture = CalibrationFixture::new();
        let original = fs::read(fixture.file()).unwrap();
        let mut preset = calibration_document(&original).unwrap();
        preset["points"]["Key0"]["x"] = json!(0.42);
        let defaults = fixture.root.join("assets/defaults");
        fs::create_dir_all(&defaults).unwrap();
        fs::write(
            defaults.join("fm-calibration.json"),
            serde_json::to_vec(&preset).unwrap(),
        )
        .unwrap();

        for damaged in [
            b"".as_slice(),
            b"{",
            b"null",
            br#"{"version":2,"points":{}}"#,
        ] {
            fs::write(fixture.file(), damaged).unwrap();
            let model = Calibration::load(&fixture.root).unwrap();
            assert!(model.recovery_notice().is_some());
            assert_eq!(model.points["Key0"].x, 0.42);
            assert_eq!(fs::read(fixture.file()).unwrap(), damaged);
            assert!(!fixture.root.join("data/backups").exists());
        }

        let damaged = br#"{"version":1,"points":{"Key0":{"x":2,"y":0}}}"#;
        fs::write(fixture.file(), damaged).unwrap();
        let model = Calibration::load(&fixture.root).unwrap();
        assert!(!model.changed());
        let error =
            save_calibration_with_ack(&fixture.root, &model, || Err("reload failed".into()))
                .unwrap_err();
        assert!(error.contains("已恢复原文件"));
        assert_eq!(fs::read(fixture.file()).unwrap(), damaged);

        let saved = save_calibration_and_reload_with_ack(&fixture.root, &model, || {
            let document = calibration_document(&fs::read(fixture.file()).unwrap()).unwrap();
            Ok(json!({"calibrationRevision":document["revision"]}))
        })
        .unwrap();
        assert_eq!(saved.points, model.points);
        assert!(saved.recovery_notice().is_none());
        let backups: Vec<_> = fs::read_dir(fixture.root.join("data/backups"))
            .unwrap()
            .map(|entry| fs::read(entry.unwrap().path()).unwrap())
            .collect();
        assert_eq!(backups, vec![damaged.to_vec(), damaged.to_vec()]);
    }

    #[test]
    fn stale_calibration_recovery_preserves_a_new_file() {
        let fixture = CalibrationFixture::new();
        let original = fs::read(fixture.file()).unwrap();
        fs::write(fixture.file(), b"").unwrap();
        let model = Calibration::load(&fixture.root).unwrap();
        fs::write(fixture.file(), &original).unwrap();
        let error = save_calibration_with_ack(&fixture.root, &model, || {
            panic!("a conflicting calibration must never be sent to the player");
        })
        .unwrap_err();
        assert!(error.contains("其它程序"));
        assert_eq!(fs::read(fixture.file()).unwrap(), original);
        assert!(!fixture.root.join("data/backups").exists());
    }

    #[test]
    fn missing_calibration_recovery_removes_its_new_file_if_reload_fails() {
        let fixture = CalibrationFixture::new();
        fs::remove_file(fixture.file()).unwrap();
        let model = Calibration::load(&fixture.root).unwrap();
        assert!(model.recovery_notice().is_some());
        let error =
            save_calibration_with_ack(&fixture.root, &model, || Err("offline".into())).unwrap_err();
        assert!(error.contains("已撤销新建文件"));
        assert!(!fixture.file().exists());
    }

    #[test]
    fn unreadable_calibration_recovery_keeps_the_original_path() {
        let fixture = CalibrationFixture::new();
        fs::remove_file(fixture.file()).unwrap();
        fs::create_dir(fixture.file()).unwrap();
        let model = Calibration::load(&fixture.root).unwrap();
        assert!(model.recovery_notice().is_some());
        assert!(
            save_calibration_with_ack(&fixture.root, &model, || {
                panic!("no calibration should be sent when the original cannot be backed up")
            })
            .is_err()
        );
        assert!(fixture.file().is_dir());
        assert!(!fixture.root.join("data/backups").exists());
    }

    #[test]
    fn partial_calibration_retains_base_coordinates_for_unlisted_keys() {
        let fixture = CalibrationFixture::new();
        let partial = br#"{"version":1,"points":{"Key0":{"x":0.25,"y":0.75}}}"#;
        fs::write(fixture.file(), partial).unwrap();
        let model = Calibration::load(&fixture.root).unwrap();
        assert!(model.recovery_notice().is_none());
        assert_eq!(model.points["Key0"], Point { x: 0.25, y: 0.75 });
        assert_eq!(model.points["Key41"], Point { x: 0.5, y: 0.5 });
        assert_eq!(fs::read(fixture.file()).unwrap(), partial);
    }

    #[test]
    fn first_run_requires_live_keyboard_before_selecting_or_showing() {
        let mut requests = Vec::new();
        let submit = |requests: &mut Vec<Value>, matched_devices: Option<u64>| {
            requests.clear();
            configure_first_run("display", Some("identity"), "keyboard", |request| {
                let settings = request["action"] == "setSettings";
                requests.push(request);
                if settings {
                    matched_devices
                        .map(|count| json!({"selectedKeyboardDevices":count}))
                        .ok_or_else(|| "keyboard is offline".into())
                } else {
                    Ok(Value::Null)
                }
            })
        };
        assert_eq!(
            submit(&mut requests, None).unwrap_err(),
            "keyboard is offline"
        );
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["requireConnectedKeyboard"], true);
        assert_eq!(requests[0]["settings"]["deviceGroup"], "keyboard");
        assert_eq!(requests[0]["settings"]["monitorIdentity"], "identity");

        assert!(submit(&mut requests, Some(0)).is_err());
        assert_eq!(requests.len(), 1);

        submit(&mut requests, Some(1)).unwrap();
        assert_eq!(requests[1]["action"], "selectPack");
        assert_eq!(requests[2]["action"], "show");
    }

    #[test]
    fn calibration_no_change_save_reloads_current_document_without_writing_or_ipc() {
        let fixture = CalibrationFixture::new();
        let model = Calibration::load(&fixture.root).unwrap();
        let mut current: Value =
            serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
        current["revision"] = json!("CURRENT");
        fs::write(fixture.file(), serde_json::to_vec_pretty(&current).unwrap()).unwrap();
        let before = fs::read(fixture.file()).unwrap();
        let saved = save_calibration_and_reload(&fixture.root, &model).unwrap();
        assert_eq!(saved.document["revision"], "CURRENT");
        assert_eq!(saved.document["customMetadata"], "preserved");
        assert_eq!(saved.original, saved.points);
        assert!(!saved.changed());
        assert_eq!(fs::read(fixture.file()).unwrap(), before);
        assert!(!fixture.root.join("data/backups").exists());
    }
    #[test]
    fn stale_calibration_save_refuses_to_overwrite_external_edits_before_ipc() {
        let fixture = CalibrationFixture::new();
        let mut model = Calibration::load(&fixture.root).unwrap();
        let mut external: Value =
            serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
        external["points"]["Key0"]["x"] = json!(0.2);
        fs::write(fixture.file(), serde_json::to_vec(&external).unwrap()).unwrap();
        let before = fs::read(fixture.file()).unwrap();
        model.move_point("Key1", 0.3, 0.4);
        assert!(
            save_calibration_and_reload(&fixture.root, &model)
                .unwrap_err()
                .contains("其它程序")
        );
        assert_eq!(fs::read(fixture.file()).unwrap(), before);
        assert!(!fixture.root.join("data/backups").exists());
    }
    #[test]
    fn calibration_real_decimal_values_save_twice_without_false_conflict() {
        let fixture = CalibrationFixture::new();
        let mut model = Calibration::load(&fixture.root).unwrap();
        model.move_point("Key0", 0.9297222561306423, 0.9338889227973091);
        let ack = || {
            let doc: Value = serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
            Ok(json!({"calibrationRevision":doc["revision"]}))
        };
        let mut saved = save_calibration_and_reload_with_ack(&fixture.root, &model, ack).unwrap();
        assert_eq!(saved.points, model.points);
        assert_eq!(saved.original, saved.points);
        saved.move_point("Key1", 0.9477777777777777, 0.23666665818956162);
        let second = save_calibration_and_reload_with_ack(&fixture.root, &saved, ack).unwrap();
        assert_eq!(second.points, saved.points);
        assert_eq!(second.original, second.points);
        assert_eq!(second.document["customMetadata"], "preserved");
    }
    #[test]
    fn calibration_integer_and_float_coordinates_do_not_create_false_conflict() {
        let fixture = CalibrationFixture::new();
        let mut model = Calibration::load(&fixture.root).unwrap();
        let mut same: Value = serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
        same["points"]["Key0"]["x"] = json!(0);
        same["points"]["Key82"]["y"] = json!(1);
        fs::write(fixture.file(), serde_json::to_vec(&same).unwrap()).unwrap();
        model.move_point("Key1", 0.31, 0.42);
        let saved = save_calibration_and_reload_with_ack(&fixture.root, &model, || {
            let doc: Value = serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
            Ok(json!({"calibrationRevision":doc["revision"]}))
        })
        .unwrap();
        assert_eq!(saved.points, model.points);
    }
    #[test]
    fn calibration_ack_failure_restores_own_write_but_not_a_later_external_write() {
        let fixture = CalibrationFixture::new();
        let mut model = Calibration::load(&fixture.root).unwrap();
        let before = fs::read(fixture.file()).unwrap();
        model.move_point("Key1", 0.32, 0.41);
        let failure =
            save_calibration_with_ack(&fixture.root, &model, || Err("host unavailable".into()))
                .unwrap_err();
        assert!(failure.contains("恢复原文件"));
        assert_eq!(fs::read(fixture.file()).unwrap(), before);
        let mut external_bytes = Vec::new();
        let failure = save_calibration_with_ack(&fixture.root, &model, || {
            let mut external: Value =
                serde_json::from_slice(&fs::read(fixture.file()).unwrap()).unwrap();
            external["points"]["Key2"]["x"] = json!(0.456);
            external["revision"] = json!("EXTERNAL");
            external_bytes = serde_json::to_vec(&external).unwrap();
            fs::write(fixture.file(), &external_bytes).unwrap();
            Err("ack lost after other writer".into())
        })
        .unwrap_err();
        assert!(failure.contains("保留最新文件"));
        assert_eq!(fs::read(fixture.file()).unwrap(), external_bytes);
    }
    #[test]
    fn settings_preserve_native_defaults() {
        let v = json!({"fps":60,"effectBrightness":2});
        let s = Settings::from_status(&v);
        assert_eq!(s.brightness, 2.);
        assert_eq!(s.color, [255; 3]);
        assert_eq!(s.scale, 1.);
        s.validate().unwrap();
    }
    #[test]
    fn reject_invalid_settings() {
        let mut s = Settings::default();
        s.scale = 3.01;
        assert!(s.validate().is_err());
        s.scale = 1.;
        s.intensity = f32::NAN;
        assert!(s.validate().is_err());
    }
    #[test]
    fn valid_color_and_exact_host_field_names() {
        let s = Settings {
            color: [12, 80, 200],
            ..Settings::default()
        };
        let r = s.request();
        assert_eq!(r["settings"]["effectColor"], json!([12, 80, 200]));
        assert_eq!(r["settings"]["effectScale"], 1.);
        assert_eq!(r["settings"]["effectIntensity"], 1.);
    }
    #[test]
    fn reject_missing_and_outside_calibration() {
        let order = (0..83).map(|i| format!("Key{i}")).collect::<Vec<_>>();
        let mut points = order
            .iter()
            .map(|k| (k.clone(), Point { x: 0.5, y: 0.5 }))
            .collect::<BTreeMap<_, _>>();
        validate_points(&points, &order).unwrap();
        points.get_mut("Key2").unwrap().x = 1.001;
        assert!(validate_points(&points, &order).is_err());
        points.remove("Key2");
        assert!(validate_points(&points, &order).is_err());
    }
    #[test]
    fn atomic_replace_overwrites_only_named_fixture() {
        let root =
            std::env::temp_dir().join(format!("action75-rust-atomic-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let a = root.join("new");
        let b = root.join("old");
        fs::write(&a, b"new").unwrap();
        fs::write(&b, b"old").unwrap();
        atomic_replace(&a, &b).unwrap();
        assert_eq!(fs::read(&b).unwrap(), b"new");
        fs::remove_file(&b).unwrap();
        fs::remove_dir(&root).unwrap();
    }
    #[test]
    fn unchanged_calibration_does_not_write_or_contact_host() {
        let order = (0..83).map(|i| format!("Key{i}")).collect::<Vec<_>>();
        let points = order
            .iter()
            .map(|k| (k.clone(), Point { x: 0.5, y: 0.5 }))
            .collect::<BTreeMap<_, _>>();
        let model = Calibration {
            document: json!({"version":1}),
            original: points.clone(),
            points,
            order,
            recovery: None,
        };
        let missing = std::env::temp_dir().join("action75-rust-nonexistent-host-check");
        assert!(
            save_calibration(&missing, &model)
                .unwrap()
                .contains("未改动")
        );
        assert!(!missing.exists());
    }
    #[test]
    fn moves_are_clamped_to_screen() {
        let mut points = BTreeMap::new();
        points.insert("KeyA".into(), Point { x: 0.3, y: 0.4 });
        let mut m = Calibration {
            document: Value::Null,
            original: points.clone(),
            points,
            order: vec!["KeyA".into()],
            recovery: None,
        };
        m.move_point("KeyA", -0.2, 1.5);
        assert_eq!(m.points["KeyA"], Point { x: 0., y: 1. });
        assert!(m.changed());
    }
    #[test]
    fn grading_roundtrip_preserves_accepted_appearance() {
        let value = json!({"fps":60,"effectBrightness":4.0,"effectScale":1.25,"effectIntensity":1.0,"effectColor":[255,255,255],"effectSaturation":1.4,"effectHue":-35.0,"effectPalette":"custom","effectPaletteColors":[[230,80,120],[30,180,200]]});
        let s = Settings::from_status(&value);
        s.validate().unwrap();
        let r = s.request();
        let mut expected = value.clone();
        expected["effectSaturation"] = r["settings"]["effectSaturation"].clone();
        assert_eq!(r["settings"], expected);
        assert!((r["settings"]["effectSaturation"].as_f64().unwrap() - 1.4).abs() < 1e-6);
        let recovered: Settings = serde_json::from_value(r["settings"].clone()).unwrap();
        assert_eq!(recovered, s);
    }
    #[test]
    fn original_color_reset_keeps_brightness_intensity_size_and_fps() {
        let mut s = Settings::from_status(
            &json!({"fps":120,"effectBrightness":4.0,"effectScale":1.25,"effectIntensity":1.0,"effectSaturation":2.0,"effectHue":90.0,"effectPalette":"candy","effectColor":[140,160,250]}),
        );
        s.reset_color_grade();
        assert_eq!(
            (s.brightness, s.scale, s.intensity, s.fps),
            (4.0, 1.25, 1.0, 120)
        );
        assert_eq!((s.saturation, s.hue), (1.0, 0.0));
        assert_eq!(s.color, [255; 3]);
        assert_eq!(s.palette, "original");
    }
    #[test]
    fn custom_palette_enforces_one_to_nine_colors() {
        let mut s = Settings::default();
        s.palette = "custom".into();
        while s.palette_colors.len() < 9 {
            assert!(s.add_palette_color([10, 20, 30]));
        }
        assert!(!s.add_palette_color([40, 50, 60]));
        assert_eq!(s.palette_colors.len(), 9);
        while s.palette_colors.len() > 1 {
            assert!(s.remove_palette_color(0));
        }
        assert!(!s.remove_palette_color(0));
        assert!(!s.remove_palette_color(99));
        s.validate().unwrap();
        s.palette_colors.clear();
        assert!(s.validate().is_err());
        s.palette_colors = vec![[0; 3]; 10];
        assert!(s.validate().is_err());
    }
    #[test]
    fn grading_rejects_nonfinite_out_of_range_and_invalid_rgb() {
        let mut s = Settings::default();
        s.saturation = 2.6;
        assert!(s.validate().is_err());
        s.saturation = 1.;
        s.hue = 181.;
        assert!(s.validate().is_err());
        s.hue = f32::NAN;
        assert!(s.validate().is_err());
        assert!(
            serde_json::from_value::<Settings>(json!({"effectPaletteColors":[[256,10,20]]}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<Settings>(json!({"effectPaletteColors":[[10,20]]})).is_err()
        );
    }
    #[test]
    fn percentages_roundtrip_and_preserve_existing_factors_until_edited() {
        for scale in [
            PercentScale::Brightness,
            PercentScale::Saturation,
            PercentScale::Intensity,
        ] {
            for percent in 0..=100 {
                let p = percent as f32;
                assert!((scale.percent(scale.factor(p)) - p).abs() < 0.0001);
            }
            assert_eq!(scale.factor(0.), 0.);
            assert_eq!(scale.factor(50.), 1.);
        }
        assert_eq!(PercentScale::Brightness.percent(4.), 100.);
        assert_eq!(PercentScale::Saturation.percent(2.5), 100.);
        assert!((PercentScale::Intensity.percent(0.8) - 40.).abs() < 0.0001);
        let mut settings = Settings::default();
        settings.brightness = 0.;
        settings.intensity = 0.;
        settings.validate().unwrap();
        assert_eq!(settings.request()["settings"]["effectBrightness"], 0.);
    }
    #[test]
    fn calibration_gate_ready_and_legacy_hosts_do_not_wait() {
        for status in [
            json!({"exclusiveDisplay":true,"displayGuardActive":false}),
            json!({}),
        ] {
            let calls = std::cell::Cell::new(0);
            confirm_calibration_access_with(
                |request, _| {
                    assert_eq!(request["action"], "status");
                    calls.set(calls.get() + 1);
                    Ok(status.clone())
                },
                |_| panic!("must not wait"),
                || Duration::ZERO,
                Duration::from_secs(2),
            )
            .unwrap();
            assert_eq!(calls.get(), 1);
        }
    }
    #[test]
    fn calibration_gate_waits_for_delayed_release_before_returning() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let calls = std::cell::Cell::new(0);
        confirm_calibration_access_with(
            |request, remaining| {
                assert_eq!(request["action"], "status");
                assert!(remaining <= Duration::from_secs(2));
                calls.set(calls.get() + 1);
                Ok(json!({"exclusiveDisplay":true,"displayGuardActive":calls.get()<3}))
            },
            |duration| clock.set(clock.get() + duration),
            || clock.get(),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(calls.get(), 3);
        assert_eq!(clock.get(), Duration::from_millis(150));
    }
    #[test]
    fn calibration_gate_inactive_pending_waits_until_cleanup_mode() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let calls = std::cell::Cell::new(0);
        confirm_calibration_access_with(
            |request, _| {
                assert_eq!(request["action"], "status");
                let mode = ["pending", "starting", "suspended"][calls.get()];
                calls.set(calls.get() + 1);
                Ok(json!({"exclusiveDisplay":true,"displayGuardActive":false,
                    "displayGuardMode":mode,"displayGuardError":null}))
            },
            |duration| clock.set(clock.get() + duration),
            || clock.get(),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(calls.get(), 3);
        assert_eq!(clock.get(), Duration::from_millis(150));
    }
    #[test]
    fn calibration_gate_returns_the_release_failure_without_sending_resume() {
        let mut calls = Vec::new();
        let result = confirm_calibration_access_with(
            |request, _| {
                calls.push(request);
                Ok(json!({"exclusiveDisplay":true,"displayGuardActive":false,
                    "displayGuardMode":"suspended","displayGuardError":"ClipCursor release failed"}))
            },
            |_| panic!("real error must fail without waiting"),
            || Duration::ZERO,
            Duration::from_secs(2),
        );
        assert_eq!(result.unwrap_err(), "ClipCursor release failed");
        assert_eq!(calls, vec![json!({"action":"status"})]);
    }
    #[test]
    fn calibration_gate_timeout_leaves_pause_cleanup_to_its_owner() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut calls = Vec::new();
        let result = confirm_calibration_access_with(
            |request, _| {
                calls.push(request.clone());
                Ok(json!({"exclusiveDisplay":true,"displayGuardActive":true}))
            },
            |duration| clock.set(clock.get() + duration),
            || clock.get(),
            Duration::from_secs(2),
        );
        assert!(result.is_err());
        assert_eq!(clock.get(), Duration::from_secs(2));
        assert!(calls.iter().all(|request| request["action"] == "status"));
    }
}
