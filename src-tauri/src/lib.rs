// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use rand::Rng;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, REFERER, USER_AGENT,
};
use tauri::{
    menu::MenuBuilder,
    path::BaseDirectory,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
use windows::Win32::UI::ColorSystem::SetDeviceGammaRamp;
use std::time::Instant;

#[derive(Default)]
struct LockState {
    labels: Mutex<Vec<String>>,
    last_update: Mutex<Option<LockUpdate>>,
}

#[derive(Default)]
struct AppState {
    allow_exit: AtomicBool,
    wallpaper_lock: Mutex<()>,
}

const TRAY_ICON: tauri::image::Image<'static> = tauri::include_image!("icons/32x32.png");
const WALLPAPER_CACHE_LIMIT: usize = 30;
const WALLPAPER_BATCH_SIZE: usize = 10;
const WALLPAPER_BATCH_INTERVAL_SECS: i64 = 7 * 24 * 60 * 60;
const WALLPAPER_MIN_INTERVAL_SECS: i64 = 1;
const WALLPAPER_MIN_WIDTH: u32 = 1920;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WallpaperFile {
    path: String,
    added_at: i64,
    source_url: String,
    #[serde(default)]
    last_shown_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WallpaperState {
    files: Vec<WallpaperFile>,
    next_source_index: usize,
    next_show_index: usize,
    last_download_at: i64,
    last_batch_at: i64,
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

#[tauri::command]
fn set_gamma(filter_enabled: bool, strength: f64, color_temp: f64) -> Result<(), String> {
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

#[tauri::command]
fn reset_gamma() -> Result<(), String> {
    apply_gamma(1.0, 1.0, 1.0)
}

#[tauri::command]
async fn show_lock_windows(
    app: tauri::AppHandle,
    state: tauri::State<'_, LockState>,
    end_at_ms: i64,
    paused: bool,
    paused_remaining: i64,
    allow_esc: bool,
) -> Result<(), String> {
    let start = Instant::now();
    let mut labels = state.labels.lock().map_err(|_| "锁状态被占用")?;
    if !labels.is_empty() {
        for label in labels.iter() {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.set_always_on_top(true);
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        return Ok(());
    }

    let monitors = app
        .available_monitors()
        .map_err(|err| err.to_string())?;
    append_app_log(&app, &format!("锁屏创建开始 monitors={}", monitors.len()));
    for (index, monitor) in monitors.into_iter().enumerate() {
        let label = format!("lockscreen-{}", index);
        let position = monitor.position();
        let size = monitor.size();
        let scale = monitor.scale_factor();
        let width = (size.width as f64 / scale).ceil() + 400.0;
        let height = (size.height as f64 / scale).ceil() + 400.0;
        let x = (position.x as f64 / scale).floor() - 200.0;
        let y = (position.y as f64 / scale).floor() - 200.0;

        let url = format!(
            "index.html?lockscreen=1&end={}&paused={}&remaining={}&allowEsc={}",
            end_at_ms,
            if paused { 1 } else { 0 },
            paused_remaining,
            if allow_esc { 1 } else { 0 }
        );
        let window = WebviewWindowBuilder::new(&app, label.clone(), WebviewUrl::App(url.into()))
        .decorations(false)
        .transparent(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .position(x, y)
        .inner_size(width, height)
        .build()
        .map_err(|err| err.to_string())?;

        let _ = window.set_fullscreen(true);
        let _ = window.set_focus();
        labels.push(label);
    }

    append_app_log(
        &app,
        &format!("锁屏创建完成 labels={} elapsed_ms={}", labels.len(), start.elapsed().as_millis()),
    );
    Ok(())
}

#[tauri::command]
fn hide_lock_windows(
    app: tauri::AppHandle,
    state: tauri::State<'_, LockState>,
) -> Result<(), String> {
    let start = Instant::now();
    let mut labels = state.labels.lock().map_err(|_| "锁状态被占用")?;
    append_app_log(&app, &format!("锁屏关闭开始 labels={}", labels.len()));
    for label in labels.iter() {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.close();
        }
    }
    labels.clear();
    append_app_log(&app, &format!("锁屏关闭完成 elapsed_ms={}", start.elapsed().as_millis()));
    Ok(())
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
fn lockscreen_action(app: tauri::AppHandle, action: String) -> Result<(), String> {
    append_app_log(&app, &format!("锁屏动作: {}", action));
    for (_label, window) in app.webview_windows() {
        let _ = window.emit("lockscreen-action", action.clone());
    }
    Ok(())
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64
}

fn hash_url(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:x}", hasher.finish())
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

fn ensure_wallpaper_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .resolve("wallpapers", BaseDirectory::AppCache)
        .map_err(|err| err.to_string())?;
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir)
}

fn append_app_log(app: &AppHandle, message: &str) {
    let dir = match ensure_wallpaper_dir(app) {
        Ok(dir) => dir,
        Err(_) => return,
    };
    let path = dir.join("app.log");
    let ts = now_ts();
    let line = format!("[{}] {}\n", ts, message);
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn append_wallpaper_log(app: &AppHandle, message: &str) {
    let Ok(dir) = ensure_wallpaper_dir(app) else {
        return;
    };
    let log_path = dir.join("prefetch.log");
    let ts = now_ts();
    let line = format!("[{}] {}\n", ts, message);
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

#[tauri::command]
fn log_app(app: AppHandle, message: String) -> Result<(), String> {
    append_app_log(&app, &message);
    Ok(())
}

fn prune_missing_files(state: &mut WallpaperState) {
    state.files.retain(|entry| Path::new(&entry.path).exists());
}

fn extract_jpg_urls(html: &str) -> Vec<String> {
    let mut urls = HashSet::new();
    let re = regex::Regex::new(r#"https?://[^"'\s>]+\.jpg[^"'\s>]*"#)
        .unwrap_or_else(|_| regex::Regex::new(r"\.jpg").unwrap());
    for cap in re.captures_iter(html) {
        if let Some(m) = cap.get(0) {
            urls.insert(m.as_str().to_string());
        }
    }
    urls.into_iter().collect()
}

fn extract_light_ids(html: &str) -> Vec<String> {
    let mut ids = HashSet::new();
    let re = regex::Regex::new(r#"[\\/]light[\\/]([0-9]+)\.html"#).unwrap();
    for cap in re.captures_iter(html) {
        if let Some(m) = cap.get(1) {
            ids.insert(m.as_str().to_string());
        }
    }
    let key_re = regex::Regex::new(r#"data-key\s*=\s*["']([0-9,]+)["']"#).unwrap();
    for cap in key_re.captures_iter(html) {
        if let Some(m) = cap.get(1) {
            for id in m.as_str().split(',') {
                if !id.is_empty() {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids.into_iter().collect()
}

fn extract_upload_urls(html: &str) -> Vec<String> {
    let mut urls = HashSet::new();
    let re = regex::Regex::new(
        r#"(?:src=|data-src=)\s*["']?(/Uploads/image/[^"'\s>]+\.jpg[^"'\s>]*)"#,
    )
    .unwrap();
    for cap in re.captures_iter(html) {
        if let Some(m) = cap.get(1) {
            let url = format!("https://www.dpm.org.cn{}", m.as_str());
            urls.insert(url);
        }
    }
    urls.into_iter().collect()
}

fn try_pick_wallpaper_from_page(
    html: &str,
    client: &reqwest::blocking::Client,
) -> Result<(String, Vec<u8>), String> {
    let urls = extract_jpg_urls(html);
    for url in urls {
        let response = client.get(&url).send().map_err(|err| err.to_string())?;
        if !response.status().is_success() {
            continue;
        }
        let bytes = response.bytes().map_err(|err| err.to_string())?;
        let reader = image::io::Reader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|err| err.to_string())?;
        let dims = reader
            .into_dimensions()
            .map_err(|err| err.to_string())?;
        if dims.0 >= WALLPAPER_MIN_WIDTH && dims.0 >= dims.1 {
            return Ok((url, bytes.to_vec()));
        }
    }
    Err("未找到符合分辨率的图片".into())
}

fn try_prefetch_wallpaper(
    app: &AppHandle,
    wall_state: &mut WallpaperState,
    dir: &Path,
    allow_download: bool,
    target_count: usize,
) {
    if !allow_download {
        append_wallpaper_log(app, "预取跳过: allow_download=false");
        return;
    }
    let now = now_ts();
    if now.saturating_sub(wall_state.last_download_at) <= WALLPAPER_MIN_INTERVAL_SECS {
        append_wallpaper_log(app, "预取跳过: 与上次下载间隔过短");
        return;
    }
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("zh-CN,zh;q=0.9"));
    headers.insert(
        REFERER,
        HeaderValue::from_static("https://www.dpm.org.cn/lights/royal.html"),
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36",
        ),
    );
    headers.insert(
        "x-requested-with",
        HeaderValue::from_static("XMLHttpRequest"),
    );

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .default_headers(headers)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            append_wallpaper_log(app, &format!("预取失败: client创建失败 {}", err));
            return;
        }
    };
    let mut attempts = 0;
    let mut rng = rand::thread_rng();
    let mut added = 0usize;
    let max_attempts = std::cmp::max(target_count * 6, 12);
    while attempts < max_attempts && added < target_count {
        attempts += 1;
        let list_url = format!(
            "https://www.dpm.org.cn/searchs/royalb.html?{}&category_id=624&p=1&pagesize=24&is_pc=0&is_wap=0&is_calendar=0&is_four_k=0",
            rng.gen::<f64>()
        );
        append_wallpaper_log(app, &format!("拉取列表: {}", list_url));
        let response = match client.get(list_url).send() {
            Ok(res) => res,
            Err(err) => {
                append_wallpaper_log(app, &format!("列表拉取失败: {}", err));
                continue;
            }
        };
        if !response.status().is_success() {
            append_wallpaper_log(app, &format!("列表状态非200: {}", response.status()));
            continue;
        }
        let final_url = response.url().to_string();
        let list_html = match response.text() {
            Ok(text) => text,
            Err(err) => {
                append_wallpaper_log(app, &format!("读取列表HTML失败: {}", err));
                continue;
            }
        };
        let ids = extract_light_ids(&list_html);
        let uploads = extract_upload_urls(&list_html);
        append_wallpaper_log(
            app,
            &format!(
                "列表解析: ids={} uploads={} final={}",
                ids.len(),
                uploads.len(),
                final_url
            ),
        );
        append_wallpaper_log(app, &format!("列表HTML长度: {}", list_html.len()));

        if !ids.is_empty() {
            let id = ids[rng.gen_range(0..ids.len())].clone();
            let detail_url = format!("https://www.dpm.org.cn/light/{}.html", id);
            append_wallpaper_log(app, &format!("抓取详情: {}", detail_url));
            if let Ok(res) = client.get(&detail_url).send() {
                if res.status().is_success() {
                    if let Ok(detail_html) = res.text() {
                        if let Ok((source_url, bytes)) = try_pick_wallpaper_from_page(&detail_html, &client) {
                            if wall_state.files.iter().any(|entry| entry.source_url == source_url) {
                                append_wallpaper_log(app, &format!("重复图片跳过: {}", source_url));
                            } else {
                                let file_name = format!("wallpaper_{}_{}.jpg", hash_url(&source_url), now);
                                let file_path = dir.join(file_name);
                                if fs::write(&file_path, &bytes).is_ok() {
                                    wall_state.files.push(WallpaperFile {
                                        path: file_path.to_string_lossy().to_string(),
                                        added_at: now,
                                        source_url,
                                        last_shown_at: 0,
                                    });
                                    wall_state.last_download_at = now;
                                    added += 1;
                                    append_wallpaper_log(app, &format!("下载成功: {}", file_path.display()));
                                    continue;
                                } else {
                                    append_wallpaper_log(app, "保存图片失败");
                                }
                            }
                        } else {
                            append_wallpaper_log(app, "详情页未找到符合分辨率的图片");
                        }
                    }
                } else {
                    append_wallpaper_log(app, &format!("详情页状态非200: {}", res.status()));
                }
            } else {
                append_wallpaper_log(app, "详情页抓取失败");
            }
        }

        if !wall_state.files.is_empty() {
            if added >= target_count {
                break;
            }
        }

        for url in uploads {
            let response = match client.get(&url).send() {
                Ok(res) => res,
                Err(_) => continue,
            };
            if !response.status().is_success() {
                continue;
            }
            let bytes = match response.bytes() {
                Ok(data) => data,
                Err(_) => continue,
            };
            let reader = match image::io::Reader::new(Cursor::new(&bytes))
                .with_guessed_format()
            {
                Ok(reader) => reader,
                Err(_) => continue,
            };
            let dims = match reader.into_dimensions() {
                Ok(dims) => dims,
                Err(_) => continue,
            };
            if dims.0 < WALLPAPER_MIN_WIDTH || dims.0 < dims.1 {
                continue;
            }
            if wall_state.files.iter().any(|entry| entry.source_url == url) {
                continue;
            }
            let file_name = format!("wallpaper_{}_{}.jpg", hash_url(&url), now);
            let file_path = dir.join(file_name);
            if fs::write(&file_path, &bytes).is_ok() {
                wall_state.files.push(WallpaperFile {
                    path: file_path.to_string_lossy().to_string(),
                    added_at: now,
                    source_url: url,
                    last_shown_at: 0,
                });
                wall_state.last_download_at = now;
                added += 1;
                append_wallpaper_log(app, &format!("列表缩略图下载成功: {}", file_path.display()));
                if added >= target_count {
                    break;
                }
            }
        }
    }
}

fn enforce_wallpaper_limit(wall_state: &mut WallpaperState) {
    if wall_state.files.len() <= WALLPAPER_CACHE_LIMIT {
        return;
    }
    wall_state.files.sort_by_key(|entry| entry.added_at);
    while wall_state.files.len() > WALLPAPER_CACHE_LIMIT {
        if let Some(oldest) = wall_state.files.first() {
            let _ = fs::remove_file(&oldest.path);
        }
        wall_state.files.remove(0);
    }
}

fn should_run_weekly_batch(wall_state: &WallpaperState) -> bool {
    let now = now_ts();
    if wall_state.files.is_empty() {
        return true;
    }
    now.saturating_sub(wall_state.last_batch_at) >= WALLPAPER_BATCH_INTERVAL_SECS
}

fn run_weekly_batch(app: AppHandle) {
    let state = app.state::<AppState>();
    let _guard = match state.wallpaper_lock.lock() {
        Ok(guard) => guard,
        Err(_) => {
            append_wallpaper_log(&app, "预取失败: 壁纸锁被占用");
            return;
        }
    };
    let dir = match ensure_wallpaper_dir(&app) {
        Ok(dir) => dir,
        Err(err) => {
            append_wallpaper_log(&app, &format!("预取失败: {}", err));
            return;
        }
    };
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    prune_missing_files(&mut wall_state);
    if !should_run_weekly_batch(&wall_state) {
        append_wallpaper_log(&app, "预取跳过: 未到每周下载时间");
        return;
    }
    append_wallpaper_log(&app, "预取触发: 每周批量下载");
    try_prefetch_wallpaper(&app, &mut wall_state, &dir, true, WALLPAPER_BATCH_SIZE);
    wall_state.last_batch_at = now_ts();
    enforce_wallpaper_limit(&mut wall_state);
    if save_wallpaper_state(&state_path, &wall_state).is_ok() {
        append_wallpaper_log(&app, "预取完成");
    }
}

#[tauri::command]
fn prefetch_lock_wallpaper(app: AppHandle) -> Result<(), String> {
    // 异步预取，避免阻塞 UI/锁屏退出
    std::thread::spawn(move || run_weekly_batch(app));
    Ok(())
}

#[tauri::command]
fn get_lock_wallpaper(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<Option<String>, String> {
    let _guard = state
        .wallpaper_lock
        .lock()
        .map_err(|_| "壁纸锁被占用")?;
    let dir = ensure_wallpaper_dir(&app)?;
    let state_path = dir.join("index.json");
    let mut wall_state = load_wallpaper_state(&state_path);
    prune_missing_files(&mut wall_state);
    enforce_wallpaper_limit(&mut wall_state);

    if wall_state.files.is_empty() {
        save_wallpaper_state(&state_path, &wall_state)?;
        append_wallpaper_log(&app, "锁屏读取: 无缓存壁纸");
        return Ok(None);
    }

    // 优先展示未出现过的最新壁纸，避免重复
    let mut unshown: Vec<(usize, i64)> = wall_state
        .files
        .iter()
        .enumerate()
        .filter(|(_, item)| item.last_shown_at == 0)
        .map(|(idx, item)| (idx, item.added_at))
        .collect();
    let show_index = if !unshown.is_empty() {
        unshown.sort_by_key(|(_, added_at)| *added_at);
        unshown.last().map(|(idx, _)| *idx).unwrap_or(0)
    } else {
        // 全部显示过后，展示最近新增但最久未显示的
        let mut candidates: Vec<(usize, i64, i64)> = wall_state
            .files
            .iter()
            .enumerate()
            .map(|(idx, item)| (idx, item.added_at, item.last_shown_at))
            .collect();
        candidates.sort_by_key(|(_, added_at, last_shown)| (*last_shown, std::cmp::Reverse(*added_at)));
        candidates.first().map(|(idx, _, _)| *idx).unwrap_or(0)
    };
    let chosen = wall_state.files[show_index].path.clone();
    wall_state.files[show_index].last_shown_at = now_ts();
    save_wallpaper_state(&state_path, &wall_state)?;
    append_wallpaper_log(&app, &format!("锁屏读取: {}", chosen));
    Ok(Some(chosen))
}


#[tauri::command]
fn request_quit(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.allow_exit.store(true, Ordering::SeqCst);
    let _ = apply_gamma(1.0, 1.0, 1.0);
    let _ = app.exit(0);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 启动时强制写入壁纸日志，确认目录
            append_wallpaper_log(app.handle(), "应用启动，日志初始化");
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.center();
                let _ = window.show();
                let _ = window.set_focus();
            }
            let tray_menu = MenuBuilder::new(app)
                .text("tray_show", "显示主界面")
                .text("tray_hide", "隐藏到托盘")
                .separator()
                .text("tray_quit", "退出")
                .build()?;

            let tray = TrayIconBuilder::new()
                .icon(TRAY_ICON.clone())
                .tooltip("护眼吧")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
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
                            if button == MouseButton::Left
                                && button_state == MouseButtonState::Up
                            {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        TrayIconEvent::DoubleClick { button, .. } => {
                            if button == MouseButton::Left {
                                let visible = window.is_visible().unwrap_or(true);
                                if visible {
                                    let _ = window.hide();
                                } else {
                                    let _ = window.show();
                                    let _ = window.set_focus();
                                }
                            }
                        }
                        _ => {}
                    }
                })
                .on_menu_event(|app, event| {
                    let Some(window) = app.get_webview_window("main") else {
                        return;
                    };
                    match event.id().as_ref() {
                        "tray_show" => {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                        "tray_hide" => {
                            let _ = window.hide();
                        }
                        "tray_quit" => {
                            if let Some(state) = app.try_state::<AppState>() {
                                state.allow_exit.store(true, Ordering::SeqCst);
                            }
                            let _ = apply_gamma(1.0, 1.0, 1.0);
                            app.exit(0);
                        }
                        _ => {}
                    }
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
                    let _ = apply_gamma(1.0, 1.0, 1.0);
                }
                WindowEvent::Destroyed => {
                    let _ = apply_gamma(1.0, 1.0, 1.0);
                }
                _ => {}
            }
        })
        .manage(LockState::default())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            greet,
            set_gamma,
            reset_gamma,
            show_lock_windows,
            hide_lock_windows,
            broadcast_lock_update,
            get_lock_update,
            lockscreen_action,
            get_lock_wallpaper,
            prefetch_lock_wallpaper,
            request_quit,
            log_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
