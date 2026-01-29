// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
use windows::Win32::UI::ColorSystem::SetDeviceGammaRamp;

#[derive(Default)]
struct LockState {
    labels: Mutex<Vec<String>>,
    last_update: Mutex<Option<LockUpdate>>,
}

#[derive(Default)]
struct AppState {
    allow_exit: AtomicBool,
}

const TRAY_ICON: tauri::image::Image<'static> = tauri::include_image!("icons/32x32.png");

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LockUpdate {
    time_text: String,
    date_text: String,
    rest_countdown: String,
    rest_paused: bool,
    allow_esc_exit: bool,
    weather_label: String,
    temp_range: String,
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

fn temperature_to_rgb(temp: f64) -> (f64, f64, f64) {
    let temp = clamp(temp, 1000.0, 40000.0) / 100.0;
    let (mut r, mut g, mut b);
    if temp <= 66.0 {
        r = 255.0;
        g = 99.4708025861 * temp.ln() - 161.1195681661;
        b = if temp <= 19.0 {
            0.0
        } else {
            138.5177312231 * (temp - 10.0).ln() - 305.0447927307
        };
    } else {
        r = 329.698727446 * (temp - 60.0).powf(-0.1332047592);
        g = 288.1221695283 * (temp - 60.0).powf(-0.0755148492);
        b = 255.0;
    }

    r = clamp(r, 0.0, 255.0);
    g = clamp(g, 0.0, 255.0);
    b = clamp(b, 0.0, 255.0);
    (r / 255.0, g / 255.0, b / 255.0)
}

fn apply_gamma(mult_r: f64, mult_g: f64, mult_b: f64) -> Result<(), String> {
    unsafe {
        let hdc = GetDC(HWND(0));
        if hdc.0 == 0 {
            return Err("无法获取显示设备句柄".into());
        }

        let mut ramp = [0u16; 256 * 3];
        for i in 0..256 {
            let base = i as f64 / 255.0;
            ramp[i] = clamp(base * 65535.0 * mult_r, 0.0, 65535.0).round() as u16;
            ramp[i + 256] = clamp(base * 65535.0 * mult_g, 0.0, 65535.0).round() as u16;
            ramp[i + 512] = clamp(base * 65535.0 * mult_b, 0.0, 65535.0).round() as u16;
        }

        let ok = SetDeviceGammaRamp(hdc, ramp.as_ptr() as *const _).as_bool();
        ReleaseDC(HWND(0), hdc);
        if !ok {
            return Err("设置色温失败".into());
        }
    }
    Ok(())
}

#[tauri::command]
fn set_gamma(filter_enabled: bool, strength: f64, color_temp: f64) -> Result<(), String> {
    if !filter_enabled {
        return apply_gamma(1.0, 1.0, 1.0);
    }
    let (r, g, b) = temperature_to_rgb(color_temp);
    let factor = clamp(strength / 100.0, 0.0, 1.0);
    let mut mult_r = (1.0 - factor) + factor * r;
    let mut mult_g = (1.0 - factor) + factor * g;
    let mut mult_b = (1.0 - factor) + factor * b;

    // Greenish bias to avoid reddish tint and reduce blue light.
    let green_boost = 0.08 * factor;
    let red_cut = 0.18 * factor;
    let blue_cut = 0.35 * factor;
    mult_r = clamp(mult_r * (1.0 - red_cut), 0.0, 1.0);
    mult_g = clamp(mult_g * (1.0 + green_boost), 0.0, 1.0);
    mult_b = clamp(mult_b * (1.0 - blue_cut), 0.0, 1.0);
    apply_gamma(mult_r, mult_g, mult_b)
}

#[tauri::command]
fn reset_gamma() -> Result<(), String> {
    apply_gamma(1.0, 1.0, 1.0)
}

#[tauri::command]
async fn show_lock_windows(
    app: tauri::AppHandle,
    state: tauri::State<'_, LockState>,
    end_at_ms: i64,
    paused: bool,
    paused_remaining: i64,
    allow_esc: bool,
) -> Result<(), String> {
    let mut labels = state.labels.lock().map_err(|_| "锁状态被占用")?;
    if !labels.is_empty() {
        for label in labels.iter() {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.set_always_on_top(true);
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        return Ok(());
    }

    let monitors = app
        .available_monitors()
        .map_err(|err| err.to_string())?;
    for (index, monitor) in monitors.into_iter().enumerate() {
        let label = format!("lockscreen-{}", index);
        let position = monitor.position();
        let size = monitor.size();
        let scale = monitor.scale_factor();
        let width = (size.width as f64 / scale).ceil() + 400.0;
        let height = (size.height as f64 / scale).ceil() + 400.0;
        let x = (position.x as f64 / scale).floor() - 200.0;
        let y = (position.y as f64 / scale).floor() - 200.0;

        let url = format!(
            "index.html?lockscreen=1&end={}&paused={}&remaining={}&allowEsc={}",
            end_at_ms,
            if paused { 1 } else { 0 },
            paused_remaining,
            if allow_esc { 1 } else { 0 }
        );
        let window = WebviewWindowBuilder::new(&app, label.clone(), WebviewUrl::App(url.into()))
        .decorations(false)
        .transparent(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .position(x, y)
        .inner_size(width, height)
        .build()
        .map_err(|err| err.to_string())?;

        let _ = window.set_fullscreen(true);
        let _ = window.set_focus();
        labels.push(label);
    }

    Ok(())
}

#[tauri::command]
fn hide_lock_windows(
    app: tauri::AppHandle,
    state: tauri::State<'_, LockState>,
) -> Result<(), String> {
    let mut labels = state.labels.lock().map_err(|_| "锁状态被占用")?;
    for label in labels.iter() {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.close();
        }
    }
    labels.clear();
    Ok(())
}

#[tauri::command]
fn broadcast_lock_update(app: tauri::AppHandle, payload: LockUpdate) -> Result<(), String> {
    if let Some(state) = app.try_state::<LockState>() {
        if let Ok(mut last) = state.last_update.lock() {
            *last = Some(payload.clone());
        }
    }
    for (_label, window) in app.webview_windows() {
        let _ = window.emit("lockscreen-update", payload.clone());
    }
    Ok(())
}

#[tauri::command]
fn get_lock_update(state: tauri::State<'_, LockState>) -> Option<LockUpdate> {
    state
        .last_update
        .lock()
        .ok()
        .and_then(|value| value.clone())
}

#[tauri::command]
fn lockscreen_action(app: tauri::AppHandle, action: String) -> Result<(), String> {
    for (_label, window) in app.webview_windows() {
        let _ = window.emit("lockscreen-action", action.clone());
    }
    Ok(())
}

#[tauri::command]
fn request_quit(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.allow_exit.store(true, Ordering::SeqCst);
    let _ = app.exit(0);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let tray_menu = MenuBuilder::new(app)
                .text("tray_show", "显示主界面")
                .text("tray_hide", "隐藏到托盘")
                .separator()
                .text("tray_quit", "退出")
                .build()?;

            let tray = TrayIconBuilder::new()
                .icon(TRAY_ICON.clone())
                .tooltip("护眼吧")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    let Some(window) = app.get_webview_window("main") else {
                        return;
                    };
                    match event {
                        TrayIconEvent::Click {
                            button,
                            button_state,
                            ..
                        } => {
                            if button == MouseButton::Left
                                && button_state == MouseButtonState::Up
                            {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        TrayIconEvent::DoubleClick { button, .. } => {
                            if button == MouseButton::Left {
                                let visible = window.is_visible().unwrap_or(true);
                                if visible {
                                    let _ = window.hide();
                                } else {
                                    let _ = window.show();
                                    let _ = window.set_focus();
                                }
                            }
                        }
                        _ => {}
                    }
                })
                .on_menu_event(|app, event| {
                    let Some(window) = app.get_webview_window("main") else {
                        return;
                    };
                    match event.id().as_ref() {
                        "tray_show" => {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                        "tray_hide" => {
                            let _ = window.hide();
                        }
                        "tray_quit" => {
                            if let Some(state) = app.try_state::<AppState>() {
                                state.allow_exit.store(true, Ordering::SeqCst);
                            }
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            app.manage(tray);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        if !state.allow_exit.load(Ordering::SeqCst) {
                            let _ = window.hide();
                            api.prevent_close();
                            return;
                        }
                    }
                    let _ = apply_gamma(1.0, 1.0, 1.0);
                }
                WindowEvent::Destroyed => {
                    let _ = apply_gamma(1.0, 1.0, 1.0);
                }
                _ => {}
            }
        })
        .manage(LockState::default())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            greet,
            set_gamma,
            reset_gamma,
            show_lock_windows,
            hide_lock_windows,
            broadcast_lock_update,
            get_lock_update,
            lockscreen_action,
            request_quit
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
