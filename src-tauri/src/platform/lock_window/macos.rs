use super::{logical_rect, LockWindowRect};
use tauri::{window::Monitor, AppHandle, Webview, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

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
    let window = WebviewWindowBuilder::new(app, label, url)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .position(rect.x, rect.y)
        .inner_size(rect.width, rect.height)
        .build()
        .map_err(|error| error.to_string())?;

    let platform_result = (|| -> Result<(), String> {
        let webview: &Webview = window.as_ref();
        let host_window = webview.window();
        host_window
            .set_simple_fullscreen(true)
            .map_err(|error| format!("启用 macOS simple fullscreen 失败: {error}"))?;
        window
            .set_visible_on_all_workspaces(true)
            .map_err(|error| format!("设置 macOS 全工作区可见失败: {error}"))?;
        window
            .set_always_on_top(true)
            .map_err(|error| format!("设置 macOS 窗口置顶失败: {error}"))?;
        window
            .set_focus()
            .map_err(|error| format!("聚焦 macOS 休息窗口失败: {error}"))?;
        Ok(())
    })();

    if let Err(error) = platform_result {
        let _ = window.close();
        return Err(error);
    }
    Ok(window)
}
