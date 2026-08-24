use super::{DisplayFilterStatus, FilterConfig};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
use windows::Win32::UI::ColorSystem::SetDeviceGammaRamp;

#[derive(Default)]
struct WindowsDisplayFilterState {
    last_config: Option<FilterConfig>,
    last_status: DisplayFilterStatus,
}

static STATE: OnceLock<Mutex<WindowsDisplayFilterState>> = OnceLock::new();

fn state() -> &'static Mutex<WindowsDisplayFilterState> {
    STATE.get_or_init(|| Mutex::new(WindowsDisplayFilterState::default()))
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

fn apply_config(filter_enabled: bool, strength: f64, color_temp: f64) -> Result<(), String> {
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

fn status_for_config(config: FilterConfig) -> DisplayFilterStatus {
    DisplayFilterStatus {
        enabled: config.enabled,
        config: config.enabled.then_some(config),
        active_display_ids: vec![0],
        original_table_display_ids: Vec::new(),
        applied_display_ids: config.enabled.then_some(0).into_iter().collect(),
        restored_display_ids: (!config.enabled).then_some(0).into_iter().collect(),
        failures: Vec::new(),
        color_sync_fallback_used: false,
    }
}

fn apply_and_record_locked(
    state: &mut WindowsDisplayFilterState,
    config: FilterConfig,
) -> Result<DisplayFilterStatus, String> {
    apply_config(config.enabled, config.strength, config.color_temp)?;
    let status = status_for_config(config);
    state.last_config = config.enabled.then_some(config);
    state.last_status = status.clone();
    Ok(status)
}

pub fn set(filter_enabled: bool, strength: f64, color_temp: f64) -> Result<(), String> {
    set_with_status(filter_enabled, strength, color_temp).map(|_| ())
}

pub fn set_with_status(
    filter_enabled: bool,
    strength: f64,
    color_temp: f64,
) -> Result<DisplayFilterStatus, String> {
    let config = FilterConfig::new(filter_enabled, strength, color_temp);
    let mut state = state()
        .lock()
        .map_err(|_| "Windows 显示滤镜状态被占用".to_string())?;
    apply_and_record_locked(&mut state, config)
}

pub fn reset() -> Result<(), String> {
    reset_with_status().map(|_| ())
}

pub fn reset_with_status() -> Result<DisplayFilterStatus, String> {
    let mut state = state()
        .lock()
        .map_err(|_| "Windows 显示滤镜状态被占用".to_string())?;
    apply_and_record_locked(&mut state, FilterConfig::new(false, 0.0, 6500.0))
}

pub fn reapply() -> Result<DisplayFilterStatus, String> {
    let mut state = state()
        .lock()
        .map_err(|_| "Windows 显示滤镜状态被占用".to_string())?;
    let config = state.last_config;
    match config {
        Some(config) => apply_and_record_locked(&mut state, config),
        None => Ok(state.last_status.clone()),
    }
}

pub fn status() -> Result<DisplayFilterStatus, String> {
    state()
        .lock()
        .map(|state| state.last_status.clone())
        .map_err(|_| "Windows 显示滤镜状态被占用".to_string())
}

pub fn restore_color_sync_settings() -> Result<DisplayFilterStatus, String> {
    reset_with_status()
}

pub fn take_reapply_requested() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{clamp, status_for_config, temperature_to_rgb, FilterConfig};

    #[test]
    fn clamp_preserves_existing_boundary_behavior() {
        assert_eq!(clamp(-1.0, 0.0, 1.0), 0.0);
        assert_eq!(clamp(0.5, 0.0, 1.0), 0.5);
        assert_eq!(clamp(2.0, 0.0, 1.0), 1.0);
    }

    #[test]
    fn temperature_is_clamped_to_existing_range() {
        assert_eq!(temperature_to_rgb(1.0), temperature_to_rgb(1000.0));
        assert_eq!(temperature_to_rgb(50000.0), temperature_to_rgb(40000.0));
    }

    #[test]
    fn disabled_status_records_a_restore_without_an_active_config() {
        let status = status_for_config(FilterConfig::new(false, 0.0, 6500.0));
        assert!(!status.enabled);
        assert_eq!(status.config, None);
        assert_eq!(status.restored_display_ids, [0]);
        assert!(status.applied_display_ids.is_empty());
    }
}
