use super::{DisplayFilterFailure, DisplayFilterOperation, DisplayFilterStatus, FilterConfig};
use core_graphics::display::{
    CGDirectDisplayID, CGDisplay, CGDisplayRegisterReconfigurationCallback,
};
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::ffi::c_void;
use std::ptr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};

type CGGammaValue = f32;
type CGError = i32;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayGammaTableCapacity(display: CGDirectDisplayID) -> u32;
    fn CGGetDisplayTransferByTable(
        display: CGDirectDisplayID,
        capacity: u32,
        red_table: *mut CGGammaValue,
        green_table: *mut CGGammaValue,
        blue_table: *mut CGGammaValue,
        sample_count: *mut u32,
    ) -> CGError;
    fn CGSetDisplayTransferByTable(
        display: CGDirectDisplayID,
        table_size: u32,
        red_table: *const CGGammaValue,
        green_table: *const CGGammaValue,
        blue_table: *const CGGammaValue,
    ) -> CGError;
    fn CGDisplayRestoreColorSyncSettings();
}

#[derive(Clone)]
struct TransferTable {
    red: Vec<CGGammaValue>,
    green: Vec<CGGammaValue>,
    blue: Vec<CGGammaValue>,
}

impl TransferTable {
    fn scaled(&self, multipliers: (f64, f64, f64)) -> Self {
        Self {
            red: scale_channel(&self.red, multipliers.0),
            green: scale_channel(&self.green, multipliers.1),
            blue: scale_channel(&self.blue, multipliers.2),
        }
    }

    fn sample_count(&self) -> u32 {
        self.red.len() as u32
    }
}

#[derive(Default)]
struct MacDisplayFilterState {
    enabled: bool,
    last_config: Option<FilterConfig>,
    original_tables: HashMap<CGDirectDisplayID, TransferTable>,
    applied_display_ids: HashSet<CGDirectDisplayID>,
    last_status: DisplayFilterStatus,
}

static STATE: OnceLock<Mutex<MacDisplayFilterState>> = OnceLock::new();
static CALLBACK_REGISTERED: AtomicBool = AtomicBool::new(false);
static CALLBACK_REGISTRATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static REAPPLY_REQUESTED: AtomicBool = AtomicBool::new(false);

fn state() -> &'static Mutex<MacDisplayFilterState> {
    STATE.get_or_init(|| Mutex::new(MacDisplayFilterState::default()))
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

fn filter_multipliers(config: FilterConfig) -> (f64, f64, f64) {
    let (r, g, b) = temperature_to_rgb(config.color_temp);
    let factor = clamp(config.strength / 100.0, 0.0, 1.0);
    let mut mult_r = (1.0 - factor) + factor * r;
    let mut mult_g = (1.0 - factor) + factor * g;
    let mut mult_b = (1.0 - factor) + factor * b;

    let green_boost = 0.08 * factor;
    let red_cut = 0.18 * factor;
    let blue_cut = 0.35 * factor;
    mult_r = clamp(mult_r * (1.0 - red_cut), 0.0, 1.0);
    mult_g = clamp(mult_g * (1.0 + green_boost), 0.0, 1.0);
    mult_b = clamp(mult_b * (1.0 - blue_cut), 0.0, 1.0);
    (mult_r, mult_g, mult_b)
}

fn scale_channel(values: &[CGGammaValue], multiplier: f64) -> Vec<CGGammaValue> {
    values
        .iter()
        .map(|value| clamp(*value as f64 * multiplier, 0.0, 1.0) as CGGammaValue)
        .collect()
}

fn capture_transfer_table(display_id: CGDirectDisplayID) -> Result<TransferTable, CGError> {
    let capacity = unsafe { CGDisplayGammaTableCapacity(display_id) };
    if capacity == 0 {
        return Err(-1);
    }

    let mut red = vec![0.0; capacity as usize];
    let mut green = vec![0.0; capacity as usize];
    let mut blue = vec![0.0; capacity as usize];
    let mut sample_count = 0;
    let result = unsafe {
        CGGetDisplayTransferByTable(
            display_id,
            capacity,
            red.as_mut_ptr(),
            green.as_mut_ptr(),
            blue.as_mut_ptr(),
            &mut sample_count,
        )
    };
    if result != 0 {
        return Err(result);
    }
    if sample_count == 0 || sample_count > capacity {
        return Err(-1);
    }

    red.truncate(sample_count as usize);
    green.truncate(sample_count as usize);
    blue.truncate(sample_count as usize);
    Ok(TransferTable { red, green, blue })
}

fn set_transfer_table(display_id: CGDirectDisplayID, table: &TransferTable) -> Result<(), CGError> {
    let result = unsafe {
        CGSetDisplayTransferByTable(
            display_id,
            table.sample_count(),
            table.red.as_ptr(),
            table.green.as_ptr(),
            table.blue.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(result)
    }
}

fn sorted_ids<I>(ids: I) -> Vec<u32>
where
    I: IntoIterator<Item = u32>,
{
    let mut ids = ids.into_iter().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn core_graphics_failure(
    display_id: Option<u32>,
    operation: DisplayFilterOperation,
    error_code: CGError,
    message: impl Into<String>,
) -> DisplayFilterFailure {
    DisplayFilterFailure {
        display_id,
        operation,
        error_code: Some(error_code),
        message: message.into(),
    }
}

unsafe extern "C" fn display_reconfiguration_callback(
    _display: CGDirectDisplayID,
    flags: u32,
    _user_info: *const c_void,
) {
    const BEGIN_CONFIGURATION_FLAG: u32 = 1;
    if flags & BEGIN_CONFIGURATION_FLAG == 0 {
        REAPPLY_REQUESTED.store(true, Ordering::Release);
    }
}

fn register_callback_once<F>(
    registered: &AtomicBool,
    registration_lock: &Mutex<()>,
    register: F,
) -> Result<(), CGError>
where
    F: FnOnce() -> CGError,
{
    if registered.load(Ordering::Acquire) {
        return Ok(());
    }

    let _guard = registration_lock.lock().map_err(|_| -1)?;
    if registered.load(Ordering::Acquire) {
        return Ok(());
    }

    let result = register();
    if result != 0 {
        return Err(result);
    }
    registered.store(true, Ordering::Release);
    Ok(())
}

fn ensure_reconfiguration_callback() -> Result<(), CGError> {
    register_callback_once(
        &CALLBACK_REGISTERED,
        CALLBACK_REGISTRATION_LOCK.get_or_init(|| Mutex::new(())),
        || unsafe {
            CGDisplayRegisterReconfigurationCallback(display_reconfiguration_callback, ptr::null())
        },
    )
}

fn apply_locked(state: &mut MacDisplayFilterState, config: FilterConfig) -> DisplayFilterStatus {
    let mut status = DisplayFilterStatus {
        enabled: true,
        config: Some(config),
        ..DisplayFilterStatus::default()
    };
    let active_display_ids = match CGDisplay::active_displays() {
        Ok(display_ids) => sorted_ids(display_ids),
        Err(error_code) => {
            status.failures.push(core_graphics_failure(
                None,
                DisplayFilterOperation::Enumerate,
                error_code,
                "无法枚举活动显示器",
            ));
            state.enabled = true;
            state.last_config = Some(config);
            state.last_status = status.clone();
            return status;
        }
    };

    status.active_display_ids = active_display_ids.clone();
    let active_set = active_display_ids.iter().copied().collect::<HashSet<_>>();
    state
        .applied_display_ids
        .retain(|display_id| active_set.contains(display_id));

    let multipliers = filter_multipliers(config);
    for display_id in active_display_ids {
        if let Entry::Vacant(entry) = state.original_tables.entry(display_id) {
            match capture_transfer_table(display_id) {
                Ok(table) => {
                    entry.insert(table);
                }
                Err(error_code) => {
                    status.failures.push(core_graphics_failure(
                        Some(display_id),
                        DisplayFilterOperation::CaptureOriginalTable,
                        error_code,
                        "无法保存显示器原始 transfer table，已跳过该显示器",
                    ));
                    state.applied_display_ids.remove(&display_id);
                    continue;
                }
            }
        }

        let filtered_table = state.original_tables[&display_id].scaled(multipliers);
        match set_transfer_table(display_id, &filtered_table) {
            Ok(()) => {
                state.applied_display_ids.insert(display_id);
                status.applied_display_ids.push(display_id);
            }
            Err(error_code) => {
                state.applied_display_ids.remove(&display_id);
                status.failures.push(core_graphics_failure(
                    Some(display_id),
                    DisplayFilterOperation::ApplyTable,
                    error_code,
                    "应用显示器 transfer table 失败",
                ));
            }
        }
    }

    status.original_table_display_ids =
        sorted_ids(state.original_tables.keys().copied().collect::<Vec<_>>());
    status.applied_display_ids = sorted_ids(status.applied_display_ids);
    state.enabled = true;
    state.last_config = Some(config);
    state.last_status = status.clone();
    status
}

fn restore_locked(state: &mut MacDisplayFilterState) -> DisplayFilterStatus {
    let mut status = DisplayFilterStatus {
        original_table_display_ids: sorted_ids(
            state.original_tables.keys().copied().collect::<Vec<_>>(),
        ),
        ..DisplayFilterStatus::default()
    };
    match CGDisplay::active_displays() {
        Ok(display_ids) => status.active_display_ids = sorted_ids(display_ids),
        Err(error_code) => status.failures.push(core_graphics_failure(
            None,
            DisplayFilterOperation::Enumerate,
            error_code,
            "恢复前无法枚举活动显示器",
        )),
    }

    let original_tables = state
        .original_tables
        .iter()
        .map(|(display_id, table)| (*display_id, table.clone()))
        .collect::<Vec<_>>();
    for (display_id, table) in original_tables {
        match set_transfer_table(display_id, &table) {
            Ok(()) => status.restored_display_ids.push(display_id),
            Err(error_code) => status.failures.push(core_graphics_failure(
                Some(display_id),
                DisplayFilterOperation::RestoreTable,
                error_code,
                "恢复显示器原始 transfer table 失败",
            )),
        }
    }

    if (state.enabled && state.original_tables.is_empty()) || !status.failures.is_empty() {
        unsafe { CGDisplayRestoreColorSyncSettings() };
        status.color_sync_fallback_used = true;
    }

    status.restored_display_ids = sorted_ids(status.restored_display_ids);
    state.enabled = false;
    state.last_config = None;
    state.original_tables.clear();
    state.applied_display_ids.clear();
    state.last_status = status.clone();
    status
}

fn legacy_apply_result(status: &DisplayFilterStatus) -> Result<(), String> {
    if status.enabled && status.applied_display_ids.is_empty() && !status.failures.is_empty() {
        Err(status.failure_summary())
    } else {
        Ok(())
    }
}

pub fn set(filter_enabled: bool, strength: f64, color_temp: f64) -> Result<(), String> {
    let status = set_with_status(filter_enabled, strength, color_temp)?;
    legacy_apply_result(&status)
}

pub fn set_with_status(
    filter_enabled: bool,
    strength: f64,
    color_temp: f64,
) -> Result<DisplayFilterStatus, String> {
    if !filter_enabled {
        return reset_with_status();
    }

    let callback_error = ensure_reconfiguration_callback().err();
    let config = FilterConfig::new(true, strength, color_temp);
    let mut state = state()
        .lock()
        .map_err(|_| "macOS 显示滤镜状态被占用".to_string())?;
    let mut status = apply_locked(&mut state, config);
    if let Some(error_code) = callback_error {
        status.failures.push(core_graphics_failure(
            None,
            DisplayFilterOperation::RegisterReconfigurationCallback,
            error_code,
            "注册显示器变更回调失败；显示器变化后需显式调用 reapply",
        ));
        state.last_status = status.clone();
    }
    Ok(status)
}

pub fn reset() -> Result<(), String> {
    reset_with_status().map(|_| ())
}

pub fn reset_with_status() -> Result<DisplayFilterStatus, String> {
    let mut state = state()
        .lock()
        .map_err(|_| "macOS 显示滤镜状态被占用".to_string())?;
    Ok(restore_locked(&mut state))
}

pub fn reapply() -> Result<DisplayFilterStatus, String> {
    REAPPLY_REQUESTED.store(false, Ordering::Release);
    let mut state = state()
        .lock()
        .map_err(|_| "macOS 显示滤镜状态被占用".to_string())?;
    match (state.enabled, state.last_config) {
        (true, Some(config)) => Ok(apply_locked(&mut state, config)),
        _ => Ok(state.last_status.clone()),
    }
}

pub fn status() -> Result<DisplayFilterStatus, String> {
    state()
        .lock()
        .map(|state| state.last_status.clone())
        .map_err(|_| "macOS 显示滤镜状态被占用".to_string())
}

pub fn restore_color_sync_settings() -> Result<DisplayFilterStatus, String> {
    let mut state = state()
        .lock()
        .map_err(|_| "macOS 显示滤镜状态被占用".to_string())?;
    unsafe { CGDisplayRestoreColorSyncSettings() };
    state.enabled = false;
    state.last_config = None;
    state.original_tables.clear();
    state.applied_display_ids.clear();
    let status = DisplayFilterStatus {
        color_sync_fallback_used: true,
        ..DisplayFilterStatus::default()
    };
    state.last_status = status.clone();
    Ok(status)
}

pub fn take_reapply_requested() -> bool {
    REAPPLY_REQUESTED.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    use super::{filter_multipliers, register_callback_once, scale_channel, FilterConfig};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };

    #[test]
    fn zero_strength_keeps_the_original_table() {
        let multipliers = filter_multipliers(FilterConfig::new(true, 0.0, 4200.0));
        assert_eq!(multipliers, (1.0, 1.0, 1.0));
        assert_eq!(
            scale_channel(&[0.0, 0.25, 1.0], multipliers.0),
            [0.0, 0.25, 1.0]
        );
    }

    #[test]
    fn scaling_is_based_on_original_values_and_clamped() {
        assert_eq!(scale_channel(&[0.0, 0.5, 1.0], 0.5), [0.0, 0.25, 0.5]);
        assert_eq!(scale_channel(&[0.0, 0.5, 1.0], 2.0), [0.0, 1.0, 1.0]);
    }

    #[test]
    fn callback_registration_retries_failures_and_caches_only_success() {
        let registered = AtomicBool::new(false);
        let registration_lock = Mutex::new(());
        let attempts = AtomicUsize::new(0);

        let first = register_callback_once(&registered, &registration_lock, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            1001
        });
        assert_eq!(first, Err(1001));
        assert!(!registered.load(Ordering::Acquire));

        let second = register_callback_once(&registered, &registration_lock, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            0
        });
        assert_eq!(second, Ok(()));
        assert!(registered.load(Ordering::Acquire));

        let third = register_callback_once(&registered, &registration_lock, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            0
        });
        assert_eq!(third, Ok(()));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
