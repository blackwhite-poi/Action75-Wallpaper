#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod calibration_geometry;
mod calibration_ui;
mod diagnostics;
mod exclusive_ui;
mod folder_picker;
mod gallery_previews;
mod gallery_selection;
mod monitor_ui;
mod pack_ui;
mod palette_ui;
mod percent_ui;
mod performance;
mod performance_ui;
mod single_instance;
mod ui_chrome;
mod ui_style;
mod update_ui;
mod wallpaper_import_ui;
mod window_frame;
use action75_manager::{
    self as core, APP_NAME, Calibration, Event, Job, command, game_compat,
    graphics_adapters::GraphicsAdapters,
    startup::{self, LegacyStartup, StartupAction, StartupState},
    storage::{GpuPreference, InterfaceTheme},
};
use eframe::egui::{self, Align, Color32, FontId, RichText, Sense, Stroke, vec2};
use serde_json::{Map, Value, json};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use ui_chrome::Icon;

// Keep the existing local directory for eframe's window settings.
const EFRAME_APP_ID: &str = "ACTION75";
const DEFAULT_WINDOW_SIZE: [f32; 2] = [1380., 880.];
const MIN_WINDOW_SIZE: [f32; 2] = [1080., 680.];

const PERFORMANCE_TAB: u8 = 4;
const NAVIGATION: [(u8, &str, Icon); 5] = [
    (0, "壁纸库", Icon::Library),
    (1, "键位校准", Icon::Calibration),
    (2, "设备与输出", Icon::Devices),
    (PERFORMANCE_TAB, "性能监控", Icon::Performance),
    (3, "设置", Icon::Settings),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrdinaryAction {
    Import,
    Update,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Restart {
    #[default]
    None,
    Graphics,
    OutputMode(bool),
    Ordinary(OrdinaryAction),
}

fn main() -> eframe::Result {
    if let Some(code) = core::update_install::helper_mode() {
        std::process::exit(code);
    }
    let mut updated_panel = false;
    let mut update_failed = false;
    let mut root = core::default_root();
    let mut calibrate = false;
    let mut devices = false;
    let mut open_settings = false;
    let mut ordinary_playback = None;
    let mut ordinary_launcher = false;
    let mut switch_error = None;
    let mut background = false;
    let mut toggle_startup = false;
    let mut game_compatible = false;
    let mut handoff_directory = None;
    let mut handoff_token = None;
    let mut check = false;
    let mut screenshot: Option<PathBuf> = None;
    let mut initial_size = None;
    let mut screenshot_tab = None;
    let mut exit_after_screenshot = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => {
                if let Some(p) = args.next() {
                    root = p.into()
                }
            }
            "--screenshot-size" => {
                if let (Some(w), Some(h)) = (args.next(), args.next()) {
                    if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                        initial_size = Some([w, h]);
                    }
                }
            }
            "--screenshot-tab" => screenshot_tab = args.next(),
            "--screenshot" => {
                if let Some(p) = args.next() {
                    screenshot = Some(p.into())
                }
            }
            "--exit-after-screenshot" => exit_after_screenshot = true,
            "--calibrate" => calibrate = true,
            "--devices" => devices = true,
            "--settings" => open_settings = true,
            "--ordinary-playback" => ordinary_playback = args.next(),
            "--ordinary-launcher" => ordinary_launcher = true,
            "--mode-switch-error" => switch_error = args.next(),
            "--background" => background = true,
            "--toggle-startup" => toggle_startup = true,
            "--game-compatible" => game_compatible = true,
            "--handoff" => handoff_directory = args.next().map(PathBuf::from),
            "--handoff-token" => handoff_token = args.next(),
            "--updated-panel" => updated_panel = true,
            "--update-failed" => update_failed = true,
            "--check" => check = true,
            _ => {}
        }
    }
    if ordinary_launcher {
        let result = core::runtime_identity::installation_endpoint(&root).and_then(|id| {
            let marker = single_instance::manager_marker(&id);
            game_compat::open_panel(&root, open_settings, ordinary_playback.as_deref(), |pid| {
                single_instance::panel_ready(pid, &marker)
            })
        });
        return result.map_err(|error| {
            core::storage::log(&root, "ERROR", &error);
            eframe::Error::AppCreation(error.into())
        });
    }
    if toggle_startup {
        toggle_startup_from_tray(&root);
        return Ok(());
    }
    if check {
        let result = json!({"status":command(&root,json!({"action":"status"})),"calibration":Calibration::load(&root).map(|c|json!({"keys":c.points.len(),"revision":c.document["revision"]}))});
        println!("{result}");
        return Ok(());
    }
    let handoff = game_compat::Handoff::from_args(handoff_directory, handoff_token)
        .map_err(|e| eframe::Error::AppCreation(e.into()))?;
    let mut prepared = if game_compatible {
        if !game_compat::is_elevated().map_err(|e| eframe::Error::AppCreation(e.into()))? {
            return game_compat::elevate_and_wait(&root)
                .map_err(|e| eframe::Error::AppCreation(e.into()));
        }
        Some(
            game_compat::prepare(&root, handoff)
                .map_err(|e| eframe::Error::AppCreation(e.into()))?,
        )
    } else {
        None
    };
    core::storage::bootstrap(&root).map_err(|e| eframe::Error::AppCreation(e.into()))?;
    core::storage::log(&root, "INFO", "管理界面启动");
    if background
        && !game_compatible
        && matches!(
            startup::update(&root, StartupAction::Inspect),
            Ok(StartupState {
                legacy: LegacyStartup::Active,
                ..
            })
        )
    {
        core::storage::log(
            &root,
            "INFO",
            "旧试用版登录任务已启用，跳过重复启动。可在设置中迁移。",
        );
        return Ok(());
    }
    if background && !game_compatible && core::storage::Preferences::load(&root).setup_complete {
        core::start_player(&root).map_err(|e| eframe::Error::AppCreation(e.into()))?;
        return Ok(());
    }
    let Some(instance) = single_instance::acquire(&root, game_compatible)
        .map_err(|e| eframe::Error::AppCreation(e.into()))?
    else {
        return Ok(());
    };
    let icon = image::load_from_memory(include_bytes!("../../../assets/app.png"))
        .expect("embedded icon")
        .into_rgba8();
    let app_icon = egui::IconData {
        width: icon.width(),
        height: icon.height(),
        rgba: icon.into_raw(),
    };
    let gpu_preference = core::storage::Preferences::load(&root).gpu_preference;
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: controller_wgpu_options(gpu_preference),
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_icon(app_icon)
            .with_decorations(false)
            .with_transparent(true)
            .with_active(!background)
            .with_inner_size(initial_size.unwrap_or(DEFAULT_WINDOW_SIZE))
            .with_min_inner_size(MIN_WINDOW_SIZE),
        ..Default::default()
    };
    let restart_on_exit = Rc::new(Cell::new(Restart::None));
    let mut create_manager = |cc: &eframe::CreationContext<'_>| -> Result<
        Box<dyn eframe::App>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        if let Some(playback) = ordinary_playback.take() {
            game_compat::restore_playback(&root, &playback)
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { error.into() })?;
        }
        instance.mark_window(cc);
        let (api, adapter) = if let Some(state) = &cc.wgpu_render_state {
            ("DirectX 12", Some(state.adapter.get_info().name))
        } else {
            use eframe::glow::HasContext;
            // eframe has made this GL context current before creating the app.
            let adapter = cc
                .gl
                .as_ref()
                .map(|gl| unsafe { gl.get_parameter_string(eframe::glow::RENDERER) })
                .filter(|name| !name.is_empty());
            ("OpenGL", adapter)
        };
        let renderer = adapter
            .as_deref()
            .map_or_else(|| api.to_owned(), |name| format!("{api}（{name}）"));
        core::storage::log(&root, "INFO", &format!("管理界面使用 {renderer} 渲染"));
        ui_style::install_fonts(&cc.egui_ctx);
        let mut manager = Manager::new(cc, root.clone(), updated_panel);
        manager.acrylic_available = cc.wgpu_render_state.is_some();
        manager.update_acrylic();
        if updated_panel && update_failed {
            manager.error = "更新安装失败，已重新打开控制台。".into();
        }
        if let Some(error) = &switch_error {
            manager.error = error.clone();
        }
        manager.started_gpu_preference = gpu_preference;
        manager.controller_renderer = renderer;
        manager.restart_on_exit = Rc::clone(&restart_on_exit);
        if calibrate {
            manager.tab = 1;
            manager.calibration.start(&cc.egui_ctx);
        } else if devices {
            manager.tab = 2;
        } else if open_settings {
            manager.tab = 3;
        }
        if let Some(tab) = screenshot_tab.as_deref() {
            manager.tab = match tab {
                "settings" => 3,
                "devices" => 2,
                "calibration" => 1,
                "performance" => PERFORMANCE_TAB,
                _ => 0,
            };
        }
        manager.screenshot = screenshot.take();
        manager.exit_after_screenshot = exit_after_screenshot;
        if let Some(prepared) = prepared.take() {
            prepared
                .ready()
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
        }
        Ok(Box::new(manager))
    };
    // Apply the hook to each launch option after cloning.
    let with_startup_size = |mut options: eframe::NativeOptions| {
        if initial_size.is_none() {
            options.window_builder = Some(Box::new(|viewport| {
                if primary_display_has_double_scale() {
                    viewport.with_inner_size(MIN_WINDOW_SIZE)
                } else {
                    viewport
                }
            }));
        }
        options
    };
    let result = eframe::run_native(
        EFRAME_APP_ID,
        with_startup_size(options.clone()),
        Box::new(|cc| create_manager(cc)),
    );
    match result {
        Err(eframe::Error::Wgpu(error)) => {
            core::storage::log(
                &root,
                "WARN",
                &format!("DirectX 12 初始化失败，改用 OpenGL。{error}"),
            );
            options = with_startup_size(options);
            options.renderer = eframe::Renderer::Glow;
            options.viewport = options.viewport.with_transparent(false);
            eframe::run_native(EFRAME_APP_ID, options, Box::new(|cc| create_manager(cc)))
        }
        result => result,
    }?;
    let restart = restart_on_exit.get();
    if restart != Restart::None {
        let mut switch_error = None;
        let settings = restart != Restart::Ordinary(OrdinaryAction::Import);
        if let Restart::OutputMode(hmd) = restart {
            // Release the controller's D3D device before Windows restarts the
            // adapter. The separate wallpaper player recovers in place.
            switch_error = game_compat::switch_display_mode(&root, hmd).err();
            if let Some(error) = &switch_error {
                core::storage::log(&root, "ERROR", error);
            }
        }
        // Release the normal instance lock before the replacement starts.
        drop(instance);
        if matches!(restart, Restart::Ordinary(_)) {
            let switched = game_compat::reopen(&root, settings);
            match switched {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let message = format!("无法关闭管理员模式，{error}");
                    core::storage::log(&root, "ERROR", &message);
                    switch_error = Some(message);
                }
            }
        }
        let result = std::env::current_exe().and_then(|exe| {
            let mut command = Command::new(exe);
            command.arg("--root").arg(&root);
            if matches!(restart, Restart::OutputMode(_)) {
                command.arg("--devices");
            } else if settings {
                command.arg("--settings");
            }
            if game_compatible {
                command.arg("--game-compatible");
            }
            if let Some(error) = switch_error {
                command.arg("--mode-switch-error").arg(error);
            }
            command.spawn()
        });
        if let Err(error) = result {
            core::storage::log(&root, "ERROR", &format!("无法重新打开控制台，{error}"));
            return Err(eframe::Error::AppCreation(error.into()));
        }
    }
    Ok(())
}

fn toggle_startup_from_tray(root: &Path) {
    use windows::{
        Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW},
        core::{HSTRING, w},
    };
    let message = match startup::update(root, StartupAction::Toggle) {
        Ok(StartupState {
            enabled: false,
            legacy: LegacyStartup::Active,
        }) => "已关闭新版开机运行。旧试用版登录任务仍启用，可在新版设置中迁移。".into(),
        Ok(_) => return,
        Err(error) => error,
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("开机运行"),
            MB_OK | MB_ICONWARNING,
        );
    }
}

fn controller_wgpu_options(preference: GpuPreference) -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetupCreateNew};

    // DXGI presents a finished surface after the clear and all UI draws.
    let mut setup = WgpuSetupCreateNew {
        instance_descriptor: wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..Default::default()
        },
        power_preference: match preference {
            GpuPreference::Discrete => wgpu::PowerPreference::HighPerformance,
            GpuPreference::Integrated => wgpu::PowerPreference::LowPower,
        },
        device_descriptor: Arc::new(|_| wgpu::DeviceDescriptor {
            label: Some("ACTION75 controller"),
            // DX12 allocates this descriptor heap up front. The UI uses a bounded texture cache.
            required_limits: wgpu::Limits {
                max_non_sampler_bindings: 4096,
                ..Default::default()
            },
            // Smaller DX12 heaps fit the transient GUI buffers and cover textures.
            memory_hints: wgpu::MemoryHints::Manual {
                suballocated_device_memory_block_size: (4 << 20)..(8 << 20),
            },
            ..Default::default()
        }),
        ..Default::default()
    };
    setup.instance_descriptor.backend_options.dx12.presentation_system =
        wgpu_types::Dx12SwapchainKind::DxgiFromVisual;
    WgpuConfiguration {
        // The controller draws on demand and needs only one frame queued.
        desired_maximum_frame_latency: Some(1),
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum GalleryCategory {
    All,
    Interactive,
    Image,
    Video,
}

impl GalleryCategory {
    fn matches(self, pack: &Value) -> bool {
        self == Self::All
            || self
                == match pack["kind"].as_str() {
                    Some("image") => Self::Image,
                    Some("video") => Self::Video,
                    _ => Self::Interactive,
                }
    }
}

struct Manager {
    acrylic_available: bool,
    acrylic: bool,
    root: PathBuf,
    tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Event>,
    status: Value,
    packs: Vec<Value>,
    visible_packs: Vec<usize>,
    catalog_loading: bool,
    catalog_changed_while_loading: bool,
    catalog_error: Option<String>,
    connection_error: String,
    monitors: Vec<Value>,
    devices: Vec<Value>,
    palettes: Vec<Value>,
    previews: gallery_previews::Cache,
    selection_frame: gallery_selection::SelectionFrame,
    queued_wallpaper: Option<String>,
    profile: core::pack_model::Draft,
    profile_save_error: Option<String>,
    profile_recovery: Option<String>,
    profile_recovery_pending: bool,
    exclusive: exclusive_ui::State,
    calibration: calibration_ui::Controller,
    closing: bool,
    compat: Option<mpsc::Receiver<Result<(), String>>>,
    compat_replaced: bool,
    selecting: bool,
    switching_display: bool,
    tab: u8,
    error: String,
    message: String,
    pending: Option<Instant>,
    last_poll: Instant,
    polling: bool,
    search: String,
    gallery_category: GalleryCategory,
    screenshot: Option<PathBuf>,
    screenshot_requested: bool,
    exit_after_screenshot: bool,
    created: Instant,
    preferences: core::storage::Preferences,
    panel_open: bool,
    panel_width: f32,
    shell: ui_style::Shell,
    window_frame: Option<window_frame::WindowFrame>,
    page_reveal: ui_style::Reveal,
    pack_reveal: ui_style::Reveal,
    performance: performance_ui::PerformancePage,
    last_tab: u8,
    shown_pack: String,
    setup_complete: bool,
    setup_monitor: Option<Value>,
    monitor_warning: Option<Value>,
    refreshing_devices: bool,
    setup_device: String,
    migrating: bool,
    folder_request: Option<(DirectoryField, mpsc::Receiver<folder_picker::Reply>)>,
    wallpaper_import: wallpaper_import_ui::ImportFlow,
    delete_confirmation: Option<(String, String, PathBuf)>,
    deleting: bool,
    startup_state: Option<StartupState>,
    startup_pending: bool,
    startup_checked: Option<Instant>,
    started_gpu_preference: GpuPreference,
    gpu_save_pending: bool,
    graphics_adapters: Option<Result<GraphicsAdapters, String>>,
    graphics_scan_needed: bool,
    restart_pending: bool,
    restart_on_exit: Rc<Cell<Restart>>,
    administrator: bool,
    ordinary_prompt: Option<OrdinaryAction>,
    controller_renderer: String,
    diagnostics_copied: Option<Instant>,
    updates: update_ui::Updates,
    preview_in_flight: bool,
    last_preview: Instant,
}

#[derive(Clone, Copy)]
enum DirectoryField {
    Wallpapers,
    Logs,
}

enum DirectoryAction {
    Pick,
    Open,
}

const SETTINGS_FORM_WIDTH: f32 = 736.;
const SETTINGS_LABEL_WIDTH: f32 = 76.;
const SETTINGS_SECTION_GAP: f32 = 16.;
const DIRECTORY_BUTTON_WIDTH: f32 = 64.;

fn settings_control_height(ui: &egui::Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Button) + 2. * ui.spacing().button_padding.y)
        .max(ui.spacing().interact_size.y)
}

fn settings_label(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(size);
        ui.label(text)
    })
    .inner
}

fn keyboard_input_heading(ui: &mut egui::Ui, title: RichText) {
    ui.horizontal_wrapped(|ui| {
        ui.label(title);
        ui.label(
            RichText::new("如按键时没有特效，请确认已选中 Action75 键盘。")
                .size(12.)
                .weak(),
        );
    });
}

fn settings_section(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 6.;
        ui.label(RichText::new(title).size(16.).strong());
        contents(ui);
    });
}

fn settings_detail(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(SETTINGS_LABEL_WIDTH + ui.spacing().item_spacing.x);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), settings_control_height(ui)),
            egui::Layout::left_to_right(Align::Center),
            contents,
        );
    });
}

fn settings_input_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - 2. * (DIRECTORY_BUTTON_WIDTH + ui.spacing().item_spacing.x)).max(80.)
}

fn setting_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    contents: impl FnOnce(&mut egui::Ui, egui::Id) -> R,
) -> R {
    let height = settings_control_height(ui);
    ui.horizontal(|ui| {
        let caption = settings_label(ui, vec2(SETTINGS_LABEL_WIDTH, height), label);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), height),
            egui::Layout::left_to_right(Align::Center),
            |ui| contents(ui, caption.id),
        )
        .inner
    })
    .inner
}

fn directory_row(
    ui: &mut egui::Ui,
    label: &str,
    path: &mut String,
    default: &Path,
) -> Option<DirectoryAction> {
    setting_row(ui, label, |ui, caption| {
        let placeholder = default.to_string_lossy();
        let height = settings_control_height(ui);
        ui.add_sized(
            [settings_input_width(ui), height],
            egui::TextEdit::singleline(path)
                .desired_width(f32::INFINITY)
                .vertical_align(Align::Center)
                .hint_text(placeholder.as_ref()),
        )
        .labelled_by(caption)
        .on_hover_ui(|ui| {
            ui.label(if path.is_empty() {
                placeholder.as_ref()
            } else {
                path.as_str()
            });
        });
        let pick = ui
            .add_sized([DIRECTORY_BUTTON_WIDTH, height], egui::Button::new("选择…"))
            .labelled_by(caption)
            .clicked();
        let open = ui
            .add_sized([DIRECTORY_BUTTON_WIDTH, height], egui::Button::new("打开"))
            .labelled_by(caption)
            .clicked();
        if pick {
            Some(DirectoryAction::Pick)
        } else if open {
            Some(DirectoryAction::Open)
        } else {
            None
        }
    })
}

fn setup_keyboard_connected(devices: &[Value], selected: &str) -> bool {
    !selected.is_empty() && devices.iter().any(|device| device["id"] == selected)
}

impl Manager {
    fn new(cc: &eframe::CreationContext<'_>, root: PathBuf, updated_panel: bool) -> Self {
        let (tx, rx) = core::worker(root.clone(), cc.egui_ctx.clone());
        let _ = tx.send(if updated_panel { Job::Full } else { Job::Start });
        let mut manager = Self::with_channels(root, tx, rx);
        manager.administrator = game_compat::is_elevated().unwrap_or(false);
        manager.updates.enable(&manager.root, manager.administrator);
        manager.window_frame = window_frame::WindowFrame::attach(cc);
        if manager.window_frame.is_none() {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        }
        manager
    }
    fn with_channels(root: PathBuf, tx: mpsc::Sender<Job>, rx: mpsc::Receiver<Event>) -> Self {
        let calibration = calibration_ui::Controller::new(root.clone());
        let preferences = core::storage::Preferences::load(&root);
        let panel_open = preferences.interface.panel_open.unwrap_or(true);
        let panel_width = preferences
            .interface
            .panel_width
            .unwrap_or(262.)
            .clamp(224., 400.);
        let setup_complete = preferences.setup_complete || cfg!(test);
        Self {
            preferences,
            panel_open,
            panel_width,
            acrylic_available: false,
            acrylic: false,
            shell: ui_style::Shell::default(),
            window_frame: None,
            page_reveal: ui_style::Reveal::default(),
            pack_reveal: ui_style::Reveal::default(),
            performance: performance_ui::PerformancePage::default(),
            last_tab: 0,
            shown_pack: String::new(),
            setup_complete,
            setup_monitor: None,
            monitor_warning: None,
            refreshing_devices: false,
            setup_device: String::new(),
            migrating: false,
            folder_request: None,
            wallpaper_import: wallpaper_import_ui::ImportFlow::default(),
            delete_confirmation: None,
            deleting: false,
            startup_state: None,
            startup_pending: false,
            startup_checked: None,
            started_gpu_preference: GpuPreference::default(),
            gpu_save_pending: false,
            graphics_adapters: None,
            graphics_scan_needed: true,
            restart_pending: false,
            restart_on_exit: Rc::new(Cell::new(Restart::None)),
            administrator: false,
            ordinary_prompt: None,
            controller_renderer: String::new(),
            diagnostics_copied: None,
            updates: update_ui::Updates::default(),
            preview_in_flight: false,
            last_preview: Instant::now(),
            root,
            tx,
            rx,
            status: Value::Null,
            packs: vec![],
            visible_packs: Vec::new(),
            catalog_loading: true,
            catalog_changed_while_loading: false,
            catalog_error: None,
            connection_error: String::new(),
            monitors: vec![],
            devices: vec![],
            palettes: vec![],
            previews: gallery_previews::Cache::default(),
            selection_frame: gallery_selection::SelectionFrame::default(),
            queued_wallpaper: None,
            profile: core::pack_model::Draft::default(),
            profile_save_error: None,
            profile_recovery: None,
            profile_recovery_pending: false,
            exclusive: exclusive_ui::State::default(),
            calibration,
            closing: false,
            compat: None,
            compat_replaced: false,
            selecting: false,
            switching_display: false,
            tab: 0,
            error: String::new(),
            message: String::new(),
            pending: None,
            last_poll: Instant::now(),
            polling: true,
            search: String::new(),
            gallery_category: GalleryCategory::All,
            screenshot: None,
            screenshot_requested: false,
            exit_after_screenshot: false,
            created: Instant::now(),
        }
    }
    fn send(&mut self, v: Value) {
        if self.calibration.has_session() || self.compat.is_some() {
            return;
        }
        self.error.clear();
        let _ = self.tx.send(Job::Request(v));
    }
    fn sync(&mut self, v: Value) {
        if v["running"] == true {
            let recovered = !self.connection_error.is_empty();
            let same_player = self.status["playerPid"].as_u64().is_some()
                && self.status["playerPid"] == v["playerPid"];
            let output_changed = self.status.get("monitorDevice").is_some()
                && v.get("monitorDevice").is_some()
                && (v["monitorDevice"] != self.status["monitorDevice"]
                    || v["monitorConnected"] != self.status["monitorConnected"]);
            if (recovered
                && (!same_player || self.catalog_error.is_some() || self.packs.is_empty()))
                || (self.status["playerPid"].as_u64().is_some() && !same_player)
            {
                self.request_catalog(false);
            } else if (recovered
                || output_changed
                || v["deviceGroup"] != self.status["deviceGroup"])
                && !self.catalog_loading
            {
                self.request_devices(Job::ReadDevices);
            }
        }
        self.apply_status(v);
    }
    fn apply_status(&mut self, v: Value) {
        if v["running"] == true {
            self.connection_error.clear();
        }
        self.exclusive.observe(&v);
        let current = v["packId"].as_str().unwrap_or("").to_owned();
        let needs_load = if current == self.profile.selected {
            self.profile.bundle.as_ref().is_some_and(|bundle| {
                v["packSettingsRevision"]
                    .as_u64()
                    .zip(bundle.revision.as_u64())
                    .is_some_and(|(live, loaded)| live > loaded)
            })
        } else {
            !self.profile.dirty()
        };
        self.status = v;
        self.polling = false;
        self.last_poll = Instant::now();
        if needs_load && !current.is_empty() && self.can_reload_profile() {
            self.open_profile(current, false);
        }
    }
    fn can_reload_profile(&self) -> bool {
        self.status["running"] == true
            && !self.selecting
            && !self.profile.writing()
            && !self.preview_in_flight
            && (self.pending.is_none() || self.profile_save_error.is_some())
    }
    fn request_catalog(&mut self, library_changed: bool) {
        if self.catalog_loading {
            self.catalog_changed_while_loading |= library_changed;
        } else {
            self.catalog_loading = self.tx.send(Job::Full).is_ok();
        }
    }
    fn request_devices(&mut self, job: Job) {
        if !self.refreshing_devices {
            self.refreshing_devices = self.tx.send(job).is_ok();
            if !self.refreshing_devices {
                self.error = "控制线程已退出".into();
            }
        }
    }
    fn request_player_start(&mut self) {
        if !self.catalog_loading {
            self.catalog_loading = self.tx.send(Job::Start).is_ok();
        }
    }
    fn open_profile(&mut self, id: String, activate: bool) {
        if activate && (self.calibration.has_session() || self.compat.is_some()) {
            return;
        }
        let selection = self.profile.select(id.clone());
        self.pending = None;
        self.preview_in_flight = false;
        self.selecting = true;
        if activate {
            self.profile_recovery = None;
            self.error.clear();
        }
        let _ = self.tx.send(if activate {
            Job::SelectPack { id, selection }
        } else {
            Job::LoadPack { id, selection }
        });
    }
    fn events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Previewed {
                    id,
                    selection,
                    error,
                } => {
                    if id == self.profile.selected && selection == self.profile.selection {
                        self.preview_in_flight = false;
                        if let Some(error) = error {
                            core::storage::log(&self.root, "ERROR", &error);
                            self.error = error;
                        }
                    }
                }
                Event::ExclusiveAck { generation, status } => {
                    if self.exclusive.ack(generation, &status) {
                        self.sync(status);
                    }
                }
                Event::ExclusiveFailed { generation, error } => {
                    self.exclusive.fail(generation, error)
                }
                Event::Status(status) => self.sync(status),
                Event::ConnectionFailed(error) => {
                    if self.connection_error != error {
                        core::storage::log(&self.root, "WARN", &error);
                    }
                    self.status["running"] = json!(false);
                    self.connection_error = error;
                    self.polling = false;
                    self.last_poll = Instant::now();
                }
                Event::DevicesRefreshed(result) => {
                    self.refreshing_devices = false;
                    match result {
                        Ok((monitors, devices)) => {
                            self.monitors = monitors;
                            self.devices = devices;
                        }
                        Err(error) => self.error = format!("刷新设备失败：{error}"),
                    }
                }
                Event::Full {
                    connection_error,
                    catalog_error,
                    status,
                    packs,
                    monitors,
                    devices,
                    palettes,
                } => {
                    let completed =
                        core::storage::Preferences::load(&self.root).setup_complete || cfg!(test);
                    if !self.setup_complete && completed {
                        self.selecting = false;
                    }
                    self.setup_complete = completed;
                    self.catalog_loading = false;
                    self.catalog_error = catalog_error;
                    self.connection_error = connection_error.unwrap_or_default();
                    self.previews.refresh(&packs);
                    self.packs = packs;
                    let ranks: std::collections::HashMap<_, _> = self
                        .preferences
                        .interface
                        .wallpaper_order
                        .iter()
                        .enumerate()
                        .map(|(rank, id)| (id.as_str(), rank))
                        .collect();
                    self.packs.sort_by_key(|pack| {
                        ranks
                            .get(pack["id"].as_str().unwrap_or_default())
                            .copied()
                            .unwrap_or(usize::MAX)
                    });
                    self.filter_wallpapers();
                    self.monitors = monitors;
                    self.devices = devices;
                    self.palettes = palettes;
                    self.apply_status(status);
                    if self.can_reload_profile()
                        && !self.profile.selected.is_empty()
                        && self.status["packId"] == self.profile.selected
                    {
                        self.open_profile(self.profile.selected.clone(), false);
                    }
                    if std::mem::take(&mut self.catalog_changed_while_loading) {
                        self.request_catalog(false);
                    }
                }
                Event::PackSelected {
                    id,
                    selection,
                    status,
                    bundle,
                } => {
                    if self.profile.load(&id, selection, bundle) {
                        self.selecting = false;
                        self.profile_save_error = None;
                        self.error.clear();
                        if let Some(pack) = self.packs.iter_mut().find(|pack| pack["id"] == id)
                            && pack["importUpdatePending"] == true
                        {
                            pack["importUpdatePending"] = Value::Bool(false);
                        }
                        self.sync(status);
                    }
                }
                Event::PackLoaded {
                    id,
                    selection,
                    bundle,
                } => {
                    if self.profile.reload(&id, selection, bundle) {
                        self.selecting = false;
                        if self.profile_recovery.as_deref() == Some(&id)
                            && !self.profile_recovery_pending
                        {
                            self.profile_recovery = None;
                        }
                        if !self.profile.dirty() {
                            self.profile_save_error = None;
                        }
                    }
                }
                Event::PackRecovered { id, result } => {
                    self.profile_recovery_pending = false;
                    match result {
                        Ok((bundle, recovered)) => {
                            if self.profile.selected == id
                                && !self.profile.dirty()
                                && !self.profile.writing()
                            {
                                self.profile.load(&id, self.profile.selection, bundle);
                            }
                            self.profile_recovery = None;
                            self.error.clear();
                            self.message = if recovered {
                                "已备份损坏的参数并恢复默认设置"
                            } else {
                                "已重新读取壁纸参数"
                            }
                            .into();
                        }
                        Err(error) => self.error = format!("无法恢复壁纸参数，{error}"),
                    }
                }
                Event::PackSaved {
                    id,
                    selection,
                    edit,
                    bundle,
                } => {
                    if self.profile.ack(&id, selection, edit, bundle) {
                        self.profile_save_error = None;
                    }
                }
                Event::PackFailed {
                    id,
                    selection,
                    edit,
                    error,
                } => {
                    if id == self.profile.selected && selection == self.profile.selection {
                        if let Some(edit) = edit {
                            if self.profile.fail(&id, selection, edit) {
                                self.profile_write_failed(error);
                                if self.profile.dirty() && self.status["running"] == true {
                                    self.open_profile(id, false);
                                }
                            }
                        } else {
                            self.selecting = false;
                            if core::damaged_profile_error(&error) {
                                self.profile_recovery = Some(id.clone());
                            }
                            self.error = error;
                            if let Some(active) = self.status["packId"].as_str().map(str::to_owned)
                            {
                                if active != self.profile.selected && !self.profile.dirty() {
                                    self.open_profile(active, false);
                                }
                            }
                        }
                    }
                }
                Event::Error(error) | Event::RequestFailed(error) => self.error = error,
                Event::SetupFinished(result) => {
                    self.selecting = false;
                    if result.is_ok() {
                        self.setup_complete = true;
                        self.preferences.setup_complete = true;
                    }
                    self.error = result.err().unwrap_or_default();
                }
                Event::MigrationFinished(result) => {
                    self.migrating = false;
                    self.error = result.err().unwrap_or_default();
                }
                Event::PackDeleted { id, result } => {
                    self.deleting = false;
                    match result {
                        Ok(()) => {
                            self.message = "壁纸已移入回收站".into();
                            self.previews.forget(&id);
                            if self.profile.selected == id {
                                self.profile.select(String::new());
                                self.pending = None;
                                self.preview_in_flight = false;
                            }
                        }
                        Err(error) => self.error = error,
                    }
                    self.request_catalog(true);
                }
                Event::Saved(message) => self.message = message,
                Event::InterfaceSaveFailed(error) => self.error = error,
                Event::Startup(result) => {
                    self.startup_pending = false;
                    match result {
                        Ok(state) => self.startup_state = Some(state),
                        Err(error) => self.error = error,
                    }
                }
                Event::GraphicsAdapters(result) => self.graphics_adapters = Some(result),
                Event::GpuPreferenceSaved(result) => {
                    self.gpu_save_pending = false;
                    match result {
                        Ok(gpu) => self.preferences.gpu_preference = gpu,
                        Err(error) => self.error = format!("无法保存显卡设置，{error}"),
                    }
                }
                Event::RestartPrepared { output_mode, result } => match result {
                    Ok(()) => {
                        self.restart_on_exit.set(
                            output_mode.map_or(Restart::Graphics, Restart::OutputMode),
                        );
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(error) => {
                        self.restart_pending = false;
                        self.switching_display = false;
                        self.error = if output_mode.is_some() {
                            format!("无法准备切换 Action75 输出模式，{error}")
                        } else {
                            format!("无法重启软件，{error}")
                        };
                    }
                },
                Event::SettingsAck { .. } | Event::SettingsFailed { .. } => {}
            }
        }
    }
    fn changed(&mut self, ctx: &egui::Context, fields: Vec<String>) {
        self.profile.changed(fields);
        self.pending = Some(Instant::now());
        ctx.request_repaint_after(Duration::from_millis(50));
    }
    fn preview_pack(&mut self, values: Map<String, Value>) {
        self.last_preview = Instant::now();
        self.preview_in_flight = self
            .tx
            .send(Job::PreviewPack {
                id: self.profile.selected.clone(),
                selection: self.profile.selection,
                values,
            })
            .is_ok();
        if !self.preview_in_flight {
            self.error = "控制线程已退出".into();
        }
    }
    fn flush_settings(&mut self, ctx: &egui::Context) {
        let Some(time) = self.pending else {
            return;
        };
        if self.selecting || self.profile.writing() || self.profile_save_error.is_some() {
            return;
        }
        if time.elapsed() < Duration::from_millis(180) {
            if !self.preview_in_flight && self.last_preview.elapsed() >= Duration::from_millis(50) {
                let values = self.profile.patch();
                if !values.is_empty() {
                    self.preview_pack(values);
                }
            }
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        let values = self.profile.patch();
        if values.is_empty() && self.preview_in_flight {
            // The worker will request a repaint when the outstanding preview replies.
            return;
        }
        self.pending = None;
        if values.is_empty() {
            self.preview_pack(values);
            return;
        }
        self.write_profile_patch(values);
    }
    fn profile_write_failed(&mut self, error: String) {
        self.pending = None;
        self.queued_wallpaper = None;
        if self.profile.dirty() {
            self.profile_save_error = Some(error);
        } else {
            self.error = error;
        }
    }
    fn write_profile_patch(&mut self, values: Map<String, Value>) {
        self.pending = None;
        let revision = self
            .profile
            .bundle
            .as_ref()
            .map(|bundle| bundle.revision.clone());
        let edit = self.profile.begin_write();
        if self
            .tx
            .send(Job::WritePack {
                id: self.profile.selected.clone(),
                selection: self.profile.selection,
                edit,
                values,
                revision,
            })
            .is_err()
        {
            self.profile
                .fail(&self.profile.selected.clone(), self.profile.selection, edit);
            self.profile_write_failed("控制线程已退出".into());
        }
    }
    fn reset_profile(&mut self) {
        let Some(bundle) = self.profile.bundle.as_ref() else {
            return;
        };
        let revision = Some(bundle.revision.clone());
        self.profile.values = core::pack_model::defaults(&bundle.schema);
        self.pending = None;
        self.profile.changed([]);
        let edit = self.profile.begin_write();
        if self
            .tx
            .send(Job::ResetPack {
                id: self.profile.selected.clone(),
                selection: self.profile.selection,
                edit,
                revision,
            })
            .is_err()
        {
            self.profile
                .fail(&self.profile.selected.clone(), self.profile.selection, edit);
            self.profile_write_failed("控制线程已退出".into());
        }
    }
    fn profile_recovery_button(&mut self, ui: &mut egui::Ui) -> Option<egui::Response> {
        let id = self.profile_recovery.as_ref()?;
        let name = self
            .packs
            .iter()
            .find(|pack| pack["id"] == *id)
            .and_then(|pack| pack["name"].as_str())
            .unwrap_or(id);
        let label = if self.profile_recovery_pending {
            "正在恢复壁纸参数…".to_owned()
        } else {
            format!("备份并恢复「{name}」的默认参数")
        };
        let enabled = self.status["running"] == true
            && !self.selecting
            && !self.profile.writing()
            && !self.wallpaper_selection_blocked()
            && !self.restart_pending;
        let response = ui.add_enabled(enabled, egui::Button::new(label));
        if response.clicked() {
            self.profile_recovery_pending = self.tx.send(Job::RecoverPack(id.clone())).is_ok();
            if !self.profile_recovery_pending {
                self.error = "控制线程已退出".into();
            }
        }
        Some(response)
    }
    fn queue_finish(&self) -> mpsc::Receiver<Result<(), String>> {
        let (done, receiver) = mpsc::channel();
        let _ = self.tx.send(Job::FlushAndQuit {
            write: self.profile.finish(self.profile_save_error.is_none()),
            done,
        });
        receiver
    }
    fn top_bar(&mut self, ctx: &egui::Context) {
        let custom_frame = self.window_frame.is_some();
        egui::TopBottomPanel::top("chrome")
            .exact_height(50.)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 28,
                        right: if custom_frame { 10 } else { 28 },
                        top: 9,
                        bottom: 9,
                    }),
            )
            .show(ctx, |ui| {
                if self.restart_pending {
                    ui.disable();
                }
                let maximized = ui.input(|input| input.viewport().maximized.unwrap_or(false));
                let bar = ui.max_rect();
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(APP_NAME).size(13.).extra_letter_spacing(0.9));
                    let drag_left = ui.available_rect_before_wrap().left();
                    let drag_right = ui
                        .with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if custom_frame {
                                ui_chrome::window_buttons(ui, maximized);
                                ui.add_space(9.);
                            }
                            let current = self.preferences.interface.theme;
                            for theme in [InterfaceTheme::Graphite, InterfaceTheme::Paper] {
                                if ui_chrome::theme_key(ui, theme, current == theme).clicked()
                                    && current != theme
                                {
                                    self.preferences.interface.theme = theme;
                                    self.update_acrylic();
                                    self.remember_interface();
                                    ctx.request_repaint();
                                }
                            }
                            ui.label(
                                RichText::new(ui_chrome::theme_name(current))
                                    .size(12.)
                                    .weak(),
                            );
                            ui.available_rect_before_wrap().right()
                        })
                        .inner;
                    if let Some(frame) = &self.window_frame {
                        frame.set_caption(
                            egui::Rect::from_x_y_ranges(
                                drag_left..=drag_right,
                                bar.y_range().expand(9.),
                            ),
                            ctx.pixels_per_point(),
                        );
                    }
                });
            });
    }

    fn header(&mut self, ctx: &egui::Context) {
        let calibrating = self.calibration.has_session();
        if calibrating {
            self.tab = 1;
        }
        let height = ctx.content_rect().height();
        let compact = height < 760.;
        let item_height = ((height - 228.) / NAVIGATION.len() as f32).clamp(70., 111.);
        egui::SidePanel::left("navigation")
            .exact_width(122.)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 14)))
            .show(ctx, |ui| {
                if self.restart_pending {
                    ui.disable();
                }
                ui.add_space(if compact { 32. } else { 51. });
                ui.spacing_mut().item_spacing.y = if compact { 7. } else { 17. };
                for (tab, name, icon) in NAVIGATION {
                    let enabled =
                        (!calibrating || tab == 1) && self.compat.is_none() && !self.migrating;
                    if ui_chrome::navigation(ui, name, icon, self.tab == tab, enabled, item_height)
                        .clicked()
                    {
                        self.tab = tab;
                        if tab == 3 {
                            self.startup_checked = None;
                            self.graphics_scan_needed = true;
                        }
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        let online = self.status["running"] == true;
                        let (dot, _) = ui.allocate_exact_size(vec2(6., 10.), Sense::hover());
                        let color = ui.visuals().weak_text_color();
                        if online {
                            ui.painter().circle_filled(dot.center(), 2.6, color);
                        } else {
                            ui.painter().circle_stroke(
                                dot.center(),
                                2.6,
                                Stroke::new(1_f32, color),
                            );
                        }
                        ui.label(
                            RichText::new(if online { "运行中" } else { "未连接" })
                                .size(11.)
                                .weak(),
                        );
                        if ui_chrome::icon_button(ui, Icon::Refresh, "刷新设备与壁纸", 19.)
                            .clicked()
                        {
                            self.request_catalog(false);
                        }
                    });
                });
            });
    }

    fn update_acrylic(&mut self) {
        let acrylic =
            self.acrylic_available && self.preferences.interface.theme == InterfaceTheme::Graphite;
        if acrylic == self.acrylic {
            return;
        }
        let Some(frame) = self.window_frame.as_ref() else {
            return;
        };
        if acrylic {
            match frame.enable_acrylic() {
                Ok(provider) => {
                    self.acrylic = true;
                    core::storage::log(
                        &self.root,
                        "INFO",
                        &format!("已启用 Desktop Acrylic（{provider}）"),
                    );
                }
                Err(error) => core::storage::log(&self.root, "WARN", &error),
            }
        } else {
            frame.disable_acrylic();
            self.acrylic = false;
            core::storage::log(&self.root, "INFO", "白色主题使用实色背景");
        }
    }

    fn remember_interface(&mut self) {
        self.preferences.interface.panel_open = Some(self.panel_open);
        self.preferences.interface.panel_width = Some(self.panel_width);
        if self
            .tx
            .send(Job::SaveInterface(self.preferences.interface.clone()))
            .is_err()
        {
            self.error = "无法保存界面设置".into();
        }
    }

    fn controls(&mut self, ctx: &egui::Context) {
        let opened = self.tab == 0 && self.panel_open;
        let progress = ui_style::animate(
            ctx,
            egui::Id::new("inspector-reveal"),
            opened,
            ui_style::PANEL,
        );
        // Keep the reveal advancing through rapid switches, even when hidden.
        if progress <= 0. || matches!(self.tab, 2 | 3 | PERFORMANCE_TAB) {
            return;
        }
        let available = ctx.available_rect();
        let max_width = (available.width() - 470.).clamp(224., 400.);
        let width = self.panel_width.min(max_width);
        let previous_open = self.panel_open;
        let mut width_committed = false;
        egui::SidePanel::right("inspector")
            .exact_width((width * progress).max(1.))
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ctx, |host| {
                let visible = host.max_rect();
                let x = visible.left() + 4.;
                let y = visible.center().y;
                let grip = egui::Rect::from_center_size(
                    egui::pos2(x, y),
                    vec2(14., (visible.height() - 100.).max(46.)),
                );
                let handle = host.interact(grip, host.id().with("width"), Sense::drag());
                if opened && progress >= 0.999 {
                    if handle.dragged() {
                        self.panel_width = (width - handle.drag_delta().x).clamp(224., max_width);
                    }
                    width_committed = handle.drag_stopped();
                    if handle.hovered() || handle.dragged() {
                        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                }
                let track = host.visuals().weak_text_color();
                host.painter().vline(
                    x,
                    grip.y_range(),
                    Stroke::new(1_f32, track.gamma_multiply(0.13 * progress)),
                );
                host.painter().rect_filled(
                    egui::Rect::from_center_size(egui::pos2(x, y), vec2(2., 31.)),
                    1,
                    track.gamma_multiply(
                        if handle.hovered() || handle.dragged() {
                            0.75
                        } else {
                            0.31
                        } * progress,
                    ),
                );

                let body = egui::Rect::from_min_max(
                    visible.min
                        + vec2(
                            27.,
                            if ctx.content_rect().height() < 760. {
                                10.
                            } else {
                                26.
                            },
                        ),
                    egui::pos2(visible.left() + width - 25., visible.bottom() - 19.),
                );
                let mut ui = host.new_child(
                    egui::UiBuilder::new()
                        .id_salt("inspector-body")
                        .max_rect(body),
                );
                ui.set_clip_rect(visible.intersect(host.clip_rect()));
                ui.set_width(body.width());
                ui.multiply_opacity(progress);
                if !opened
                    || self.calibration.has_session()
                    || self.compat.is_some()
                    || self.migrating
                {
                    ui.disable();
                }
                self.inspector_body(&mut ui, body);
            });
        if previous_open != self.panel_open || width_committed {
            self.remember_interface();
        }
    }

    fn inspector_body(&mut self, ui: &mut egui::Ui, body: egui::Rect) {
        let ctx = ui.ctx().clone();
        let id = if self.profile.bundle.is_some() {
            &self.profile.selected
        } else {
            self.status["packId"]
                .as_str()
                .unwrap_or(&self.profile.selected)
        };
        let changed = self.shown_pack != id;
        if changed {
            id.clone_into(&mut self.shown_pack);
        }
        let reveal = self.pack_reveal.opacity(&ctx, changed);
        let name = self
            .packs
            .iter()
            .find(|pack| pack["id"] == id)
            .and_then(|pack| pack["name"].as_str())
            .unwrap_or("壁纸参数");
        let name = name.split(" · ").next().unwrap_or(name);
        ui.horizontal(|ui| {
            ui.set_height(38.);
            ui.scope(|ui| {
                ui.multiply_opacity(reveal);
                ui.add(
                    egui::Label::new(RichText::new(name).font(ui_style::display_font(27.)))
                        .truncate(),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui_chrome::icon_button(ui, Icon::ClosePanel, "收起参数", 26.).clicked() {
                    self.panel_open = false;
                }
            });
        });
        ui.add_space(if ctx.content_rect().height() < 760. {
            11.
        } else {
            20.
        });
        let online = self.status["running"] == true;
        let paused = self.status["paused"] == true;
        let footer =
            egui::Rect::from_min_max(egui::pos2(body.left(), body.bottom() - 29.), body.max);
        let mut footer_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("playback-footer")
                .max_rect(footer),
        );
        footer_ui.horizontal(|ui| {
            if online {
                if ui
                    .add(
                        egui::Button::new(if paused {
                            "继续播放"
                        } else {
                            "暂停播放"
                        })
                        .frame(false),
                    )
                    .clicked()
                {
                    self.send(json!({"action": if paused { "resume" } else { "pause" }}));
                }
            } else if ui.button("启动壁纸").clicked() {
                self.request_player_start();
            }
        });
        let height = (footer.top() - ui.cursor().top() - 12.).max(60.);
        egui::ScrollArea::vertical()
            .id_salt("controls-scroll")
            .max_height(height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let viewport = ui.available_rect_before_wrap();
                let scroll = ui.spacing().scroll;
                // The floating bar expands over the viewport on hover.
                let overlay = if scroll.floating {
                    scroll.bar_width + scroll.bar_outer_margin
                } else {
                    0.
                };
                let content_right = viewport.right() - overlay;
                ui.set_max_width((content_right - viewport.left() - 8.).max(0.));
                let clip = ui.clip_rect();
                ui.set_clip_rect(clip.with_max_x(clip.right().min(content_right)));
                ui.spacing_mut().item_spacing.y = 6.;
                ui.multiply_opacity(reveal);
                if online && self.selecting && !self.migrating && self.profile.bundle.is_some() {
                    // Keep the cached controls visible while a settings refresh locks input.
                    ui.visuals_mut().disabled_alpha = 1.;
                }
                ui.add_enabled_ui(online && !self.selecting && !self.migrating, |ui| {
                    if let Some(bundle) = self.profile.bundle.as_ref() {
                        let outcome = ui
                            .push_id(("pack-settings", &self.profile.selected), |ui| {
                                pack_ui::show(
                                    ui,
                                    &bundle.schema,
                                    &mut self.profile.values,
                                    &self.palettes,
                                )
                            })
                            .inner;
                        if !outcome.changed_fields.is_empty() {
                            self.changed(&ctx, outcome.changed_fields);
                        }
                        ui.add_space(8.);
                        if ui
                            .add_enabled(
                                !self.profile.writing(),
                                egui::Button::new("恢复此壁纸默认设置"),
                            )
                            .clicked()
                        {
                            self.reset_profile();
                        }
                    }
                });
            });
    }

    fn filter_wallpapers(&mut self) {
        self.selection_frame.reset();
        let query = self.search.trim().to_lowercase();
        self.visible_packs.clear();
        self.visible_packs
            .extend(self.packs.iter().enumerate().filter_map(|(i, pack)| {
                let name = pack["name"].as_str().unwrap_or_default();
                (self.gallery_category.matches(pack)
                    && (query.is_empty() || name.to_lowercase().contains(&query)))
                .then_some(i)
            }));
    }

    fn move_gallery_pack(&mut self, source: &str, target: &str, after: bool) {
        let index = |id: &str| self.packs.iter().position(|pack| pack["id"] == id);
        let (Some(from), Some(to)) = (index(source), index(target)) else {
            return;
        };
        if from == to {
            return;
        }
        let insert = to + usize::from(after) - usize::from(from < to);
        if insert == from {
            return;
        }
        let pack = self.packs.remove(from);
        self.packs.insert(insert, pack);
        let mut reordered = self.packs.iter().filter_map(|p| p["id"].as_str());
        let present: std::collections::HashSet<_> = reordered.clone().collect();
        let order = &mut self.preferences.interface.wallpaper_order;
        // Replace the slots occupied by available wallpapers and keep absent IDs in place.
        order.retain_mut(|id| {
            if !present.contains(id.as_str()) {
                return true;
            }
            let Some(next) = reordered.next() else {
                return false;
            };
            next.clone_into(id);
            true
        });
        order.extend(reordered.map(str::to_owned));
        self.filter_wallpapers();
        self.remember_interface();
    }

    fn wallpaper_selection_blocked(&self) -> bool {
        self.deleting
            || self.migrating
            || self.calibration.has_session()
            || self.compat.is_some()
            || self.profile_recovery_pending
    }

    fn dispatch_wallpaper_selection(&mut self) {
        let Some(id) = self.queued_wallpaper.as_ref() else {
            return;
        };
        if self.status["running"] != true
            || self.wallpaper_selection_blocked()
            || self.profile_save_error.is_some()
        {
            self.queued_wallpaper = None;
            return;
        }
        if self.selecting || self.profile.writing() {
            return;
        }
        if self.status["packId"] == *id
            && !self
                .packs
                .iter()
                .any(|pack| pack["id"] == *id && pack["importUpdatePending"] == true)
        {
            self.queued_wallpaper = None;
            return;
        }
        if self.profile.dirty() {
            self.write_profile_patch(self.profile.patch());
            return;
        }
        let id = self.queued_wallpaper.take().unwrap();
        self.open_profile(id, true);
    }

    fn gallery_tools(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if !self.panel_open
                && ui_chrome::icon_button(ui, Icon::OpenPanel, "展开参数", 26.).clicked()
            {
                self.panel_open = true;
                self.remember_interface();
            }
            ui.scope(|ui| {
                ui.visuals_mut().hyperlink_color = ui.visuals().weak_text_color();
                ui.hyperlink_to(
                    RichText::new("获取壁纸 ↗").size(13.),
                    "https://github.com/blackwhite-poi/Action75-Wallpaper-Library",
                )
                .on_hover_text("在浏览器中打开壁纸仓库");
            });
            if self.wallpaper_import.button(
                ui,
                self.setup_complete
                    && !self.deleting
                    && !self.migrating
                    && self.compat.is_none()
                    && self.folder_request.is_none()
                    && !self.restart_pending,
                self.administrator,
            ) {
                self.ordinary_prompt = Some(OrdinaryAction::Import);
            }
        });
    }

    fn gallery(&mut self, ui: &mut egui::Ui) {
        let compact = ui.ctx().content_rect().height() < 760.;
        let stack_tools = ui.available_width() < 525.;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("壁纸库").font(ui_style::display_font(if compact {
                    33.
                } else {
                    36.
                })),
            );
            ui.label(
                RichText::new(format!("{} 款", self.visible_packs.len()))
                    .size(13.)
                    .weak(),
            );
            if self.selecting && self.profile.bundle.is_none() {
                ui.spinner();
            }
            if !stack_tools {
                self.gallery_tools(ui);
            }
        });
        if stack_tools {
            ui.horizontal(|ui| self.gallery_tools(ui));
        }
        ui.horizontal_wrapped(|ui| {
            for (category, label) in [
                (GalleryCategory::All, "全部"),
                (GalleryCategory::Interactive, "互动壁纸"),
                (GalleryCategory::Image, "图片"),
                (GalleryCategory::Video, "视频"),
            ] {
                let selected = self.gallery_category == category;
                let response = ui.add(egui::Button::new(label).frame_when_inactive(false));
                if selected {
                    let rect = response
                        .rect
                        .shrink2(vec2(ui.spacing().button_padding.x, 0.));
                    ui.painter().hline(
                        rect.x_range(),
                        rect.bottom(),
                        egui::Stroke::new(1.5_f32, ui.visuals().text_color()),
                    );
                }
                if response.clicked() && !selected {
                    self.gallery_category = category;
                    self.filter_wallpapers();
                }
            }
            let width = ui.available_size_before_wrap().x.max(174.);
            let changed = ui
                .allocate_ui_with_layout(
                    vec2(width, 27.),
                    egui::Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.allocate_ui_with_layout(
                            vec2(171., 27.),
                            egui::Layout::left_to_right(Align::Center),
                            |ui| ui_chrome::search(ui, &mut self.search),
                        )
                        .inner
                        .changed()
                    },
                )
                .inner;
            if changed {
                self.filter_wallpapers();
            }
        });
        self.wallpaper_import.status(ui);
        if let Some(error) = &self.catalog_error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "壁纸列表读取失败，已显示本地目录",
            )
            .on_hover_text(error);
        }
        ui.add_space(if compact { 6. } else { 10. });
        if self.visible_packs.is_empty() {
            ui.label(if self.catalog_loading {
                "正在加载…"
            } else {
                "没有匹配的壁纸"
            });
            return;
        }
        let gap = if ui.available_width() < 650. {
            18.
        } else {
            22.
        };
        let columns = if ui.available_width() < 536. { 2 } else { 3 };
        const INSET: f32 = 7.;
        let width = ((ui.available_width() - gap * (columns - 1) as f32 - 2. * INSET - 6.)
            / columns as f32)
            .floor();
        let image_height = width * 0.375;
        let row_height = image_height + if compact { 38. } else { 43. };
        let rows = self.visible_packs.len().div_ceil(columns);
        let step = vec2(width + gap, row_height + ui.spacing().item_spacing.y);
        let running = self.status["running"] == true;
        let blocked = self.wallpaper_selection_blocked();
        let playing = self.status["packId"].as_str().unwrap_or_default();
        let selected = self
            .queued_wallpaper
            .as_deref()
            .or_else(|| (running && self.selecting).then_some(self.profile.selected.as_str()))
            .filter(|id| !id.is_empty())
            .unwrap_or(playing);
        let mut selection = None;
        struct DraggedWallpaper {
            id: String,
            name: String,
            image: Option<egui::TextureHandle>,
        }
        let mut reorder = None;
        let mut open_folder = None;
        let allow_delete =
            !blocked && !self.selecting && !self.wallpaper_import.busy() && !self.restart_pending;
        let mut scroll =
            egui::ScrollArea::vertical().id_salt(("wallpaper-gallery", self.gallery_category));
        if !self.catalog_loading
            && let Some(id) = &self.wallpaper_import.reveal
            && let Some(index) = self
                .visible_packs
                .iter()
                .position(|&i| self.packs[i]["id"] == *id)
        {
            scroll = scroll.vertical_scroll_offset((index / columns) as f32 * step.y);
            self.wallpaper_import.reveal = None;
        }
        scroll
            .auto_shrink([false, false])
            .show_rows(ui, row_height, rows, |ui, range| {
                if egui::DragAndDrop::has_payload_of_type::<DraggedWallpaper>(ui.ctx())
                    && let Some(pos) = ui.ctx().pointer_interact_pos()
                    && ui.clip_rect().contains(pos)
                {
                    let clip = ui.clip_rect();
                    let speed = (28. - (pos.y - clip.top())).max(0.)
                        - (28. - (clip.bottom() - pos.y)).max(0.);
                    if speed != 0. {
                        ui.scroll_with_delta(vec2(
                            0.,
                            speed * 16. * ui.input(|i| i.stable_dt.min(0.05)),
                        ));
                        ui.ctx().request_repaint();
                    }
                }
                let end = (range.end * columns).min(self.visible_packs.len());
                let start = (range.start * columns).min(end);
                self.previews.prepare_visible(
                    ui.ctx(),
                    self.visible_packs[start..end]
                        .iter()
                        .map(|&index| &self.packs[index]),
                );
                // Reserve the glass behind every visible tile, so thumbnails stay sharp.
                let painter = ui.painter().clone();
                let slot = painter.add(egui::Shape::Noop);
                let mut target = None;
                let mut clicked_target = None;
                for row in range {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap - 2. * INSET;
                        let start = row * columns;
                        let end = (start + columns).min(self.visible_packs.len());
                        for (column, &index) in self.visible_packs[start..end].iter().enumerate() {
                            let pack = &self.packs[index];
                            let id = pack["id"].as_str().unwrap_or_default();
                            let active = !playing.is_empty() && playing == id;
                            let is_selected = !selected.is_empty() && selected == id;
                            let name = pack["name"].as_str().unwrap_or(id);
                            let name = name.split(" · ").next().unwrap_or(name);
                            let (rect, response) = ui.allocate_exact_size(
                                vec2(width + 2. * INSET, row_height),
                                Sense::click_and_drag(),
                            );
                            if !blocked {
                                if response.drag_started_by(egui::PointerButton::Primary) {
                                    response.dnd_set_drag_payload(DraggedWallpaper {
                                        id: id.to_owned(),
                                        name: name.to_owned(),
                                        image: self
                                            .previews
                                            .get(id)
                                            .map(|preview| preview.image.clone()),
                                    });
                                }
                                if let Some(source) =
                                    response.dnd_hover_payload::<DraggedWallpaper>()
                                    && source.id != id
                                    && let Some(pos) = ui.ctx().pointer_interact_pos()
                                {
                                    let after = pos.x > rect.center().x;
                                    let x = if after { rect.right() } else { rect.left() };
                                    ui.painter().vline(
                                        x,
                                        rect.y_range(),
                                        Stroke::new(2_f32, ui.visuals().text_color()),
                                    );
                                    if response.dnd_release_payload::<DraggedWallpaper>().is_some()
                                    {
                                        reorder = Some((source.id.clone(), id.to_owned(), after));
                                    }
                                }
                            }
                            if let Some(path) = pack["path"].as_str() {
                                response.context_menu(|ui| {
                                    if ui.button("打开壁纸文件夹").clicked() {
                                        open_folder = Some(PathBuf::from(path));
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui
                                        .add_enabled(allow_delete, egui::Button::new("删除壁纸…"))
                                        .clicked()
                                    {
                                        self.delete_confirmation =
                                            Some((id.to_owned(), name.to_owned(), path.into()));
                                        ui.close();
                                    }
                                });
                            }
                            let clicked = response.clicked()
                                && !blocked
                                && !id.is_empty()
                                && (!is_selected || pack["importUpdatePending"] == true);
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::SelectableLabel,
                                    ui.is_enabled(),
                                    is_selected || (running && clicked),
                                    name,
                                )
                            });
                            let image = egui::Rect::from_min_size(
                                rect.min + egui::Vec2::splat(INSET),
                                vec2(width, image_height),
                            );
                            let preview = self.previews.get(id);
                            if let Some(preview) = preview {
                                egui::Image::new((preview.image.id(), image.size()))
                                    .corner_radius(3)
                                    .paint_at(ui, image);
                            } else {
                                ui.painter()
                                    .rect_filled(image, 3, ui.visuals().faint_bg_color);
                            }
                            let hover = ui_style::animate(
                                ui.ctx(),
                                ui.id().with((id, "hover")),
                                response.hovered(),
                                ui_style::FEEDBACK,
                            );
                            if hover > 0. {
                                ui.painter().rect_filled(
                                    image,
                                    3,
                                    Color32::from_white_alpha((18. * hover) as u8),
                                );
                            }
                            if response.has_focus() {
                                ui.painter().rect_stroke(
                                    image.expand(2.),
                                    5,
                                    Stroke::new(1_f32, ui.visuals().text_color()),
                                    egui::StrokeKind::Outside,
                                );
                            }
                            let marker = ui_style::animate(
                                ui.ctx(),
                                ui.id().with((id, "playing")),
                                active,
                                ui_style::FEEDBACK,
                            );
                            let caption = image.left_bottom() + vec2(0., 10.);
                            if marker > 0. {
                                ui.painter().circle_filled(
                                    caption + vec2(2.5, 7.),
                                    2.5 * marker,
                                    ui.visuals().text_color(),
                                );
                            }
                            ui.painter().text(
                                caption + vec2(10. * marker, 0.),
                                egui::Align2::LEFT_TOP,
                                name,
                                FontId::proportional(13.),
                                ui.visuals().text_color(),
                            );
                            if response.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if is_selected || (running && clicked) {
                                let tile = gallery_selection::Tile {
                                    rect: egui::Rect::from_min_max(
                                        rect.min,
                                        egui::pos2(rect.right(), image.bottom() + 30.),
                                    ),
                                    cell: vec2(column as f32, row as f32),
                                    frost: preview.map(|p| p.frost.clone()),
                                };
                                if clicked {
                                    clicked_target = Some(tile);
                                } else {
                                    target = Some(tile);
                                }
                            }
                            if clicked {
                                selection = Some(id.to_owned());
                            }
                        }
                    });
                }
                self.selection_frame.paint(
                    &painter,
                    slot,
                    clicked_target.or(target),
                    columns,
                    step,
                );
            });
        if let Some(drag) = egui::DragAndDrop::payload::<DraggedWallpaper>(ui.ctx())
            && ui.input(|input| input.pointer.primary_down())
            && let Some(pointer) = ui.ctx().pointer_interact_pos()
        {
            let screen = ui.ctx().content_rect();
            let size = vec2(152., 82.);
            let mut origin = pointer + vec2(14., 14.);
            if origin.x + size.x > screen.right() {
                origin.x = pointer.x - size.x - 14.;
            }
            if origin.y + size.y > screen.bottom() {
                origin.y = pointer.y - size.y - 14.;
            }
            let card = egui::Rect::from_min_size(origin, size);
            let mut painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                ui.id().with("dragged-wallpaper"),
            ));
            painter.set_opacity(0.86);
            painter.rect(
                card,
                5.,
                ui.visuals().window_fill(),
                ui.visuals().window_stroke(),
                egui::StrokeKind::Inside,
            );
            let cover = egui::Rect::from_min_size(origin + vec2(4., 4.), vec2(144., 54.));
            if let Some(image) = &drag.image {
                painter.image(
                    image.id(),
                    cover,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                    Color32::WHITE,
                );
            } else {
                painter.rect_filled(cover, 3., ui.visuals().faint_bg_color);
            }
            painter.with_clip_rect(card.shrink(4.)).text(
                origin + vec2(6., 62.),
                egui::Align2::LEFT_TOP,
                &drag.name,
                FontId::proportional(13.),
                ui.visuals().text_color(),
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if let Some((source, target, after)) = reorder {
            self.move_gallery_pack(&source, &target, after);
        }
        if let Some(path) = open_folder {
            self.open_directory(path);
        }
        if let Some(id) = selection {
            if running {
                self.queued_wallpaper = Some(id);
                self.dispatch_wallpaper_selection();
            } else {
                self.error = "请先启动壁纸".into();
            }
        }
    }

    fn confirm_delete(&mut self, ctx: &egui::Context) {
        let Some((id, name, path)) = &self.delete_confirmation else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("delete-wallpaper")).show(ctx, |ui| {
            ui.set_width(365.);
            ui.heading(format!("删除「{name}」？"));
            ui.label("以下壁纸文件夹将移入回收站。");
            ui.weak(path.display().to_string());
            if self.status["packId"] == id.as_str() {
                ui.label("这款壁纸正在播放，删除后将停止播放。");
            }
            ui.add_space(8.);
            ui.horizontal(|ui| {
                (
                    ui.button("取消").clicked(),
                    ui.button("移入回收站").clicked(),
                )
            })
            .inner
        });
        if modal.inner.1 {
            let (id, _, path) = self.delete_confirmation.take().unwrap();
            self.deleting = self.tx.send(Job::DeletePack { id, path }).is_ok();
        } else if modal.inner.0 || modal.should_close() {
            self.delete_confirmation = None;
        }
    }

    fn request_folder(
        &mut self,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        field: DirectoryField,
    ) {
        let (title, initial) = match field {
            DirectoryField::Wallpapers => ("选择壁纸目录", self.preferences.wallpapers(&self.root)),
            DirectoryField::Logs => ("选择日志目录", self.preferences.logs(&self.root)),
        };
        match folder_picker::open(frame, ctx, initial, title) {
            Ok(receive) => self.folder_request = Some((field, receive)),
            Err(error) => self.error = format!("无法打开文件夹选择窗口，{error}"),
        }
    }

    fn poll_folder(&mut self) {
        let Some((field, receive)) = &self.folder_request else {
            return;
        };
        let field = *field;
        let reply = match receive.try_recv() {
            Ok(reply) => reply,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("文件夹选择已中断".into()),
        };
        self.folder_request = None;
        match reply {
            Ok(Some(path)) => match field {
                DirectoryField::Wallpapers => self.preferences.wallpaper_dir = path,
                DirectoryField::Logs => self.preferences.log_dir = path,
            },
            Ok(None) => {}
            Err(error) => self.error = format!("无法选择文件夹，{error}"),
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        if std::mem::take(&mut self.graphics_scan_needed)
            && self.tx.send(Job::DetectGraphicsAdapters).is_err()
        {
            self.graphics_adapters = Some(Err("无法开始检测显卡".into()));
        }
        if self.migrating
            || self.folder_request.is_some()
            || self.restart_pending
            || self.wallpaper_import.busy()
        {
            ui.disable();
        }
        let width = ui.available_width().min(SETTINGS_FORM_WIDTH);
        ui.allocate_ui_with_layout(vec2(width, 0.), egui::Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            ui.add_space(10.);
            ui.label(RichText::new("设置").font(ui_style::display_font(32.)));
            ui.add_space(14.);

            settings_section(ui, "启动", |ui| self.startup_setting(ui));
            ui.add_space(SETTINGS_SECTION_GAP);
            settings_section(ui, "图形", |ui| self.graphics_setting(ui));
            ui.add_space(SETTINGS_SECTION_GAP);
            settings_section(ui, "目录", |ui| {
                self.directory_setting(ui, frame, DirectoryField::Wallpapers);
                self.directory_setting(ui, frame, DirectoryField::Logs);
                self.directory_actions(ui);
            });
            ui.add_space(SETTINGS_SECTION_GAP);
            settings_section(ui, "诊断", |ui| self.diagnostics_setting(ui));
        });
    }

    fn directory_actions(&mut self, ui: &mut egui::Ui) {
        settings_detail(ui, |ui| {
            if self.migrating {
                ui.spinner();
                ui.label("正在迁移…");
            } else if ui.button("保存并迁移").clicked() {
                self.error.clear();
                self.migrating = self.tx.send(Job::Migrate(self.preferences.clone())).is_ok();
                if !self.migrating {
                    self.error = "无法开始目录迁移".into();
                }
            }
            ui.label(RichText::new("日志每份 10 MB，保留 10 份").size(12.).weak());
        });
    }

    fn can_install_update(&self) -> bool {
        !self.wallpaper_selection_blocked()
            && !self.wallpaper_import.busy()
            && !self.restart_pending
            && !self.gpu_save_pending
            && !self.startup_pending
            && !self.selecting
            && self.queued_wallpaper.is_none()
            && self.exclusive.pending.is_none()
            && self.pending.is_none()
            && !self.profile.writing()
            && !self.profile.dirty()
            && self.folder_request.is_none()
    }

    fn confirm_ordinary_mode(&mut self, ctx: &egui::Context) {
        let Some(action) = self.ordinary_prompt else {
            return;
        };
        let text = match action {
            OrdinaryAction::Import => "管理员模式下不允许导入壁纸",
            OrdinaryAction::Update => "管理员模式下不允许安装更新",
        };
        let ready = self.can_install_update();
        let frame = egui::Frame::popup(&ctx.style()).inner_margin(20);
        let modal = egui::Modal::new("ordinary-mode".into())
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(400.);
                ui.horizontal_top(|ui| {
                    let (icon, _) = ui.allocate_exact_size(vec2(26., 26.), Sense::hover());
                    ui_chrome::paint_icon(
                        ui, Icon::Shield, icon.center(), 24., ui.visuals().weak_text_color(),
                    );
                    ui.vertical(|ui| {
                        ui.label(RichText::new(text).font(ui_style::display_font(20.)));
                        ui.add_space(3.);
                        ui.weak("将以普通权限重开控制台，当前壁纸会短暂重载。");
                    });
                });
                ui.add_space(14.);
                ui.separator();
                ui.add_space(8.);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().button_padding = vec2(11., 6.);
                    let confirm = egui::Button::new("关闭管理员模式")
                        .min_size(vec2(130., 34.))
                        .fill(ui.visuals().selection.bg_fill);
                    let confirm = ui.add_enabled(ready, confirm).clicked();
                    let cancel = ui
                        .add(egui::Button::new("取消").min_size(vec2(68., 34.)))
                        .clicked();
                    (confirm, cancel)
                })
                .inner
            });
        let (confirm, cancel) = modal.inner;
        if confirm {
            self.ordinary_prompt = None;
            self.restart_on_exit.set(Restart::Ordinary(action));
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if cancel || modal.should_close() {
            self.ordinary_prompt = None;
        }
    }

    fn diagnostics_setting(&mut self, ui: &mut egui::Ui) {
        setting_row(ui, "完全退出快捷键", |ui, _| {
            ui.label("Ctrl + Alt + Shift + Q");
            if !single_instance::owns_exit_shortcut()
                && self.status["exitShortcutRegistered"] != true
            {
                ui.weak("快捷键已被占用");
            }
        });
        setting_row(ui, "软件版本", |ui, _| {
            ui.weak(format!("{APP_NAME} {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(8.);
            if ui.button("复制诊断信息").clicked() {
                let error = [
                    self.error.as_str(),
                    self.connection_error.as_str(),
                    self.status["error"].as_str().unwrap_or_default(),
                    self.status["displayGuardError"]
                        .as_str()
                        .unwrap_or_default(),
                ]
                .into_iter()
                .find(|text| !text.is_empty())
                .unwrap_or_default();
                ui.ctx().copy_text(diagnostics::report(
                    &self.controller_renderer,
                    &self.status,
                    error,
                ));
                self.diagnostics_copied = Some(Instant::now());
                ui.ctx().request_repaint_after(Duration::from_secs(2));
            }
            if self
                .diagnostics_copied
                .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
            {
                ui.weak("已复制");
            }
        });
        let ready = self.can_install_update();
        setting_row(ui, "版本更新", |ui, _| {
            if self.updates.row(ui, ready) {
                self.ordinary_prompt = Some(OrdinaryAction::Update);
            }
        });
    }

    fn startup_setting(&mut self, ui: &mut egui::Ui) {
        let focused = ui.input(|input| input.events.contains(&egui::Event::WindowFocused(true)));
        if !self.startup_pending
            && (focused
                || self
                    .startup_checked
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(3)))
        {
            self.request_startup(StartupAction::Inspect);
        }
        let mut enabled = self.startup_state.is_some_and(|state| state.enabled);
        let legacy = self
            .startup_state
            .map_or(LegacyStartup::None, |state| state.legacy);
        let can_change = self.startup_state.is_some()
            && !self.startup_pending
            && (enabled || legacy != LegacyStartup::Active);
        let changed = setting_row(ui, "开机运行", |ui, caption| {
            ui.add_enabled(can_change, egui::Checkbox::without_text(&mut enabled))
                .labelled_by(caption)
                .changed()
        });
        if changed {
            self.error.clear();
            self.request_startup(StartupAction::Set(enabled));
        }
        match legacy {
            LegacyStartup::Active => {
                setting_row(ui, "旧试用版", |ui, _| {
                    ui.label("登录任务已启用。迁移后，新版以普通权限启动。");
                    if ui
                        .add_enabled(
                            !self.startup_pending,
                            egui::Button::new("停用旧任务并开启新版"),
                        )
                        .clicked()
                    {
                        self.error.clear();
                        self.request_startup(StartupAction::MigrateTrial);
                    }
                });
            }
            LegacyStartup::Unidentified => {
                ui.weak("有同名登录任务，无法确认归属。");
            }
            LegacyStartup::Unavailable => {
                ui.weak("无法检查旧试用版登录任务。");
            }
            LegacyStartup::None => {}
        }
    }

    fn request_startup(&mut self, action: StartupAction) {
        self.startup_checked = Some(Instant::now());
        self.startup_pending = self.tx.send(Job::Startup(action)).is_ok();
        if !self.startup_pending {
            self.error = "无法读取或修改开机运行".into();
        }
    }

    fn graphics_setting(&mut self, ui: &mut egui::Ui) {
        let saved = self.preferences.gpu_preference;
        let mut chosen = saved;
        let detected = self.graphics_adapters.as_ref();
        let adapters = detected.and_then(|result| result.as_ref().ok());
        let option_text = |gpu| {
            let mode = match gpu {
                GpuPreference::Discrete => "优先独显",
                GpuPreference::Integrated => "优先集显",
            };
            adapters
                .and_then(|list| list.preferred(gpu))
                .map_or_else(|| mode.to_owned(), |name| format!("{mode}  ·  {name}"))
        };
        let names = adapters.map(|list| list.names.as_slice());
        setting_row(ui, "渲染显卡", |ui, caption| match (detected, names) {
            (None, _) => {
                ui.weak("正在检测显卡…");
            }
            (_, Some([])) => {
                ui.weak("未检测到硬件显卡");
            }
            (_, Some([name])) => {
                ui.label(name).labelled_by(caption);
            }
            _ => {
                let selected = option_text(saved);
                let width = settings_input_width(ui);
                ui.add_enabled_ui(!self.gpu_save_pending, |ui| {
                    egui::ComboBox::from_id_salt("render-gpu-preference")
                        .width(width)
                        .truncate()
                        .selected_text(selected.as_str())
                        .show_ui(ui, |ui| {
                            for gpu in [GpuPreference::Discrete, GpuPreference::Integrated] {
                                let option =
                                    egui::Button::selectable(chosen == gpu, option_text(gpu));
                                if ui.add(option).clicked() {
                                    chosen = gpu;
                                }
                            }
                        })
                        .response
                        .labelled_by(caption)
                        .on_hover_text(selected);
                });
                if let Some(Err(error)) = detected {
                    ui.weak("显卡检测失败").on_hover_text(error);
                }
            }
        });

        if let Some(adapters) = adapters.filter(|list| list.names.len() > 1) {
            let mut others = adapters.other_names().peekable();
            if others.peek().is_some() {
                let names = others.collect::<Vec<_>>().join("、");
                setting_row(ui, "其他显卡", |ui, _| ui.label(names));
            }
        }
        let player_preference = match self.status["gpuPreference"].as_str() {
            Some("integrated") => Some(GpuPreference::Integrated),
            Some("discrete") => Some(GpuPreference::Discrete),
            _ => None,
        };
        let restart_needed = !matches!(names, Some([_]))
            && (saved != self.started_gpu_preference
                || (self.status["running"] == true
                    && player_preference.is_some_and(|gpu| gpu != saved)));
        if restart_needed || self.restart_pending {
            settings_detail(ui, |ui| {
                if self.restart_pending {
                    ui.spinner();
                    ui.weak("正在重启…");
                } else {
                    ui.weak("重启软件生效");
                    let can_restart = !self.gpu_save_pending
                        && !self.selecting
                        && self.profile_save_error.is_none()
                        && self.queued_wallpaper.is_none()
                        && self.compat.is_none();
                    if ui
                        .add_enabled(can_restart, egui::Button::new("立即重启"))
                        .clicked()
                    {
                        self.request_restart();
                    }
                }
            });
        }
        if chosen != saved {
            self.error.clear();
            self.gpu_save_pending = self.tx.send(Job::SaveGpuPreference(chosen)).is_ok();
            if !self.gpu_save_pending {
                self.error = "无法保存显卡设置".into();
            }
        }
    }

    fn request_restart(&mut self) {
        self.prepare_restart(None);
    }

    fn prepare_restart(&mut self, output_mode: Option<bool>) {
        if self.profile_save_error.is_some() {
            return;
        }
        self.error.clear();
        match self.tx.send(Job::PrepareRestart {
            write: self.profile.finish(true),
            output_mode,
        }) {
            Ok(()) => {
                self.pending = None;
                self.restart_pending = true;
                self.switching_display = output_mode.is_some();
            }
            Err(_) => self.error = "控制线程已退出".into(),
        }
    }

    fn directory_setting(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        field: DirectoryField,
    ) {
        let (label, draft, default) = match field {
            DirectoryField::Wallpapers => (
                "壁纸目录",
                &mut self.preferences.wallpaper_dir,
                self.root.join("wallpapers-native"),
            ),
            DirectoryField::Logs => (
                "日志目录",
                &mut self.preferences.log_dir,
                self.root.join("logs"),
            ),
        };
        match directory_row(ui, label, draft, &default) {
            Some(DirectoryAction::Pick) => self.request_folder(frame, ui.ctx(), field),
            Some(DirectoryAction::Open) => {
                let path = if draft.is_empty() {
                    default
                } else {
                    PathBuf::from(draft.as_str())
                };
                self.open_directory(path);
            }
            None => {}
        }
    }

    fn open_directory(&mut self, path: PathBuf) {
        if !path.is_dir() {
            self.error = "目录不存在".into();
        } else if let Err(error) = Command::new("explorer.exe").arg(path).spawn() {
            self.error = format!("无法打开目录，{error}");
        }
    }
    fn monitor_guide(&mut self, ui: &mut egui::Ui) {
        if self.monitors.iter().any(|monitor| monitor["hmd"] == true) {
            ui.label("已检测到 Action75 HMD。选择这块屏幕即可直接输出壁纸。");
            return;
        }
        let dark = ui.visuals().dark_mode;
        let (fill, accent) = if dark {
            (
                Color32::from_rgb(52, 48, 41),
                Color32::from_rgb(214, 168, 98),
            )
        } else {
            (
                Color32::from_rgb(246, 242, 234),
                Color32::from_rgb(157, 109, 48),
            )
        };
        let card = egui::Frame::new()
            .fill(fill)
            .corner_radius(5)
            .inner_margin(egui::Margin {
                left: 14,
                right: 12,
                top: 9,
                bottom: 9,
            })
            .show(ui, |ui| {
                ui.label(
                    RichText::new("请先开启 Windows 扩展模式")
                        .size(16.)
                        .strong(),
                );
                ui.add_space(2.);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.;
                    ui.label("按");
                    for key in ["Win", "P"] {
                        if key == "P" {
                            ui.label("+");
                        }
                        egui::Frame::new()
                            .fill(ui.visuals().panel_fill)
                            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                            .corner_radius(3)
                            .inner_margin(egui::Margin::symmetric(5, 1))
                            .show(ui, |ui| ui.label(RichText::new(key).strong()));
                    }
                    ui.label("选择「扩展」，再在下方选择键盘屏。");
                });
                ui.add_space(3.);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.;
                    ui.strong("如下方没有「键盘屏候选」，");
                    ui.label("请检查键盘屏连接，再点「刷新设备」。");
                });
            });
        let rect = card.response.rect;
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + 2., rect.top() + 6.),
                egui::pos2(rect.left() + 2., rect.bottom() - 6.),
            ],
            Stroke::new(2.0_f32, accent),
        );
        ui.add_space(3.);
        if !self.monitors.iter().any(monitor_ui::keyboard_resolution) {
            ui.weak("当前未检测到 1920 × 720 的可选副屏。");
        }
        let label = if self.refreshing_devices {
            "正在刷新…"
        } else {
            "刷新设备"
        };
        if ui
            .add_enabled(
                !self.refreshing_devices && !self.selecting,
                egui::Button::new(label),
            )
            .clicked()
        {
            self.error.clear();
            self.request_devices(Job::RefreshDevices);
        }
    }

    fn select_monitor(&mut self, monitor: Value) {
        if monitor_ui::selection_key(&monitor).is_none() {
            return;
        }
        if monitor_ui::keyboard_resolution(&monitor) {
            self.apply_monitor(monitor);
        } else {
            self.monitor_warning = Some(monitor);
        }
    }

    fn apply_monitor(&mut self, monitor: Value) {
        if self.setup_complete {
            let Some(request) = monitor_ui::selection_request(&monitor) else {
                return;
            };
            self.send(request);
            self.request_devices(Job::ReadDevices);
        } else {
            self.setup_monitor = Some(monitor);
        }
    }

    fn confirm_monitor(&mut self, ctx: &egui::Context) {
        let Some(candidate) = self.monitor_warning.as_ref() else {
            return;
        };
        let current = self
            .monitors
            .iter()
            .find(|m| monitor_ui::same_choice(candidate, m))
            .cloned();
        let shortcut_available =
            single_instance::owns_exit_shortcut() || self.status["exitShortcutRegistered"] == true;
        let modal = egui::Modal::new("monitor-resolution-warning".into())
            .frame(egui::Frame::popup(&ctx.style()).inner_margin(20))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 56.).clamp(200., 424.));
                ui.horizontal(|ui| {
                    let (accent, _) = ui.allocate_exact_size(vec2(3., 20.), Sense::hover());
                    ui.painter()
                        .rect_filled(accent, 1., Color32::from_rgb(178, 117, 52));
                    ui.label(RichText::new("使用这块屏幕？").font(ui_style::display_font(22.)));
                });
                ui.add_space(5.);
                if let Some(monitor) = &current {
                    ui.label("所选屏幕的分辨率与 Action75 键盘屏不一致。");
                    ui.add_space(9.);
                    ui.columns(2, |col| {
                        col[0].weak("所选屏幕");
                        col[0].label(
                            RichText::new(format!("{} × {}", monitor["width"], monitor["height"]))
                                .size(17.),
                        );
                        col[1].weak("Action75 键盘屏");
                        col[1].label(RichText::new("1920 × 720").size(17.));
                    });
                    ui.add_space(9.);
                    ui.label("启用键盘副屏专用模式后，鼠标将无法进入这块屏幕。");
                } else {
                    ui.label("屏幕连接或分辨率已改变，请刷新后重新选择。");
                }
                ui.add_space(6.);
                ui.separator();
                ui.add_space(4.);
                ui.horizontal_wrapped(|ui| {
                    ui.label("完全退出");
                    ui.label(
                        RichText::new("Ctrl + Alt + Shift + Q")
                            .strong()
                            .background_color(ui.visuals().faint_bg_color),
                    );
                });
                ui.weak(if shortcut_available {
                    "按下快捷键可停止壁纸并释放鼠标。"
                } else {
                    "当前快捷键不可用，请从系统托盘菜单退出软件。"
                });
                ui.add_space(12.);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let ink = ui.visuals().text_color();
                    let paper = ui.visuals().panel_fill;
                    let back =
                        ui.add(egui::Button::new(RichText::new("返回选择").color(paper)).fill(ink));
                    if ui.memory(|m| m.focused().is_none()) {
                        back.request_focus();
                    }
                    (
                        back.clicked(),
                        ui.add_enabled(current.is_some(), egui::Button::new("仍然使用"))
                            .clicked(),
                    )
                })
                .inner
            });
        if modal.inner.1
            && let Some(m) = current
        {
            self.monitor_warning = None;
            self.apply_monitor(m);
        } else if modal.inner.0 || modal.should_close() {
            self.monitor_warning = None;
        }
    }

    fn setup(&mut self, ctx: &egui::Context) {
        if self.setup_complete {
            return;
        }
        if self.setup_monitor.is_none() {
            let mut hmds = self.monitors.iter().filter(|monitor| {
                monitor["hmd"] == true && monitor_ui::selection_key(monitor).is_some()
            });
            if let Some(hmd) = hmds.next()
                && hmds.next().is_none()
            {
                self.setup_monitor = Some(hmd.clone());
            }
        }
        if self.setup_device.is_empty()
            && setup_keyboard_connected(&self.devices, "VID_1CA6&PID_1A0B")
        {
            self.setup_device = "VID_1CA6&PID_1A0B".into();
        }
        egui::Modal::new(egui::Id::new("first-run-setup")).show(ctx, |ui| {
            ui.set_width(440.);
            ui.heading("连接 Action75");
            ui.add_space(18.);
            ui.label("键盘屏");
            self.monitor_guide(ui);
            let mut clicked = None;
            for monitor in self
                .monitors
                .iter()
                .filter(|m| monitor_ui::selection_key(m).is_some())
            {
                let selected = self
                    .setup_monitor
                    .as_ref()
                    .is_some_and(|m| monitor_ui::same_choice(m, monitor));
                if ui
                    .selectable_label(selected, monitor["label"].as_str().unwrap_or("副屏"))
                    .clicked()
                {
                    clicked = Some(monitor.clone());
                }
            }
            if let Some(monitor) = clicked {
                self.select_monitor(monitor);
            }
            ui.add_space(16.);
            keyboard_input_heading(ui, RichText::new("键盘输入"));
            for device in &self.devices {
                let Some(id) = device["id"].as_str().filter(|id| !id.is_empty()) else {
                    continue;
                };
                let label = device["label"].as_str().unwrap_or(id);
                ui.selectable_value(&mut self.setup_device, id.to_owned(), label);
            }
            let keyboard_connected = setup_keyboard_connected(&self.devices, &self.setup_device);
            if !self.setup_device.is_empty() && !keyboard_connected {
                ui.label("所选键盘已断开。重新连接后刷新设备，或选择其他键盘。");
            }
            ui.add_space(20.);
            let monitor = self.monitors.iter().find(|monitor| {
                self.setup_monitor
                    .as_ref()
                    .is_some_and(|m| monitor_ui::same_choice(m, monitor))
            });
            if ui
                .add_enabled(
                    monitor.is_some() && keyboard_connected && !self.selecting,
                    egui::Button::new("开始使用"),
                )
                .clicked()
                && let Some(monitor) = monitor
            {
                self.selecting = true;
                self.error.clear();
                if self
                    .tx
                    .send(Job::Setup {
                        monitor: monitor["device"].as_str().unwrap_or_default().to_owned(),
                        monitor_identity: monitor["monitorIdentity"].as_str().map(str::to_owned),
                        device: self.setup_device.clone(),
                    })
                    .is_err()
                {
                    self.selecting = false;
                    self.error = "控制线程已退出".into();
                }
            }
            if !self.error.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &self.error);
            }
            if self.status["running"] != true && ui.button("重新连接").clicked() {
                self.request_player_start();
            }
        });
    }
    fn calibration_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("键位校准").font(ui_style::display_font(32.)));
        ui.add_space(23.);
        if self.switching_display {
            ui.disable();
        }
        self.calibration.show_page(ui, &self.status, &self.monitors);
    }
    fn can_game_compat(&self) -> bool {
        !self.wallpaper_selection_blocked()
            && !self.switching_display
            && !self.wallpaper_import.busy()
            && !self.restart_pending
            && !self.gpu_save_pending
            && !self.startup_pending
            && self.folder_request.is_none()
            && self.exclusive.pending.is_none()
            && !self.selecting
            && self.queued_wallpaper.is_none()
            && self.pending.is_none()
            && !self.profile.writing()
            && !self.profile.dirty()
            && self.status["inputElevated"] != true
    }
    fn poll_game_compat(&mut self, ctx: &egui::Context) {
        if self.compat_replaced {
            return;
        }
        let result = match self.compat.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => result,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err("管理员模式交接已中断，请重试".into())
            }
            _ => return,
        };
        match result {
            Ok(()) => {
                self.compat_replaced = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(error) => {
                self.compat = None;
                self.error = error;
            }
        }
    }
    fn protect_game_compat_close(&self, ctx: &egui::Context) {
        if self.compat.is_some()
            && !self.compat_replaced
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
    }
    fn switch_display_mode(&mut self, hmd: bool) {
        if !self.can_switch_display() || action75_output_mode(&self.monitors) != Some(!hmd)
        {
            return;
        }
        self.prepare_restart(Some(hmd));
    }

    fn can_switch_display(&self) -> bool {
        self.can_install_update() && !self.calibration.has_session() && self.profile_save_error.is_none()
            && self.compat.is_none() && self.status["running"] == true
            && self.status["monitorConnected"] == true
    }

    fn devices_page(&mut self, ui: &mut egui::Ui) {
        if self.compat.is_some() {
            ui.disable();
        }
        ui.set_max_width(760.);
        ui.label(RichText::new("设备与输出").font(ui_style::display_font(32.)));
        ui.add_space(20.);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 4.;
            if game_compat::button(ui, self.can_game_compat()).clicked() {
                self.error.clear();
                self.compat = Some(game_compat::begin(self.root.clone(), ui.ctx().clone()));
            }
            ui.label(
                RichText::new("游戏中看不到按键特效时，可尝试开启。")
                    .small()
                    .weak(),
            );
            if self.compat.is_some() {
                ui.spinner();
            }
            ui.add_space(14.);

            let hmd = self.status["hmdSelected"] == true;
            if !hmd {
                if let (_, Some(request)) = exclusive_ui::show(ui, &mut self.exclusive) {
                    if self
                        .tx
                        .send(Job::SetExclusive {
                            generation: request.generation,
                            request: request.json(),
                        })
                        .is_err()
                    {
                        self.exclusive
                            .fail(request.generation, "控制线程已退出".into());
                    }
                }
                ui.label(
                    RichText::new("开启并生效后，鼠标无法进入键盘副屏。校准时会暂时解除保护。")
                        .small()
                        .weak(),
                );
                let guard_error = if !self.exclusive.error.is_empty() {
                    self.exclusive.error.as_str()
                } else {
                    self.status["displayGuardError"].as_str().unwrap_or("")
                };
                if !guard_error.trim().is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, guard_error);
                }
                ui.add_space(14.);
            }

            let requested = self.status["hideKeyboardTaskbar"].as_bool();
            let mut hide_taskbar = requested.unwrap_or(false);
            if ui
                .add_enabled(
                    !hmd && requested.is_some() && self.status["running"] == true,
                    egui::Checkbox::new(&mut hide_taskbar, "隐藏键盘副屏任务栏"),
                )
                .changed()
            {
                self.send(
                    json!({"action":"setSettings","settings":{"hideKeyboardTaskbar":hide_taskbar}}),
                );
            }
            ui.label(
                RichText::new("运行期间隐藏所选键盘副屏的 Windows 任务栏。")
                    .small()
                    .weak(),
            );
            if self.status["keyboardTaskbarFailed"] == true {
                ui.colored_label(ui.visuals().error_fg_color, "暂时无法隐藏副屏任务栏。");
            }
        });
        ui.add_space(14.);

        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(12)
            .inner_margin(22)
            .show(ui, |ui| {
                ui.label(RichText::new("显示器").size(18.).strong());
                self.monitor_guide(ui);
                ui.add_space(8.);
                if let Some(error) = self.status["monitorError"].as_str() {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                for m in self.monitors.clone() {
                    if ui.add_enabled_ui(!self.switching_display, |ui| monitor_ui::show(ui, &m)).inner.clicked() {
                        self.select_monitor(m);
                    }
                }
                if self.switching_display {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在切换 Action75 输出模式…");
                    });
                } else if let Some(hmd) = action75_output_mode(&self.monitors) {
                    ui.add_space(8.);
                    let label = if hmd { "切回普通扩展" } else { "切换到 HMD 独占" };
                    if ui.add_enabled(self.can_switch_display(), egui::Button::new(label)).clicked() {
                        self.switch_display_mode(!hmd);
                    }
                    ui.label(RichText::new("需要管理员权限。控制台会自动重开，主屏可能短暂黑屏。").small().weak());
                }
                ui.add_space(20.);
                ui.separator();
                ui.add_space(16.);
                keyboard_input_heading(ui, RichText::new("键盘输入").size(18.).strong());
                for d in self.devices.clone() {
                    if ui
                        .selectable_label(
                            d["selected"] == true,
                            d["label"].as_str().unwrap_or("键盘"),
                        )
                        .clicked()
                    {
                        self.send(
                            json!({"action":"setSettings","settings":{"deviceGroup":d["id"]}}),
                        );
                        self.request_devices(Job::ReadDevices);
                    }
                }
            });
        ui.add_space(22.);
        if ui.button("打开壁纸文件夹").clicked() {
            self.open_directory(
                core::storage::Preferences::load(&self.root).wallpapers(&self.root),
            );
        }
    }
}

fn action75_output_mode(monitors: &[Value]) -> Option<bool> {
    let mut matches = monitors.iter().filter(|monitor| {
        monitor["monitorIdentity"].as_str().is_some_and(|id| {
            id.to_ascii_uppercase().starts_with(r"\\?\DISPLAY#HIV1230#")
        })
    });
    let monitor = matches.next()?;
    if matches.next().is_some() || monitor["selected"] != true || monitor["primary"] == true {
        return None;
    }
    monitor["hmd"].as_bool()
}

impl eframe::App for Manager {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        // Covers are at most 480×180. Use a narrower atlas for the same rasterized glyphs.
        let limit = 2048;
        input.max_texture_side = Some(input.max_texture_side.unwrap_or(limit).min(limit));
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        if self.acrylic {
            [0.; 4]
        } else {
            self.shell.clear_color(self.preferences.interface.theme)
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if single_instance::full_exit_requested() {
            self.restart_on_exit.set(Restart::None);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.events(ctx);
        if self.restart_on_exit.get() != Restart::None {
            return;
        }
        if self.restart_pending && ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let ready = self.can_install_update();
        if self.updates.poll(&self.root, ctx, ready) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.poll_folder();
        if let Some(receipt) = self.wallpaper_import.poll(&self.root, ctx) {
            if receipt.updated {
                self.previews.forget(&receipt.id);
            }
            self.wallpaper_import.reveal = Some(receipt.id);
            self.tab = 0;
            self.search.clear();
            self.gallery_category = GalleryCategory::All;
            self.filter_wallpapers();
            self.request_catalog(true);
        }
        if !self.restart_pending {
            self.dispatch_wallpaper_selection();
        }
        if self.migrating && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.error = "请等待目录迁移完成".into();
        }
        self.poll_game_compat(ctx);
        self.protect_game_compat_close(ctx);
        self.calibration.poll(ctx);
        if self.calibration.take_close_cancelled() {
            self.closing = false;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.calibration.has_session() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
            self.calibration.end(ctx);
        }
        if self.closing && !self.calibration.has_session() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Read the rendered UI without starting a desktop capture session.
        if self.screenshot.is_some() {
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    if let Some(path) = self.screenshot.take() {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let bytes: Vec<u8> = image
                            .pixels
                            .iter()
                            .flat_map(|c| c.to_srgba_unmultiplied())
                            .collect();
                        if let Err(e) = image::save_buffer(
                            &path,
                            &bytes,
                            image.size[0] as u32,
                            image.size[1] as u32,
                            image::ColorType::Rgba8,
                        ) {
                            self.error = e.to_string();
                        }
                        if self.exit_after_screenshot {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }
            }
            if !self.screenshot_requested && self.created.elapsed() > Duration::from_secs(2) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                self.screenshot_requested = true;
            }
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        let minimized = ctx.input(|input| input.viewport().minimized == Some(true));
        let idle_minimized = minimized && ready && self.screenshot.is_none();
        if !self.restart_pending {
            self.flush_settings(ctx);
            // Restore repaints immediately. Keep the usual retries for a lost connection.
            if (!idle_minimized || !self.connection_error.is_empty())
                && !self.polling
                && self.last_poll.elapsed() > Duration::from_secs(3)
            {
                let _ = self.tx.send(Job::Status);
                self.polling = true;
            }
        }
        if !idle_minimized {
            self.shell.paint(ctx, self.preferences.interface.theme, self.acrylic);
            self.top_bar(ctx);
            self.header(ctx);
        }
        let monitor_visible = !minimized && self.tab == PERFORMANCE_TAB && !self.restart_pending;
        self.performance.update(
            ctx,
            monitor_visible,
            performance::PlayerProcesses::from_status(&self.status),
        );
        if idle_minimized {
            // Worker replies and window restore wake egui independently. Keep a
            // slow timer for the installed client's scheduled update check.
            let seconds = if self.connection_error.is_empty() {
                60
            } else {
                3
            };
            ctx.request_repaint_after(Duration::from_secs(seconds));
            self.updates.protect_handoff(ctx);
            return;
        }
        ctx.request_repaint_after(Duration::from_secs(3));
        self.controls(ctx);
        if self.updates.has_update() && self.tab != 3 {
            egui::TopBottomPanel::bottom("available_update").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.updates.row(ui, ready) {
                        self.ordinary_prompt = Some(OrdinaryAction::Update);
                    }
                });
            });
        }
        if !self.error.is_empty()
            || !self.connection_error.is_empty()
            || self.profile_save_error.is_some()
            || self.profile_recovery.is_some()
        {
            egui::TopBottomPanel::bottom("error")
                .frame(
                    egui::Frame::new()
                        .fill(ctx.style().visuals.panel_fill)
                        .inner_margin(12),
                )
                .show(ctx, |ui| {
                    let error = if self.error.is_empty() {
                        &self.connection_error
                    } else {
                        &self.error
                    };
                    if !error.is_empty() {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    if let Some(error) = &self.profile_save_error {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!("壁纸参数未保存，{error}"),
                        );
                        let enabled = !self.selecting
                            && !self.profile.writing()
                            && !self.restart_pending
                            && !self.wallpaper_selection_blocked();
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(
                                    enabled
                                        && self.status["running"] == true
                                        && self.profile.dirty(),
                                    egui::Button::new("重试保存"),
                                )
                                .clicked()
                            {
                                self.write_profile_patch(self.profile.patch());
                            }
                            if ui
                                .add_enabled(enabled, egui::Button::new("放弃未保存的修改"))
                                .clicked()
                            {
                                self.profile_save_error = None;
                                self.profile.discard();
                                self.pending = None;
                                if self.status["running"] == true {
                                    self.open_profile(self.profile.selected.clone(), false);
                                }
                            }
                        });
                    }
                    let _ = self.profile_recovery_button(ui);
                    if self.connection_error.contains("（5）")
                        && ui
                            .add_enabled(
                                self.can_game_compat(),
                                egui::Button::new("重新连接管理员播放器"),
                            )
                            .clicked()
                    {
                        self.error.clear();
                        self.compat = Some(game_compat::begin(self.root.clone(), ui.ctx().clone()));
                    }
                });
        }
        let changed = std::mem::replace(&mut self.last_tab, self.tab) != self.tab;
        let tab = self.tab;
        let (opacity, entrance) = if matches!(tab, 2 | 3 | PERFORMANCE_TAB) {
            (1., self.page_reveal.progress(ctx, changed))
        } else {
            (self.page_reveal.opacity(ctx, changed), 1.)
        };
        let top = if ctx.content_rect().height() < 760. {
            10
        } else {
            18
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 26,
                right: 26,
                top,
                bottom: 19,
            }))
            .show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                if tab == 0 {
                    self.gallery(ui);
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("page", tab))
                        .show(ui, |ui| match tab {
                            1 => self.calibration_page(ui),
                            2 => ui_style::reveal_page_content(ui, entrance, |ui| {
                                self.devices_page(ui);
                            }),
                            PERFORMANCE_TAB => ui_style::reveal_page_content(ui, entrance, |ui| {
                                self.performance.show(
                                    ui,
                                    self.status["gpuAdapter"].as_str(),
                                    self.status["gpuAdapterLuid"].as_u64(),
                                );
                            }),
                            _ => ui_style::reveal_page_content(ui, entrance, |ui| {
                                self.settings_page(ui, frame);
                            }),
                        });
                }
            });
        self.calibration.show_viewport(ctx);
        self.wallpaper_import.open_picker(frame, ctx);
        self.wallpaper_import.confirm(ctx);
        self.confirm_ordinary_mode(ctx);
        self.updates.protect_handoff(ctx);
        self.confirm_delete(ctx);
        if self.monitor_warning.is_none() {
            self.setup(ctx);
        }
        self.confirm_monitor(ctx);
    }
    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.calibration.shutdown();
        if self.compat_replaced || matches!(self.restart_on_exit.get(), Restart::Graphics | Restart::OutputMode(_)) {
            // The handoff or restart has already settled the profile writes.
            let _ = self.tx.send(Job::Quit);
            return;
        }
        // One worker drains older writes, commits the final patch, then exits.
        // The GUI never races a separate connection against its own queued values.
        let done = self.queue_finish();
        let _ = done.recv_timeout(Duration::from_secs(7));
        if single_instance::full_exit_requested() {
            let _ = core::command(&self.root, json!({"action":"stop"}));
        }
    }
}
#[cfg(test)]
mod manager_tests {
    use super::*;

    #[test]
    fn action75_output_switch_prepares_both_directions_and_reopens_the_controller() {
        let (mut manager, events, jobs) = fixture();
        manager.status = json!({"running":true, "monitorConnected":true});
        let display = json!({
            "monitorIdentity": r"\\?\display#hiv1230#instance#{class}",
            "selected": true,
            "hmd": false,
        });
        manager.monitors = vec![display.clone()];
        manager.switch_display_mode(true);
        assert!(matches!(jobs.try_recv(), Ok(Job::PrepareRestart { output_mode: Some(true), .. })));
        assert!(manager.switching_display);
        manager.switch_display_mode(true);
        assert!(jobs.try_recv().is_err());
        events.send(Event::RestartPrepared { output_mode: Some(true), result: Ok(()) }).unwrap();
        manager.events(&egui::Context::default());
        assert_eq!(manager.restart_on_exit.get(), Restart::OutputMode(true));
        let (mut manager, _, jobs) = fixture();
        manager.status = json!({"running":true, "monitorConnected":true});
        manager.monitors = vec![display.clone()];
        manager.monitors[0]["hmd"] = json!(true);
        manager.switch_display_mode(false);
        assert!(matches!(jobs.try_recv(), Ok(Job::PrepareRestart { output_mode: Some(false), .. })));
        assert_eq!(action75_output_mode(&[display.clone(), display]), None);
    }

    #[test]
    fn first_run_refresh_disables_a_missing_keyboard_and_accepts_it_after_reconnection() {
        let selected = "VID_1CA6&PID_1A0B";
        let other = json!({"id":"other-keyboard"});
        let keyboard = json!({"id":selected});
        assert!(!setup_keyboard_connected(&[other.clone()], selected));
        assert!(setup_keyboard_connected(&[other, keyboard], selected));
        assert!(!setup_keyboard_connected(&[json!({"id":""})], ""));
    }

    #[test]
    fn narrow_top_bar_routes_theme_and_window_buttons_at_common_dpi() {
        use egui::ViewportCommand::{Close, Maximized, Minimized};
        use egui::{Event as Input, Modifiers, PointerButton, Pos2, RawInput, Rect, ViewportId};

        for scale in [1., 1.25, 1.5] {
            let (mut manager, _, _) = fixture();
            manager.window_frame = Some(window_frame::WindowFrame::default());
            manager.preferences.interface.theme = InterfaceTheme::Graphite;
            let ctx = egui::Context::default();
            ui_style::install_fonts(&ctx);
            ctx.style_mut(|style| style.spacing.item_spacing.x = 9.);
            let frame = |manager: &mut Manager, maximized, events| {
                let mut input = RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1080., 680.))),
                    events,
                    ..Default::default()
                };
                let viewport = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
                viewport.native_pixels_per_point = Some(scale);
                viewport.maximized = Some(maximized);
                let mut output = ctx.run(input, |ctx| manager.top_bar(ctx));
                assert_eq!(ctx.pixels_per_point(), scale);
                output
                    .viewport_output
                    .remove(&ViewportId::ROOT)
                    .unwrap()
                    .commands
            };
            let click = |manager: &mut Manager, x, maximized| {
                let pos = Pos2::new(x, 25.);
                frame(manager, maximized, vec![Input::PointerMoved(pos)]);
                let events = |pressed| {
                    vec![Input::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    }]
                };
                assert!(frame(manager, maximized, events(true)).is_empty());
                frame(manager, maximized, events(false))
            };

            assert_eq!(click(&mut manager, 971., false), [Minimized(true)]);
            assert_eq!(click(&mut manager, 1011., false), [Maximized(true)]);
            assert_eq!(click(&mut manager, 1011., true), [Maximized(false)]);
            assert_eq!(click(&mut manager, 1051., false), [Close]);
            assert!(click(&mut manager, 882., false).is_empty());
            assert_eq!(manager.preferences.interface.theme, InterfaceTheme::Paper);
        }
    }

    #[test]
    fn inspector_retracts_during_calibration_and_leaves_other_pages_full_width() {
        for destination in [1, 2, 3, PERFORMANCE_TAB] {
            let (mut manager, _, _) = fixture();
            let ctx = egui::Context::default();
            ui_style::install_fonts(&ctx);
            manager.panel_open = true;
            let mut width_at = |tab, frame| {
                manager.tab = tab;
                let mut width = 0.;
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            vec2(1080., 680.),
                        )),
                        time: Some(1. + f64::from(frame) / 60.),
                        ..Default::default()
                    },
                    |ctx| {
                        manager.controls(ctx);
                        width = ctx.available_rect().width();
                    },
                );
                width
            };
            let gallery = width_at(0, 0);
            let first_frame = width_at(destination, 1);
            let calibration = width_at(1, 2);
            assert!(gallery < calibration && calibration < 1080.);
            if destination == 1 {
                assert!(first_frame < calibration);
            } else {
                assert_eq!(first_frame, 1080.);
            }
            for frame in 3..=18 {
                width_at(1, frame);
            }
            assert_eq!(width_at(1, 19), 1080.);
        }
    }

    #[test]
    fn folder_selection_updates_only_its_draft_and_cancel_preserves_it() {
        let (mut manager, _, jobs) = fixture();
        manager.preferences.wallpaper_dir = "wallpapers".into();
        manager.preferences.log_dir = "logs".into();
        for (field, path, wallpapers, logs) in [
            (DirectoryField::Logs, None, "wallpapers", "logs"),
            (
                DirectoryField::Wallpapers,
                Some("D:/自定义壁纸"),
                "D:/自定义壁纸",
                "logs",
            ),
            (
                DirectoryField::Logs,
                Some("D:/应用日志"),
                "D:/自定义壁纸",
                "D:/应用日志",
            ),
        ] {
            let (send, receive) = mpsc::channel();
            manager.folder_request = Some((field, receive));
            send.send(Ok(path.map(str::to_owned))).unwrap();
            manager.poll_folder();
            assert_eq!(manager.preferences.wallpaper_dir, wallpapers);
            assert_eq!(manager.preferences.log_dir, logs);
            assert!(manager.folder_request.is_none());
        }
        assert!(matches!(jobs.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn same_wallpaper_refresh_keeps_previous_controls_until_reply() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.packs = vec![json!({"id":"a","name":"A"})];
        let previous = m.profile.select("a".into());
        assert!(m.profile.load("a", previous, bundle("a", 80)));

        events
            .send(Event::Status(
                json!({"running":true,"packId":"a","packSettingsRevision":90}),
            ))
            .unwrap();
        m.events(&ctx);
        assert!(m.selecting);
        assert_eq!(m.profile.values["any"], 80);
        assert_eq!(m.profile.bundle.as_ref().unwrap().revision, 80);
        let Job::LoadPack { id, selection } = jobs.try_recv().unwrap() else {
            panic!("expected a background profile load");
        };
        assert_eq!(id, "a");
        assert_ne!(selection, previous);

        events
            .send(Event::PackLoaded {
                id,
                selection,
                bundle: bundle("a", 90),
            })
            .unwrap();
        m.events(&ctx);
        assert!(!m.selecting);
        assert_eq!(m.profile.values["any"], 90);
    }

    #[test]
    fn disconnect_marks_offline_keeps_gallery_and_recovers_with_one_reload() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.status = json!({"running":true});
        m.catalog_loading = false;
        m.packs = vec![json!({"id":"snow","name":"Snow"})];
        events
            .send(Event::ConnectionFailed("播放器权限不匹配（5）".into()))
            .unwrap();
        m.events(&ctx);
        assert_eq!(m.status["running"], false);
        assert_eq!(m.packs.len(), 1);
        assert!(!m.connection_error.is_empty());
        events.send(Event::Status(json!({"running":true}))).unwrap();
        m.events(&ctx);
        assert!(m.connection_error.is_empty());
        assert!(matches!(jobs.try_recv().unwrap(), Job::Full));
        events.send(Event::Status(json!({"running":true}))).unwrap();
        m.events(&ctx);
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn an_older_catalog_reply_does_not_complete_a_queued_directory_migration() {
        let (mut m, events, jobs) = fixture();
        m.migrating = true;
        m.catalog_loading = true;
        m.error = "preserved migration diagnostic".into();
        events
            .send(Event::Full {
                connection_error: None,
                catalog_error: None,
                status: json!({"running":true}),
                packs: vec![],
                monitors: vec![],
                devices: vec![],
                palettes: vec![],
            })
            .unwrap();
        m.events(&egui::Context::default());
        assert!(
            m.migrating,
            "a catalog result cannot acknowledge a migration"
        );
        assert_eq!(m.error, "preserved migration diagnostic");
        assert!(
            !m.catalog_loading,
            "the catalog result still completes its own read"
        );
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn a_failed_unrelated_request_does_not_unlock_queued_operations() {
        let (mut m, events, jobs) = fixture();
        m.selecting = true;
        m.catalog_loading = true;
        m.migrating = true;
        m.polling = true;
        events
            .send(Event::RequestFailed("older request failed".into()))
            .unwrap();
        m.events(&egui::Context::default());
        assert!(
            m.selecting,
            "an older ordinary request is not a selection reply"
        );
        assert!(
            m.catalog_loading,
            "an older ordinary request is not a catalog reply"
        );
        assert!(
            m.migrating,
            "an older ordinary request is not a migration reply"
        );
        assert!(m.polling, "an older ordinary request is not a status reply");
        assert_eq!(m.error, "older request failed");
        assert!(jobs.try_recv().is_err());
        events.send(Event::Error("untyped error".into())).unwrap();
        m.events(&egui::Context::default());
        assert!(m.selecting && m.catalog_loading && m.migrating && m.polling);
        assert_eq!(m.error, "untyped error");
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn setup_completion_only_unlocks_setup_and_catalog_preserves_its_result() {
        for error in [None, Some("setup failed")] {
            let (mut m, events, jobs) = fixture();
            m.selecting = true;
            m.catalog_loading = true;
            m.migrating = true;
            m.polling = true;
            m.error = "previous diagnostic".into();
            events
                .send(Event::SetupFinished(
                    error.map_or(Ok(()), |error| Err(error.into())),
                ))
                .unwrap();
            let ctx = egui::Context::default();
            m.events(&ctx);
            assert!(!m.selecting);
            assert!(
                m.catalog_loading && m.migrating && m.polling,
                "setup completion must not unlock catalog, migration or status work"
            );
            assert_eq!(m.error, error.unwrap_or_default());
            events
                .send(Event::Full {
                    connection_error: None,
                    catalog_error: None,
                    status: json!({"running":true}),
                    packs: vec![],
                    monitors: vec![],
                    devices: vec![],
                    palettes: vec![],
                })
                .unwrap();
            m.events(&ctx);
            assert!(
                m.migrating,
                "the following catalog still cannot complete migration"
            );
            assert_eq!(m.error, error.unwrap_or_default());
            assert!(jobs.try_recv().is_err());
        }
    }

    #[test]
    fn migration_completion_only_unlocks_migration_and_catalog_preserves_its_result() {
        for error in [None, Some("migration failed")] {
            let (mut m, events, jobs) = fixture();
            m.selecting = true;
            m.catalog_loading = true;
            m.migrating = true;
            m.polling = true;
            m.error = "previous diagnostic".into();
            events
                .send(Event::MigrationFinished(
                    error.map_or(Ok(()), |error| Err(error.into())),
                ))
                .unwrap();
            let ctx = egui::Context::default();
            m.events(&ctx);
            assert!(!m.migrating);
            assert!(
                m.selecting && m.catalog_loading && m.polling,
                "migration completion must not unlock selection, catalog or status work"
            );
            assert_eq!(m.error, error.unwrap_or_default());
            events
                .send(Event::Full {
                    connection_error: None,
                    catalog_error: None,
                    status: json!({"running":true}),
                    packs: vec![],
                    monitors: vec![],
                    devices: vec![],
                    palettes: vec![],
                })
                .unwrap();
            m.events(&ctx);
            assert!(
                m.selecting,
                "the following catalog cannot complete wallpaper selection"
            );
            assert_eq!(m.error, error.unwrap_or_default());
            assert!(jobs.try_recv().is_err());
        }
    }

    #[test]
    fn monitor_changes_only_read_devices_and_catalog_refreshes_are_coalesced() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.catalog_loading = false;
        m.status =
            json!({"running":true,"playerPid":42,"monitorDevice":"old","monitorConnected":true});
        let new =
            json!({"running":true,"playerPid":42,"monitorDevice":"new","monitorConnected":true});
        m.sync(new.clone());
        assert!(matches!(jobs.try_recv().unwrap(), Job::ReadDevices));
        m.sync(new.clone());
        assert!(jobs.try_recv().is_err());
        m.request_catalog(false);
        m.request_catalog(false);
        assert!(matches!(jobs.try_recv().unwrap(), Job::Full));
        assert!(jobs.try_recv().is_err());
        // A completed import during an older scan needs exactly one later scan.
        m.request_catalog(true);
        m.request_catalog(true);
        for attempt in [0, 1] {
            events
                .send(Event::Full {
                    connection_error: None,
                    catalog_error: None,
                    status: new.clone(),
                    packs: vec![],
                    monitors: vec![],
                    devices: vec![],
                    palettes: vec![],
                })
                .unwrap();
            m.events(&ctx);
            if attempt == 0 {
                assert!(matches!(jobs.try_recv().unwrap(), Job::Full));
            }
            assert!(jobs.try_recv().is_err());
        }
    }
    #[test]
    fn pending_admin_handoff_blocks_native_close_until_ready_or_failure() {
        let (mut m, _, _) = fixture();
        let (_tx, rx) = mpsc::channel();
        m.compat = Some(rx);
        let ctx = egui::Context::default();
        for (pending, replaced, blocked) in [
            (true, false, true),
            (true, true, false),
            (false, false, false),
        ] {
            if !pending {
                m.compat = None;
            }
            m.compat_replaced = replaced;
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(egui::ViewportEvent::Close);
            let output = ctx.run(input, |ctx| m.protect_game_compat_close(ctx));
            assert_eq!(
                output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::CancelClose)),
                blocked
            );
        }
    }
    #[test]
    fn game_compat_waits_until_calibration_and_settings_are_clean() {
        let (mut m, _, _) = fixture();
        m.status = json!({"inputElevated":false});
        assert!(m.can_game_compat());
        m.pending = Some(Instant::now());
        assert!(!m.can_game_compat());
        m.pending = None;
        m.exclusive.pending = Some(1);
        assert!(!m.can_game_compat());
        m.exclusive.pending = None;
        m.calibration.test_set_active(true);
        assert!(!m.can_game_compat());
        m.calibration.test_set_active(false);
        let selection = m.profile.select("a".into());
        m.profile.load("a", selection, bundle("a", 50));
        m.profile.values.insert("any".into(), json!(60));
        assert!(!m.can_game_compat());
        m.profile.values.insert("any".into(), json!(50));
        assert!(m.can_game_compat());
        m.profile.begin_write();
        assert!(!m.can_game_compat());
    }
    #[test]
    fn game_compat_waits_for_mutating_background_operations_and_folder_picker() {
        let (mut m, _, _) = fixture();
        assert!(m.can_game_compat());
        let operations: [fn(&mut Manager, bool); 5] = [
            |m, value| m.migrating = value,
            |m, value| m.deleting = value,
            |m, value| m.restart_pending = value,
            |m, value| m.gpu_save_pending = value,
            |m, value| m.startup_pending = value,
        ];
        for operation in operations {
            operation(&mut m, true);
            assert!(!m.can_game_compat());
            operation(&mut m, false);
            assert!(m.can_game_compat());
        }
        let (_send, receive) = mpsc::channel();
        m.folder_request = Some((DirectoryField::Wallpapers, receive));
        assert!(!m.can_game_compat());
        m.folder_request = None;
        m.queued_wallpaper = Some("wallpaper".into());
        assert!(!m.can_game_compat());
    }
    #[test]
    fn starting_import_blocks_game_compat_before_opening_the_native_picker() {
        let (mut m, _, _) = fixture();
        let ctx = egui::Context::default();
        let mut frame = |events| {
            ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| m.wallpaper_import.button(ui, true, false));
                },
            )
        };
        for label in ["导入", "视频或图片"] {
            let output = frame(vec![]);
            let position = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        Some(text.pos + text.galley.rect.center().to_vec2())
                    }
                    _ => None,
                })
                .unwrap();
            frame(vec![egui::Event::PointerMoved(position)]);
            for pressed in [true, false] {
                frame(vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
        }
        assert!(m.wallpaper_import.busy());
        assert!(!m.can_game_compat());
    }
    #[test]
    fn interrupted_game_compat_releases_the_close_and_navigation_lock() {
        let (mut m, _, _) = fixture();
        let (send, receive) = mpsc::channel();
        m.compat = Some(receive);
        let ctx = egui::Context::default();
        m.poll_game_compat(&ctx);
        assert!(
            m.compat.is_some(),
            "an empty channel still owns the handoff"
        );
        assert!(!m.can_game_compat());
        drop(send);
        m.poll_game_compat(&ctx);
        assert!(m.compat.is_none());
        assert!(!m.compat_replaced);
        assert!(m.can_game_compat());
        assert!(m.error.contains("交接已中断"));
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = ctx.run(input, |ctx| m.protect_game_compat_close(ctx));
        assert!(
            !output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::CancelClose))
        );
    }
    #[test]
    fn completed_admin_handoff_never_replays_cached_profile_on_exit() {
        let (mut m, _, jobs) = fixture();
        let selection = m.profile.select("a".into());
        m.profile.load("a", selection, bundle("a", 50));
        m.profile.values.insert("any".into(), json!(60));
        m.compat_replaced = true;
        eframe::App::on_exit(&mut m, None);
        assert!(matches!(jobs.try_recv().unwrap(), Job::Quit));
        assert!(jobs.try_recv().is_err());
    }
    fn bundle(id: &str, value: i32) -> core::pack_model::Bundle {
        serde_json::from_value(json!({"packId":id,"schema":{"version":1,"groups":[]},"values":{"any":value},"revision":value})).unwrap()
    }
    #[test]
    fn monitor_resolution_warning_precedes_changes_in_both_flows() {
        let normal =
            json!({"device":"DISPLAY2","monitorIdentity":"panel","width":1920,"height":720});
        let other =
            json!({"device":"DISPLAY3","monitorIdentity":"desk","width":2560,"height":1440});
        let mut primary = normal.clone();
        primary["primary"] = json!(true);
        for wizard in [true, false] {
            let (mut manager, _, jobs) = fixture();
            manager.setup_complete = !wizard;
            manager.select_monitor(primary.clone());
            assert!(manager.monitor_warning.is_none());
            manager.select_monitor(other.clone());
            assert_eq!(manager.monitor_warning.take(), Some(other.clone()));
            assert!(jobs.try_recv().is_err() && manager.setup_monitor.is_none());
            assert!(!monitor_ui::same_choice(&other, &normal));
            let mut changed = other.clone();
            changed["width"] = json!(1920);
            assert!(!monitor_ui::same_choice(&other, &changed));
            manager.apply_monitor(other.clone());
            if wizard {
                assert_eq!(manager.setup_monitor, Some(other.clone()));
            } else {
                assert!(
                    matches!(jobs.try_recv(),Ok(Job::Request(r)) if r["settings"]["monitorIdentity"]=="desk")
                );
            }
            manager.select_monitor(normal.clone());
            assert!(manager.monitor_warning.is_none());
        }
    }

    #[test]
    fn resolution_dialog_commits_only_on_continue() {
        let other =
            json!({"device":"DISPLAY3","monitorIdentity":"desk","width":2560,"height":1440});
        for (button, accept) in [("返回选择", false), ("仍然使用", true)] {
            let (mut manager, _, jobs) = fixture();
            manager.monitors = vec![other.clone()];
            manager.select_monitor(other.clone());
            let ctx = egui::Context::default();
            ui_style::install_fonts(&ctx);
            let mut frame = |events| {
                ctx.run(
                    egui::RawInput {
                        events,
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(850., 520.),
                        )),
                        ..Default::default()
                    },
                    |ctx| manager.confirm_monitor(ctx),
                )
            };
            frame(vec![]);
            let output = frame(vec![]);
            let pos = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == button => {
                        Some(t.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .unwrap();
            frame(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                frame(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
            assert!(manager.monitor_warning.is_none());
            assert_eq!(matches!(jobs.try_recv(), Ok(Job::Request(_))), accept);
        }
    }

    fn fixture() -> (Manager, mpsc::Sender<Event>, mpsc::Receiver<Job>) {
        let (tx, jobs) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        (Manager::with_channels(PathBuf::new(), tx, rx), events, jobs)
    }
    #[test]
    fn empty_gallery_can_open_the_wallpaper_library() {
        let (mut manager, _, _) = fixture();
        let ctx = egui::Context::default();
        ui_style::install_fonts(&ctx);
        let mut frame = |events| {
            ctx.run(
                egui::RawInput {
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(460., 500.),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| manager.gallery(ui));
                },
            )
        };
        let output = frame(vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text().contains("获取壁纸") => {
                    Some(t.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap();
        frame(vec![egui::Event::PointerMoved(pos)]);
        let press = |pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![press(true)]);
        let result = frame(vec![press(false)]);
        assert!(result.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::OpenUrl(u) if u.url == "https://github.com/blackwhite-poi/Action75-Wallpaper-Library")));
    }

    #[test]
    fn gallery_category_and_search_preserve_order_and_playback() {
        let (mut m, _, jobs) = fixture();
        m.packs = vec![
            json!({"id":"a","name":"Aurora","kind":"video"}),
            json!({"id":"b","name":"Sun","kind":"native"}),
            json!({"id":"c","name":"Sunrise","kind":"image"}),
            json!({"id":"d","name":"Sunset","kind":"video"}),
            json!({"id":"e","name":"Sun","kind":"future-effect"}),
        ];
        m.status = json!({"running":true,"packId":"b"});
        m.search = " SUN ".into();
        m.filter_wallpapers();
        let ctx = egui::Context::default();
        ui_style::install_fonts(&ctx);
        let frame = |m: &mut Manager, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(416., 430.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| m.gallery(ui));
                },
            )
        };
        for (label, indexes) in [
            ("图片", vec![2]),
            ("视频", vec![3]),
            ("互动壁纸", vec![1, 4]),
            ("全部", vec![1, 2, 3, 4]),
        ] {
            let output = frame(&mut m, vec![]);
            if label == "图片" {
                let rect = |caption| {
                    output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(t) if t.galley.text() == caption => {
                                Some(t.visual_bounding_rect())
                            }
                            _ => None,
                        })
                        .unwrap()
                };
                assert!(rect("壁纸库").bottom() < rect("导入").top());
            }
            let pos = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(t) if t.galley.text() == label => {
                        let bounds = t.visual_bounding_rect();
                        assert!(shape.clip_rect.contains_rect(bounds));
                        Some(bounds.center())
                    }
                    _ => None,
                })
                .unwrap();
            let press = |pressed| egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            frame(&mut m, vec![egui::Event::PointerMoved(pos), press(true)]);
            frame(&mut m, vec![press(false)]);
            assert_eq!(m.visible_packs, indexes);
        }
        assert_eq!(m.status["packId"], "b");
        assert!(m.queued_wallpaper.is_none() && jobs.try_recv().is_err());
    }

    #[test]
    fn dragging_a_filtered_card_persists_order_without_switching_wallpaper() {
        use egui::{Event as Input, Modifiers, PointerButton, Pos2, RawInput, Rect};
        let (mut manager, _, jobs) = fixture();
        manager.packs = ["card-a", "hidden", "card-b", "card-c"]
            .map(|id| json!({"id":id,"name":id}))
            .into();
        manager.preferences.interface.wallpaper_order =
            ["card-a", "offline", "hidden", "card-b", "card-c"]
                .map(str::to_owned)
                .into();
        manager.search = "card".into();
        manager.filter_wallpapers();
        let ctx = egui::Context::default();
        ui_style::install_fonts(&ctx);
        let mut frame = |events| {
            ctx.run(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700., 600.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| manager.gallery(ui));
                },
            )
        };
        let output = frame(vec![]);
        let pos = |name| {
            output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == name => {
                        Some(t.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .unwrap()
        };
        let from = pos("card-c");
        let to = pos("card-a");
        let press = |pos, pressed| Input::PointerButton {
            pos,
            pressed,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        };
        frame(vec![Input::PointerMoved(from), press(from, true)]);
        frame(vec![Input::PointerMoved(from - vec2(12., 0.))]);
        let caption_count = |out: egui::FullOutput| {
            out.shapes
                .iter()
                .filter(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "card-c"))
                .count()
        };
        assert_eq!(caption_count(frame(vec![Input::PointerMoved(to)])), 2);
        assert_eq!(caption_count(frame(vec![press(to, false)])), 1);
        assert!(
            matches!(jobs.try_recv(), Ok(Job::SaveInterface(p)) if p.wallpaper_order == ["card-c","offline","card-a","hidden","card-b"])
        );
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn wallpaper_switch_dispatches_only_the_latest_waiting_selection() {
        use egui::{Event as Input, Modifiers, PointerButton, Pos2, RawInput, Rect};

        let (mut manager, events, jobs) = fixture();
        manager.status = json!({"running": true, "packId": "a"});
        manager.packs = ["a", "b", "c", "d"]
            .map(|id| json!({"id":id,"name":id}))
            .into();
        manager.filter_wallpapers();
        let ctx = egui::Context::default();
        ui_style::install_fonts(&ctx);
        let frame = |manager: &mut Manager, input| {
            ctx.run(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700., 600.))),
                    events: input,
                    ..Default::default()
                },
                |ctx| {
                    manager.events(ctx);
                    manager.dispatch_wallpaper_selection();
                    egui::CentralPanel::default().show(ctx, |ui| manager.gallery(ui));
                },
            )
        };
        let click = |manager: &mut Manager, name| {
            let pos = frame(manager, vec![])
                .shapes
                .into_iter()
                .find_map(|clipped| match clipped.shape {
                    egui::Shape::Text(text) if text.galley.job.text == name => {
                        Some(text.pos + text.galley.rect.center().to_vec2())
                    }
                    _ => None,
                })
                .unwrap();
            frame(manager, vec![Input::PointerMoved(pos)]);
            for pressed in [true, false] {
                frame(
                    manager,
                    vec![Input::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    }],
                );
            }
        };

        click(&mut manager, "b");
        let Job::SelectPack { id, selection } = jobs.try_recv().unwrap() else {
            panic!("expected the first wallpaper to be dispatched");
        };
        assert_eq!(id, "b");
        for name in ["c", "d"] {
            click(&mut manager, name);
            assert!(jobs.try_recv().is_err());
        }
        events
            .send(Event::PackSelected {
                id,
                selection,
                status: json!({"running": true, "packId": "b"}),
                bundle: bundle("b", 1),
            })
            .unwrap();
        frame(&mut manager, vec![]);
        assert!(matches!(jobs.try_recv().unwrap(), Job::SelectPack { id, .. } if id == "d"));
        assert!(jobs.try_recv().is_err());
    }
    #[test]
    fn stale_profile_ack_does_not_change_current_wallpaper() {
        let (mut m, events, _) = fixture();
        let ctx = egui::Context::default();
        let a = m.profile.select("a".into());
        m.profile.load("a", a, bundle("a", 80));
        let edit = m.profile.begin_write();
        let b = m.profile.select("b".into());
        m.profile.load("b", b, bundle("b", 20));
        events
            .send(Event::PackSaved {
                id: "a".into(),
                selection: a,
                edit,
                bundle: bundle("a", 95),
            })
            .unwrap();
        m.events(&ctx);
        assert_eq!(m.profile.values["any"], 20);
    }
    #[test]
    fn debounce_emits_patch_for_selected_pack_only() {
        let (mut m, _, jobs) = fixture();
        let ctx = egui::Context::default();
        let s = m.profile.select("a".into());
        m.profile.load("a", s, bundle("a", 20));
        m.profile.values.insert("any".into(), json!(50));
        m.changed(&ctx, vec!["any".into()]);
        m.pending = Some(Instant::now() - Duration::from_millis(200));
        m.flush_settings(&ctx);
        match jobs.try_recv().unwrap() {
            Job::WritePack {
                id,
                selection,
                values,
                revision,
                ..
            } => {
                assert_eq!(id, "a");
                assert_eq!(selection, s);
                assert_eq!(values["any"], 50);
                assert_eq!(revision, Some(json!(20)));
            }
            _ => panic!("expected a pack-specific patch"),
        }
    }
    #[test]
    fn returning_to_baseline_requests_preview_restore_after_pending_reply() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        let selection = m.profile.select("a".into());
        assert!(m.profile.load("a", selection, bundle("a", 20)));
        m.profile.values.insert("any".into(), json!(50));
        m.changed(&ctx, vec!["any".into()]);
        m.last_preview = Instant::now() - Duration::from_millis(60);
        m.flush_settings(&ctx);
        assert!(
            matches!(jobs.try_recv().unwrap(), Job::PreviewPack { values, .. }
            if values["any"] == 50)
        );

        m.profile.values.insert("any".into(), json!(20));
        m.changed(&ctx, vec!["any".into()]);
        m.pending = Some(Instant::now() - Duration::from_millis(200));
        m.flush_settings(&ctx);
        assert!(jobs.try_recv().is_err());
        events
            .send(Event::Previewed {
                id: "a".into(),
                selection,
                error: None,
            })
            .unwrap();
        m.events(&ctx);
        m.flush_settings(&ctx);
        assert!(
            matches!(jobs.try_recv().unwrap(), Job::PreviewPack { id, values, .. }
            if id == "a" && values.is_empty())
        );
        m.flush_settings(&ctx);
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn switching_waits_for_a_versioned_save_of_the_old_draft() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.status = json!({"running":true,"packId":"a"});
        let s = m.profile.select("a".into());
        m.profile.load("a", s, bundle("a", 20));
        m.profile.values.insert("any".into(), json!(70));
        m.queued_wallpaper = Some("b".into());
        m.dispatch_wallpaper_selection();
        let Job::WritePack {
            id,
            selection,
            edit,
            values,
            revision,
        } = jobs.try_recv().unwrap()
        else {
            panic!("expected the old draft to save first");
        };
        assert_eq!(
            (id.as_str(), selection, revision),
            ("a", s, Some(json!(20)))
        );
        assert_eq!(values["any"], 70);
        m.dispatch_wallpaper_selection();
        assert!(jobs.try_recv().is_err());
        events
            .send(Event::PackSaved {
                id,
                selection,
                edit,
                bundle: bundle("a", 70),
            })
            .unwrap();
        m.events(&ctx);
        m.dispatch_wallpaper_selection();
        assert!(matches!(jobs.try_recv().unwrap(), Job::SelectPack { id, .. } if id == "b"));
        assert!(m.profile.values.is_empty());
    }

    #[test]
    fn rejected_save_cancels_switch_and_rebases_draft_without_replaying_it() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.status = json!({"running":true,"packId":"a"});
        let s = m.profile.select("a".into());
        m.profile.load("a", s, bundle("a", 20));
        m.profile.values.insert("any".into(), json!(70));
        m.queued_wallpaper = Some("b".into());
        m.dispatch_wallpaper_selection();
        let Job::WritePack {
            id,
            selection,
            edit,
            ..
        } = jobs.try_recv().unwrap()
        else {
            panic!("expected write before switching");
        };
        events
            .send(Event::PackFailed {
                id,
                selection,
                edit: Some(edit),
                error: "stale revision".into(),
            })
            .unwrap();
        events
            .send(Event::Status(
                json!({"running":true,"packId":"a","packSettingsRevision":21}),
            ))
            .unwrap();
        m.events(&ctx);
        let Job::LoadPack { id, selection } = jobs.try_recv().unwrap() else {
            panic!("expected only a read after the write was rejected");
        };
        assert_eq!(id, "a");
        let mut fresh = bundle("a", 21);
        fresh.values.insert("other".into(), json!(30));
        events
            .send(Event::PackLoaded {
                id,
                selection,
                bundle: fresh,
            })
            .unwrap();
        m.events(&ctx);
        m.dispatch_wallpaper_selection();
        m.flush_settings(&ctx);
        assert!(jobs.try_recv().is_err());
        assert_eq!(m.profile.values["any"], 70);
        assert_eq!(m.profile.values["other"], 30);
        assert_eq!(m.profile_save_error.as_deref(), Some("stale revision"));
        assert!(m.queued_wallpaper.is_none());
        m.write_profile_patch(m.profile.patch());
        assert!(
            matches!(jobs.try_recv().unwrap(), Job::WritePack { values, revision: Some(revision), .. }
            if values["any"] == 70 && values.len() == 1 && revision == 21)
        );
    }

    #[test]
    fn restoring_the_active_profile_preserves_the_wallpaper_selection_error() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.status = json!({"running":true,"packId":"a"});
        m.open_profile("b".into(), true);
        let Job::SelectPack { id, selection } = jobs.try_recv().unwrap() else {
            panic!("expected selection")
        };
        events
            .send(Event::PackFailed {
                id,
                selection,
                edit: None,
                error: "failed to load b".into(),
            })
            .unwrap();
        m.events(&ctx);
        let Job::LoadPack { id, selection } = jobs.try_recv().unwrap() else {
            panic!("expected fallback")
        };
        assert_eq!(id, "a");
        events
            .send(Event::PackLoaded {
                id,
                selection,
                bundle: bundle("a", 20),
            })
            .unwrap();
        m.events(&ctx);
        assert_eq!(m.error, "failed to load b");
        assert_eq!(m.profile.selected, "a");
    }

    #[test]
    fn damaged_profile_can_be_recovered_through_the_button_without_a_bundle() {
        use egui::{Event as Input, Modifiers, PointerButton, RawInput};

        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.sync(json!({"running":true,"packId":"a"}));
        let Job::LoadPack { id, selection } = jobs.try_recv().unwrap() else {
            panic!("expected load")
        };
        events
            .send(Event::PackFailed {
                id,
                selection,
                edit: None,
                error: "Invalid wallpaper profile revision".into(),
            })
            .unwrap();
        m.events(&ctx);
        assert!(m.profile.bundle.is_none());
        let mut draw = |input| {
            let mut rect = egui::Rect::NOTHING;
            let _ = ctx.run(
                RawInput {
                    events: input,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| rect = m.profile_recovery_button(ui).unwrap().rect);
                },
            );
            rect
        };
        let pos = draw(vec![]).center();
        draw(vec![Input::PointerMoved(pos)]);
        for pressed in [true, false] {
            draw(vec![Input::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }]);
        }
        assert!(matches!(jobs.try_recv().unwrap(), Job::RecoverPack(id) if id == "a"));
        events
            .send(Event::PackRecovered {
                id: "a".into(),
                result: Ok((bundle("a", 5), true)),
            })
            .unwrap();
        m.events(&ctx);
        assert_eq!(m.profile.values["any"], 5);
        assert!(m.profile_recovery.is_none());
        assert!(m.error.is_empty());
    }
    #[test]
    fn resetting_then_switching_cannot_resurrect_old_draft() {
        let (mut m, events, jobs) = fixture();
        let ctx = egui::Context::default();
        m.status = json!({"running":true,"packId":"a"});
        let a = m.profile.select("a".into());
        let mut b = bundle("a", 20);
        b.schema = json!({"version":1,"groups":[{"id":"g","controls":[{"id":"any","default":5}]}]});
        m.profile.load("a", a, b);
        m.profile.values.insert("any".into(), json!(99));
        m.reset_profile();
        assert_eq!(m.profile.values["any"], 5);
        m.queued_wallpaper = Some("b".into());
        m.dispatch_wallpaper_selection();
        let Job::ResetPack {
            id,
            selection,
            edit,
            ..
        } = jobs.try_recv().unwrap()
        else {
            panic!("expected reset")
        };
        assert_eq!(id, "a");
        assert!(jobs.try_recv().is_err());
        events
            .send(Event::PackSaved {
                id,
                selection,
                edit,
                bundle: bundle("a", 5),
            })
            .unwrap();
        m.events(&ctx);
        m.dispatch_wallpaper_selection();
        assert!(matches!(jobs.try_recv().unwrap(), Job::SelectPack { id, .. } if id == "b"));
    }
    #[test]
    fn final_flush_is_fifo_and_uses_only_changed_profile_fields() {
        let (mut m, _, jobs) = fixture();
        let s = m.profile.select("a".into());
        m.profile.load("a", s, bundle("a", 20));
        m.profile.values.insert("any".into(), json!(60));
        m.write_profile_patch(m.profile.patch());
        m.profile.values.insert("any".into(), json!(70));
        m.profile.changed(["any".into()]);
        let _done = m.queue_finish();
        assert!(matches!(jobs.try_recv().unwrap(), Job::WritePack { .. }));
        match jobs.try_recv().unwrap() {
            Job::FlushAndQuit { write, .. } => {
                assert_eq!(write.id, "a");
                assert_eq!(write.values["any"], 70);
                assert_eq!(write.values.len(), 1);
                assert_eq!(write.after, Some((s, 0)));
            }
            _ => panic!("final write must follow queued writes"),
        }
    }
    #[test]
    fn revert_to_baseline_while_write_inflight_is_flushed_on_exit_and_switch() {
        for exiting in [true, false] {
            let (mut m, events, jobs) = fixture();
            m.status = json!({"running":true,"packId":"a"});
            let s = m.profile.select("a".into());
            let mut b = bundle("a", 50);
            b.values.insert("untouched".into(), json!(90));
            m.profile.load("a", s, b);
            m.profile.values.insert("any".into(), json!(60));
            m.profile.changed(["any".into()]);
            let edit = m.profile.begin_write();
            m.profile.values.insert("any".into(), json!(50));
            m.profile.changed(["any".into()]);
            assert!(m.profile.patch().is_empty());
            if exiting {
                let _ = m.queue_finish();
                match jobs.try_recv().unwrap() {
                    Job::FlushAndQuit { write, .. } => {
                        assert_eq!(write.id, "a");
                        assert_eq!(write.values["any"], 50);
                        assert!(!write.values.contains_key("untouched"));
                        assert_eq!(write.after, Some((s, edit)));
                    }
                    _ => panic!("expected latest baseline flush"),
                }
            } else {
                m.queued_wallpaper = Some("b".into());
                m.dispatch_wallpaper_selection();
                assert!(jobs.try_recv().is_err());
                let mut saved = bundle("a", 60);
                saved.values.insert("untouched".into(), json!(90));
                events
                    .send(Event::PackSaved {
                        id: "a".into(),
                        selection: s,
                        edit,
                        bundle: saved,
                    })
                    .unwrap();
                m.events(&egui::Context::default());
                m.dispatch_wallpaper_selection();
                match jobs.try_recv().unwrap() {
                    Job::WritePack {
                        id,
                        values,
                        revision,
                        ..
                    } => {
                        assert_eq!(id, "a");
                        assert_eq!(values["any"], 50);
                        assert!(!values.contains_key("untouched"));
                        assert_eq!(revision, Some(json!(60)));
                    }
                    _ => panic!("expected latest baseline flush"),
                }
            }
        }
    }
}

#[cfg(test)]
mod calibration_lock_tests {
    use super::*;
    use egui::{Event as InputEvent, Modifiers, PointerButton, Pos2, RawInput, Rect};
    fn frame(
        ctx: &egui::Context,
        manager: &mut Manager,
        events: Vec<InputEvent>,
    ) -> egui::FullOutput {
        ctx.run(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1180., 790.))),
                events,
                ..Default::default()
            },
            |ctx| {
                manager.header(ctx);
                manager.controls(ctx);
                egui::CentralPanel::default().show(ctx, |ui| match manager.tab {
                    1 => manager.calibration_page(ui),
                    2 => manager.devices_page(ui),
                    _ => manager.gallery(ui),
                });
                manager.confirm_delete(ctx);
            },
        )
    }
    fn click(ctx: &egui::Context, manager: &mut Manager, label: &str) {
        click_button(ctx, manager, label, PointerButton::Primary);
    }
    fn click_button(
        ctx: &egui::Context,
        manager: &mut Manager,
        label: &str,
        button: PointerButton,
    ) {
        let output = frame(ctx, manager, vec![]);
        let p = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label => {
                    Some(t.pos + t.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing actual widget {label}"));
        frame(ctx, manager, vec![InputEvent::PointerMoved(p)]);
        for pressed in [true, false] {
            frame(
                ctx,
                manager,
                vec![InputEvent::PointerButton {
                    pos: p,
                    button,
                    pressed,
                    modifiers: Modifiers::NONE,
                }],
            );
        }
    }
    #[test]
    fn gallery_context_delete_requires_confirmation_and_keeps_selection() {
        let (tx, jobs) = mpsc::channel();
        let (_events, rx) = mpsc::channel();
        let mut manager = Manager::with_channels(PathBuf::new(), tx, rx);
        manager.status = json!({"running":true,"packId":"a"});
        manager.packs = vec![json!({"id":"b","name":"测试壁纸","path":"D:/library/b"})];
        manager.filter_wallpapers();
        let ctx = egui::Context::default();
        ui_style::install_fonts(&ctx);
        ctx.style_mut(|style| style.animation_time = 0.);
        for confirm in [false, true] {
            click_button(&ctx, &mut manager, "测试壁纸", PointerButton::Secondary);
            frame(&ctx, &mut manager, vec![]);
            click(&ctx, &mut manager, "删除壁纸…");
            frame(&ctx, &mut manager, vec![]);
            assert!(jobs.try_recv().is_err());
            click(
                &ctx,
                &mut manager,
                if confirm { "移入回收站" } else { "取消" },
            );
            if confirm {
                assert!(
                    matches!(jobs.try_recv().unwrap(),Job::DeletePack{id,path} if id=="b" && path==PathBuf::from("D:/library/b"))
                );
            } else {
                assert!(jobs.try_recv().is_err());
            }
            assert_eq!(manager.status["packId"], "a");
        }
    }
    #[test]
    fn inspector_drag_remembers_width_only_on_release() {
        let (tx, jobs) = mpsc::channel();
        let (_events, rx) = mpsc::channel();
        let mut manager = Manager::with_channels(PathBuf::new(), tx, rx);
        let ctx = egui::Context::default();
        ui_style::install_fonts(&ctx);
        ctx.style_mut(|style| style.animation_time = 0.);

        let initial = manager.panel_width;
        let start = Pos2::new(1180. - initial + 4., 420.);
        let end = start - vec2(40., 0.);
        frame(&ctx, &mut manager, vec![InputEvent::PointerMoved(start)]);
        frame(
            &ctx,
            &mut manager,
            vec![InputEvent::PointerButton {
                pos: start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
        );
        frame(&ctx, &mut manager, vec![InputEvent::PointerMoved(end)]);
        assert_eq!(manager.panel_width, initial + 40.);
        assert!(jobs.try_recv().is_err());

        frame(
            &ctx,
            &mut manager,
            vec![InputEvent::PointerButton {
                pos: end,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(matches!(
            jobs.try_recv().unwrap(),
            Job::SaveInterface(interface) if interface.panel_width == Some(initial + 40.)
        ));
    }

    #[test]
    fn actual_navigation_and_playback_lock_during_calibration_then_restore_after_end() {
        let (tx, jobs) = mpsc::channel();
        let (_events, rx) = mpsc::channel();
        let mut m = Manager::with_channels(PathBuf::new(), tx, rx);
        let ctx = egui::Context::default();
        ui_style::install_fonts(&ctx);
        ctx.style_mut(|style| style.animation_time = 0.);
        m.status = json!({"running":true,"paused":true,"monitorConnected":true});
        m.monitors = vec![
            json!({"device":"SECONDARY","label":"Keyboard Display","width":1920,"height":720,"primary":false,"selected":false}),
        ];
        m.calibration.test_set_active(true);
        click(&ctx, &mut m, "壁纸库");
        assert_eq!(m.tab, 1);
        click(&ctx, &mut m, "设备与输出");
        assert_eq!(m.tab, 1);
        assert!(jobs.try_recv().is_err());
        m.send(json!({"action":"resume"}));
        m.send(json!({"action":"setSettings","settings":{"monitorDevice":"SECONDARY"}}));
        m.open_profile("another-wallpaper".into(), true);
        assert!(jobs.try_recv().is_err());
        assert!(m.profile.selected.is_empty());
        m.calibration.test_set_active(false);
        click(&ctx, &mut m, "壁纸库");
        click(&ctx, &mut m, "继续播放");
        assert!(matches!(jobs.try_recv().unwrap(),Job::Request(v) if v["action"]=="resume"));
        click(&ctx, &mut m, "设备与输出");
        assert_eq!(m.tab, 2);
        click(&ctx, &mut m, "Keyboard Display");
        assert!(
            matches!(jobs.try_recv().unwrap(),Job::Request(v) if v["settings"]["monitorDevice"]=="SECONDARY")
        );
        assert!(matches!(jobs.try_recv().unwrap(), Job::ReadDevices));
        m.open_profile("another-wallpaper".into(), true);
        assert!(
            matches!(jobs.try_recv().unwrap(),Job::SelectPack{id,..} if id=="another-wallpaper")
        );
    }
}

fn primary_display_has_double_scale() -> bool {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
        UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
    };

    // The window builder calls this after winit has made the process DPI-aware.
    let monitor = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
    let (mut x, mut y) = (0, 0);
    unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_ok() && x == 2 * 96
}
