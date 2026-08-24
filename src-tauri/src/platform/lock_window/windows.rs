use super::{logical_rect, LockWindowRect};
use tauri::{window::Monitor, AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub(super) fn monitor_rect(monitor: &Monitor) -> LockWindowRect {
    logical_rect(monitor, 200.0)
}

pub(super) fn create_lock_window(
    app: &AppHandle,
    label: String,
    url: WebviewUrl,
    monitor: &Monitor,
) -> Result<WebviewWindow, String> {
    let rect = monitor_rect(monitor);
    let window = WebviewWindowBuilder::new(app, label, url)
        .decorations(false)
        .transparent(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .position(rect.x, rect.y)
        .inner_size(rect.width, rect.height)
        .build()
        .map_err(|error| error.to_string())?;

    // Preserve the existing Windows behavior, including best-effort fullscreen/focus.
    let _ = window.set_fullscreen(true);
    let _ = window.set_focus();
    Ok(window)
}
