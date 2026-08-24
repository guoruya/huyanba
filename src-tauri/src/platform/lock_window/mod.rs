use serde::{Deserialize, Serialize};
use tauri::{window::Monitor, AppHandle, WebviewUrl, WebviewWindow};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LockWindowRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

fn logical_rect_from_physical(
    position_x: i32,
    position_y: i32,
    width: u32,
    height: u32,
    scale_factor: f64,
    margin: f64,
) -> LockWindowRect {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    LockWindowRect {
        x: (position_x as f64 / scale_factor).floor() - margin,
        y: (position_y as f64 / scale_factor).floor() - margin,
        width: (width as f64 / scale_factor).ceil() + margin * 2.0,
        height: (height as f64 / scale_factor).ceil() + margin * 2.0,
    }
}

pub(crate) fn logical_rect(monitor: &Monitor, margin: f64) -> LockWindowRect {
    let position = monitor.position();
    let size = monitor.size();
    logical_rect_from_physical(
        position.x,
        position.y,
        size.width,
        size.height,
        monitor.scale_factor(),
        margin,
    )
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as implementation;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use unsupported as implementation;
#[cfg(target_os = "windows")]
use windows as implementation;

/// Creates and configures one platform-appropriate rest overlay window.
pub fn create_lock_window(
    app: &AppHandle,
    label: impl Into<String>,
    url: WebviewUrl,
    monitor: &Monitor,
) -> Result<WebviewWindow, String> {
    implementation::create_lock_window(app, label.into(), url, monitor)
}

#[cfg(test)]
mod tests {
    use super::logical_rect_from_physical;

    #[test]
    fn logical_rect_handles_retina_and_negative_origins() {
        let rect = logical_rect_from_physical(-3840, 0, 3840, 2160, 2.0, 0.0);
        assert_eq!(rect.x, -1920.0);
        assert_eq!(rect.y, 0.0);
        assert_eq!(rect.width, 1920.0);
        assert_eq!(rect.height, 1080.0);
    }

    #[test]
    fn windows_margin_preserves_existing_compensation() {
        let rect = logical_rect_from_physical(0, 0, 1920, 1080, 1.0, 200.0);
        assert_eq!(rect.x, -200.0);
        assert_eq!(rect.y, -200.0);
        assert_eq!(rect.width, 2320.0);
        assert_eq!(rect.height, 1480.0);
    }

    #[test]
    fn invalid_scale_uses_safe_logical_default() {
        let rect = logical_rect_from_physical(10, 20, 300, 400, 0.0, 0.0);
        assert_eq!(rect.x, 10.0);
        assert_eq!(rect.y, 20.0);
        assert_eq!(rect.width, 300.0);
        assert_eq!(rect.height, 400.0);
    }
}
