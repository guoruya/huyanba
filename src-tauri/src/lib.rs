// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod platform;
mod scheduler;
mod settings;

use rand::seq::SliceRandom;
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, AUTHORIZATION, REFERER, USER_AGENT},
    Url,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{hash_map::DefaultHasher, HashSet};
use std::env;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::RecvTimeoutError,
    Arc, Mutex,
};
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{
    menu::MenuBuilder,
    path::BaseDirectory,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewUrl, WindowEvent,
};

use platform::display_filter::{DisplayFilterStatus, FilterConfig};
use scheduler::{
    RestFinishedReason, RestScheduler, SchedulerConfig, SchedulerEvent, SchedulerPhase,
    SchedulerState, StateChangeReason,
};
use settings::{AppSettings, SettingsStore};

type MonitorSignature = Vec<(i32, i32, u32, u32, u64)>;
static SETTINGS_UPDATE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct LockWindowsState {
    labels: Vec<String>,
    monitor_signature: Option<MonitorSignature>,
}

#[derive(Default)]
struct LockState {
    operation: Mutex<()>,
    windows: Mutex<LockWindowsState>,
    last_update: Mutex<Option<LockUpdate>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
enum PalaceStagingRefreshState {
    #[default]
    Idle,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PalaceStagingRefreshStatus {
    state: PalaceStagingRefreshState,
    target_page: usize,
    current_committed_page: usize,
    processed_entries: usize,
    total_entries: usize,
    message: String,
    error_message: Option<String>,
    batch: Option<PalaceStagingBatchResult>,
}

impl Default for PalaceStagingRefreshStatus {
    fn default() -> Self {
        Self {
            state: PalaceStagingRefreshState::Idle,
            target_page: 1,
            current_committed_page: 1,
            processed_entries: 0,
            total_entries: 0,
            message: String::new(),
            error_message: None,
            batch: None,
        }
    }
}

#[derive(Default)]
struct AppState {
    allow_exit: AtomicBool,
    wallpaper_lock: Mutex<()>,
    palace_refresh_status: Mutex<PalaceStagingRefreshStatus>,
    cancel_refresh: AtomicBool,
    batch_in_progress: Arc<AtomicBool>,
}

struct BatchInProgressGuard {
    flag: Arc<AtomicBool>,
}

impl BatchInProgressGuard {
    fn try_acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self {
                flag: Arc::clone(flag),
            })
    }
}

impl Drop for BatchInProgressGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[cfg(not(target_os = "macos"))]
const TRAY_ICON: tauri::image::Image<'static> = tauri::include_image!("icons/32x32.png");
#[cfg(target_os = "macos")]
const MACOS_TRAY_ICON: tauri::image::Image<'static> =
    tauri::include_image!("icons/tray-template.png");
const WALLPAPER_CACHE_LIMIT: usize = 30;
const WALLPAPER_BATCH_SIZE: usize = 10;
const WALLPAPER_MANUAL_REFRESH_SIZE: usize = 4;
const WALLPAPER_BATCH_INTERVAL_SECS: i64 = 7 * 24 * 60 * 60;
const WALLPAPER_LIST_PAGE_SAMPLE: usize = 6;
const WALLPAPER_SEARCH_PAGE_SIZE: usize = 12;
const WALLPAPER_MIN_INTERVAL_SECS: i64 = 1;
const WALLPAPER_MIN_WIDTH: u32 = 1920;
const PALACE_BATCH_TOTAL_TIMEOUT_SECS: u64 = 120;
const MAX_FALLBACK_CHAIN_DEPTH: u32 = 3;
const PALACE_LIST_PAGE_SIZE: usize = 24;
const PALACE_STAGING_DIR_NAME: &str = "palace-staging";
const PALACE_STAGING_TEMP_DIR_NAME: &str = "palace-staging-next";
const PALACE_STAGING_BACKUP_DIR_NAME: &str = "palace-staging-prev";
const PALACE_STAGING_BATCH_SIZE: usize = 12;
const UNSPLASH_API_BASE: &str = "https://api.unsplash.com";
const UNSPLASH_TOPIC_SLUG: &str = "wallpapers";
const UNSPLASH_DOWNLOAD_WIDTH: u32 = 2560;
const UNSPLASH_DOWNLOAD_QUALITY: u32 = 80;
const PALACE_LIST_ENDPOINT: &str = "https://www.dpm.org.cn/searchs/royalb.html";

fn default_source_kind() -> String {
    "legacy".into()
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum WallpaperRemoteSource {
    #[default]
    Unsplash,
    Palace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WallpaperFile {
    path: String,
    added_at: i64,
    source_url: String,
    #[serde(default)]
    last_shown_at: i64,
    #[serde(default = "default_source_kind")]
    source_kind: String,
    #[serde(default)]
    remote_id: String,
    #[serde(default)]
    thumb_url: String,
    #[serde(default)]
    author_name: String,
    #[serde(default)]
    author_url: String,
    #[serde(default)]
    photo_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WallpaperState {
    files: Vec<WallpaperFile>,
    next_source_index: usize,
    next_show_index: usize,
    last_download_at: i64,
    last_batch_at: i64,
    #[serde(default, alias = "preferredSourceUrl")]
    fixed_source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WallpaperStorageConfig {
    #[serde(default)]
    custom_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UnsplashSettingsConfig {
    #[serde(default)]
    access_key: String,
}

#[derive(Default, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PrefetchStats {
    source_kind: String,
    source_label: String,
    list_successes: usize,
    added: usize,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WallpaperStorageSettings {
    current_dir: String,
    default_dir: String,
    is_default: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WallpaperStorageUpdateResult {
    settings: WallpaperStorageSettings,
    migrated_files: usize,
    restored_default: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum UnsplashConfigSource {
    AppConfig,
    EnvLocal,
    Env,
    None,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UnsplashSettings {
    effective_configured: bool,
    config_source: UnsplashConfigSource,
    has_stored_key: bool,
    masked_stored_key: Option<String>,
    palace_enabled: bool,
}

#[derive(Clone)]
struct ResolvedUnsplashAccessKey {
    access_key: Option<String>,
    source: UnsplashConfigSource,
    has_stored_key: bool,
    masked_stored_key: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RemoteWallpaperSearchResult {
    source: WallpaperRemoteSource,
    configured: bool,
    page: usize,
    per_page: usize,
    total_pages: usize,
    total_results: usize,
    has_next_page: bool,
    items: Vec<RemoteWallpaperSummary>,
    error_message: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RemoteWallpaperSummary {
    source: WallpaperRemoteSource,
    id: String,
    title: String,
    description: String,
    width: u32,
    height: u32,
    thumb_url: String,
    preview_url: String,
    credit_name: String,
    credit_url: String,
    photo_url: String,
    download_payload: Value,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LocalWallpaperSummary {
    path: String,
    source_url: String,
    source_kind: String,
    added_at: i64,
    last_shown_at: i64,
    thumb_url: String,
    author_name: String,
    author_url: String,
    photo_url: String,
    is_fixed: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadWallpaperResult {
    added: bool,
    source_url: String,
    path: String,
    is_fixed: bool,
    wallpaper: LocalWallpaperSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PalaceStagingState {
    items: Vec<PalaceStagingEntry>,
    last_batch_at: i64,
    current_page: usize,
    has_prev_page: bool,
    has_next_page: bool,
    max_page: usize,
}

impl Default for PalaceStagingState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            last_batch_at: 0,
            current_page: 1,
            has_prev_page: false,
            has_next_page: false,
            max_page: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PalaceStagingEntry {
    id: String,
    title: String,
    file_name: String,
    source_url: String,
    width: u32,
    height: u32,
    credit_name: String,
    credit_url: String,
    photo_url: String,
    added_at: i64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PalaceStagingWallpaperSummary {
    id: String,
    title: String,
    path: String,
    source_url: String,
    width: u32,
    height: u32,
    credit_name: String,
    credit_url: String,
    photo_url: String,
    added_at: i64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PalaceStagingBatchResult {
    fetched: usize,
    replaced_previous_batch: bool,
    page: usize,
    has_prev_page: bool,
    has_next_page: bool,
    max_page: usize,
    processed_count: usize,
    skipped_count: usize,
    remaining_items: usize,
    items: Vec<PalaceStagingWallpaperSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnsplashDownloadRequest {
    id: String,
    download_location: String,
    raw_url: String,
    thumb_url: String,
    photo_url: String,
    author_name: String,
    author_url: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PalaceDownloadRequest {
    id: String,
    title: String,
    image_url: String,
    thumb_url: String,
    photo_url: String,
    credit_name: String,
    credit_url: String,
    width: u32,
    height: u32,
}

#[derive(Clone)]
struct PalaceListEntry {
    id: String,
    title: String,
    thumb_url: String,
    detail_url: String,
}

#[derive(Clone)]
struct PalaceResolvedWallpaper {
    id: String,
    title: String,
    width: u32,
    height: u32,
    thumb_url: String,
    image_url: String,
    detail_url: String,
}

#[derive(Debug, Clone, Copy)]
struct PalacePageMeta {
    current_page: usize,
    has_prev_page: bool,
    has_next_page: bool,
    max_page: usize,
}

impl Default for PalacePageMeta {
    fn default() -> Self {
        Self {
            current_page: 1,
            has_prev_page: false,
            has_next_page: false,
            max_page: 1,
        }
    }
}

#[derive(Deserialize)]
struct UnsplashApiSearchResponse {
    total: usize,
    total_pages: usize,
    results: Vec<UnsplashApiPhoto>,
}

#[derive(Deserialize)]
struct UnsplashApiPhoto {
    id: String,
    width: u32,
    height: u32,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    alt_description: Option<String>,
    urls: UnsplashApiPhotoUrls,
    user: UnsplashApiUser,
    links: UnsplashApiLinks,
}

#[derive(Deserialize)]
struct UnsplashApiPhotoUrls {
    raw: String,
    regular: String,
    thumb: String,
}

#[derive(Deserialize)]
struct UnsplashApiUser {
    name: String,
    links: UnsplashApiUserLinks,
}

#[derive(Deserialize)]
struct UnsplashApiUserLinks {
    html: String,
}

#[derive(Deserialize)]
struct UnsplashApiLinks {
    html: String,
    download_location: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LockUpdate {
    time_text: String,
    date_text: String,
    rest_countdown: String,
    rest_paused: bool,
    allow_esc_exit: bool,
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn set_gamma(filter_enabled: bool, strength: f64, color_temp: f64) -> Result<(), String> {
    platform::display_filter::set(filter_enabled, strength, color_temp)
}

#[tauri::command]
fn set_gamma_with_status(
    filter_enabled: bool,
    strength: f64,
    color_temp: f64,
) -> Result<DisplayFilterStatus, String> {
    platform::apply_display_filter(FilterConfig::new(filter_enabled, strength, color_temp))
}

#[tauri::command]
fn reset_gamma() -> Result<(), String> {
    platform::display_filter::reset()
}

#[tauri::command]
fn get_display_filter_status() -> Result<DisplayFilterStatus, String> {
    platform::display_filter::status()
}

#[tauri::command]
fn restore_display_colors(app: AppHandle) -> Result<DisplayFilterStatus, String> {
    restore_colors_for_user(&app)
}

fn show_lock_windows_inner(
    app: &AppHandle,
    state: &LockState,
    end_at_ms: i64,
    paused: bool,
    paused_remaining: i64,
    allow_esc: bool,
) -> Result<(), String> {
    let _operation = state
        .operation
        .lock()
        .map_err(|_| "锁窗操作被占用".to_string())?;
    show_lock_windows_serialized(app, state, end_at_ms, paused, paused_remaining, allow_esc)
}

fn show_lock_windows_serialized(
    app: &AppHandle,
    state: &LockState,
    end_at_ms: i64,
    paused: bool,
    paused_remaining: i64,
    allow_esc: bool,
) -> Result<(), String> {
    let start = Instant::now();
    let (tracked_labels, saved_signature) = state
        .windows
        .lock()
        .map(|windows| (windows.labels.clone(), windows.monitor_signature.clone()))
        .map_err(|_| "锁状态被占用".to_string())?;
    let live_windows = tracked_labels
        .iter()
        .filter_map(|label| app.get_webview_window(label))
        .collect::<Vec<_>>();
    let existing_windows_complete = saved_signature.as_ref().is_some_and(|signature| {
        signature.len() == tracked_labels.len() && signature.len() == live_windows.len()
    });
    if existing_windows_complete {
        for window in live_windows {
            let _ = window.set_always_on_top(true);
            let _ = window.show();
            let _ = window.set_focus();
        }
        return Ok(());
    }

    {
        let mut lock_windows = state.windows.lock().map_err(|_| "锁状态被占用")?;
        lock_windows.labels.clear();
        lock_windows.monitor_signature = None;
    }
    for label in &tracked_labels {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.close();
        }
    }

    let monitors = app.available_monitors().map_err(|err| err.to_string())?;
    let created_monitor_signature = monitor_signature_from_monitors(&monitors);
    append_app_log(app, &format!("锁屏创建开始 monitors={}", monitors.len()));
    let mut created_labels: Vec<String> = Vec::with_capacity(monitors.len());
    for (index, monitor) in monitors.into_iter().enumerate() {
        let label = format!("lockscreen-{}", index);
        let url = format!(
            "index.html?lockscreen=1&end={}&paused={}&remaining={}&allowEsc={}",
            end_at_ms,
            if paused { 1 } else { 0 },
            paused_remaining,
            if allow_esc { 1 } else { 0 }
        );
        let window = match platform::lock_window::create_lock_window(
            app,
            label.clone(),
            WebviewUrl::App(url.into()),
            &monitor,
        ) {
            Ok(window) => window,
            Err(error) => {
                for created_label in &created_labels {
                    if let Some(window) = app.get_webview_window(created_label) {
                        let _ = window.close();
                    }
                }
                return Err(error);
            }
        };

        apply_default_window_icon(app, &window);
        created_labels.push(label);
    }
    let created_count = created_labels.len();
    let mut lock_windows = match state.windows.lock() {
        Ok(windows) => windows,
        Err(_) => {
            for created_label in &created_labels {
                if let Some(window) = app.get_webview_window(created_label) {
                    let _ = window.close();
                }
            }
            return Err("锁状态被占用".to_string());
        }
    };
    lock_windows.labels = created_labels;
    lock_windows.monitor_signature = Some(created_monitor_signature);
    drop(lock_windows);

    append_app_log(
        app,
        &format!(
            "锁屏创建完成 labels={} elapsed_ms={}",
            created_count,
            start.elapsed().as_millis()
        ),
    );
    Ok(())
}

#[tauri::command]
async fn show_lock_windows(
    app: AppHandle,
    state: tauri::State<'_, LockState>,
    end_at_ms: i64,
    paused: bool,
    paused_remaining: i64,
    allow_esc: bool,
) -> Result<(), String> {
    show_lock_windows_inner(&app, &state, end_at_ms, paused, paused_remaining, allow_esc)
}

fn hide_lock_windows_inner(app: &AppHandle, state: &LockState) -> Result<(), String> {
    let _operation = state
        .operation
        .lock()
        .map_err(|_| "锁窗操作被占用".to_string())?;
    hide_lock_windows_serialized(app, state)
}

fn hide_lock_windows_serialized(app: &AppHandle, state: &LockState) -> Result<(), String> {
    let start = Instant::now();
    let labels = {
        let mut lock_windows = state.windows.lock().map_err(|_| "锁状态被占用")?;
        lock_windows.monitor_signature = None;
        std::mem::take(&mut lock_windows.labels)
    };
    append_app_log(app, &format!("锁屏关闭开始 labels={}", labels.len()));
    for label in &labels {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.close();
        }
    }
    append_app_log(
        app,
        &format!("锁屏关闭完成 elapsed_ms={}", start.elapsed().as_millis()),
    );
    Ok(())
}

#[tauri::command]
fn hide_lock_windows(app: AppHandle, state: tauri::State<'_, LockState>) -> Result<(), String> {
    hide_lock_windows_inner(&app, &state)
}

fn scheduler_lock_args(state: &SchedulerState) -> (i64, bool, i64, bool) {
    let end_at_ms = state
        .rest_end_at_ms
        .unwrap_or_default()
        .min(i64::MAX as u64) as i64;
    let paused_remaining = state
        .paused_remaining_ms
        .unwrap_or_default()
        .saturating_add(999)
        / 1_000;
    (
        end_at_ms,
        state.paused,
        paused_remaining.min(i64::MAX as u64) as i64,
        state.config.allow_esc,
    )
}

fn sync_lock_windows_to_scheduler(app: &AppHandle, state: &SchedulerState) -> Result<(), String> {
    let lock_state = app
        .try_state::<LockState>()
        .ok_or_else(|| "锁屏状态尚未初始化".to_string())?;
    if state.phase == SchedulerPhase::Resting {
        let (end_at_ms, paused, paused_remaining, allow_esc) = scheduler_lock_args(state);
        show_lock_windows_inner(
            app,
            &lock_state,
            end_at_ms,
            paused,
            paused_remaining,
            allow_esc,
        )
    } else {
        hide_lock_windows_inner(app, &lock_state)
    }
}

fn rebuild_lock_windows(app: &AppHandle, state: &SchedulerState) -> Result<(), String> {
    let lock_state = app
        .try_state::<LockState>()
        .ok_or_else(|| "锁屏状态尚未初始化".to_string())?;
    let _operation = lock_state
        .operation
        .lock()
        .map_err(|_| "锁窗操作被占用".to_string())?;
    hide_lock_windows_serialized(app, &lock_state)?;
    if state.phase == SchedulerPhase::Resting {
        let (end_at_ms, paused, paused_remaining, allow_esc) = scheduler_lock_args(state);
        show_lock_windows_serialized(
            app,
            &lock_state,
            end_at_ms,
            paused,
            paused_remaining,
            allow_esc,
        )?;
    }
    Ok(())
}

fn dispatch_scheduler_event(app: &AppHandle, event: SchedulerEvent) {
    let _ = app.emit("rest-scheduler-event", event.clone());
    let should_reapply_filter = matches!(
        &event,
        SchedulerEvent::RestFinished {
            reason: RestFinishedReason::Sleep,
            ..
        } | SchedulerEvent::StateChanged {
            reason: StateChangeReason::Wake,
            ..
        }
    );
    if should_reapply_filter {
        match platform::reapply_display_filter() {
            Ok(status) => {
                let _ = app.emit("display-filter-status", status);
            }
            Err(error) => {
                append_app_log(app, &format!("唤醒后重放显示滤镜失败: {error}"));
                let _ = app.emit("display-filter-error", error);
            }
        }
    }

    if !matches!(
        &event,
        SchedulerEvent::RestStarted { .. } | SchedulerEvent::RestFinished { .. }
    ) {
        return;
    }

    let state = event.state().clone();
    let is_starting_rest = state.phase == SchedulerPhase::Resting;
    let dispatcher = app.clone();
    let callback_app = dispatcher.clone();
    if let Err(error) = dispatcher.run_on_main_thread(move || {
        if let Err(error) = sync_lock_windows_to_scheduler(&callback_app, &state) {
            append_app_log(&callback_app, &format!("同步休息窗口失败: {error}"));
            let _ = callback_app.emit("rest-scheduler-error", error.clone());
            if state.phase == SchedulerPhase::Resting {
                if let Some(scheduler) = callback_app.try_state::<RestScheduler>() {
                    let _ = scheduler.finish();
                }
            }
        }
    }) {
        append_app_log(app, &format!("提交休息窗口主线程任务失败: {error}"));
        if is_starting_rest {
            if let Some(scheduler) = app.try_state::<RestScheduler>() {
                let _ = scheduler.finish();
            }
        }
    }
}

fn monitor_signature_from_monitors(monitors: &[tauri::Monitor]) -> MonitorSignature {
    let mut signature = monitors
        .iter()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            (
                position.x,
                position.y,
                size.width,
                size.height,
                monitor.scale_factor().to_bits(),
            )
        })
        .collect::<Vec<_>>();
    signature.sort_unstable();
    signature
}

fn monitor_signature(app: &AppHandle) -> Result<MonitorSignature, String> {
    app.available_monitors()
        .map(|monitors| monitor_signature_from_monitors(&monitors))
        .map_err(|error| error.to_string())
}

fn lock_windows_need_rebuild(
    created_signature: Option<&MonitorSignature>,
    current_signature: &MonitorSignature,
    tracked_label_count: usize,
    live_window_count: usize,
) -> bool {
    created_signature.is_some_and(|created| {
        created != current_signature
            || tracked_label_count != created.len()
            || live_window_count != created.len()
    })
}

fn spawn_scheduler_bridge(
    app: AppHandle,
    receiver: std::sync::mpsc::Receiver<SchedulerEvent>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("rest-scheduler-events".to_string())
        .spawn(move || loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(event) => dispatch_scheduler_event(&app, event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if platform::power_events::take_wake_requested() {
                if let Some(scheduler) = app.try_state::<RestScheduler>() {
                    match scheduler.reset_after_wake() {
                        Ok(_) => append_app_log(&app, "检测到系统唤醒，已重置休息周期"),
                        Err(error) => {
                            let message = format!("系统唤醒后重置休息周期失败: {error}");
                            append_app_log(&app, &message);
                            let _ = app.emit("rest-scheduler-error", message);
                        }
                    }
                }
            }

            let display_reconfigured = platform::display_filter::take_reapply_requested();
            if display_reconfigured {
                match platform::reapply_display_filter() {
                    Ok(status) => {
                        let _ = app.emit("display-filter-status", status);
                    }
                    Err(error) => {
                        append_app_log(&app, &format!("显示器变更后重放滤镜失败: {error}"));
                        let _ = app.emit("display-filter-error", error);
                    }
                }
            }

            let Some(scheduler) = app.try_state::<RestScheduler>() else {
                continue;
            };
            let Ok(state) = scheduler.query() else {
                break;
            };
            if state.phase != SchedulerPhase::Resting {
                continue;
            }
            let Ok(signature) = monitor_signature(&app) else {
                continue;
            };
            let lock_window_snapshot = app.try_state::<LockState>().and_then(|lock_state| {
                lock_state
                    .windows
                    .lock()
                    .ok()
                    .map(|windows| (windows.monitor_signature.clone(), windows.labels.clone()))
            });
            let Some((created_signature, tracked_labels)) = lock_window_snapshot else {
                continue;
            };
            let live_window_count = tracked_labels
                .iter()
                .filter(|label| app.get_webview_window(label).is_some())
                .count();
            let lock_windows_changed = lock_windows_need_rebuild(
                created_signature.as_ref(),
                &signature,
                tracked_labels.len(),
                live_window_count,
            );
            if !lock_windows_changed && !display_reconfigured {
                continue;
            }

            let dispatcher = app.clone();
            let callback_app = dispatcher.clone();
            let state = state.clone();
            if let Err(error) = dispatcher.run_on_main_thread(move || {
                if let Err(error) = rebuild_lock_windows(&callback_app, &state) {
                    append_app_log(
                        &callback_app,
                        &format!("显示器变更后重建休息窗口失败: {error}"),
                    );
                    let _ = callback_app.emit("rest-scheduler-error", error);
                    if let Some(scheduler) = callback_app.try_state::<RestScheduler>() {
                        let _ = scheduler.finish();
                    }
                }
            }) {
                append_app_log(&app, &format!("提交休息窗口重建任务失败: {error}"));
                if let Some(scheduler) = app.try_state::<RestScheduler>() {
                    let _ = scheduler.finish();
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("无法启动休息调度事件桥: {error}"))
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
fn lockscreen_action(
    app: AppHandle,
    scheduler: tauri::State<'_, RestScheduler>,
    action: String,
) -> Result<(), String> {
    append_app_log(&app, &format!("锁屏动作: {}", action));
    match action.as_str() {
        "exit" => {
            scheduler.finish().map_err(|error| error.to_string())?;
        }
        "toggle_pause" => {
            let current = scheduler.query().map_err(|error| error.to_string())?;
            if current.phase == SchedulerPhase::Resting {
                if current.paused {
                    scheduler.resume().map_err(|error| error.to_string())?;
                } else {
                    scheduler.pause().map_err(|error| error.to_string())?;
                }
            }
        }
        _ => return Err(format!("未知锁屏动作: {action}")),
    }
    for (_label, window) in app.webview_windows() {
        let _ = window.emit("lockscreen-action", action.clone());
    }
    Ok(())
}

#[tauri::command]
fn get_app_settings(store: tauri::State<'_, SettingsStore>) -> Result<AppSettings, String> {
    store.load().map_err(|error| error.to_string())
}

trait SettingsUpdateBackend {
    fn load_settings(&mut self) -> Result<AppSettings, String>;
    fn save_settings(&mut self, settings: &AppSettings) -> Result<(), String>;
    fn configure_scheduler(&mut self, config: SchedulerConfig) -> Result<(), String>;
    fn query_scheduler_config(&mut self) -> Result<SchedulerConfig, String>;
}

struct LiveSettingsUpdateBackend<'a> {
    store: &'a SettingsStore,
    scheduler: &'a RestScheduler,
}

impl SettingsUpdateBackend for LiveSettingsUpdateBackend<'_> {
    fn load_settings(&mut self) -> Result<AppSettings, String> {
        self.store.load().map_err(|error| error.to_string())
    }

    fn save_settings(&mut self, settings: &AppSettings) -> Result<(), String> {
        self.store.save(settings).map_err(|error| error.to_string())
    }

    fn configure_scheduler(&mut self, config: SchedulerConfig) -> Result<(), String> {
        self.scheduler
            .configure(config)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn query_scheduler_config(&mut self) -> Result<SchedulerConfig, String> {
        self.scheduler
            .query()
            .map(|state| state.config)
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SettingsUpdateFailure {
    primary: String,
    rollback_target: &'static str,
    rollback_failures: Vec<String>,
}

impl fmt::Display for SettingsUpdateFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.primary)?;
        if self.rollback_failures.is_empty() {
            write!(
                formatter,
                "；已验证{}恢复到更新前状态",
                self.rollback_target
            )
        } else {
            write!(
                formatter,
                "；{}回滚未完全成功：{}",
                self.rollback_target,
                self.rollback_failures.join("；")
            )
        }
    }
}

fn rollback_persisted_settings(
    backend: &mut impl SettingsUpdateBackend,
    previous: &AppSettings,
    failures: &mut Vec<String>,
) {
    if let Err(error) = backend.save_settings(previous) {
        failures.push(format!("恢复设置文件写入失败: {error}"));
    }
    match backend.load_settings() {
        Ok(current) if current == *previous => {}
        Ok(_) => failures.push("恢复设置文件后读取验证不一致".to_string()),
        Err(error) => failures.push(format!("恢复设置文件后读取验证失败: {error}")),
    }
}

fn rollback_scheduler_config(
    backend: &mut impl SettingsUpdateBackend,
    previous: SchedulerConfig,
    failures: &mut Vec<String>,
) {
    if let Err(error) = backend.configure_scheduler(previous) {
        failures.push(format!("恢复休息调度配置失败: {error}"));
    }
    match backend.query_scheduler_config() {
        Ok(current) if current == previous => {}
        Ok(_) => failures.push("恢复休息调度配置后查询验证不一致".to_string()),
        Err(error) => failures.push(format!("恢复休息调度配置后查询验证失败: {error}")),
    }
}

fn update_app_settings_with_backend(
    backend: &mut impl SettingsUpdateBackend,
    settings: &AppSettings,
) -> Result<(), String> {
    settings.validate().map_err(|error| error.to_string())?;
    let previous = backend
        .load_settings()
        .map_err(|error| format!("读取更新前设置失败: {error}"))?;

    if let Err(error) = backend.save_settings(settings) {
        let mut rollback_failures = Vec::new();
        rollback_persisted_settings(backend, &previous, &mut rollback_failures);
        return Err(SettingsUpdateFailure {
            primary: format!("保存应用设置失败: {error}"),
            rollback_target: "设置文件",
            rollback_failures,
        }
        .to_string());
    }

    if let Err(error) = backend.configure_scheduler(SchedulerConfig::from(settings)) {
        let mut rollback_failures = Vec::new();
        rollback_persisted_settings(backend, &previous, &mut rollback_failures);
        rollback_scheduler_config(
            backend,
            SchedulerConfig::from(&previous),
            &mut rollback_failures,
        );
        return Err(SettingsUpdateFailure {
            primary: format!("更新休息调度失败: {error}"),
            rollback_target: "设置文件和休息调度配置",
            rollback_failures,
        }
        .to_string());
    }

    Ok(())
}

#[tauri::command]
fn update_app_settings(
    settings: AppSettings,
    store: tauri::State<'_, SettingsStore>,
    scheduler: tauri::State<'_, RestScheduler>,
) -> Result<AppSettings, String> {
    let _guard = SETTINGS_UPDATE_LOCK
        .lock()
        .map_err(|_| "设置更新串行锁已损坏".to_string())?;
    let mut backend = LiveSettingsUpdateBackend {
        store: store.inner(),
        scheduler: scheduler.inner(),
    };
    update_app_settings_with_backend(&mut backend, &settings)?;
    Ok(settings)
}

#[tauri::command]
fn get_scheduler_state(
    scheduler: tauri::State<'_, RestScheduler>,
) -> Result<SchedulerState, String> {
    scheduler.query().map_err(|error| error.to_string())
}

#[tauri::command]
fn start_rest_now(scheduler: tauri::State<'_, RestScheduler>) -> Result<SchedulerState, String> {
    scheduler.start_now().map_err(|error| error.to_string())
}

#[tauri::command]
fn finish_rest(scheduler: tauri::State<'_, RestScheduler>) -> Result<SchedulerState, String> {
    scheduler.finish().map_err(|error| error.to_string())
}

#[tauri::command]
fn toggle_rest_pause(scheduler: tauri::State<'_, RestScheduler>) -> Result<SchedulerState, String> {
    let current = scheduler.query().map_err(|error| error.to_string())?;
    if current.phase != SchedulerPhase::Resting {
        return Ok(current);
    }
    if current.paused {
        scheduler.resume().map_err(|error| error.to_string())
    } else {
        scheduler.pause().map_err(|error| error.to_string())
    }
}

#[tauri::command]
fn restart_rest_cycle(
    scheduler: tauri::State<'_, RestScheduler>,
) -> Result<SchedulerState, String> {
    scheduler.restart_cycle().map_err(|error| error.to_string())
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as i64
}

fn hash_url(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn image_extension_from_url(url: &str) -> &'static str {
    Path::new(url)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .filter(|ext| matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp"))
        .map(|ext| match ext.as_str() {
            "png" => "png",
            "webp" => "webp",
            _ => "jpg",
        })
        .unwrap_or("jpg")
}

fn load_wallpaper_state(path: &Path) -> WallpaperState {
    let Ok(data) = fs::read_to_string(path) else {
        return WallpaperState::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_wallpaper_state(path: &Path, state: &WallpaperState) -> Result<(), String> {
    let data = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
}

fn load_palace_staging_state(path: &Path) -> PalaceStagingState {
    let Ok(data) = fs::read_to_string(path) else {
        return PalaceStagingState::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_palace_staging_state(path: &Path, state: &PalaceStagingState) -> Result<(), String> {
    let data = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
}

fn load_wallpaper_storage_config(path: &Path) -> WallpaperStorageConfig {
    let Ok(data) = fs::read_to_string(path) else {
        return WallpaperStorageConfig::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_wallpaper_storage_config(
    path: &Path,
    config: &WallpaperStorageConfig,
) -> Result<(), String> {
    let data = serde_json::to_string_pretty(config).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
}

fn load_unsplash_settings_config(path: &Path) -> UnsplashSettingsConfig {
    let Ok(data) = fs::read_to_string(path) else {
        return UnsplashSettingsConfig::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_unsplash_settings_config(
    path: &Path,
    config: &UnsplashSettingsConfig,
) -> Result<(), String> {
    let data = serde_json::to_string_pretty(config).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn wallpaper_default_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .resolve("wallpapers", BaseDirectory::AppCache)
        .map_err(|err| err.to_string())
}

fn wallpaper_storage_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .resolve("", BaseDirectory::AppConfig)
        .map_err(|err| err.to_string())?;
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.join("wallpaper-storage.json"))
}

fn unsplash_settings_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .resolve("", BaseDirectory::AppConfig)
        .map_err(|err| err.to_string())?;
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.join("unsplash-settings.json"))
}

fn wallpaper_storage_settings_from_config(
    app: &AppHandle,
    config: &WallpaperStorageConfig,
) -> Result<WallpaperStorageSettings, String> {
    let default_dir = wallpaper_default_dir(app)?;
    let current_dir = if config.custom_dir.trim().is_empty() {
        default_dir.clone()
    } else {
        PathBuf::from(config.custom_dir.trim())
    };
    Ok(WallpaperStorageSettings {
        current_dir: path_to_string(&current_dir),
        default_dir: path_to_string(&default_dir),
        is_default: current_dir == default_dir,
    })
}

fn get_wallpaper_storage_settings_inner(
    app: &AppHandle,
) -> Result<WallpaperStorageSettings, String> {
    let config_path = wallpaper_storage_config_path(app)?;
    let config = load_wallpaper_storage_config(&config_path);
    wallpaper_storage_settings_from_config(app, &config)
}

fn read_unsplash_settings_config(app: &AppHandle) -> UnsplashSettingsConfig {
    let Ok(config_path) = unsplash_settings_config_path(app) else {
        return UnsplashSettingsConfig::default();
    };
    load_unsplash_settings_config(&config_path)
}

fn mask_access_key(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars = trimmed.chars().collect::<Vec<_>>();
    let len = chars.len();
    let (head_len, tail_len) = if len > 8 {
        (4, 4)
    } else {
        (2.min(len), 2.min(len))
    };
    let head = chars.iter().take(head_len).collect::<String>();
    let tail = chars
        .iter()
        .skip(len.saturating_sub(tail_len))
        .collect::<String>();
    Some(if len <= head_len + tail_len {
        format!("{}...", head)
    } else {
        format!("{}...{}", head, tail)
    })
}

fn resolve_unsplash_access_key(app: &AppHandle) -> ResolvedUnsplashAccessKey {
    let stored_config = read_unsplash_settings_config(app);
    let stored_key = stored_config.access_key.trim().to_string();
    let has_stored_key = !stored_key.is_empty();
    let masked_stored_key = mask_access_key(&stored_key);
    if has_stored_key {
        return ResolvedUnsplashAccessKey {
            access_key: Some(stored_key),
            source: UnsplashConfigSource::AppConfig,
            has_stored_key,
            masked_stored_key,
        };
    }

    if let Some(value) = read_local_env_value("UNSPLASH_ACCESS_KEY") {
        return ResolvedUnsplashAccessKey {
            access_key: Some(value),
            source: UnsplashConfigSource::EnvLocal,
            has_stored_key,
            masked_stored_key,
        };
    }

    if let Ok(value) = env::var("UNSPLASH_ACCESS_KEY") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return ResolvedUnsplashAccessKey {
                access_key: Some(value),
                source: UnsplashConfigSource::Env,
                has_stored_key,
                masked_stored_key,
            };
        }
    }

    ResolvedUnsplashAccessKey {
        access_key: None,
        source: UnsplashConfigSource::None,
        has_stored_key,
        masked_stored_key,
    }
}

fn unsplash_settings_from_resolved(resolved: ResolvedUnsplashAccessKey) -> UnsplashSettings {
    UnsplashSettings {
        effective_configured: resolved.access_key.is_some(),
        config_source: resolved.source,
        has_stored_key: resolved.has_stored_key,
        masked_stored_key: resolved.masked_stored_key,
        palace_enabled: palace_wallpaper_source_enabled(),
    }
}

fn get_unsplash_settings_inner(app: &AppHandle) -> UnsplashSettings {
    unsplash_settings_from_resolved(resolve_unsplash_access_key(app))
}

fn allow_wallpaper_dir_on_scope(app: &AppHandle, dir: &Path) -> Result<(), String> {
    app.asset_protocol_scope()
        .allow_directory(dir, true)
        .map_err(|err| err.to_string())
}

fn ensure_wallpaper_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let settings = get_wallpaper_storage_settings_inner(app)?;
    let dir = PathBuf::from(settings.current_dir);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    allow_wallpaper_dir_on_scope(app, &dir)?;
    Ok(dir)
}

fn palace_staging_dir(root_dir: &Path) -> PathBuf {
    root_dir.join(PALACE_STAGING_DIR_NAME)
}

fn palace_staging_temp_dir(root_dir: &Path) -> PathBuf {
    root_dir.join(PALACE_STAGING_TEMP_DIR_NAME)
}

fn palace_staging_backup_dir(root_dir: &Path) -> PathBuf {
    root_dir.join(PALACE_STAGING_BACKUP_DIR_NAME)
}

fn palace_staging_index_path(stage_dir: &Path) -> PathBuf {
    stage_dir.join("index.json")
}

fn append_line(path: &Path, message: &str) {
    let ts = now_ts();
    let line = format!("[{}] {}\n", ts, message);
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn ensure_dir_ready(path: &Path, label: &str) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| format!("{}不可创建或不可写: {}", label, err))
}

fn append_app_log(app: &AppHandle, message: &str) {
    let dir = match ensure_wallpaper_dir(app) {
        Ok(dir) => dir,
        Err(_) => return,
    };
    append_line(&dir.join("app.log"), message);
}

fn append_wallpaper_log(app: &AppHandle, message: &str) {
    let Ok(dir) = ensure_wallpaper_dir(app) else {
        return;
    };
    append_line(&dir.join("prefetch.log"), message);
}

fn append_palace_debug_log(app: &AppHandle, message: &str) {
    let Ok(dir) = ensure_wallpaper_dir(app) else {
        return;
    };
    append_line(&dir.join("palace-debug.log"), message);
}

fn append_palace_debug(app: Option<&AppHandle>, message: &str) {
    if let Some(app) = app {
        append_palace_debug_log(app, message);
    }
}

fn summarize_for_log(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut summary = String::new();
    for ch in compact.chars().take(max_chars) {
        summary.push(ch);
    }
    if compact.chars().count() > max_chars {
        summary.push_str("...");
    }
    summary
}

fn is_directory_empty(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(true);
    }
    let mut entries = fs::read_dir(path).map_err(|err| err.to_string())?;
    Ok(entries.next().is_none())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|err| err.to_string())?;
    for entry in fs::read_dir(source).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn move_or_copy_path(source: &Path, target: &Path) -> Result<(), String> {
    match fs::rename(source, target) {
        Ok(_) => Ok(()),
        Err(_) => {
            if source.is_dir() {
                copy_dir_recursive(source, target)?;
                fs::remove_dir_all(source).map_err(|err| err.to_string())
            } else {
                fs::copy(source, target).map_err(|err| err.to_string())?;
                fs::remove_file(source).map_err(|err| err.to_string())
            }
        }
    }
}

fn rewrite_wallpaper_state_paths(
    state_path: &Path,
    source_dir: &Path,
    target_dir: &Path,
) -> Result<(), String> {
    if !state_path.exists() {
        return Ok(());
    }

    let mut wall_state = load_wallpaper_state(state_path);
    let mut changed = false;

    for file in &mut wall_state.files {
        let current_path = PathBuf::from(file.path.trim());
        let next_path = if current_path.is_absolute() {
            if let Ok(relative) = current_path.strip_prefix(source_dir) {
                Some(target_dir.join(relative))
            } else if current_path.starts_with(target_dir) {
                Some(current_path.clone())
            } else {
                None
            }
        } else if !file.path.trim().is_empty() {
            Some(target_dir.join(file.path.trim()))
        } else {
            None
        };

        if let Some(next_path) = next_path {
            let next_value = path_to_string(&next_path);
            if file.path != next_value {
                file.path = next_value;
                changed = true;
            }
        }
    }

    if changed {
        save_wallpaper_state(state_path, &wall_state)?;
    }

    Ok(())
}

fn migrate_wallpaper_dir_contents(source: &Path, target: &Path) -> Result<usize, String> {
    if !source.exists() {
        return Ok(0);
    }
    ensure_dir_ready(target, "目标目录")?;
    let entries = fs::read_dir(source).map_err(|err| err.to_string())?;
    let mut moved_pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut moved = 0;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if target_path.exists() {
            return Err("目标目录必须为空，请选择一个空文件夹。".into());
        }
        if let Err(err) = move_or_copy_path(&source_path, &target_path) {
            for (original_source, moved_target) in moved_pairs.iter().rev() {
                let _ = move_or_copy_path(moved_target, original_source);
            }
            return Err(format!("迁移壁纸失败: {}", err));
        }
        moved_pairs.push((source_path, target_path));
        moved += 1;
    }
    if let Err(err) = rewrite_wallpaper_state_paths(&target.join("index.json"), source, target) {
        for (original_source, moved_target) in moved_pairs.iter().rev() {
            let _ = move_or_copy_path(moved_target, original_source);
        }
        return Err(format!("迁移壁纸失败: {}", err));
    }
    if source.exists() && is_directory_empty(source).unwrap_or(false) {
        let _ = fs::remove_dir(source);
    }
    Ok(moved)
}

#[tauri::command]
fn log_app(app: AppHandle, message: String) -> Result<(), String> {
    append_app_log(&app, &message);
    Ok(())
}

fn emit_wallpaper_storage_updated(
    app: &AppHandle,
    settings: &WallpaperStorageSettings,
) -> Result<(), String> {
    app.emit("wallpaper-storage-updated", settings.clone())
        .map_err(|err| err.to_string())
}

fn prune_missing_files(state: &mut WallpaperState) {
    state.files.retain(|entry| Path::new(&entry.path).exists());
    if !state.fixed_source_url.is_empty()
        && !state
            .files
            .iter()
            .any(|item| item.source_url == state.fixed_source_url)
    {
        state.fixed_source_url.clear();
    }
}

fn clamp_wallpaper_indices(state: &mut WallpaperState) {
    if state.files.is_empty() {
        state.next_show_index = 0;
        state.next_source_index = 0;
        return;
    }
    if state.next_show_index >= state.files.len() {
        state.next_show_index %= state.files.len();
    }
    if state.next_source_index >= state.files.len() {
        state.next_source_index %= state.files.len();
    }
}

fn file_timestamp_secs(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .or_else(|_| metadata.created())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_else(now_ts)
}

fn is_supported_wallpaper_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp")
    )
}

fn repair_wallpaper_paths_for_dir(state: &mut WallpaperState, dir: &Path) -> usize {
    let mut repaired = 0;
    for file in &mut state.files {
        let current_path = PathBuf::from(file.path.trim());
        if current_path.exists() {
            continue;
        }
        let Some(name) = current_path.file_name() else {
            continue;
        };
        let candidate = dir.join(name);
        if candidate.exists() {
            file.path = path_to_string(&candidate);
            repaired += 1;
        }
    }
    repaired
}

fn recover_wallpaper_entries_from_dir(
    dir: &Path,
    state: &mut WallpaperState,
) -> Result<usize, String> {
    let mut known_paths: HashSet<String> =
        state.files.iter().map(|file| file.path.clone()).collect();
    let mut recovered = 0;

    for entry in fs::read_dir(dir).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        if !is_supported_wallpaper_file(&path) {
            continue;
        }

        let path_value = path_to_string(&path);
        if known_paths.contains(&path_value) {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.metadata().map_err(|err| err.to_string())?;
        state.files.push(WallpaperFile {
            path: path_value.clone(),
            added_at: file_timestamp_secs(&metadata),
            source_url: format!("local-file://{}", file_name),
            last_shown_at: 0,
            source_kind: "local".into(),
            remote_id: String::new(),
            thumb_url: String::new(),
            author_name: String::new(),
            author_url: String::new(),
            photo_url: String::new(),
        });
        known_paths.insert(path_value);
        recovered += 1;
    }

    Ok(recovered)
}

fn normalize_wallpaper_state(
    dir: &Path,
    state: &mut WallpaperState,
) -> Result<(usize, usize), String> {
    let repaired = repair_wallpaper_paths_for_dir(state, dir);
    prune_missing_files(state);
    let recovered = recover_wallpaper_entries_from_dir(dir, state)?;
    clamp_wallpaper_indices(state);
    Ok((repaired, recovered))
}

fn prune_missing_palace_staging_files(stage_dir: &Path, state: &mut PalaceStagingState) {
    state
        .items
        .retain(|entry| stage_dir.join(&entry.file_name).exists());
}

fn normalize_palace_staging_pagination(state: &mut PalaceStagingState) {
    if state.current_page == 0 {
        state.current_page = 1;
    }
    if state.max_page == 0 {
        state.max_page = state.current_page.max(1);
    }
    if state.max_page < state.current_page {
        state.max_page = state.current_page;
    }
    state.has_prev_page = state.current_page > 1;
    state.has_next_page = state.current_page < state.max_page;
}

fn normalize_palace_staging_state(stage_dir: &Path, state: &mut PalaceStagingState) {
    prune_missing_palace_staging_files(stage_dir, state);
    normalize_palace_staging_pagination(state);
}

fn palace_staging_summary(
    stage_dir: &Path,
    entry: &PalaceStagingEntry,
) -> PalaceStagingWallpaperSummary {
    PalaceStagingWallpaperSummary {
        id: entry.id.clone(),
        title: entry.title.clone(),
        path: path_to_string(&stage_dir.join(&entry.file_name)),
        source_url: entry.source_url.clone(),
        width: entry.width,
        height: entry.height,
        credit_name: entry.credit_name.clone(),
        credit_url: entry.credit_url.clone(),
        photo_url: entry.photo_url.clone(),
        added_at: entry.added_at,
    }
}

fn list_palace_staging_summaries(
    stage_dir: &Path,
    state: &PalaceStagingState,
) -> Vec<PalaceStagingWallpaperSummary> {
    state
        .items
        .iter()
        .map(|entry| palace_staging_summary(stage_dir, entry))
        .collect::<Vec<_>>()
}

fn palace_staging_batch_result(
    stage_dir: &Path,
    state: &PalaceStagingState,
    fetched: usize,
    replaced_previous_batch: bool,
    processed_count: usize,
    skipped_count: usize,
) -> PalaceStagingBatchResult {
    let items = list_palace_staging_summaries(stage_dir, state);
    PalaceStagingBatchResult {
        fetched,
        replaced_previous_batch,
        page: state.current_page.max(1),
        has_prev_page: state.has_prev_page,
        has_next_page: state.has_next_page,
        max_page: state.max_page.max(state.current_page).max(1),
        processed_count,
        skipped_count,
        remaining_items: items.len(),
        items,
    }
}

fn emit_palace_staging_refresh(app: &AppHandle, status: &PalaceStagingRefreshStatus) {
    let _ = app.emit("palace-staging-refresh", status.clone());
}

fn set_palace_staging_refresh_status(
    app: &AppHandle,
    status: PalaceStagingRefreshStatus,
) -> Result<PalaceStagingRefreshStatus, String> {
    let state = app.state::<AppState>();
    let mut guard = state
        .palace_refresh_status
        .lock()
        .map_err(|_| "故宫刷新状态被占用".to_string())?;
    *guard = status.clone();
    drop(guard);
    emit_palace_staging_refresh(app, &status);
    Ok(status)
}

fn current_palace_staging_refresh_status(
    app: &AppHandle,
) -> Result<PalaceStagingRefreshStatus, String> {
    let state = app.state::<AppState>();
    state
        .palace_refresh_status
        .lock()
        .map_err(|_| "故宫刷新状态被占用".to_string())
        .map(|status| status.clone())
}

fn palace_refresh_running_status(
    target_page: usize,
    current_committed_page: usize,
    processed_entries: usize,
    total_entries: usize,
    message: String,
) -> PalaceStagingRefreshStatus {
    PalaceStagingRefreshStatus {
        state: PalaceStagingRefreshState::Running,
        target_page: target_page.max(1),
        current_committed_page: current_committed_page.max(1),
        processed_entries,
        total_entries,
        message,
        error_message: None,
        batch: None,
    }
}

fn ensure_palace_refresh_not_running(app: &AppHandle) -> Result<(), String> {
    let status = current_palace_staging_refresh_status(app)?;
    if status.state == PalaceStagingRefreshState::Running {
        return Err("故宫候选页正在后台刷新，请稍后再试。".into());
    }
    Ok(())
}

fn local_wallpaper_summary(file: &WallpaperFile, fixed_source_url: &str) -> LocalWallpaperSummary {
    LocalWallpaperSummary {
        path: file.path.clone(),
        source_url: file.source_url.clone(),
        source_kind: file.source_kind.clone(),
        added_at: file.added_at,
        last_shown_at: file.last_shown_at,
        thumb_url: file.thumb_url.clone(),
        author_name: file.author_name.clone(),
        author_url: file.author_url.clone(),
        photo_url: file.photo_url.clone(),
        is_fixed: !fixed_source_url.is_empty() && file.source_url == fixed_source_url,
    }
}

fn find_wallpaper_index(
    wall_state: &WallpaperState,
    source_url: &str,
    remote_id: &str,
) -> Option<usize> {
    wall_state.files.iter().position(|entry| {
        (!source_url.is_empty() && entry.source_url == source_url)
            || (!remote_id.is_empty() && entry.remote_id == remote_id)
    })
}

fn remote_source_kind(source: WallpaperRemoteSource) -> &'static str {
    match source {
        WallpaperRemoteSource::Unsplash => "unsplash",
        WallpaperRemoteSource::Palace => "palace",
    }
}

fn remote_source_label(source: WallpaperRemoteSource) -> &'static str {
    match source {
        WallpaperRemoteSource::Unsplash => "Unsplash",
        WallpaperRemoteSource::Palace => "故宫壁纸",
    }
}

const PALACE_WALLPAPER_DISABLED_MESSAGE: &str = "macOS 暂未启用故宫壁纸来源，请使用 Unsplash。";

const fn palace_wallpaper_source_enabled() -> bool {
    !cfg!(target_os = "macos")
}

fn ensure_palace_wallpaper_source_enabled() -> Result<(), String> {
    if palace_wallpaper_source_enabled() {
        Ok(())
    } else {
        Err(PALACE_WALLPAPER_DISABLED_MESSAGE.into())
    }
}

fn choose_online_source(
    has_unsplash_access_key: bool,
    palace_enabled: bool,
) -> WallpaperRemoteSource {
    if has_unsplash_access_key || !palace_enabled {
        WallpaperRemoteSource::Unsplash
    } else {
        WallpaperRemoteSource::Palace
    }
}

fn resolve_online_source(app: &AppHandle) -> WallpaperRemoteSource {
    choose_online_source(
        resolve_unsplash_access_key(app).access_key.is_some(),
        palace_wallpaper_source_enabled(),
    )
}

fn canonical_unsplash_source_url(photo_url: &str, remote_id: &str) -> String {
    if !photo_url.trim().is_empty() {
        photo_url.to_string()
    } else {
        format!("https://unsplash.com/photos/{}", remote_id)
    }
}

fn canonical_palace_source_url(photo_url: &str, remote_id: &str) -> String {
    if !photo_url.trim().is_empty() {
        photo_url.to_string()
    } else {
        format!("https://www.dpm.org.cn/light/{}.html", remote_id)
    }
}

fn inspect_image_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| err.to_string())?;
    reader.into_dimensions().map_err(|err| err.to_string())
}

fn is_valid_wallpaper_size(width: u32, height: u32) -> bool {
    width >= WALLPAPER_MIN_WIDTH && width >= height
}

fn unsplash_error_message(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "Unsplash Access Key 无效或没有权限。".into(),
        429 => "Unsplash 请求过于频繁，请稍后再试。".into(),
        _ => format!("Unsplash 请求失败: HTTP {}", status.as_u16()),
    }
}

fn palace_error_message(status: reqwest::StatusCode) -> String {
    format!("故宫壁纸请求失败: HTTP {}", status.as_u16())
}

fn collapse_text(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn strip_html_tags(value: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    collapse_text(&re.replace_all(value, " "))
}

fn extract_light_ids(html: &str) -> Vec<String> {
    let mut ids = HashSet::new();
    let re = regex::Regex::new(r#"[\\/]light[\\/]([0-9]+)\.html"#).unwrap();
    for cap in re.captures_iter(html) {
        if let Some(matched) = cap.get(1) {
            ids.insert(matched.as_str().to_string());
        }
    }
    let key_re = regex::Regex::new(r#"data-key\s*=\s*["']([0-9,]+)["']"#).unwrap();
    for cap in key_re.captures_iter(html) {
        if let Some(matched) = cap.get(1) {
            for id in matched.as_str().split(',') {
                let id = id.trim();
                if !id.is_empty() {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids.into_iter().collect()
}

fn normalize_palace_asset_url(url: &str) -> String {
    let trimmed = url.trim().replace("&amp;", "&");
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed
    } else if trimmed.starts_with("//") {
        format!("https:{}", trimmed)
    } else if trimmed.starts_with('/') {
        format!("https://www.dpm.org.cn{}", trimmed)
    } else {
        trimmed
    }
}

fn parse_wallpaper_size_label(value: &str) -> Option<(u32, u32)> {
    let re = regex::Regex::new(r"(?i)(\d{3,5})\s*[*xX]\s*(\d{3,5})").unwrap();
    let caps = re.captures(value)?;
    let width = caps.get(1)?.as_str().parse::<u32>().ok()?;
    let height = caps.get(2)?.as_str().parse::<u32>().ok()?;
    Some((width, height))
}

fn extract_palace_list_entries(html: &str) -> Vec<PalaceListEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let block_re = regex::Regex::new(
        r#"(?is)<a\s+href=["'](?P<href>/light/(?P<id>\d+)\.html)["'][^>]*class=["'][^"']*item-a[^"']*["'][^>]*>(?P<body>.*?)</a>"#,
    )
    .unwrap();
    let img_src_re =
        regex::Regex::new(r#"(?is)<img[^>]+src=["'](?P<src>[^"']+)["'][^>]*>"#).unwrap();
    let img_alt_re =
        regex::Regex::new(r#"(?is)<img[^>]+alt=["'](?P<alt>[^"']*)["'][^>]*>"#).unwrap();
    let title_re =
        regex::Regex::new(r#"(?is)<div\s+class=["'][^"']*txt[^"']*["'][^>]*>(?P<title>.*?)</div>"#)
            .unwrap();

    for caps in block_re.captures_iter(html) {
        let Some(id) = caps.name("id").map(|value| value.as_str().to_string()) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let body = caps
            .name("body")
            .map(|value| value.as_str())
            .unwrap_or_default();
        let thumb_url = img_src_re
            .captures(body)
            .and_then(|img| {
                img.name("src")
                    .map(|value| normalize_palace_asset_url(value.as_str()))
            })
            .unwrap_or_default();
        let alt_title = img_alt_re
            .captures(body)
            .and_then(|img| img.name("alt").map(|value| strip_html_tags(value.as_str())))
            .unwrap_or_default();
        let title = title_re
            .captures(body)
            .and_then(|value| {
                value
                    .name("title")
                    .map(|matched| strip_html_tags(matched.as_str()))
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| alt_title.clone());
        entries.push(PalaceListEntry {
            id: id.clone(),
            title,
            thumb_url,
            detail_url: format!("https://www.dpm.org.cn/light/{}.html", id),
        });
    }

    if !entries.is_empty() {
        return entries;
    }

    for id in extract_light_ids(html) {
        if !seen.insert(id.clone()) {
            continue;
        }
        entries.push(PalaceListEntry {
            title: format!("故宫壁纸 {}", id),
            thumb_url: String::new(),
            detail_url: format!("https://www.dpm.org.cn/light/{}.html", id),
            id,
        });
    }

    entries
}

fn extract_palace_page_meta(html: &str, requested_page: usize) -> PalacePageMeta {
    let requested_page = requested_page.max(1);
    let current_re = regex::Regex::new(
        r#"(?is)<a[^>]*class=["'][^"']*paging-link[^"']*cur[^"']*["'][^>]*data-key=["'](?P<page>\d+)["']"#,
    )
    .unwrap();
    let page_re = regex::Regex::new(
        r#"(?is)<a[^>]*class=["'][^"']*paging-link[^"']*["'][^>]*data-key=["'](?P<page>\d+)["']"#,
    )
    .unwrap();
    let max_re = regex::Regex::new(r#"(?is)data-max=["'](?P<max>\d+)["']"#).unwrap();

    let current_page = current_re
        .captures(html)
        .and_then(|caps| caps.name("page"))
        .and_then(|value| value.as_str().parse::<usize>().ok())
        .unwrap_or(requested_page);

    let max_from_links = page_re
        .captures_iter(html)
        .filter_map(|caps| caps.name("page"))
        .filter_map(|value| value.as_str().parse::<usize>().ok())
        .max()
        .unwrap_or(current_page);

    let max_from_input = max_re
        .captures(html)
        .and_then(|caps| caps.name("max"))
        .and_then(|value| value.as_str().parse::<usize>().ok())
        .unwrap_or(max_from_links);

    let max_page = max_from_input.max(max_from_links).max(current_page).max(1);

    PalacePageMeta {
        current_page,
        has_prev_page: current_page > 1,
        has_next_page: current_page < max_page,
        max_page,
    }
}

fn extract_palace_download_candidates(html: &str) -> Vec<(String, Option<(u32, u32)>)> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    let re = regex::Regex::new(
        r#"(?is)<a[^>]*data-urls=["'](?P<url>[^"']+)["'][^>]*>(?P<label>.*?)</a>"#,
    )
    .unwrap();
    for caps in re.captures_iter(html) {
        let Some(url) = caps
            .name("url")
            .map(|value| normalize_palace_asset_url(value.as_str()))
        else {
            continue;
        };
        if !seen.insert(url.clone()) {
            continue;
        }
        let label = caps
            .name("label")
            .map(|value| strip_html_tags(value.as_str()))
            .unwrap_or_default();
        values.push((url, parse_wallpaper_size_label(&label)));
    }
    values
}

fn extract_palace_image_urls(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    let re = regex::Regex::new(
        r#"(?is)(?:data-urls|src|data-src)\s*=\s*["'](?P<url>(?:https?://|//|/)[^"']+?\.(?:jpe?g))(?:\?[^"']*)?["']"#,
    )
    .unwrap();
    for caps in re.captures_iter(html) {
        let Some(url) = caps
            .name("url")
            .map(|value| normalize_palace_asset_url(value.as_str()))
        else {
            continue;
        };
        if seen.insert(url.clone()) {
            urls.push(url);
        }
    }
    urls
}

fn extract_palace_detail_title(html: &str) -> Option<String> {
    let title_re =
        regex::Regex::new(r#"(?is)<img[^>]+title=["'](?P<title>[^"']+)["'][^>]*>"#).unwrap();
    if let Some(caps) = title_re.captures(html) {
        if let Some(title) = caps.name("title") {
            let normalized = collapse_text(title.as_str());
            if !normalized.is_empty() {
                return Some(normalized);
            }
        }
    }
    let alt_re = regex::Regex::new(r#"(?is)<img[^>]+alt=["'](?P<title>[^"']+)["'][^>]*>"#).unwrap();
    alt_re
        .captures(html)
        .and_then(|caps| {
            caps.name("title")
                .map(|value| collapse_text(value.as_str()))
        })
        .filter(|value| !value.is_empty())
}

fn read_local_env_value(key: &str) -> Option<String> {
    let mut candidate_dirs = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        for dir in cwd.ancestors().take(4) {
            candidate_dirs.push(dir.to_path_buf());
        }
    }
    if let Ok(exe_path) = env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            for dir in exe_dir.ancestors().take(4) {
                let dir = dir.to_path_buf();
                if !candidate_dirs.contains(&dir) {
                    candidate_dirs.push(dir);
                }
            }
        }
    }

    for dir in candidate_dirs {
        for file_name in [".env.local", ".env"] {
            let path = dir.join(file_name);
            let Ok(content) = fs::read_to_string(path) else {
                continue;
            };
            for raw_line in content.lines() {
                let line = raw_line.trim().trim_start_matches('\u{feff}');
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((left, right)) = line.split_once('=') else {
                    continue;
                };
                if left.trim() != key {
                    continue;
                }
                let value = right
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

#[derive(Debug)]
enum HttpFetchError {
    Transport(reqwest::Error),
    HttpStatus {
        status: reqwest::StatusCode,
        message: String,
    },
    Terminal(String),
}

impl HttpFetchError {
    fn http_status(status: reqwest::StatusCode, message: impl Into<String>) -> Self {
        Self::HttpStatus {
            status,
            message: message.into(),
        }
    }

    fn terminal(message: impl Into<String>) -> Self {
        Self::Terminal(message.into())
    }
}

impl fmt::Display for HttpFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(formatter),
            Self::HttpStatus { message, .. } | Self::Terminal(message) => {
                formatter.write_str(message)
            }
        }
    }
}

fn should_retry_without_proxy(error: &HttpFetchError) -> bool {
    match error {
        HttpFetchError::Transport(error) => {
            error.status().is_none() && !error.is_decode() && !error.is_builder()
        }
        HttpFetchError::HttpStatus { status, .. } => {
            debug_assert!(!status.is_success());
            *status == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED
                || status.is_server_error()
        }
        HttpFetchError::Terminal(_) => false,
    }
}

fn build_unsplash_api_client(
    access_key: &str,
    disable_proxy: bool,
) -> Result<Client, HttpFetchError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("Huyanba/2.4.0 (desktop; https://github.com/guoruya/huyanba)"),
    );
    headers.insert("Accept-Version", HeaderValue::from_static("v1"));
    let auth = HeaderValue::from_str(&format!("Client-ID {}", access_key))
        .map_err(|error| HttpFetchError::terminal(error.to_string()))?;
    headers.insert(AUTHORIZATION, auth);

    let mut builder = Client::builder()
        .use_native_tls()
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .default_headers(headers);
    if disable_proxy {
        builder = builder.no_proxy();
    }
    builder.build().map_err(HttpFetchError::Transport)
}

fn build_palace_client(disable_proxy: bool) -> Result<Client, HttpFetchError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(platform::download_transport::PALACE_ACCEPT),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static(platform::download_transport::PALACE_ACCEPT_LANGUAGE),
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(platform::download_transport::DESKTOP_USER_AGENT),
    );
    headers.insert(
        REFERER,
        HeaderValue::from_static(platform::download_transport::PALACE_REFERER),
    );
    headers.insert(
        "x-requested-with",
        HeaderValue::from_static("XMLHttpRequest"),
    );

    let mut builder = Client::builder()
        .use_native_tls()
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .default_headers(headers);
    if disable_proxy {
        builder = builder.no_proxy();
    }
    builder.build().map_err(HttpFetchError::Transport)
}

fn with_unsplash_client<T, F>(
    access_key: &str,
    app: Option<&AppHandle>,
    context: &str,
    mut operation: F,
) -> Result<T, String>
where
    F: FnMut(&Client) -> Result<T, HttpFetchError>,
{
    let first_result =
        build_unsplash_api_client(access_key, false).and_then(|client| operation(&client));
    match first_result {
        Ok(value) => Ok(value),
        Err(err) => {
            if !should_retry_without_proxy(&err) {
                return Err(err.to_string());
            }
            if let Some(app) = app {
                append_wallpaper_log(
                    app,
                    &format!(
                        "Unsplash {} 首次连接失败，改用 no_proxy 重试: {}",
                        context, err
                    ),
                );
            }
            build_unsplash_api_client(access_key, true)
                .and_then(|client| operation(&client))
                .map_err(|error| error.to_string())
        }
    }
}

fn log_palace_transport(
    app: Option<&AppHandle>,
    context: &str,
    transport: &str,
    url: &str,
    detail: &str,
) {
    let message = format!(
        "故宫传输: context={} transport={} url={} detail={}",
        context, transport, url, detail
    );
    if let Some(app) = app {
        append_wallpaper_log(app, &message);
        append_palace_debug_log(app, &message);
    }
}

fn palace_temp_download_path(kind: &str, url: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "huyanba_palace_{}_{}_{}.tmp",
        kind,
        hash_url(url),
        now_millis()
    ))
}

fn powershell_available() -> bool {
    platform::download_transport::powershell_available()
}

fn curl_available() -> bool {
    platform::download_transport::curl_available()
}

fn palace_fetch_text_via_powershell(url: &str) -> Result<String, String> {
    let temp_path = palace_temp_download_path("html", url);
    let result = (|| {
        platform::download_transport::run_powershell_download(url, &temp_path)?;
        fs::read_to_string(&temp_path).map_err(|err| err.to_string())
    })();
    let _ = fs::remove_file(&temp_path);
    result
}

fn palace_fetch_text_via_curl(url: &str) -> Result<String, String> {
    let temp_path = palace_temp_download_path("curl_html", url);
    let result = (|| {
        platform::download_transport::run_curl_download(url, &temp_path)?;
        fs::read_to_string(&temp_path).map_err(|err| err.to_string())
    })();
    let _ = fs::remove_file(&temp_path);
    result
}

fn palace_fetch_bytes_via_powershell(url: &str) -> Result<Vec<u8>, String> {
    let temp_path = palace_temp_download_path("bytes", url);
    let result = (|| {
        platform::download_transport::run_powershell_download(url, &temp_path)?;
        fs::read(&temp_path).map_err(|err| err.to_string())
    })();
    let _ = fs::remove_file(&temp_path);
    result
}

fn palace_fetch_bytes_via_curl(url: &str) -> Result<Vec<u8>, String> {
    let temp_path = palace_temp_download_path("curl_bytes", url);
    let result = (|| {
        platform::download_transport::run_curl_download(url, &temp_path)?;
        fs::read(&temp_path).map_err(|err| err.to_string())
    })();
    let _ = fs::remove_file(&temp_path);
    result
}

fn palace_fetch_text_with_client(client: &Client, url: &str) -> Result<String, HttpFetchError> {
    let response = client.get(url).send().map_err(HttpFetchError::Transport)?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(HttpFetchError::http_status(
            status,
            palace_error_message(status),
        ));
    }
    response.text().map_err(HttpFetchError::Transport)
}

fn palace_fetch_bytes_with_client(client: &Client, url: &str) -> Result<Vec<u8>, HttpFetchError> {
    let response = client.get(url).send().map_err(HttpFetchError::Transport)?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(HttpFetchError::http_status(
            status,
            palace_error_message(status),
        ));
    }
    response
        .bytes()
        .map(|bytes| bytes.to_vec())
        .map_err(HttpFetchError::Transport)
}

fn advance_fallback_depth(depth: u32) -> Option<u32> {
    depth
        .checked_add(1)
        .filter(|next_depth| *next_depth <= MAX_FALLBACK_CHAIN_DEPTH)
}

fn palace_fetch_with_fallback<T, F, G, H>(
    app: Option<&AppHandle>,
    context: &str,
    url: &str,
    fallback_depth: u32,
    mut reqwest_fetch: F,
    mut powershell_fetch: G,
    mut curl_fetch: H,
) -> Result<T, String>
where
    F: FnMut(&Client, &str) -> Result<T, HttpFetchError>,
    G: FnMut(&str) -> Result<T, String>,
    H: FnMut(&str) -> Result<T, String>,
{
    let first_result = build_palace_client(false).and_then(|client| reqwest_fetch(&client, url));
    let (no_proxy_err, mut fallback_depth) = match first_result {
        Ok(value) => {
            log_palace_transport(app, context, "reqwest_success", url, "ok");
            return Ok(value);
        }
        Err(first_err) => {
            if !should_retry_without_proxy(&first_err) {
                return Err(first_err.to_string());
            }
            let no_proxy_depth = advance_fallback_depth(fallback_depth).ok_or_else(|| {
                format!(
                    "故宫壁纸请求失败（回退链已达上限，跳过 no_proxy/powershell/curl）: {}",
                    first_err
                )
            })?;
            let no_proxy_result =
                build_palace_client(true).and_then(|client| reqwest_fetch(&client, url));
            match no_proxy_result {
                Ok(value) => {
                    log_palace_transport(app, context, "reqwest_no_proxy_retry_success", url, "ok");
                    return Ok(value);
                }
                Err(no_proxy_err) if should_retry_without_proxy(&no_proxy_err) => {
                    (no_proxy_err, no_proxy_depth)
                }
                Err(no_proxy_err) => return Err(no_proxy_err.to_string()),
            }
        }
    };

    let powershell_available = powershell_available();
    let mut powershell_attempted = false;
    if powershell_available {
        fallback_depth = advance_fallback_depth(fallback_depth).ok_or_else(|| {
            format!(
                "故宫壁纸请求失败（回退链已达上限，跳过 powershell/curl）: {}",
                no_proxy_err
            )
        })?;
        powershell_attempted = true;
        if let Ok(value) = powershell_fetch(url) {
            log_palace_transport(app, context, "powershell_fallback_success", url, "ok");
            return Ok(value);
        }
    }

    let curl_available = curl_available();
    let mut curl_attempted = false;
    if curl_available {
        let _curl_depth = advance_fallback_depth(fallback_depth).ok_or_else(|| {
            format!(
                "故宫壁纸请求失败（回退链已达上限，跳过 curl）: {}",
                no_proxy_err
            )
        })?;
        curl_attempted = true;
        if let Ok(value) = curl_fetch(url) {
            log_palace_transport(app, context, "curl_fallback_success", url, "ok");
            return Ok(value);
        }
    }

    Err(format!(
        "故宫壁纸请求失败: reqwest+no_proxy={}（powershell={} curl={}）",
        no_proxy_err,
        if powershell_attempted {
            "已尝试"
        } else {
            "不可用"
        },
        if curl_attempted {
            "已尝试"
        } else {
            "不可用"
        }
    ))
}

fn palace_fetch_text(app: Option<&AppHandle>, context: &str, url: &str) -> Result<String, String> {
    palace_fetch_with_fallback(
        app,
        context,
        url,
        0,
        palace_fetch_text_with_client,
        palace_fetch_text_via_powershell,
        palace_fetch_text_via_curl,
    )
}

fn palace_fetch_bytes(
    app: Option<&AppHandle>,
    context: &str,
    url: &str,
) -> Result<Vec<u8>, String> {
    palace_fetch_with_fallback(
        app,
        context,
        url,
        0,
        palace_fetch_bytes_with_client,
        palace_fetch_bytes_via_powershell,
        palace_fetch_bytes_via_curl,
    )
}

fn build_unsplash_download_url(raw_url: &str) -> Result<String, String> {
    let mut url = Url::parse(raw_url).map_err(|err| err.to_string())?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("w", &UNSPLASH_DOWNLOAD_WIDTH.to_string());
        pairs.append_pair("fit", "max");
        pairs.append_pair("q", &UNSPLASH_DOWNLOAD_QUALITY.to_string());
        pairs.append_pair("fm", "jpg");
    }
    Ok(url.to_string())
}

fn map_unsplash_photo(photo: UnsplashApiPhoto) -> Option<RemoteWallpaperSummary> {
    if !is_valid_wallpaper_size(photo.width, photo.height) {
        return None;
    }
    let id = photo.id;
    let width = photo.width;
    let height = photo.height;
    let title = photo
        .description
        .clone()
        .or(photo.alt_description.clone())
        .unwrap_or_else(|| "Unsplash 壁纸".into());
    let description = photo
        .description
        .or(photo.alt_description)
        .unwrap_or_default();
    let thumb_url = photo.urls.thumb;
    let preview_url = photo.urls.regular;
    let raw_url = photo.urls.raw;
    let author_name = photo.user.name;
    let author_url = photo.user.links.html;
    let photo_url = photo.links.html;
    let download_location = photo.links.download_location;
    Some(RemoteWallpaperSummary {
        source: WallpaperRemoteSource::Unsplash,
        id: id.clone(),
        title,
        description,
        width,
        height,
        thumb_url: thumb_url.clone(),
        preview_url: preview_url.clone(),
        credit_name: author_name.clone(),
        credit_url: author_url.clone(),
        photo_url: photo_url.clone(),
        download_payload: json!({
            "id": id,
            "downloadLocation": download_location,
            "rawUrl": raw_url,
            "thumbUrl": thumb_url,
            "photoUrl": photo_url,
            "authorName": author_name,
            "authorUrl": author_url,
        }),
    })
}

fn map_palace_resolved_wallpaper(wallpaper: PalaceResolvedWallpaper) -> RemoteWallpaperSummary {
    let payload = PalaceDownloadRequest {
        id: wallpaper.id.clone(),
        title: wallpaper.title.clone(),
        image_url: wallpaper.image_url.clone(),
        thumb_url: wallpaper.thumb_url.clone(),
        photo_url: wallpaper.detail_url.clone(),
        credit_name: "故宫博物院".into(),
        credit_url: wallpaper.detail_url.clone(),
        width: wallpaper.width,
        height: wallpaper.height,
    };
    RemoteWallpaperSummary {
        source: WallpaperRemoteSource::Palace,
        id: wallpaper.id.clone(),
        title: wallpaper.title.clone(),
        description: wallpaper.title,
        width: wallpaper.width,
        height: wallpaper.height,
        thumb_url: if wallpaper.thumb_url.is_empty() {
            wallpaper.image_url.clone()
        } else {
            wallpaper.thumb_url
        },
        preview_url: wallpaper.image_url.clone(),
        credit_name: "故宫博物院".into(),
        credit_url: wallpaper.detail_url.clone(),
        photo_url: wallpaper.detail_url,
        download_payload: serde_json::to_value(payload).unwrap_or(Value::Null),
    }
}

fn not_configured_search_result(page: usize, per_page: usize) -> RemoteWallpaperSearchResult {
    RemoteWallpaperSearchResult {
        source: WallpaperRemoteSource::Unsplash,
        configured: false,
        page,
        per_page,
        total_pages: 0,
        total_results: 0,
        has_next_page: false,
        items: Vec::new(),
        error_message: Some(
            "未配置 Unsplash Access Key，可在壁纸设置里填写，或继续使用环境变量。".into(),
        ),
    }
}

fn fetch_unsplash_search_page(
    client: &Client,
    query: &str,
    page: usize,
    per_page: usize,
) -> Result<RemoteWallpaperSearchResult, HttpFetchError> {
    let page = page.max(1);
    let per_page = per_page.clamp(1, 30);
    if query.trim().is_empty() {
        let mut url = Url::parse(&format!(
            "{}/topics/{}/photos",
            UNSPLASH_API_BASE, UNSPLASH_TOPIC_SLUG
        ))
        .map_err(|error| HttpFetchError::terminal(error.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("page", &page.to_string());
            pairs.append_pair("per_page", &per_page.to_string());
            pairs.append_pair("orientation", "landscape");
            pairs.append_pair("order_by", "latest");
        }
        let response = client.get(url).send().map_err(HttpFetchError::Transport)?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(HttpFetchError::http_status(
                status,
                unsplash_error_message(status),
            ));
        }
        let photos = response
            .json::<Vec<UnsplashApiPhoto>>()
            .map_err(HttpFetchError::Transport)?;
        let has_next_page = photos.len() >= per_page;
        return Ok(RemoteWallpaperSearchResult {
            source: WallpaperRemoteSource::Unsplash,
            configured: true,
            page,
            per_page,
            total_pages: if has_next_page { page + 1 } else { page },
            total_results: 0,
            has_next_page,
            items: photos.into_iter().filter_map(map_unsplash_photo).collect(),
            error_message: None,
        });
    }

    let mut url = Url::parse(&format!("{}/search/photos", UNSPLASH_API_BASE))
        .map_err(|error| HttpFetchError::terminal(error.to_string()))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("query", query.trim());
        pairs.append_pair("page", &page.to_string());
        pairs.append_pair("per_page", &per_page.to_string());
        pairs.append_pair("orientation", "landscape");
    }
    let response = client.get(url).send().map_err(HttpFetchError::Transport)?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(HttpFetchError::http_status(
            status,
            unsplash_error_message(status),
        ));
    }
    let payload = response
        .json::<UnsplashApiSearchResponse>()
        .map_err(HttpFetchError::Transport)?;
    Ok(RemoteWallpaperSearchResult {
        source: WallpaperRemoteSource::Unsplash,
        configured: true,
        page,
        per_page,
        total_pages: payload.total_pages,
        total_results: payload.total,
        has_next_page: page < payload.total_pages,
        items: payload
            .results
            .into_iter()
            .filter_map(map_unsplash_photo)
            .collect(),
        error_message: None,
    })
}

fn search_unsplash_wallpapers_blocking(
    app: &AppHandle,
    query: String,
    page: usize,
    per_page: usize,
) -> Result<RemoteWallpaperSearchResult, String> {
    let page = page.max(1);
    let per_page = per_page.clamp(1, 30);
    let Some(access_key) = resolve_unsplash_access_key(app).access_key else {
        return Ok(not_configured_search_result(page, per_page));
    };
    with_unsplash_client(&access_key, None, "search", |client| {
        fetch_unsplash_search_page(client, &query, page, per_page)
    })
}

fn build_palace_list_url(page: usize, per_page: usize) -> String {
    format!(
        "{}?category_id=624&p={}&pagesize={}&title=&is_pc=1&is_wap=0&is_calendar=0&is_four_k=0&_={}",
        PALACE_LIST_ENDPOINT,
        page.max(1),
        per_page.clamp(1, PALACE_LIST_PAGE_SIZE),
        now_millis(),
    )
}

fn palace_resolution_score(width: u32, height: u32) -> (u8, u64, u32, u32) {
    let preferred_rank = match (width, height) {
        (4000, 2250) => 4,
        (2560, 1440) => 3,
        (1920, 1080) => 2,
        (1920, 1280) => 1,
        _ => 0,
    };
    (preferred_rank, width as u64 * height as u64, width, height)
}

fn resolve_palace_wallpaper_from_bytes(
    entry: &PalaceListEntry,
    title: &str,
    image_url: &str,
    bytes: &[u8],
) -> Result<Option<PalaceResolvedWallpaper>, String> {
    let (width, height) = inspect_image_dimensions(bytes)?;
    if !is_valid_wallpaper_size(width, height) {
        return Ok(None);
    }
    Ok(Some(PalaceResolvedWallpaper {
        id: entry.id.clone(),
        title: title.to_string(),
        width,
        height,
        thumb_url: if entry.thumb_url.is_empty() {
            image_url.to_string()
        } else {
            entry.thumb_url.clone()
        },
        image_url: image_url.to_string(),
        detail_url: entry.detail_url.clone(),
    }))
}

fn fetch_palace_list_entries(
    app: Option<&AppHandle>,
    page: usize,
    per_page: usize,
) -> Result<(Vec<PalaceListEntry>, PalacePageMeta), String> {
    let per_page = per_page.clamp(1, PALACE_LIST_PAGE_SIZE);
    let list_url = build_palace_list_url(page, per_page);
    append_palace_debug(
        app,
        &format!(
            "list_fetch_start page={} per_page={} url={}",
            page.max(1),
            per_page,
            list_url
        ),
    );
    let html = palace_fetch_text(app, &format!("list page={}", page.max(1)), &list_url)?;
    let entries = extract_palace_list_entries(&html);
    let page_meta = extract_palace_page_meta(&html, page.max(1));
    append_palace_debug(
        app,
        &format!(
            "list_fetch_done page={} html_len={} entries={} light_ids={} uploads={} taociguan={} has_next={} max_page={}",
            page.max(1),
            html.len(),
            entries.len(),
            extract_light_ids(&html).len(),
            html.matches("/Uploads/image/").count(),
            html.matches("https://taociguan.dpm.org.cn/").count(),
            page_meta.has_next_page,
            page_meta.max_page
        ),
    );
    if entries.is_empty() {
        append_palace_debug(
            app,
            &format!(
                "list_fetch_empty page={} snippet={}",
                page.max(1),
                summarize_for_log(&html, 320)
            ),
        );
    }
    Ok((entries, page_meta))
}

fn resolve_palace_wallpaper(
    app: Option<&AppHandle>,
    entry: &PalaceListEntry,
) -> Result<Option<PalaceResolvedWallpaper>, String> {
    let mut title = if entry.title.is_empty() {
        format!("故宫壁纸 {}", entry.id)
    } else {
        entry.title.clone()
    };
    append_palace_debug(
        app,
        &format!(
            "detail_resolve_start id={} title={} thumb_url={} detail_url={}",
            entry.id, title, entry.thumb_url, entry.detail_url
        ),
    );

    match palace_fetch_text(app, &format!("detail id={}", entry.id), &entry.detail_url) {
        Ok(html) => {
            if let Some(detail_title) =
                extract_palace_detail_title(&html).filter(|value| !value.is_empty())
            {
                title = detail_title;
            }

            let structured = extract_palace_download_candidates(&html);
            let detail_image_urls = extract_palace_image_urls(&html);
            append_palace_debug(
                app,
                &format!(
                    "detail_fetch_done id={} html_len={} structured_candidates={} image_url_candidates={}",
                    entry.id,
                    html.len(),
                    structured.len(),
                    detail_image_urls.len()
                ),
            );
            if let Some((image_url, width, height)) = structured
                .into_iter()
                .filter_map(|(url, size)| size.map(|(width, height)| (url, width, height)))
                .filter(|(_, width, height)| is_valid_wallpaper_size(*width, *height))
                .max_by_key(|(_, width, height)| palace_resolution_score(*width, *height))
            {
                append_palace_debug(
                    app,
                    &format!(
                        "detail_resolve_structured id={} width={} height={} image_url={}",
                        entry.id, width, height, image_url
                    ),
                );
                return Ok(Some(PalaceResolvedWallpaper {
                    id: entry.id.clone(),
                    title,
                    width,
                    height,
                    thumb_url: if entry.thumb_url.is_empty() {
                        image_url.clone()
                    } else {
                        entry.thumb_url.clone()
                    },
                    image_url,
                    detail_url: entry.detail_url.clone(),
                }));
            }

            for url in detail_image_urls {
                match palace_fetch_bytes(app, &format!("detail image id={}", entry.id), &url) {
                    Ok(bytes) => {
                        if let Some(wallpaper) =
                            resolve_palace_wallpaper_from_bytes(entry, &title, &url, &bytes)?
                        {
                            append_palace_debug(
                                app,
                                &format!(
                                    "detail_resolve_image_fallback id={} width={} height={} image_url={}",
                                    entry.id, wallpaper.width, wallpaper.height, url
                                ),
                            );
                            return Ok(Some(wallpaper));
                        }
                        append_palace_debug(
                            app,
                            &format!(
                                "detail_image_rejected id={} image_url={} reason=not_desktop_size",
                                entry.id, url
                            ),
                        );
                    }
                    Err(err) => {
                        if let Some(app) = app {
                            append_wallpaper_log(
                                app,
                                &format!(
                                    "故宫详情图片兜底失败: id={} url={} error={}",
                                    entry.id, url, err
                                ),
                            );
                        }
                    }
                }
            }
        }
        Err(err) => {
            append_palace_debug(
                app,
                &format!("detail_fetch_failed id={} error={}", entry.id, err),
            );
            if let Some(app) = app {
                append_wallpaper_log(
                    app,
                    &format!("故宫详情抓取失败: id={} error={}", entry.id, err),
                );
            }
        }
    }

    if !entry.thumb_url.is_empty() {
        match palace_fetch_bytes(
            app,
            &format!("list preview id={}", entry.id),
            &entry.thumb_url,
        ) {
            Ok(bytes) => {
                if let Some(wallpaper) =
                    resolve_palace_wallpaper_from_bytes(entry, &title, &entry.thumb_url, &bytes)?
                {
                    append_palace_debug(
                        app,
                        &format!(
                            "list_preview_resolve_success id={} width={} height={} image_url={}",
                            entry.id, wallpaper.width, wallpaper.height, entry.thumb_url
                        ),
                    );
                    return Ok(Some(wallpaper));
                }
                append_palace_debug(
                    app,
                    &format!(
                        "list_preview_rejected id={} image_url={} reason=not_desktop_size",
                        entry.id, entry.thumb_url
                    ),
                );
            }
            Err(err) => {
                append_palace_debug(
                    app,
                    &format!("list_preview_fetch_failed id={} error={}", entry.id, err),
                );
                if let Some(app) = app {
                    append_wallpaper_log(
                        app,
                        &format!("故宫列表预览兜底失败: id={} error={}", entry.id, err),
                    );
                }
            }
        }
    }

    append_palace_debug(
        app,
        &format!("detail_resolve_none id={} title={}", entry.id, title),
    );
    Ok(None)
}

fn browse_palace_wallpapers_blocking(
    page: usize,
    per_page: usize,
) -> Result<RemoteWallpaperSearchResult, String> {
    let page = page.max(1);
    let per_page = per_page.clamp(1, PALACE_LIST_PAGE_SIZE);
    let (entries, page_meta) = fetch_palace_list_entries(None, page, per_page)?;
    let mut items = Vec::new();
    for entry in entries {
        match resolve_palace_wallpaper(None, &entry) {
            Ok(Some(wallpaper)) => items.push(map_palace_resolved_wallpaper(wallpaper)),
            Ok(None) => {}
            Err(_) => {}
        }
    }
    Ok(RemoteWallpaperSearchResult {
        source: WallpaperRemoteSource::Palace,
        configured: true,
        page,
        per_page,
        total_pages: page_meta.max_page,
        total_results: 0,
        has_next_page: page_meta.has_next_page,
        items,
        error_message: None,
    })
}

fn clear_dir_if_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn stage_palace_wallpaper(
    app: Option<&AppHandle>,
    stage_dir: &Path,
    wallpaper: &PalaceResolvedWallpaper,
) -> Result<PalaceStagingEntry, String> {
    let source_url = canonical_palace_source_url(&wallpaper.detail_url, &wallpaper.id);
    append_palace_debug(
        app,
        &format!(
            "stage_download_start id={} source_url={} image_url={}",
            wallpaper.id, source_url, wallpaper.image_url
        ),
    );
    let bytes = palace_fetch_bytes(
        app,
        &format!("stage download id={}", wallpaper.id),
        &wallpaper.image_url,
    )?;
    let (width, height) = inspect_image_dimensions(&bytes)?;
    if !is_valid_wallpaper_size(width, height) {
        return Err("下载图片尺寸不满足桌面壁纸要求。".into());
    }

    let now = now_ts();
    let file_name = format!(
        "palace_stage_{}_{}.{}",
        hash_url(&source_url),
        now,
        image_extension_from_url(&wallpaper.image_url)
    );
    let file_path = stage_dir.join(&file_name);
    fs::write(&file_path, &bytes).map_err(|err| err.to_string())?;
    append_palace_debug(
        app,
        &format!(
            "stage_download_done id={} width={} height={} file_name={}",
            wallpaper.id, width, height, file_name
        ),
    );

    Ok(PalaceStagingEntry {
        id: wallpaper.id.clone(),
        title: wallpaper.title.clone(),
        file_name,
        source_url,
        width,
        height,
        credit_name: "故宫博物院".into(),
        credit_url: wallpaper.detail_url.clone(),
        photo_url: wallpaper.detail_url.clone(),
        added_at: now,
    })
}

fn replace_palace_staging_batch(root_dir: &Path) -> Result<bool, String> {
    let current_dir = palace_staging_dir(root_dir);
    let next_dir = palace_staging_temp_dir(root_dir);
    let backup_dir = palace_staging_backup_dir(root_dir);

    clear_dir_if_exists(&backup_dir)?;
    let had_previous = current_dir.exists();
    if had_previous {
        fs::rename(&current_dir, &backup_dir).map_err(|err| err.to_string())?;
    }

    match fs::rename(&next_dir, &current_dir) {
        Ok(_) => {
            clear_dir_if_exists(&backup_dir)?;
            Ok(had_previous)
        }
        Err(err) => {
            if had_previous && backup_dir.exists() {
                let _ = fs::rename(&backup_dir, &current_dir);
            }
            Err(err.to_string())
        }
    }
}

fn refresh_palace_staging_batch_inner<F>(
    app: &AppHandle,
    wall_state: &WallpaperState,
    root_dir: &Path,
    page: usize,
    mut progress: F,
) -> Result<(PalaceStagingState, PrefetchStats, bool), String>
where
    F: FnMut(usize, usize, String),
{
    let page = page.max(1);
    append_palace_debug_log(
        app,
        &format!(
            "batch_refresh_start root_dir={} page={} existing_local={}",
            root_dir.to_string_lossy(),
            page,
            wall_state.files.len()
        ),
    );
    let stage_dir = palace_staging_dir(root_dir);
    let temp_dir = palace_staging_temp_dir(root_dir);
    let temp_index_path = palace_staging_index_path(&temp_dir);
    clear_dir_if_exists(&temp_dir)?;
    fs::create_dir_all(&temp_dir).map_err(|err| err.to_string())?;

    let mut next_state = PalaceStagingState {
        current_page: page,
        max_page: page,
        ..PalaceStagingState::default()
    };
    let mut known_sources = wall_state
        .files
        .iter()
        .map(|entry| entry.source_url.clone())
        .collect::<HashSet<_>>();
    let mut stats = PrefetchStats {
        source_kind: remote_source_kind(WallpaperRemoteSource::Palace).into(),
        source_label: remote_source_label(WallpaperRemoteSource::Palace).into(),
        ..PrefetchStats::default()
    };

    let mut cancelled = false;

    let refresh_result = (|| {
        append_wallpaper_log(app, &format!("故宫候选批次拉取: page={}", page));
        let (entries, page_meta) =
            fetch_palace_list_entries(Some(app), page, PALACE_LIST_PAGE_SIZE)?;
        next_state.current_page = page_meta.current_page;
        next_state.has_prev_page = page_meta.has_prev_page;
        next_state.has_next_page = page_meta.has_next_page;
        next_state.max_page = page_meta.max_page;
        append_palace_debug_log(
            app,
            &format!(
                "batch_page_done page={} entries={} has_prev={} has_next={} max_page={}",
                page_meta.current_page,
                entries.len(),
                page_meta.has_prev_page,
                page_meta.has_next_page,
                page_meta.max_page
            ),
        );
        progress(
            0,
            entries.len(),
            format!("正在获取第 {} 页候选壁纸...", page_meta.current_page),
        );

        stats.list_successes += 1;
        let started = std::time::Instant::now();
        let total_entries = entries.len();
        for (processed_entries, entry) in entries.into_iter().enumerate() {
            // 批次总超时检查
            if started.elapsed().as_secs() >= PALACE_BATCH_TOTAL_TIMEOUT_SECS {
                append_wallpaper_log(app, "故宫候选批次超时，终止剩余条目获取");
                break;
            }
            // 用户取消检查
            if app
                .state::<AppState>()
                .cancel_refresh
                .load(Ordering::SeqCst)
            {
                append_wallpaper_log(app, "故宫候选批次获取被用户取消");
                let _ = set_palace_staging_refresh_status(
                    app,
                    PalaceStagingRefreshStatus {
                        state: PalaceStagingRefreshState::Cancelled,
                        target_page: page_meta.current_page,
                        current_committed_page: next_state.current_page.max(1),
                        processed_entries: processed_entries + 1,
                        total_entries,
                        message: "故宫候选壁纸获取已取消。".into(),
                        error_message: None,
                        batch: None,
                    },
                );
                cancelled = true;
                break;
            }
            let processed_entries = processed_entries + 1;
            progress(
                processed_entries,
                total_entries,
                format!(
                    "正在整理故宫第 {} 页候选图（{} / {}）...",
                    page_meta.current_page, processed_entries, total_entries
                ),
            );
            let resolved = match resolve_palace_wallpaper(Some(app), &entry) {
                Ok(Some(wallpaper)) => wallpaper,
                Ok(None) => continue,
                Err(err) => {
                    append_wallpaper_log(
                        app,
                        &format!("故宫候选详情解析失败: id={} error={}", entry.id, err),
                    );
                    continue;
                }
            };

            let source_url = canonical_palace_source_url(&resolved.detail_url, &resolved.id);
            if !known_sources.insert(source_url.clone()) {
                append_wallpaper_log(app, &format!("故宫候选重复跳过: {}", source_url));
                continue;
            }

            match stage_palace_wallpaper(Some(app), &temp_dir, &resolved) {
                Ok(item) => {
                    append_wallpaper_log(app, &format!("故宫候选下载成功: {}", item.file_name));
                    next_state.items.push(item);
                }
                Err(err) => {
                    append_wallpaper_log(
                        app,
                        &format!("故宫候选下载失败: id={} error={}", resolved.id, err),
                    );
                }
            }
        }
        progress(
            total_entries,
            total_entries,
            format!("正在提交故宫第 {} 页候选壁纸...", page_meta.current_page),
        );
        Ok::<(), String>(())
    })();

    if cancelled {
        let _ = clear_dir_if_exists(&temp_dir);
        return Err("已取消".into());
    }

    if let Err(err) = refresh_result {
        let _ = clear_dir_if_exists(&temp_dir);
        append_palace_debug_log(app, &format!("batch_refresh_failed error={}", err));
        return Err(format!("故宫候选壁纸获取失败: {}", err));
    }

    if next_state.items.is_empty() {
        let _ = clear_dir_if_exists(&temp_dir);
        append_palace_debug_log(
            app,
            &format!(
                "batch_refresh_empty page={} no_desktop_wallpapers_after_filter",
                next_state.current_page
            ),
        );
        return Err("故宫候选壁纸获取失败：当前页没有可预览的桌面横屏图片。".into());
    }

    next_state.last_batch_at = now_ts();
    save_palace_staging_state(&temp_index_path, &next_state)?;
    let app_state = app.state::<AppState>();
    let _guard = app_state
        .wallpaper_lock
        .lock()
        .map_err(|_| "壁纸锁被占用".to_string())?;
    let replaced_previous_batch = replace_palace_staging_batch(root_dir)?;
    let stage_state_path = palace_staging_index_path(&stage_dir);
    let mut final_state = load_palace_staging_state(&stage_state_path);
    normalize_palace_staging_state(&stage_dir, &mut final_state);
    save_palace_staging_state(&stage_state_path, &final_state)?;
    drop(_guard);
    stats.added = final_state.items.len();
    append_palace_debug_log(
        app,
        &format!(
            "batch_refresh_done page={} replaced_previous_batch={} final_items={} list_successes={} has_prev={} has_next={} max_page={}",
            final_state.current_page,
            replaced_previous_batch,
            stats.added,
            stats.list_successes,
            final_state.has_prev_page,
            final_state.has_next_page,
            final_state.max_page
        ),
    );
    Ok((final_state, stats, replaced_previous_batch))
}

fn promote_palace_staging_entry_internal(
    dir: &Path,
    wall_state: &mut WallpaperState,
    stage_dir: &Path,
    stage_state: &mut PalaceStagingState,
    source_url: &str,
    set_fixed: bool,
) -> Result<DownloadWallpaperResult, String> {
    let Some(index) = stage_state
        .items
        .iter()
        .position(|entry| entry.source_url == source_url)
    else {
        return Err("未找到指定故宫候选壁纸，请先重新获取一批。".into());
    };
    let entry = stage_state.items.remove(index);

    if let Some(existing_index) = find_wallpaper_index(wall_state, &entry.source_url, &entry.id) {
        {
            let item = &mut wall_state.files[existing_index];
            item.source_kind = remote_source_kind(WallpaperRemoteSource::Palace).into();
            item.remote_id = entry.id.clone();
            item.author_name = entry.credit_name.clone();
            item.author_url = entry.credit_url.clone();
            item.photo_url = entry.photo_url.clone();
        }
        if set_fixed {
            wall_state.fixed_source_url = entry.source_url.clone();
        }
        let staged_file = stage_dir.join(&entry.file_name);
        match fs::remove_file(&staged_file) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("删除故宫候选文件失败: {}", err)),
        }
        let summary = local_wallpaper_summary(
            &wall_state.files[existing_index],
            &wall_state.fixed_source_url,
        );
        return Ok(DownloadWallpaperResult {
            added: false,
            source_url: entry.source_url.clone(),
            path: wall_state.files[existing_index].path.clone(),
            is_fixed: summary.is_fixed,
            wallpaper: summary,
        });
    }

    let staged_file = stage_dir.join(&entry.file_name);
    if !staged_file.exists() {
        return Err("故宫候选图片文件不存在，请先重新获取一批。".into());
    }

    let now = now_ts();
    let extension = Path::new(&entry.file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("jpg");
    let file_name = format!(
        "wallpaper_{}_{}.{}",
        hash_url(&entry.source_url),
        now,
        extension
    );
    let target_path = dir.join(file_name);
    move_or_copy_path(&staged_file, &target_path)?;

    wall_state.files.push(WallpaperFile {
        path: path_to_string(&target_path),
        added_at: now,
        source_url: entry.source_url.clone(),
        last_shown_at: 0,
        source_kind: remote_source_kind(WallpaperRemoteSource::Palace).into(),
        remote_id: entry.id.clone(),
        thumb_url: String::new(),
        author_name: entry.credit_name.clone(),
        author_url: entry.credit_url.clone(),
        photo_url: entry.photo_url.clone(),
    });
    wall_state.last_download_at = now;
    if set_fixed {
        wall_state.fixed_source_url = entry.source_url.clone();
    }
    enforce_wallpaper_limit(wall_state);
    let local_index = find_wallpaper_index(wall_state, &entry.source_url, &entry.id)
        .ok_or_else(|| "采纳故宫候选壁纸失败。".to_string())?;
    let summary =
        local_wallpaper_summary(&wall_state.files[local_index], &wall_state.fixed_source_url);
    Ok(DownloadWallpaperResult {
        added: true,
        source_url: entry.source_url.clone(),
        path: wall_state.files[local_index].path.clone(),
        is_fixed: summary.is_fixed,
        wallpaper: summary,
    })
}

fn list_local_wallpaper_summaries(wall_state: &WallpaperState) -> Vec<LocalWallpaperSummary> {
    let mut items = wall_state
        .files
        .iter()
        .map(|file| local_wallpaper_summary(file, &wall_state.fixed_source_url))
        .collect::<Vec<_>>();
    items.sort_by_key(|item| std::cmp::Reverse(item.added_at));
    items
}

fn track_unsplash_download(app: &AppHandle, client: &Client, download_location: &str) {
    if download_location.trim().is_empty() {
        return;
    }
    if let Err(err) = client.get(download_location).send() {
        append_wallpaper_log(app, &format!("Unsplash 下载追踪失败: {}", err));
    }
}

struct WallpaperDownloadMetadata<'a> {
    source_kind: WallpaperRemoteSource,
    remote_id: &'a str,
    source_url: String,
    thumb_url: &'a str,
    author_name: &'a str,
    author_url: &'a str,
    photo_url: &'a str,
}

fn persist_downloaded_wallpaper(
    wall_state: &mut WallpaperState,
    dir: &Path,
    bytes: &[u8],
    metadata: WallpaperDownloadMetadata<'_>,
    set_fixed: bool,
) -> Result<DownloadWallpaperResult, String> {
    let WallpaperDownloadMetadata {
        source_kind,
        remote_id,
        source_url,
        thumb_url,
        author_name,
        author_url,
        photo_url,
    } = metadata;
    if let Some(index) = find_wallpaper_index(wall_state, &source_url, remote_id) {
        {
            let entry = &mut wall_state.files[index];
            entry.source_kind = remote_source_kind(source_kind).into();
            entry.remote_id = remote_id.to_string();
            entry.thumb_url = thumb_url.to_string();
            entry.author_name = author_name.to_string();
            entry.author_url = author_url.to_string();
            entry.photo_url = photo_url.to_string();
        }
        if set_fixed {
            wall_state.fixed_source_url = source_url.clone();
        }
        let summary =
            local_wallpaper_summary(&wall_state.files[index], &wall_state.fixed_source_url);
        return Ok(DownloadWallpaperResult {
            added: false,
            source_url,
            path: wall_state.files[index].path.clone(),
            is_fixed: summary.is_fixed,
            wallpaper: summary,
        });
    }

    let now = now_ts();
    let file_name = format!("wallpaper_{}_{}.jpg", hash_url(&source_url), now);
    let file_path = dir.join(file_name);
    fs::write(&file_path, bytes).map_err(|err| err.to_string())?;
    let stored = WallpaperFile {
        path: file_path.to_string_lossy().to_string(),
        added_at: now,
        source_url: source_url.clone(),
        last_shown_at: 0,
        source_kind: remote_source_kind(source_kind).into(),
        remote_id: remote_id.to_string(),
        thumb_url: thumb_url.to_string(),
        author_name: author_name.to_string(),
        author_url: author_url.to_string(),
        photo_url: photo_url.to_string(),
    };
    wall_state.files.push(stored.clone());
    wall_state.last_download_at = now;
    if set_fixed {
        wall_state.fixed_source_url = source_url.clone();
    }
    let summary = local_wallpaper_summary(&stored, &wall_state.fixed_source_url);
    Ok(DownloadWallpaperResult {
        added: true,
        source_url,
        path: stored.path.clone(),
        is_fixed: summary.is_fixed,
        wallpaper: summary,
    })
}

fn download_unsplash_wallpaper_internal(
    app: &AppHandle,
    client: &Client,
    wall_state: &mut WallpaperState,
    dir: &Path,
    request: &UnsplashDownloadRequest,
    set_fixed: bool,
) -> Result<DownloadWallpaperResult, HttpFetchError> {
    let source_url = canonical_unsplash_source_url(&request.photo_url, &request.id);
    track_unsplash_download(app, client, &request.download_location);
    let image_url =
        build_unsplash_download_url(&request.raw_url).map_err(HttpFetchError::terminal)?;
    let response = client
        .get(image_url)
        .send()
        .map_err(HttpFetchError::Transport)?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(HttpFetchError::http_status(
            status,
            unsplash_error_message(status),
        ));
    }
    let bytes = response.bytes().map_err(HttpFetchError::Transport)?;
    let (width, height) = inspect_image_dimensions(&bytes).map_err(HttpFetchError::terminal)?;
    if !is_valid_wallpaper_size(width, height) {
        return Err(HttpFetchError::terminal("下载图片尺寸不满足桌面壁纸要求。"));
    }
    persist_downloaded_wallpaper(
        wall_state,
        dir,
        &bytes,
        WallpaperDownloadMetadata {
            source_kind: WallpaperRemoteSource::Unsplash,
            remote_id: &request.id,
            source_url,
            thumb_url: &request.thumb_url,
            author_name: &request.author_name,
            author_url: &request.author_url,
            photo_url: &request.photo_url,
        },
        set_fixed,
    )
    .map_err(HttpFetchError::terminal)
}

fn download_palace_wallpaper_internal(
    app: Option<&AppHandle>,
    wall_state: &mut WallpaperState,
    dir: &Path,
    request: &PalaceDownloadRequest,
    set_fixed: bool,
) -> Result<DownloadWallpaperResult, String> {
    let source_url = canonical_palace_source_url(&request.photo_url, &request.id);
    append_palace_debug(
        app,
        &format!(
            "manual_download_start id={} image_url={} set_fixed={}",
            request.id, request.image_url, set_fixed
        ),
    );
    let bytes = palace_fetch_bytes(
        app,
        &format!("manual download id={}", request.id),
        &request.image_url,
    )?;
    let (width, height) = inspect_image_dimensions(&bytes)?;
    if !is_valid_wallpaper_size(width, height) {
        return Err("下载图片尺寸不满足桌面壁纸要求。".into());
    }
    append_palace_debug(
        app,
        &format!(
            "manual_download_dimensions id={} width={} height={}",
            request.id, width, height
        ),
    );
    persist_downloaded_wallpaper(
        wall_state,
        dir,
        &bytes,
        WallpaperDownloadMetadata {
            source_kind: WallpaperRemoteSource::Palace,
            remote_id: &request.id,
            source_url,
            thumb_url: &request.thumb_url,
            author_name: &request.credit_name,
            author_url: &request.credit_url,
            photo_url: &request.photo_url,
        },
        set_fixed,
    )
}

fn try_prefetch_unsplash_wallpaper(
    app: &AppHandle,
    wall_state: &mut WallpaperState,
    dir: &Path,
    allow_download: bool,
    target_count: usize,
) -> Result<PrefetchStats, String> {
    if !allow_download {
        append_wallpaper_log(app, "预取跳过: allow_download=false");
        return Ok(PrefetchStats::default());
    }
    let now = now_ts();
    if now.saturating_sub(wall_state.last_download_at) <= WALLPAPER_MIN_INTERVAL_SECS {
        append_wallpaper_log(app, "预取跳过: 与上次下载间隔过短");
        return Ok(PrefetchStats::default());
    }
    let access_key = resolve_unsplash_access_key(app).access_key.ok_or_else(|| {
        "未配置 Unsplash Access Key，可在壁纸设置里填写，或继续使用环境变量。".to_string()
    })?;
    with_unsplash_client(&access_key, Some(app), "prefetch", |client| {
        let mut stats = PrefetchStats {
            source_kind: remote_source_kind(WallpaperRemoteSource::Unsplash).into(),
            source_label: remote_source_label(WallpaperRemoteSource::Unsplash).into(),
            ..PrefetchStats::default()
        };
        let mut rng = rand::thread_rng();
        for page in 1..=WALLPAPER_LIST_PAGE_SAMPLE {
            if stats.added >= target_count {
                break;
            }
            append_wallpaper_log(app, &format!("Unsplash 主题拉取: page={}", page));
            let search = fetch_unsplash_search_page(client, "", page, WALLPAPER_SEARCH_PAGE_SIZE)?;
            stats.list_successes += 1;
            let mut items = search
                .items
                .into_iter()
                .filter_map(|item| {
                    serde_json::from_value::<UnsplashDownloadRequest>(item.download_payload).ok()
                })
                .collect::<Vec<_>>();
            items.shuffle(&mut rng);
            for item in items {
                if stats.added >= target_count {
                    break;
                }
                match download_unsplash_wallpaper_internal(
                    app, client, wall_state, dir, &item, false,
                ) {
                    Ok(result) => {
                        if result.added {
                            stats.added += 1;
                            append_wallpaper_log(
                                app,
                                &format!("Unsplash 下载成功: {}", result.path),
                            );
                        } else {
                            append_wallpaper_log(
                                app,
                                &format!("Unsplash 重复图片跳过: {}", result.source_url),
                            );
                        }
                    }
                    Err(err) => {
                        append_wallpaper_log(app, &format!("Unsplash 下载失败: {}", err));
                    }
                }
            }
            if !search.has_next_page {
                break;
            }
        }
        Ok(stats)
    })
}

fn try_prefetch_palace_wallpaper(
    app: &AppHandle,
    wall_state: &WallpaperState,
    dir: &Path,
    allow_download: bool,
    _target_count: usize,
) -> Result<PrefetchStats, String> {
    ensure_palace_wallpaper_source_enabled()?;
    if !allow_download {
        append_wallpaper_log(app, "故宫预取跳过: allow_download=false");
        return Ok(PrefetchStats::default());
    }
    let now = now_ts();
    if now.saturating_sub(wall_state.last_batch_at) <= WALLPAPER_MIN_INTERVAL_SECS {
        append_wallpaper_log(app, "故宫候选批次跳过: 与上次批次间隔过短");
        return Ok(PrefetchStats::default());
    }
    let (_, stats, _) = refresh_palace_staging_batch_inner(app, wall_state, dir, 1, |_, _, _| {})?;
    Ok(stats)
}

fn try_prefetch_wallpaper(
    app: &AppHandle,
    wall_state: &mut WallpaperState,
    dir: &Path,
    allow_download: bool,
    target_count: usize,
) -> Result<PrefetchStats, String> {
    match resolve_online_source(app) {
        WallpaperRemoteSource::Unsplash => {
            try_prefetch_unsplash_wallpaper(app, wall_state, dir, allow_download, target_count)
        }
        WallpaperRemoteSource::Palace => {
            try_prefetch_palace_wallpaper(app, wall_state, dir, allow_download, target_count)
        }
    }
}

fn enforce_wallpaper_limit(wall_state: &mut WallpaperState) {
    if wall_state.files.len() <= WALLPAPER_CACHE_LIMIT {
        clamp_wallpaper_indices(wall_state);
        return;
    }
    wall_state.files.sort_by_key(|entry| entry.added_at);
    while wall_state.files.len() > WALLPAPER_CACHE_LIMIT {
        if let Some(oldest) = wall_state.files.first() {
            if oldest.source_url == wall_state.fixed_source_url {
                wall_state.fixed_source_url.clear();
            }
            let _ = fs::remove_file(&oldest.path);
        }
        wall_state.files.remove(0);
        if wall_state.next_show_index > 0 {
            wall_state.next_show_index -= 1;
        }
        if wall_state.next_source_index > 0 {
            wall_state.next_source_index -= 1;
        }
    }
    clamp_wallpaper_indices(wall_state);
}

fn should_run_weekly_batch(wall_state: &WallpaperState) -> bool {
    let now = now_ts();
    if wall_state.files.is_empty() {
        return true;
    }
    if wall_state.last_download_at > 0
        && now.saturating_sub(wall_state.last_download_at) >= WALLPAPER_BATCH_INTERVAL_SECS
    {
        return true;
    }
    now.saturating_sub(wall_state.last_batch_at) >= WALLPAPER_BATCH_INTERVAL_SECS
}

fn should_run_palace_staging_batch(stage_state: &PalaceStagingState) -> bool {
    let now = now_ts();
    if stage_state.items.is_empty() {
        return true;
    }
    now.saturating_sub(stage_state.last_batch_at) >= WALLPAPER_BATCH_INTERVAL_SECS
}

fn run_wallpaper_batch(
    app: &AppHandle,
    force: bool,
    target_count: usize,
    reason: &str,
) -> Result<PrefetchStats, String> {
    let state = app.state::<AppState>();

    // 防并发：同一时间只允许一个 batch 在运行
    let Some(_batch_guard) = BatchInProgressGuard::try_acquire(&state.batch_in_progress) else {
        append_wallpaper_log(app, &format!("批量获取跳过（已有任务在运行）: {}", reason));
        return Ok(PrefetchStats::default());
    };

    run_wallpaper_batch_inner(app, force, target_count, reason)
}

#[allow(clippy::too_many_arguments)]
fn run_wallpaper_batch_inner(
    app: &AppHandle,
    force: bool,
    target_count: usize,
    reason: &str,
) -> Result<PrefetchStats, String> {
    let state = app.state::<AppState>();
    let _guard = state
        .wallpaper_lock
        .lock()
        .map_err(|_| "壁纸锁被占用".to_string())?;
    let dir = ensure_wallpaper_dir(app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    let (repaired, recovered) = normalize_wallpaper_state(&dir, &mut wall_state)?;
    if repaired > 0 || recovered > 0 {
        append_wallpaper_log(
            app,
            &format!(
                "壁纸状态已修复: repaired={} recovered={}",
                repaired, recovered
            ),
        );
    }

    let source = resolve_online_source(app);
    let effective_target_count = if source == WallpaperRemoteSource::Palace {
        PALACE_STAGING_BATCH_SIZE
    } else {
        target_count
    };
    append_wallpaper_log(
        app,
        &format!("{} source={}", reason, remote_source_label(source)),
    );

    if source == WallpaperRemoteSource::Unsplash && !force && !should_run_weekly_batch(&wall_state)
    {
        append_wallpaper_log(app, "预取跳过: 未到每周下载时间");
        return Ok(PrefetchStats::default());
    }

    if source == WallpaperRemoteSource::Palace {
        let stage_dir = palace_staging_dir(&dir);
        let stage_index = palace_staging_index_path(&stage_dir);
        let mut stage_state = load_palace_staging_state(&stage_index);
        normalize_palace_staging_state(&stage_dir, &mut stage_state);
        if !force && !should_run_palace_staging_batch(&stage_state) {
            append_wallpaper_log(app, "故宫候选批次跳过: 未到每周刷新时间");
            return Ok(PrefetchStats::default());
        }
    }

    let stats = try_prefetch_wallpaper(app, &mut wall_state, &dir, true, effective_target_count)?;
    if stats.list_successes > 0 {
        wall_state.last_batch_at = now_ts();
    } else {
        append_wallpaper_log(app, "预取未拿到有效列表响应，本次不更新批次时间");
    }
    enforce_wallpaper_limit(&mut wall_state);
    save_wallpaper_state(&state_path, &wall_state)?;
    append_wallpaper_log(
        app,
        &format!(
            "预取完成: source={} list_successes={} added={}",
            stats.source_label, stats.list_successes, stats.added
        ),
    );
    Ok(stats)
}

fn load_palace_staging_batch_result_with_lock(
    app: &AppHandle,
) -> Result<PalaceStagingBatchResult, String> {
    let state = app.state::<AppState>();
    let _guard = state
        .wallpaper_lock
        .lock()
        .map_err(|_| "壁纸锁被占用".to_string())?;
    let dir = ensure_wallpaper_dir(app)?;
    let stage_dir = palace_staging_dir(&dir);
    fs::create_dir_all(&stage_dir).map_err(|err| err.to_string())?;
    let state_path = palace_staging_index_path(&stage_dir);
    let mut stage_state = load_palace_staging_state(&state_path);
    normalize_palace_staging_state(&stage_dir, &mut stage_state);
    save_palace_staging_state(&state_path, &stage_state)?;
    Ok(palace_staging_batch_result(
        &stage_dir,
        &stage_state,
        0,
        false,
        0,
        0,
    ))
}

fn prepare_palace_staging_refresh(
    app: &AppHandle,
) -> Result<(PathBuf, WallpaperState, PalaceStagingBatchResult), String> {
    let state = app.state::<AppState>();
    let _guard = state
        .wallpaper_lock
        .lock()
        .map_err(|_| "壁纸锁被占用".to_string())?;
    let dir = ensure_wallpaper_dir(app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    normalize_wallpaper_state(&dir, &mut wall_state)?;
    save_wallpaper_state(&state_path, &wall_state)?;

    let stage_dir = palace_staging_dir(&dir);
    fs::create_dir_all(&stage_dir).map_err(|err| err.to_string())?;
    let stage_state_path = palace_staging_index_path(&stage_dir);
    let mut stage_state = load_palace_staging_state(&stage_state_path);
    normalize_palace_staging_state(&stage_dir, &mut stage_state);
    save_palace_staging_state(&stage_state_path, &stage_state)?;
    let batch = palace_staging_batch_result(&stage_dir, &stage_state, 0, false, 0, 0);
    Ok((dir, wall_state, batch))
}

fn run_palace_staging_refresh_task(
    app: &AppHandle,
    root_dir: PathBuf,
    wall_state: WallpaperState,
    current_committed_page: usize,
    target_page: usize,
) -> Result<PalaceStagingBatchResult, String> {
    let (stage_state, stats, replaced_previous_batch) = refresh_palace_staging_batch_inner(
        app,
        &wall_state,
        &root_dir,
        target_page,
        |processed_entries, total_entries, message| {
            let _ = set_palace_staging_refresh_status(
                app,
                palace_refresh_running_status(
                    target_page,
                    current_committed_page,
                    processed_entries,
                    total_entries,
                    message,
                ),
            );
        },
    )?;
    let stage_dir = palace_staging_dir(&root_dir);
    Ok(palace_staging_batch_result(
        &stage_dir,
        &stage_state,
        stats.added,
        replaced_previous_batch,
        0,
        0,
    ))
}

fn run_weekly_batch(app: AppHandle) {
    if let Err(err) =
        run_wallpaper_batch(&app, false, WALLPAPER_BATCH_SIZE, "预取触发: 每周批量下载")
    {
        append_wallpaper_log(&app, &format!("预取失败: {}", err));
    }
}

fn apply_default_window_icon<R: tauri::Runtime>(
    app: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
) {
    if let Some(icon) = app.default_window_icon().cloned() {
        let _ = window.set_icon(icon);
    }
}

#[tauri::command]
fn prefetch_lock_wallpaper(app: AppHandle) -> Result<(), String> {
    std::thread::spawn(move || run_weekly_batch(app));
    Ok(())
}

#[tauri::command]
async fn refresh_lock_wallpaper_now(app: AppHandle) -> Result<PrefetchStats, String> {
    tauri::async_runtime::spawn_blocking(move || {
        run_wallpaper_batch(
            &app,
            true,
            WALLPAPER_MANUAL_REFRESH_SIZE,
            "手动刷新触发: 立即刷新壁纸",
        )
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
fn get_wallpaper_storage_settings(app: AppHandle) -> Result<WallpaperStorageSettings, String> {
    let settings = get_wallpaper_storage_settings_inner(&app)?;
    let current_dir = PathBuf::from(&settings.current_dir);
    fs::create_dir_all(&current_dir).map_err(|err| err.to_string())?;
    allow_wallpaper_dir_on_scope(&app, &current_dir)?;
    Ok(settings)
}

#[tauri::command]
fn set_wallpaper_storage_dir(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    path: Option<String>,
) -> Result<WallpaperStorageUpdateResult, String> {
    ensure_palace_refresh_not_running(&app)?;
    let _guard = state.wallpaper_lock.lock().map_err(|_| "壁纸锁被占用")?;
    let config_path = wallpaper_storage_config_path(&app)?;
    let current_config = load_wallpaper_storage_config(&config_path);
    let current_settings = wallpaper_storage_settings_from_config(&app, &current_config)?;
    let current_dir = ensure_wallpaper_dir(&app)?;
    let default_dir = wallpaper_default_dir(&app)?;
    let mut restored_default = false;

    let next_dir = match path {
        Some(value) if !value.trim().is_empty() => {
            let candidate = PathBuf::from(value.trim());
            if !candidate.is_absolute() {
                return Err("请输入绝对路径，或者使用“选择文件夹”按钮。".into());
            }
            candidate
        }
        _ => {
            restored_default = true;
            default_dir.clone()
        }
    };

    if !current_dir.exists() {
        ensure_dir_ready(&current_dir, "当前壁纸目录")?;
    }
    ensure_dir_ready(&next_dir, "目标目录")?;

    let same_dir = fs::canonicalize(&current_dir).unwrap_or(current_dir.clone())
        == fs::canonicalize(&next_dir).unwrap_or(next_dir.clone());

    if !same_dir && !is_directory_empty(&next_dir)? {
        return Err("目标目录必须为空，请选择一个空文件夹。".into());
    }

    let migrated_files = if same_dir {
        0
    } else {
        match migrate_wallpaper_dir_contents(&current_dir, &next_dir) {
            Ok(count) => count,
            Err(err) if restored_default => return Err(format!("恢复默认失败: {}", err)),
            Err(err) => return Err(err),
        }
    };

    let next_config = WallpaperStorageConfig {
        custom_dir: if restored_default {
            String::new()
        } else {
            path_to_string(&next_dir)
        },
    };
    save_wallpaper_storage_config(&config_path, &next_config)
        .map_err(|err| format!("保存壁纸目录配置失败: {}", err))?;

    allow_wallpaper_dir_on_scope(&app, &next_dir)?;
    let settings = wallpaper_storage_settings_from_config(&app, &next_config)?;
    append_wallpaper_log(
        &app,
        &format!(
            "壁纸目录已切换: from={} to={} migrated_files={}",
            current_settings.current_dir, settings.current_dir, migrated_files
        ),
    );
    emit_wallpaper_storage_updated(&app, &settings)?;

    Ok(WallpaperStorageUpdateResult {
        settings,
        migrated_files,
        restored_default,
    })
}

#[tauri::command]
fn get_unsplash_settings(app: AppHandle) -> Result<UnsplashSettings, String> {
    Ok(get_unsplash_settings_inner(&app))
}

#[tauri::command]
fn set_unsplash_access_key(app: AppHandle, access_key: String) -> Result<UnsplashSettings, String> {
    let access_key = access_key.trim();
    if access_key.is_empty() {
        return Err("请输入 Unsplash Access Key。".into());
    }
    let config_path = unsplash_settings_config_path(&app)?;
    let config = UnsplashSettingsConfig {
        access_key: access_key.to_string(),
    };
    save_unsplash_settings_config(&config_path, &config)
        .map_err(|err| format!("保存 Unsplash Access Key 失败: {}", err))?;
    Ok(get_unsplash_settings_inner(&app))
}

#[tauri::command]
fn clear_unsplash_access_key(app: AppHandle) -> Result<UnsplashSettings, String> {
    let config_path = unsplash_settings_config_path(&app)?;
    if config_path.exists() {
        fs::remove_file(&config_path)
            .map_err(|err| format!("清除 Unsplash Access Key 失败: {}", err))?;
    }
    Ok(get_unsplash_settings_inner(&app))
}

#[tauri::command]
async fn search_unsplash_wallpapers(
    app: AppHandle,
    query: String,
    page: usize,
    per_page: usize,
) -> Result<RemoteWallpaperSearchResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        search_unsplash_wallpapers_blocking(&app, query, page, per_page)
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn browse_palace_wallpapers(
    page: usize,
    per_page: usize,
) -> Result<RemoteWallpaperSearchResult, String> {
    ensure_palace_wallpaper_source_enabled()?;
    tauri::async_runtime::spawn_blocking(move || browse_palace_wallpapers_blocking(page, per_page))
        .await
        .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn refresh_palace_staging_batch(
    app: AppHandle,
    page: Option<usize>,
) -> Result<PalaceStagingBatchResult, String> {
    ensure_palace_wallpaper_source_enabled()?;
    let state = app.state::<AppState>();
    let Some(batch_guard) = BatchInProgressGuard::try_acquire(&state.batch_in_progress) else {
        return Err("已有壁纸批量任务在运行，请稍后再试。".into());
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _batch_guard = batch_guard;
        let (dir, wall_state, current_batch) = prepare_palace_staging_refresh(&app)?;
        let target_page = page.unwrap_or(current_batch.page).max(1);
        let stage_dir = palace_staging_dir(&dir);
        let (stage_state, stats, replaced_previous_batch) =
            refresh_palace_staging_batch_inner(&app, &wall_state, &dir, target_page, |_, _, _| {})?;
        append_wallpaper_log(
            &app,
            &format!(
                "故宫候选批次刷新完成: page={} list_successes={} fetched={} replaced_previous_batch={}",
                stage_state.current_page,
                stats.list_successes,
                stats.added,
                replaced_previous_batch
            ),
        );
        Ok(palace_staging_batch_result(
            &stage_dir,
            &stage_state,
            stats.added,
            replaced_previous_batch,
            0,
            0,
        ))
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
fn get_palace_staging_batch(app: AppHandle) -> Result<PalaceStagingBatchResult, String> {
    ensure_palace_wallpaper_source_enabled()?;
    load_palace_staging_batch_result_with_lock(&app)
}

#[tauri::command]
fn get_palace_staging_refresh_status(app: AppHandle) -> Result<PalaceStagingRefreshStatus, String> {
    ensure_palace_wallpaper_source_enabled()?;
    let mut status = current_palace_staging_refresh_status(&app)?;
    let batch = load_palace_staging_batch_result_with_lock(&app)?;
    if status.current_committed_page == 0 || status.state != PalaceStagingRefreshState::Running {
        status.current_committed_page = batch.page.max(1);
    }
    if status.target_page == 0 {
        status.target_page = batch.page.max(1);
    }
    if status.state == PalaceStagingRefreshState::Idle && status.message.is_empty() {
        status.message = format!("当前展示的是故宫第 {} 页候选壁纸。", batch.page.max(1));
    }
    Ok(status)
}

#[tauri::command]
fn start_palace_staging_refresh(
    app: AppHandle,
    page: Option<usize>,
) -> Result<PalaceStagingRefreshStatus, String> {
    ensure_palace_wallpaper_source_enabled()?;
    let existing_status = current_palace_staging_refresh_status(&app)?;
    if existing_status.state == PalaceStagingRefreshState::Running {
        return Ok(existing_status);
    }

    let state = app.state::<AppState>();
    let Some(batch_guard) = BatchInProgressGuard::try_acquire(&state.batch_in_progress) else {
        return Err("已有壁纸批量任务在运行，请稍后再试。".into());
    };
    // 重置取消标志，开始新一轮刷新
    state.cancel_refresh.store(false, Ordering::SeqCst);

    let (root_dir, wall_state, current_batch) = prepare_palace_staging_refresh(&app)?;
    let target_page = page.unwrap_or(current_batch.page).max(1);
    let running_status = set_palace_staging_refresh_status(
        &app,
        palace_refresh_running_status(
            target_page,
            current_batch.page.max(1),
            0,
            0,
            format!("正在获取第 {} 页候选壁纸...", target_page),
        ),
    )?;

    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        let _batch_guard = batch_guard;
        let background_result = tauri::async_runtime::spawn_blocking({
            let app_for_block = app_for_task.clone();
            move || {
                run_palace_staging_refresh_task(
                    &app_for_block,
                    root_dir,
                    wall_state,
                    current_batch.page.max(1),
                    target_page,
                )
            }
        })
        .await;

        match background_result {
            Ok(Ok(batch)) => {
                let _ = set_palace_staging_refresh_status(
                    &app_for_task,
                    PalaceStagingRefreshStatus {
                        state: PalaceStagingRefreshState::Succeeded,
                        target_page,
                        current_committed_page: batch.page.max(1),
                        processed_entries: batch.items.len(),
                        total_entries: batch.items.len(),
                        message: format!(
                            "故宫第 {} 页候选壁纸获取完成，当前有 {} 张候选图。",
                            batch.page.max(1),
                            batch.items.len()
                        ),
                        error_message: None,
                        batch: Some(batch),
                    },
                );
            }
            Ok(Err(err)) => {
                let _ = set_palace_staging_refresh_status(
                    &app_for_task,
                    PalaceStagingRefreshStatus {
                        state: PalaceStagingRefreshState::Failed,
                        target_page,
                        current_committed_page: current_batch.page.max(1),
                        processed_entries: 0,
                        total_entries: 0,
                        message: format!("故宫第 {} 页候选壁纸获取失败。", target_page),
                        error_message: Some(err),
                        batch: None,
                    },
                );
            }
            Err(err) => {
                let _ = set_palace_staging_refresh_status(
                    &app_for_task,
                    PalaceStagingRefreshStatus {
                        state: PalaceStagingRefreshState::Failed,
                        target_page,
                        current_committed_page: current_batch.page.max(1),
                        processed_entries: 0,
                        total_entries: 0,
                        message: format!("故宫第 {} 页候选壁纸获取失败。", target_page),
                        error_message: Some(err.to_string()),
                        batch: None,
                    },
                );
            }
        }
    });

    Ok(running_status)
}

#[tauri::command]
fn cancel_palace_staging_refresh(app: AppHandle) -> Result<PalaceStagingRefreshStatus, String> {
    ensure_palace_wallpaper_source_enabled()?;
    let state = app.state::<AppState>();
    state.cancel_refresh.store(true, Ordering::SeqCst);
    let mut status = PalaceStagingRefreshStatus {
        state: PalaceStagingRefreshState::Cancelled,
        target_page: 1,
        current_committed_page: 1,
        processed_entries: 0,
        total_entries: 0,
        message: "故宫候选壁纸获取已取消。".into(),
        error_message: None,
        batch: None,
    };
    let batch = load_palace_staging_batch_result_with_lock(&app).ok();
    if let Some(b) = &batch {
        status.current_committed_page = b.page.max(1);
        status.target_page = b.page.max(1);
    }
    set_palace_staging_refresh_status(&app, status)
}

#[tauri::command]
fn list_palace_staging_wallpapers(
    app: AppHandle,
) -> Result<Vec<PalaceStagingWallpaperSummary>, String> {
    let result = get_palace_staging_batch(app)?;
    Ok(result.items)
}

#[tauri::command]
async fn promote_palace_staging_wallpaper(
    app: AppHandle,
    source_url: String,
    set_fixed: bool,
) -> Result<DownloadWallpaperResult, String> {
    ensure_palace_wallpaper_source_enabled()?;
    tauri::async_runtime::spawn_blocking(move || {
        ensure_palace_refresh_not_running(&app)?;
        let state = app.state::<AppState>();
        let _guard = state
            .wallpaper_lock
            .lock()
            .map_err(|_| "壁纸锁被占用".to_string())?;
        let dir = ensure_wallpaper_dir(&app)?;
        let state_path = dir.join("index.json");
        let mut wall_state = load_wallpaper_state(&state_path);
        normalize_wallpaper_state(&dir, &mut wall_state)?;

        let stage_dir = palace_staging_dir(&dir);
        fs::create_dir_all(&stage_dir).map_err(|err| err.to_string())?;
        let stage_state_path = palace_staging_index_path(&stage_dir);
        let mut stage_state = load_palace_staging_state(&stage_state_path);
        normalize_palace_staging_state(&stage_dir, &mut stage_state);
        let result = promote_palace_staging_entry_internal(
            &dir,
            &mut wall_state,
            &stage_dir,
            &mut stage_state,
            &source_url,
            set_fixed,
        )?;

        save_wallpaper_state(&state_path, &wall_state)?;
        save_palace_staging_state(&stage_state_path, &stage_state)?;
        append_wallpaper_log(
            &app,
            &format!(
                "故宫候选已加入本地库: source={} added={} fixed={}",
                result.source_url, result.added, result.is_fixed
            ),
        );
        Ok(result)
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn promote_palace_staging_wallpapers(
    app: AppHandle,
    source_urls: Vec<String>,
) -> Result<PalaceStagingBatchResult, String> {
    ensure_palace_wallpaper_source_enabled()?;
    tauri::async_runtime::spawn_blocking(move || {
        ensure_palace_refresh_not_running(&app)?;
        let state = app.state::<AppState>();
        let _guard = state
            .wallpaper_lock
            .lock()
            .map_err(|_| "壁纸锁被占用".to_string())?;
        let requested = source_urls
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if requested.is_empty() {
            return Err("请先选择至少一张故宫候选壁纸。".into());
        }

        let dir = ensure_wallpaper_dir(&app)?;
        let state_path = dir.join("index.json");
        let mut wall_state = load_wallpaper_state(&state_path);
        normalize_wallpaper_state(&dir, &mut wall_state)?;

        let stage_dir = palace_staging_dir(&dir);
        fs::create_dir_all(&stage_dir).map_err(|err| err.to_string())?;
        let stage_state_path = palace_staging_index_path(&stage_dir);
        let mut stage_state = load_palace_staging_state(&stage_state_path);
        normalize_palace_staging_state(&stage_dir, &mut stage_state);

        let mut processed_count = 0usize;
        let mut skipped_count = 0usize;
        let mut first_error: Option<String> = None;

        for source_url in requested {
            if !stage_state
                .items
                .iter()
                .any(|entry| entry.source_url == source_url)
            {
                continue;
            }
            match promote_palace_staging_entry_internal(
                &dir,
                &mut wall_state,
                &stage_dir,
                &mut stage_state,
                &source_url,
                false,
            ) {
                Ok(result) => {
                    if result.added {
                        processed_count += 1;
                    } else {
                        skipped_count += 1;
                    }
                }
                Err(err) => {
                    append_wallpaper_log(
                        &app,
                        &format!("故宫候选批量加入失败: source={} error={}", source_url, err),
                    );
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        if processed_count == 0 && skipped_count == 0 {
            return Err(first_error.unwrap_or_else(|| "没有可处理的故宫候选壁纸。".into()));
        }

        save_wallpaper_state(&state_path, &wall_state)?;
        save_palace_staging_state(&stage_state_path, &stage_state)?;
        append_wallpaper_log(
            &app,
            &format!(
                "故宫候选批量加入完成: processed={} skipped={} remaining={}",
                processed_count,
                skipped_count,
                stage_state.items.len()
            ),
        );
        Ok(palace_staging_batch_result(
            &stage_dir,
            &stage_state,
            0,
            false,
            processed_count,
            skipped_count,
        ))
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
fn list_local_wallpapers(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<LocalWallpaperSummary>, String> {
    let _guard = state.wallpaper_lock.lock().map_err(|_| "壁纸锁被占用")?;
    let dir = ensure_wallpaper_dir(&app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    normalize_wallpaper_state(&dir, &mut wall_state)?;
    enforce_wallpaper_limit(&mut wall_state);
    save_wallpaper_state(&state_path, &wall_state)?;
    Ok(list_local_wallpaper_summaries(&wall_state))
}

#[tauri::command]
fn set_fixed_wallpaper(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    source_url: String,
) -> Result<(), String> {
    let _guard = state.wallpaper_lock.lock().map_err(|_| "壁纸锁被占用")?;
    let dir = ensure_wallpaper_dir(&app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    normalize_wallpaper_state(&dir, &mut wall_state)?;
    if wall_state
        .files
        .iter()
        .any(|entry| entry.source_url == source_url)
    {
        wall_state.fixed_source_url = source_url.clone();
        save_wallpaper_state(&state_path, &wall_state)?;
        append_wallpaper_log(&app, &format!("固定壁纸已更新: {}", source_url));
        return Ok(());
    }
    Err("未找到指定壁纸".into())
}

#[tauri::command]
fn clear_fixed_wallpaper(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let _guard = state.wallpaper_lock.lock().map_err(|_| "壁纸锁被占用")?;
    let dir = ensure_wallpaper_dir(&app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    normalize_wallpaper_state(&dir, &mut wall_state)?;
    wall_state.fixed_source_url.clear();
    save_wallpaper_state(&state_path, &wall_state)?;
    append_wallpaper_log(&app, "固定壁纸已取消");
    Ok(())
}

#[tauri::command]
fn delete_local_wallpaper(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    source_url: String,
) -> Result<(), String> {
    let _guard = state.wallpaper_lock.lock().map_err(|_| "壁纸锁被占用")?;
    let dir = ensure_wallpaper_dir(&app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    normalize_wallpaper_state(&dir, &mut wall_state)?;

    let Some(index) = wall_state
        .files
        .iter()
        .position(|entry| entry.source_url == source_url)
    else {
        return Err("未找到指定壁纸".into());
    };

    let removed = wall_state.files.remove(index);
    match fs::remove_file(&removed.path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("删除本地壁纸文件失败: {}", err)),
    }

    if removed.source_url == wall_state.fixed_source_url {
        wall_state.fixed_source_url.clear();
    }
    if index < wall_state.next_show_index {
        wall_state.next_show_index -= 1;
    }
    if index < wall_state.next_source_index {
        wall_state.next_source_index -= 1;
    }
    clamp_wallpaper_indices(&mut wall_state);
    save_wallpaper_state(&state_path, &wall_state)?;
    append_wallpaper_log(&app, &format!("本地壁纸已删除: {}", removed.path));
    Ok(())
}

#[tauri::command]
async fn download_unsplash_wallpaper(
    app: AppHandle,
    payload: UnsplashDownloadRequest,
    set_fixed: bool,
) -> Result<DownloadWallpaperResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _guard = state
            .wallpaper_lock
            .lock()
            .map_err(|_| "壁纸锁被占用".to_string())?;
        let dir = ensure_wallpaper_dir(&app)?;
        let state_path = dir.join("index.json");
        let mut wall_state = load_wallpaper_state(&state_path);
        normalize_wallpaper_state(&dir, &mut wall_state)?;
        let access_key = resolve_unsplash_access_key(&app)
            .access_key
            .ok_or_else(|| {
                "未配置 Unsplash Access Key，可在壁纸设置里填写，或继续使用环境变量。".to_string()
            })?;
        let result = with_unsplash_client(&access_key, Some(&app), "download", |client| {
            download_unsplash_wallpaper_internal(
                &app,
                client,
                &mut wall_state,
                &dir,
                &payload,
                set_fixed,
            )
        })?;
        enforce_wallpaper_limit(&mut wall_state);
        save_wallpaper_state(&state_path, &wall_state)?;
        append_wallpaper_log(
            &app,
            &format!(
                "手动下载完成: source={} added={} fixed={}",
                result.source_url, result.added, result.is_fixed
            ),
        );
        Ok(result)
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn download_palace_wallpaper(
    app: AppHandle,
    payload: PalaceDownloadRequest,
    set_fixed: bool,
) -> Result<DownloadWallpaperResult, String> {
    ensure_palace_wallpaper_source_enabled()?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _guard = state
            .wallpaper_lock
            .lock()
            .map_err(|_| "壁纸锁被占用".to_string())?;
        let dir = ensure_wallpaper_dir(&app)?;
        let state_path = dir.join("index.json");
        let mut wall_state = load_wallpaper_state(&state_path);
        normalize_wallpaper_state(&dir, &mut wall_state)?;
        let result = download_palace_wallpaper_internal(
            Some(&app),
            &mut wall_state,
            &dir,
            &payload,
            set_fixed,
        )?;
        enforce_wallpaper_limit(&mut wall_state);
        save_wallpaper_state(&state_path, &wall_state)?;
        append_wallpaper_log(
            &app,
            &format!(
                "手动下载完成: source={} added={} fixed={}",
                result.source_url, result.added, result.is_fixed
            ),
        );
        Ok(result)
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
fn get_lock_wallpaper(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<String>, String> {
    let _guard = state.wallpaper_lock.lock().map_err(|_| "壁纸锁被占用")?;
    let dir = ensure_wallpaper_dir(&app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    normalize_wallpaper_state(&dir, &mut wall_state)?;
    enforce_wallpaper_limit(&mut wall_state);

    if wall_state.files.is_empty() {
        save_wallpaper_state(&state_path, &wall_state)?;
        append_wallpaper_log(&app, "锁屏读取: 无缓存壁纸");
        return Ok(None);
    }

    if !wall_state.fixed_source_url.is_empty() {
        if let Some(index) = wall_state
            .files
            .iter()
            .position(|entry| entry.source_url == wall_state.fixed_source_url)
        {
            let chosen = wall_state.files[index].path.clone();
            wall_state.files[index].last_shown_at = now_ts();
            save_wallpaper_state(&state_path, &wall_state)?;
            append_wallpaper_log(&app, &format!("锁屏读取固定壁纸: {}", chosen));
            return Ok(Some(chosen));
        }
        wall_state.fixed_source_url.clear();
    }

    clamp_wallpaper_indices(&mut wall_state);
    let show_index = wall_state.next_show_index.min(wall_state.files.len() - 1);
    let chosen = wall_state.files[show_index].path.clone();
    wall_state.files[show_index].last_shown_at = now_ts();
    wall_state.next_show_index = (show_index + 1) % wall_state.files.len();
    save_wallpaper_state(&state_path, &wall_state)?;
    append_wallpaper_log(&app, &format!("锁屏读取: {}", chosen));
    Ok(Some(chosen))
}

#[tauri::command]
fn request_quit(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.allow_exit.store(true, Ordering::SeqCst);
    restore_display_filter_for_exit(&app, "前端退出请求");
    app.exit(0);
    Ok(())
}

fn restore_colors_for_user(app: &AppHandle) -> Result<DisplayFilterStatus, String> {
    let status = platform::display_filter::restore_color_sync_settings()?;
    let _ = app.emit("display-filter-status", status.clone());
    if let Some(store) = app.try_state::<SettingsStore>() {
        let _guard = SETTINGS_UPDATE_LOCK
            .lock()
            .map_err(|_| "设置更新串行锁已损坏".to_string())?;
        let settings = store
            .update(|settings| settings.filter_enabled = false)
            .map_err(|error| error.to_string())?;
        let _ = app.emit("app-settings-updated", settings);
    }
    Ok(status)
}

fn restore_before_exit(app: &AppHandle) {
    if let Some(scheduler) = app.try_state::<RestScheduler>() {
        scheduler.shutdown();
    }
    restore_display_filter_for_exit(app, "应用退出事件");
}

fn restore_display_filter_for_exit(app: &AppHandle, context: &str) {
    match platform::restore_display_filter() {
        Ok(status) if !status.failures.is_empty() => append_app_log(
            app,
            &format!(
                "{context}恢复显示颜色部分失败（ColorSync 兜底={}）: {}",
                status.color_sync_fallback_used,
                status.failure_summary()
            ),
        ),
        Ok(_) => {}
        Err(error) => append_app_log(app, &format!("{context}恢复显示颜色失败: {error}")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec!["--hidden"]),
    ));

    let app = builder
        .manage(LockState::default())
        .manage(AppState::default())
        .setup(|app| {
            if let Err(error) = platform::power_events::register() {
                append_app_log(
                    app.handle(),
                    &format!("注册系统唤醒通知失败，将继续使用调度 tick-gap 兜底: {error}"),
                );
            }
            let settings_store = SettingsStore::from_app(app.handle())?;
            let settings_outcome = settings_store.load_or_recover()?;
            if settings_outcome.recovered_from.is_some()
                || settings_outcome.recovery_reason.is_some()
            {
                let recovered_from = settings_outcome
                    .recovered_from
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "未提供恢复路径".to_string());
                let recovery_reason = settings_outcome
                    .recovery_reason
                    .as_deref()
                    .unwrap_or("未提供损坏原因");
                append_app_log(
                    app.handle(),
                    &format!(
                        "应用设置已恢复为默认值: reason={recovery_reason}; recovered_from={recovered_from}"
                    ),
                );
            }
            let settings = settings_outcome.settings;
            let (scheduler, scheduler_events) =
                RestScheduler::spawn(SchedulerConfig::from(&settings))?;
            app.manage(settings_store);
            app.manage(scheduler);
            spawn_scheduler_bridge(app.handle().clone(), scheduler_events)
                .map_err(std::io::Error::other)?;

            #[cfg(target_os = "macos")]
            if let Err(error) = platform::display_filter::restore_color_sync_settings() {
                append_app_log(
                    app.handle(),
                    &format!("启动时恢复 ColorSync 默认设置失败: {error}"),
                );
            }
            match platform::apply_display_filter(FilterConfig::new(
                settings.filter_enabled,
                f64::from(settings.filter_strength),
                f64::from(settings.color_temperature),
            )) {
                Ok(status) if !status.failures.is_empty() => append_app_log(
                    app.handle(),
                    &format!("启动时显示滤镜部分失败: {}", status.failure_summary()),
                ),
                Ok(_) => {}
                Err(error) => {
                    append_app_log(app.handle(), &format!("启动时应用显示滤镜失败: {error}"))
                }
            }

            let wallpaper_dir = ensure_wallpaper_dir(app.handle())?;
            allow_wallpaper_dir_on_scope(app.handle(), &wallpaper_dir)?;
            // 启动时强制写入壁纸日志，确认目录
            append_wallpaper_log(app.handle(), "应用启动，日志初始化");
            let start_hidden = env::args_os().any(|argument| argument == "--hidden");
            if let Some(window) = app.get_webview_window("main") {
                apply_default_window_icon(app.handle(), &window);
                let _ = window.center();
                if !start_hidden {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            let tray_menu = MenuBuilder::new(app)
                .text("tray_show", "显示主界面")
                .text("tray_hide", "隐藏到托盘")
                .separator()
                .text("tray_rest", "立即休息")
                .text("tray_restore_colors", "恢复屏幕原始颜色")
                .separator()
                .text("tray_quit", "退出")
                .build()?;

            let tray_builder = TrayIconBuilder::new().tooltip("护眼吧").menu(&tray_menu);
            #[cfg(target_os = "macos")]
            let tray_builder = tray_builder
                .icon(MACOS_TRAY_ICON.clone())
                .icon_as_template(true)
                .show_menu_on_left_click(true);
            #[cfg(not(target_os = "macos"))]
            let tray_builder = tray_builder
                .icon(TRAY_ICON.clone())
                .show_menu_on_left_click(false);

            let tray = tray_builder
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
                            if cfg!(target_os = "windows")
                                && button == MouseButton::Left
                                && button_state == MouseButtonState::Up
                            {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        TrayIconEvent::DoubleClick { button, .. }
                            if cfg!(target_os = "windows") && button == MouseButton::Left =>
                        {
                            let visible = window.is_visible().unwrap_or(true);
                            if visible {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        _ => {}
                    }
                })
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "tray_show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "tray_hide" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.hide();
                        }
                    }
                    "tray_rest" => {
                        if let Some(scheduler) = app.try_state::<RestScheduler>() {
                            if let Err(error) = scheduler.start_now() {
                                append_app_log(app, &format!("托盘立即休息失败: {error}"));
                                let _ = app.emit("rest-scheduler-error", error.to_string());
                            }
                        }
                    }
                    "tray_restore_colors" => {
                        if let Err(error) = restore_colors_for_user(app) {
                            append_app_log(app, &format!("托盘恢复屏幕原始颜色失败: {error}"));
                            let _ = app.emit("display-filter-error", error);
                        }
                    }
                    "tray_quit" => {
                        if let Some(state) = app.try_state::<AppState>() {
                            state.allow_exit.store(true, Ordering::SeqCst);
                        }
                        restore_display_filter_for_exit(app, "托盘退出");
                        app.exit(0);
                    }
                    _ => {}
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
                    restore_display_filter_for_exit(window.app_handle(), "主窗口退出");
                }
                WindowEvent::Destroyed => {
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        if state.allow_exit.load(Ordering::SeqCst) {
                            restore_display_filter_for_exit(window.app_handle(), "主窗口销毁");
                        }
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            set_gamma,
            set_gamma_with_status,
            reset_gamma,
            get_display_filter_status,
            restore_display_colors,
            show_lock_windows,
            hide_lock_windows,
            broadcast_lock_update,
            get_lock_update,
            lockscreen_action,
            get_app_settings,
            update_app_settings,
            get_scheduler_state,
            start_rest_now,
            finish_rest,
            toggle_rest_pause,
            restart_rest_cycle,
            get_lock_wallpaper,
            prefetch_lock_wallpaper,
            refresh_lock_wallpaper_now,
            get_wallpaper_storage_settings,
            set_wallpaper_storage_dir,
            get_unsplash_settings,
            set_unsplash_access_key,
            clear_unsplash_access_key,
            search_unsplash_wallpapers,
            browse_palace_wallpapers,
            refresh_palace_staging_batch,
            get_palace_staging_batch,
            get_palace_staging_refresh_status,
            start_palace_staging_refresh,
            cancel_palace_staging_refresh,
            list_palace_staging_wallpapers,
            promote_palace_staging_wallpaper,
            promote_palace_staging_wallpapers,
            list_local_wallpapers,
            set_fixed_wallpaper,
            clear_fixed_wallpaper,
            delete_local_wallpaper,
            download_unsplash_wallpaper,
            download_palace_wallpaper,
            request_quit,
            log_app
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
            restore_before_exit(app);
        }
        tauri::RunEvent::Resumed => {
            if let Err(error) = app.state::<RestScheduler>().reset_after_wake() {
                let message = format!("唤醒后重置休息周期失败: {error}");
                append_app_log(app, &message);
                let _ = app.emit("rest-scheduler-error", message);
            }
            if let Ok(status) = platform::reapply_display_filter() {
                let _ = app.emit("display-filter-status", status);
            }
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::{
        advance_fallback_depth, choose_online_source, lock_windows_need_rebuild,
        should_retry_without_proxy, update_app_settings_with_backend, AppSettings,
        BatchInProgressGuard, HttpFetchError, MonitorSignature, SchedulerConfig,
        SettingsUpdateBackend, WallpaperRemoteSource, MAX_FALLBACK_CHAIN_DEPTH,
    };
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct FakeSettingsUpdateBackend {
        persisted: AppSettings,
        scheduler_config: SchedulerConfig,
        save_calls: usize,
        configure_calls: usize,
        save_fail_before: Vec<usize>,
        save_fail_after: Vec<usize>,
        configure_fail_before: Vec<usize>,
        calls: Vec<&'static str>,
    }

    impl FakeSettingsUpdateBackend {
        fn new(settings: AppSettings) -> Self {
            Self {
                scheduler_config: SchedulerConfig::from(&settings),
                persisted: settings,
                save_calls: 0,
                configure_calls: 0,
                save_fail_before: Vec::new(),
                save_fail_after: Vec::new(),
                configure_fail_before: Vec::new(),
                calls: Vec::new(),
            }
        }
    }

    impl SettingsUpdateBackend for FakeSettingsUpdateBackend {
        fn load_settings(&mut self) -> Result<AppSettings, String> {
            self.calls.push("load");
            Ok(self.persisted.clone())
        }

        fn save_settings(&mut self, settings: &AppSettings) -> Result<(), String> {
            self.calls.push("save");
            self.save_calls += 1;
            if self.save_fail_before.contains(&self.save_calls) {
                return Err(format!("save failure {}", self.save_calls));
            }
            self.persisted = settings.clone();
            if self.save_fail_after.contains(&self.save_calls) {
                return Err(format!("save failure {}", self.save_calls));
            }
            Ok(())
        }

        fn configure_scheduler(&mut self, config: SchedulerConfig) -> Result<(), String> {
            self.calls.push("configure");
            self.configure_calls += 1;
            if self.configure_fail_before.contains(&self.configure_calls) {
                return Err(format!("configure failure {}", self.configure_calls));
            }
            self.scheduler_config = config;
            Ok(())
        }

        fn query_scheduler_config(&mut self) -> Result<SchedulerConfig, String> {
            self.calls.push("query");
            Ok(self.scheduler_config)
        }
    }

    fn changed_app_settings() -> AppSettings {
        AppSettings {
            filter_strength: 55,
            work_interval_minutes: 45,
            rest_duration_seconds: 90,
            allow_esc: false,
            ..AppSettings::default()
        }
    }

    #[test]
    fn batch_guard_releases_the_flag_on_early_return() {
        fn fail_after_acquire(flag: &Arc<AtomicBool>) -> Result<(), &'static str> {
            let _guard = BatchInProgressGuard::try_acquire(flag).ok_or("busy")?;
            Err("initialization failed")
        }

        let flag = Arc::new(AtomicBool::new(false));
        assert_eq!(fail_after_acquire(&flag), Err("initialization failed"));
        assert!(!flag.load(Ordering::SeqCst));
        assert!(BatchInProgressGuard::try_acquire(&flag).is_some());
    }

    #[test]
    fn batch_guard_rejects_a_concurrent_owner() {
        let flag = Arc::new(AtomicBool::new(false));
        let first = BatchInProgressGuard::try_acquire(&flag).expect("first owner");
        assert!(BatchInProgressGuard::try_acquire(&flag).is_none());
        drop(first);
        assert!(BatchInProgressGuard::try_acquire(&flag).is_some());
    }

    #[test]
    fn fallback_depth_is_request_scoped_input() {
        assert_eq!(advance_fallback_depth(0), Some(1));
        assert_eq!(
            advance_fallback_depth(MAX_FALLBACK_CHAIN_DEPTH - 1),
            Some(MAX_FALLBACK_CHAIN_DEPTH)
        );
        assert_eq!(advance_fallback_depth(MAX_FALLBACK_CHAIN_DEPTH), None);
        assert_eq!(advance_fallback_depth(u32::MAX), None);
    }

    #[test]
    fn disabled_palace_source_never_becomes_the_automatic_fallback() {
        assert_eq!(
            choose_online_source(false, false),
            WallpaperRemoteSource::Unsplash
        );
        assert_eq!(
            choose_online_source(false, true),
            WallpaperRemoteSource::Palace
        );
        assert_eq!(
            choose_online_source(true, true),
            WallpaperRemoteSource::Unsplash
        );
    }

    #[test]
    fn business_http_status_does_not_trigger_transport_fallback() {
        let not_found =
            HttpFetchError::http_status(reqwest::StatusCode::NOT_FOUND, "deterministic response");
        let rate_limited = HttpFetchError::http_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "deterministic response",
        );
        assert!(!should_retry_without_proxy(&not_found));
        assert!(!should_retry_without_proxy(&rate_limited));
    }

    #[test]
    fn proxy_and_server_http_statuses_continue_the_fallback_chain() {
        for status in [
            reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            reqwest::StatusCode::GATEWAY_TIMEOUT,
        ] {
            let error = HttpFetchError::http_status(status, "retryable response");
            assert!(should_retry_without_proxy(&error), "status={status}");
        }
    }

    #[test]
    fn lock_windows_compare_topology_and_live_window_count() {
        let created: MonitorSignature = vec![(0, 0, 1920, 1080, 1.0_f64.to_bits())];
        let unchanged = created.clone();
        let changed = vec![
            (0, 0, 1920, 1080, 1.0_f64.to_bits()),
            (1920, 0, 2560, 1440, 2.0_f64.to_bits()),
        ];

        assert!(!lock_windows_need_rebuild(Some(&created), &unchanged, 1, 1));
        assert!(lock_windows_need_rebuild(Some(&created), &changed, 1, 1));
        assert!(lock_windows_need_rebuild(Some(&created), &unchanged, 1, 0));
        assert!(lock_windows_need_rebuild(Some(&created), &unchanged, 0, 0));
        assert!(!lock_windows_need_rebuild(None, &changed, 0, 0));
    }

    #[test]
    fn settings_update_persists_before_configuring_the_scheduler() {
        let previous = AppSettings::default();
        let next = changed_app_settings();
        let mut backend = FakeSettingsUpdateBackend::new(previous);

        update_app_settings_with_backend(&mut backend, &next).expect("update settings");

        assert_eq!(backend.persisted, next);
        assert_eq!(backend.scheduler_config, SchedulerConfig::from(&next));
        assert_eq!(backend.calls, ["load", "save", "configure"]);
    }

    #[test]
    fn settings_save_failure_after_replace_is_rolled_back_and_verified() {
        let previous = AppSettings::default();
        let next = changed_app_settings();
        let mut backend = FakeSettingsUpdateBackend::new(previous.clone());
        backend.save_fail_after = vec![1];

        let error =
            update_app_settings_with_backend(&mut backend, &next).expect_err("save must fail");

        assert_eq!(backend.persisted, previous);
        assert_eq!(
            backend.scheduler_config,
            SchedulerConfig::from(&AppSettings::default())
        );
        assert_eq!(backend.calls, ["load", "save", "save", "load"]);
        assert_eq!(
            error,
            "保存应用设置失败: save failure 1；已验证设置文件恢复到更新前状态"
        );
    }

    #[test]
    fn scheduler_failure_rolls_back_and_verifies_both_resources() {
        let previous = AppSettings::default();
        let next = changed_app_settings();
        let mut backend = FakeSettingsUpdateBackend::new(previous.clone());
        backend.configure_fail_before = vec![1];

        let error =
            update_app_settings_with_backend(&mut backend, &next).expect_err("configure must fail");

        assert_eq!(backend.persisted, previous.clone());
        assert_eq!(backend.scheduler_config, SchedulerConfig::from(&previous));
        assert_eq!(
            backend.calls,
            [
                "load",
                "save",
                "configure",
                "save",
                "load",
                "configure",
                "query"
            ]
        );
        assert_eq!(
            error,
            "更新休息调度失败: configure failure 1；已验证设置文件和休息调度配置恢复到更新前状态"
        );
    }

    #[test]
    fn rollback_failures_are_combined_without_masking_the_primary_error() {
        let previous = AppSettings::default();
        let next = changed_app_settings();
        let mut backend = FakeSettingsUpdateBackend::new(previous);
        backend.save_fail_before = vec![2];
        backend.configure_fail_before = vec![1, 2];

        let error =
            update_app_settings_with_backend(&mut backend, &next).expect_err("configure must fail");

        assert_eq!(backend.persisted, next);
        assert!(error.starts_with("更新休息调度失败: configure failure 1"));
        assert!(error.contains("设置文件和休息调度配置回滚未完全成功"));
        assert!(error.contains("恢复设置文件写入失败: save failure 2"));
        assert!(error.contains("恢复设置文件后读取验证不一致"));
        assert!(error.contains("恢复休息调度配置失败: configure failure 2"));
    }
}
