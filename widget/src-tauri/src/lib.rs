//! RawKVM widget: menu bar / tray app and settings window. Everything that
//! touches the protocol lives in `agent-core` (`widget`, `canvas`) and here;
//! the Svelte front end only renders [`view::View`] and calls commands.
//!
//! Local config: `~/Library/Application Support/rawkvm/widget.toml` (hub,
//! token, pinned certificate only; the layout lives in the hub). Log:
//! `~/Library/Logs/rawkvm-widget.log`.

mod canvas_view;
mod view;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agent_core::widget::{run_widget, WidgetShared};
use agent_core::{AgentConfig, AgentOptions};
use proto::Control;
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Runtime, WindowEvent};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tracing::{error, info, warn};
use uuid::Uuid;

const TRAY: &str = "main";
const WINDOW: &str = "main";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn this_os() -> proto::Os {
    if cfg!(target_os = "macos") {
        proto::Os::MacOs
    } else if cfg!(windows) {
        proto::Os::Windows
    } else {
        proto::Os::Linux
    }
}

struct AppState {
    config: PathBuf,
    shared: Arc<WidgetShared>,
    tx: Mutex<Option<UnboundedSender<Control>>>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    tray_key: Mutex<String>,
}

impl AppState {
    fn config(&self) -> AgentConfig {
        AgentConfig::load_or_init(&self.config).unwrap_or_else(|e| {
            warn!("{e:#}");
            AgentConfig::default()
        })
    }

    fn view(&self) -> view::View {
        view::view(&self.shared.snapshot(), &self.config())
    }

    /// (Re)start the hub session with the current config, if paired.
    fn restart(&self) {
        if let Some(t) = lock(&self.task).take() {
            t.abort();
        }
        *lock(&self.tx) = None;
        let cfg = self.config();
        if cfg.hub.is_empty() || cfg.token.is_empty() {
            info!("not paired");
            return;
        }
        let (tx, rx) = unbounded_channel();
        *lock(&self.tx) = Some(tx);
        let opts = AgentOptions {
            config_path: self.config.clone(),
            os: this_os(),
        };
        let shared = self.shared.clone();
        info!(hub = %cfg.hub, "connecting");
        *lock(&self.task) = Some(tauri::async_runtime::spawn(async move {
            if let Err(e) = run_widget(opts, shared, rx).await {
                error!("{e:#}");
            }
        }));
    }

    /// Ask the hub to send "nothing pressed" to every agent (a widget's
    /// switch to the node that is already active).
    fn release_keys(&self) -> Result<(), String> {
        let active = self
            .shared
            .snapshot()
            .status
            .and_then(|s| s.active)
            .ok_or("no hay ningún equipo activo")?;
        info!("release keys");
        self.send(Control::SwitchTo {
            target: active,
            entry: None,
        })
    }

    fn send(&self, msg: Control) -> Result<(), String> {
        let tx = lock(&self.tx);
        let tx = tx.as_ref().ok_or("no hay conexión con el hub")?;
        tx.send(msg)
            .map_err(|_| "la sesión con el hub se cerró".to_string())
    }
}

// ---- tray --------------------------------------------------------------

fn build_menu<R: Runtime>(app: &AppHandle<R>, v: &view::View) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    let header = match (&v.hub_name, v.connected, v.paired) {
        (Some(name), true, _) => format!("RawKVM · {name}"),
        (_, false, true) => "RawKVM · sin conexión con el hub".into(),
        _ => "RawKVM · sin emparejar".into(),
    };
    menu.append(&MenuItem::with_id(
        app,
        "header",
        header,
        false,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let agents: Vec<_> = v.nodes.iter().filter(|n| n.role == "agent").collect();
    for n in &agents {
        let mut label = format!("{}  {}", n.index, n.name);
        if !n.has_helper {
            label.push_str("  (sin helper)");
        }
        menu.append(&CheckMenuItem::with_id(
            app,
            format!("switch:{}", n.id),
            label,
            v.connected && !n.active,
            n.active,
            None::<&str>,
        )?)?;
    }
    if !agents.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&MenuItem::with_id(
            app,
            "release",
            "Liberar teclas",
            v.connected && v.active.is_some(),
            None::<&str>,
        )?)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    menu.append(&MenuItem::with_id(
        app,
        "settings",
        "Ajustes…",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "quit",
        "Salir de RawKVM",
        true,
        None::<&str>,
    )?)?;
    Ok(menu)
}

/// Push the state to the window and, if needed, rebuild the tray menu.
fn refresh(app: &AppHandle) {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let v = state.view();
    let _ = app.emit("state", &v);
    let key = view::tray_key(&v);
    {
        let mut k = lock(&state.tray_key);
        if *k == key {
            return;
        }
        *k = key;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id(TRAY) else {
            return;
        };
        match build_menu(&handle, &v) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(e) => warn!("tray menu: {e}"),
        }
        let title = v
            .nodes
            .iter()
            .find(|n| n.active)
            .map(|n| n.index.to_string());
        let _ = tray.set_title(title);
        let tip = match &v.hub_name {
            Some(h) if v.connected => format!("RawKVM: {h}"),
            _ => "RawKVM: sin conexión".into(),
        };
        let _ = tray.set_tooltip(Some(tip));
    });
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "settings" => show_settings(app),
        "quit" => app.exit(0),
        "release" => {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                if let Err(e) = state.release_keys() {
                    warn!("{e}");
                }
            }
        }
        id => {
            let Some(node) = id
                .strip_prefix("switch:")
                .and_then(|s| s.parse::<Uuid>().ok())
            else {
                return;
            };
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                info!(%node, "switch from the tray");
                if let Err(e) = state.send(Control::SwitchTo {
                    target: node,
                    entry: None,
                }) {
                    warn!("{e}");
                }
            }
        }
    }
}

// ---- commands ----------------------------------------------------------

#[tauri::command]
fn get_view(state: tauri::State<'_, Arc<AppState>>) -> view::View {
    state.view()
}

#[derive(Serialize)]
struct HubView {
    name: String,
    addr: String,
    fingerprint: Option<String>,
    proto_ver: Option<u16>,
    /// Same protocol version as this widget.
    compatible: bool,
}

#[tauri::command]
async fn discover() -> Result<Vec<HubView>, String> {
    let hubs = agent_core::discovery::discover(Duration::from_secs(3))
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(hubs
        .into_iter()
        .map(|h| {
            let addr = h
                .addrs
                .iter()
                .find(|a| a.is_ipv4())
                .or(h.addrs.first())
                .map(|a| a.to_string())
                .unwrap_or_default();
            HubView {
                compatible: h.proto_ver == Some(proto::PROTO_VERSION),
                name: h.name,
                addr,
                fingerprint: h.fingerprint,
                proto_ver: h.proto_ver,
            }
        })
        .collect())
}

/// Save hub and token (the certificate is pinned on the first accepted
/// connection, as the agents do) and connect.
#[tauri::command]
fn pair(state: tauri::State<'_, Arc<AppState>>, hub: String, token: String) -> Result<(), String> {
    let hub = hub.trim().to_string();
    if hub.is_empty() {
        return Err("falta la dirección del hub".into());
    }
    let token = token.trim().to_lowercase();
    proto::parse_token(&token).ok_or("el token son 64 caracteres hexadecimales")?;
    let mut cfg = state.config();
    cfg.hub = hub;
    cfg.token = token;
    cfg.hub_fingerprint.clear();
    cfg.save(&state.config).map_err(|e| format!("{e:#}"))?;
    info!(hub = %cfg.hub, "paired; connecting");
    state.shared.reset();
    state.restart();
    Ok(())
}

#[tauri::command]
fn forget(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut cfg = state.config();
    cfg.hub.clear();
    cfg.token.clear();
    cfg.hub_fingerprint.clear();
    cfg.save(&state.config).map_err(|e| format!("{e:#}"))?;
    info!("hub forgotten");
    state.restart();
    state.shared.reset();
    Ok(())
}

#[tauri::command]
fn release_keys(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    state.release_keys()
}

#[tauri::command]
fn switch_to(state: tauri::State<'_, Arc<AppState>>, node: Uuid) -> Result<(), String> {
    state.send(Control::SwitchTo {
        target: node,
        entry: None,
    })
}

// ---- options -----------------------------------------------------------

/// Hysteresis and hotkey: the hub applies them at once and keeps them in
/// its settings.toml.
#[tauri::command]
fn set_settings(
    state: tauri::State<'_, Arc<AppState>>,
    hysteresis_ms: u32,
    hotkey: bool,
) -> Result<(), String> {
    if hysteresis_ms > 5000 {
        return Err("la histéresis va de 0 a 5000 ms".into());
    }
    info!(hysteresis_ms, hotkey, "settings");
    state.send(Control::Settings(proto::HubSettings {
        hysteresis_ms,
        hotkey,
    }))
}

/// Edge options travel with the layout: resend the layout in force (the
/// hub's saved one, or the current arrangement if the hub still uses its
/// seed) with the new options.
#[tauri::command]
fn set_edge_options(
    state: tauri::State<'_, Arc<AppState>>,
    push_ms: u16,
    corner_pct: u8,
    dead_zone_px: u16,
) -> Result<(), String> {
    if corner_pct > 40 || dead_zone_px > 50 || push_ms > 2000 {
        return Err("valor fuera de rango".into());
    }
    let st = state.shared.snapshot();
    let options = proto::EdgeOptions {
        push_ms,
        corner_pct,
        dead_zone_px,
    };
    let placements = canvas_view::arrangement(&st, None);
    let layout = if st.canvas.is_empty() {
        let Control::Layout { edges, canvas, .. } = canvas_view::layout_for(&st, &placements)
        else {
            unreachable!()
        };
        Control::Layout {
            screens: Vec::new(),
            edges,
            options,
            canvas,
        }
    } else {
        Control::Layout {
            screens: Vec::new(),
            edges: st.edges.clone(),
            options,
            canvas: st.canvas.clone(),
        }
    };
    info!(?options, "edge options");
    state.send(layout)
}

/// How long the receiver accepts a new device (Solaar uses 30 s too).
const PAIRING_TIMEOUT_S: u8 = 30;

#[tauri::command]
fn pair_start(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    state.send(Control::PairingStart {
        receiver: String::new(),
        timeout_s: PAIRING_TIMEOUT_S,
    })
}

#[tauri::command]
fn pair_stop(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    state.send(Control::PairingStop)
}

#[tauri::command]
fn unpair(state: tauri::State<'_, Arc<AppState>>, slot: u8) -> Result<(), String> {
    state.send(Control::Unpair { slot })
}

#[tauri::command]
fn set_pointer_speed(
    state: tauri::State<'_, Arc<AppState>>,
    slot: u8,
    value: u16,
) -> Result<(), String> {
    state.send(Control::SetPointerSpeed { slot, value })
}

#[tauri::command]
fn set_fn_swap(state: tauri::State<'_, Arc<AppState>>, slot: u8, on: bool) -> Result<(), String> {
    state.send(Control::SetFnSwap { slot, on })
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let a = app.autolaunch();
    let r = if enabled { a.enable() } else { a.disable() };
    info!(enabled, "start at login");
    r.map_err(|e| e.to_string())
}

// ---- monitors canvas ---------------------------------------------------

/// What to draw for `draft` (or, without one, the hub's arrangement or a
/// proposal from the links in force).
#[tauri::command]
fn canvas_view(
    state: tauri::State<'_, Arc<AppState>>,
    draft: Option<Vec<proto::Placement>>,
) -> canvas_view::CanvasView {
    canvas_view::build(&state.shared.snapshot(), draft.as_deref())
}

/// A block was dropped at (x, y): move it, snap it, return the new view.
#[tauri::command]
fn canvas_drop(
    state: tauri::State<'_, Arc<AppState>>,
    draft: Vec<proto::Placement>,
    node: Uuid,
    x: f64,
    y: f64,
) -> canvas_view::CanvasView {
    let st = state.shared.snapshot();
    let placed = canvas_view::drop_block(&st, &draft, node, x, y);
    canvas_view::build(&st, Some(&placed))
}

#[tauri::command]
fn canvas_scale(
    state: tauri::State<'_, Arc<AppState>>,
    draft: Vec<proto::Placement>,
    node: Uuid,
    scale: Option<f32>,
) -> canvas_view::CanvasView {
    let st = state.shared.snapshot();
    let placed = canvas_view::set_scale(&st, &draft, node, scale);
    canvas_view::build(&st, Some(&placed))
}

/// Send the arrangement and its links to the hub, which keeps them.
#[tauri::command]
fn canvas_apply(
    state: tauri::State<'_, Arc<AppState>>,
    draft: Vec<proto::Placement>,
) -> Result<(), String> {
    let st = state.shared.snapshot();
    let placed = canvas_view::arrangement(&st, Some(&draft));
    let layout = canvas_view::layout_for(&st, &placed);
    if let Control::Layout { edges, .. } = &layout {
        info!(
            links = edges.len(),
            blocks = placed.len(),
            "applying the canvas"
        );
    }
    state.send(layout)
}

/// Drop the widget's layout: the hub goes back to its `[[layout]]` seed.
#[tauri::command]
fn canvas_reset(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    info!("back to the hub seed");
    state.send(Control::Layout {
        screens: Vec::new(),
        edges: Vec::new(),
        options: state.shared.snapshot().options,
        canvas: Vec::new(),
    })
}

// ---- start -------------------------------------------------------------

fn init_logging() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let path = dirs::home_dir().map(|h| h.join("Library/Logs/rawkvm-widget.log"));
    let file = path.as_ref().and_then(|p| {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    let fmt = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter);
    match file {
        Some(f) => fmt.with_writer(Mutex::new(f)).init(),
        None => fmt.init(),
    }
}

fn config_path() -> PathBuf {
    AgentConfig::default_path()
        .map(|p| p.with_file_name("widget.toml"))
        .unwrap_or_else(|_| PathBuf::from("widget.toml"))
}

pub fn run() {
    init_logging();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        "rawkvm-widget starting"
    );
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            let shared = WidgetShared::new(move |_| refresh(&handle));
            let state = Arc::new(AppState {
                config: config_path(),
                shared,
                tx: Mutex::new(None),
                task: Mutex::new(None),
                tray_key: Mutex::new(String::new()),
            });
            app.manage(state.clone());

            let v = state.view();
            let menu = build_menu(app.handle(), &v)?;
            TrayIconBuilder::with_id(TRAY)
                .icon(tauri::image::Image::from_bytes(include_bytes!(
                    "../icons/tray.png"
                ))?)
                .icon_as_template(true)
                .tooltip("RawKVM")
                .menu(&menu)
                .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
                .build(app)?;

            state.restart();
            if !v.paired {
                show_settings(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window only hides it: the app lives in
            // the menu bar until "Salir".
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_view,
            discover,
            pair,
            forget,
            switch_to,
            release_keys,
            pair_start,
            pair_stop,
            unpair,
            set_pointer_speed,
            set_fn_swap,
            canvas_view,
            canvas_drop,
            canvas_scale,
            canvas_apply,
            canvas_reset,
            set_settings,
            set_edge_options,
            get_autostart,
            set_autostart
        ])
        .run(tauri::generate_context!())
        .expect("running the RawKVM widget");
}
