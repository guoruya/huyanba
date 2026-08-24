use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FilterConfig {
    pub enabled: bool,
    pub strength: f64,
    pub color_temp: f64,
}

impl FilterConfig {
    pub const fn new(enabled: bool, strength: f64, color_temp: f64) -> Self {
        Self {
            enabled,
            strength,
            color_temp,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DisplayFilterOperation {
    Enumerate,
    RegisterReconfigurationCallback,
    CaptureOriginalTable,
    ApplyTable,
    RestoreTable,
    RestoreColorSync,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DisplayFilterFailure {
    pub display_id: Option<u32>,
    pub operation: DisplayFilterOperation,
    pub error_code: Option<i32>,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DisplayFilterStatus {
    pub enabled: bool,
    pub config: Option<FilterConfig>,
    pub active_display_ids: Vec<u32>,
    pub original_table_display_ids: Vec<u32>,
    pub applied_display_ids: Vec<u32>,
    pub restored_display_ids: Vec<u32>,
    pub failures: Vec<DisplayFilterFailure>,
    pub color_sync_fallback_used: bool,
}

impl DisplayFilterStatus {
    pub fn failure_summary(&self) -> String {
        self.failures
            .iter()
            .map(|failure| match failure.display_id {
                Some(display_id) => format!(
                    "display {} {:?}: {}",
                    display_id, failure.operation, failure.message
                ),
                None => format!("{:?}: {}", failure.operation, failure.message),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{
    reapply, reset, reset_with_status, restore_color_sync_settings, set, set_with_status, status,
    take_reapply_requested,
};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use unsupported::{
    reapply, reset, reset_with_status, restore_color_sync_settings, set, set_with_status, status,
    take_reapply_requested,
};
#[cfg(target_os = "windows")]
pub use windows::{
    reapply, reset, reset_with_status, restore_color_sync_settings, set, set_with_status, status,
    take_reapply_requested,
};
