pub mod display_filter;
pub mod download_transport;
pub mod lock_window;
pub mod power_events;

pub use display_filter::{DisplayFilterStatus, FilterConfig};

/// Applies the platform display filter and returns per-display status.
pub fn apply_display_filter(config: FilterConfig) -> Result<DisplayFilterStatus, String> {
    display_filter::set_with_status(config.enabled, config.strength, config.color_temp)
}

/// Restores every display for which the application saved an original table.
pub fn restore_display_filter() -> Result<DisplayFilterStatus, String> {
    display_filter::reset_with_status()
}

/// Re-enumerates displays and replays the last enabled filter configuration.
pub fn reapply_display_filter() -> Result<DisplayFilterStatus, String> {
    display_filter::reapply()
}
