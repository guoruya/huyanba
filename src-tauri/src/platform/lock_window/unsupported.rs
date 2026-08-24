use super::{logical_rect, LockWindowRect};
use tauri::{window::Monitor, AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub(super) fn monitor_rect(monitor: &Monitor) -> LockWindowRect {
    logical_rect(monitor, 0.0)
}

pub(super) fn create_lock_window(
    app: &AppHandle,
    label: String,
    url: WebviewUrl,
    monitor: &Monitor,
) -> Result<WebviewWindow, String> {
    let rect = monitor_rect(monitor);
    WebviewWindowBuilder::new(app, label, url)
        .decorations(false)
        .transparent(false)
        .resizable(false)
        .always_on_top(true)
        .position(rect.x, rect.y)
        .inner_size(rect.width, rect.height)
        .build()
        .map_err(|error| error.to_string())
}
