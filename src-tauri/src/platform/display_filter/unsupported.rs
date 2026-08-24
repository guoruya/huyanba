use super::{DisplayFilterFailure, DisplayFilterOperation, DisplayFilterStatus, FilterConfig};

fn unsupported_status() -> DisplayFilterStatus {
    DisplayFilterStatus {
        failures: vec![DisplayFilterFailure {
            display_id: None,
            operation: DisplayFilterOperation::ApplyTable,
            error_code: None,
            message: "当前平台不支持显示滤镜".into(),
        }],
        ..DisplayFilterStatus::default()
    }
}

pub fn set(_filter_enabled: bool, _strength: f64, _color_temp: f64) -> Result<(), String> {
    Err("当前平台不支持显示滤镜".into())
}

pub fn set_with_status(
    _filter_enabled: bool,
    _strength: f64,
    _color_temp: f64,
) -> Result<DisplayFilterStatus, String> {
    Ok(unsupported_status())
}

pub fn reset() -> Result<(), String> {
    Ok(())
}

pub fn reset_with_status() -> Result<DisplayFilterStatus, String> {
    Ok(DisplayFilterStatus::default())
}

pub fn reapply() -> Result<DisplayFilterStatus, String> {
    Ok(unsupported_status())
}

pub fn status() -> Result<DisplayFilterStatus, String> {
    Ok(unsupported_status())
}

pub fn restore_color_sync_settings() -> Result<DisplayFilterStatus, String> {
    reset_with_status()
}

pub fn take_reapply_requested() -> bool {
    false
}

#[allow(dead_code)]
fn _keep_filter_config_visible(_config: FilterConfig) {}
