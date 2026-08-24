use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{path::BaseDirectory, AppHandle, Manager, Runtime};

pub const SETTINGS_FILE_NAME: &str = "app-settings.json";

const MIN_COLOR_TEMPERATURE: u32 = 1_000;
const MAX_COLOR_TEMPERATURE: u32 = 40_000;
const TEMP_FILE_ATTEMPTS: usize = 32;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static RECOVERY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// User preferences owned by the application.
///
/// Autostart intentionally is not represented here. Its source of truth is the
/// platform autostart service/plugin rather than this JSON file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub filter_enabled: bool,
    pub filter_strength: u8,
    #[serde(alias = "colorTemp")]
    pub color_temperature: u32,
    #[serde(alias = "activePreset")]
    pub filter_preset: String,
    pub rest_enabled: bool,
    #[serde(alias = "restMinutes")]
    pub work_interval_minutes: u32,
    #[serde(alias = "restDuration")]
    pub rest_duration_seconds: u32,
    #[serde(alias = "allowEscExit")]
    pub allow_esc: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            filter_enabled: true,
            filter_strength: 30,
            color_temperature: 4_700,
            filter_preset: "智能".to_string(),
            rest_enabled: true,
            work_interval_minutes: 30,
            rest_duration_seconds: 60,
            allow_esc: true,
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> Result<(), SettingsValidationError> {
        if !(MIN_COLOR_TEMPERATURE..=MAX_COLOR_TEMPERATURE).contains(&self.color_temperature) {
            return Err(SettingsValidationError::ColorTemperatureOutOfRange {
                value: self.color_temperature,
            });
        }
        if self.work_interval_minutes == 0 {
            return Err(SettingsValidationError::WorkIntervalIsZero);
        }
        if self.rest_duration_seconds == 0 {
            return Err(SettingsValidationError::RestDurationIsZero);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsValidationError {
    ColorTemperatureOutOfRange { value: u32 },
    WorkIntervalIsZero,
    RestDurationIsZero,
}

impl fmt::Display for SettingsValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ColorTemperatureOutOfRange { value } => write!(
                formatter,
                "色温必须在 {MIN_COLOR_TEMPERATURE} 到 {MAX_COLOR_TEMPERATURE} 之间，当前为 {value}"
            ),
            Self::WorkIntervalIsZero => formatter.write_str("工作间隔分钟数必须大于 0"),
            Self::RestDurationIsZero => formatter.write_str("休息时长秒数必须大于 0"),
        }
    }
}

impl Error for SettingsValidationError {}

#[derive(Debug)]
pub enum SettingsError {
    AppConfigPath(String),
    LockPoisoned,
    Invalid(SettingsValidationError),
    Json {
        operation: &'static str,
        path: PathBuf,
        source: serde_json::Error,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppConfigPath(message) => write!(formatter, "无法解析 AppConfig 目录: {message}"),
            Self::LockPoisoned => formatter.write_str("设置存储锁已损坏"),
            Self::Invalid(error) => write!(formatter, "设置无效: {error}"),
            Self::Json {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "{operation}设置 JSON 失败 ({}): {source}",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "{operation}设置文件失败 ({}): {source}",
                path.display()
            ),
        }
    }
}

impl Error for SettingsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid(error) => Some(error),
            Self::Json { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::AppConfigPath(_) | Self::LockPoisoned => None,
        }
    }
}

impl From<SettingsValidationError> for SettingsError {
    fn from(error: SettingsValidationError) -> Self {
        Self::Invalid(error)
    }
}

/// Serialized access to the settings file. Keeping this value in Tauri managed
/// state also prevents concurrent update calls from losing one another.
pub struct SettingsStore {
    path: PathBuf,
    io_lock: Mutex<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsLoadOutcome {
    pub settings: AppSettings,
    pub recovered_from: Option<PathBuf>,
    pub recovery_reason: Option<String>,
}

impl SettingsStore {
    pub fn from_app<R: Runtime>(app: &AppHandle<R>) -> Result<Self, SettingsError> {
        let directory = app
            .path()
            .resolve("", BaseDirectory::AppConfig)
            .map_err(|error| SettingsError::AppConfigPath(error.to_string()))?;
        Ok(Self::new(directory.join(SETTINGS_FILE_NAME)))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            io_lock: Mutex::new(()),
        }
    }

    /// A missing file means first launch and returns current defaults. Malformed
    /// or unreadable files are reported instead of silently overwriting them.
    pub fn load(&self) -> Result<AppSettings, SettingsError> {
        let _guard = self
            .io_lock
            .lock()
            .map_err(|_| SettingsError::LockPoisoned)?;
        self.load_unlocked()
    }

    /// Loads settings while preserving a malformed or semantically invalid
    /// file under a unique recovery name. Missing files still use defaults;
    /// filesystem and permission errors remain visible to the caller.
    pub fn load_or_recover(&self) -> Result<SettingsLoadOutcome, SettingsError> {
        let _guard = self
            .io_lock
            .lock()
            .map_err(|_| SettingsError::LockPoisoned)?;
        match self.load_unlocked() {
            Ok(settings) => Ok(SettingsLoadOutcome {
                settings,
                recovered_from: None,
                recovery_reason: None,
            }),
            Err(error @ (SettingsError::Json { .. } | SettingsError::Invalid(_))) => {
                let recovery_reason = error.to_string();
                let recovered_from = quarantine_invalid_file(&self.path)?;
                Ok(SettingsLoadOutcome {
                    settings: AppSettings::default(),
                    recovered_from: Some(recovered_from),
                    recovery_reason: Some(recovery_reason),
                })
            }
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, settings: &AppSettings) -> Result<(), SettingsError> {
        settings.validate()?;
        let _guard = self
            .io_lock
            .lock()
            .map_err(|_| SettingsError::LockPoisoned)?;
        self.save_unlocked(settings)
    }

    /// Loads, mutates, validates, and persists while holding one store lock.
    pub fn update(
        &self,
        update: impl FnOnce(&mut AppSettings),
    ) -> Result<AppSettings, SettingsError> {
        let _guard = self
            .io_lock
            .lock()
            .map_err(|_| SettingsError::LockPoisoned)?;
        let mut settings = self.load_unlocked()?;
        update(&mut settings);
        settings.validate()?;
        self.save_unlocked(&settings)?;
        Ok(settings)
    }

    fn load_unlocked(&self) -> Result<AppSettings, SettingsError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(AppSettings::default())
            }
            Err(source) => {
                return Err(SettingsError::Io {
                    operation: "读取",
                    path: self.path.clone(),
                    source,
                })
            }
        };

        let settings = serde_json::from_slice::<AppSettings>(&bytes).map_err(|source| {
            SettingsError::Json {
                operation: "解析",
                path: self.path.clone(),
                source,
            }
        })?;
        settings.validate()?;
        Ok(settings)
    }

    fn save_unlocked(&self, settings: &AppSettings) -> Result<(), SettingsError> {
        let mut bytes =
            serde_json::to_vec_pretty(settings).map_err(|source| SettingsError::Json {
                operation: "序列化",
                path: self.path.clone(),
                source,
            })?;
        bytes.push(b'\n');
        atomic_write(&self.path, &bytes)
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), SettingsError> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
        operation: "创建目录以保存",
        path: parent.to_path_buf(),
        source,
    })?;

    let (temporary_path, mut temporary_file) = create_temporary_file(path)?;
    let result = (|| {
        temporary_file
            .write_all(contents)
            .map_err(|source| SettingsError::Io {
                operation: "写入临时",
                path: temporary_path.clone(),
                source,
            })?;
        temporary_file
            .sync_all()
            .map_err(|source| SettingsError::Io {
                operation: "同步临时",
                path: temporary_path.clone(),
                source,
            })?;
        drop(temporary_file);

        replace_file(&temporary_path, path).map_err(|source| SettingsError::Io {
            operation: "原子替换",
            path: path.to_path_buf(),
            source,
        })?;
        sync_parent_directory(parent).map_err(|source| SettingsError::Io {
            operation: "同步目录",
            path: parent.to_path_buf(),
            source,
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn quarantine_invalid_file(path: &Path) -> Result<PathBuf, SettingsError> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(SETTINGS_FILE_NAME);

    for _ in 0..TEMP_FILE_ATTEMPTS {
        let sequence = RECOVERY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let recovery_path = parent.join(format!(
            "{file_name}.invalid.{}.{}",
            std::process::id(),
            sequence
        ));
        let reservation = match open_new_file(&recovery_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(SettingsError::Io {
                    operation: "创建坏设置恢复文件",
                    path: recovery_path,
                    source,
                })
            }
        };
        drop(reservation);

        if let Err(source) = replace_file(path, &recovery_path) {
            let _ = fs::remove_file(&recovery_path);
            return Err(SettingsError::Io {
                operation: "隔离坏设置",
                path: path.to_path_buf(),
                source,
            });
        }
        sync_parent_directory(parent).map_err(|source| SettingsError::Io {
            operation: "同步坏设置恢复目录",
            path: parent.to_path_buf(),
            source,
        })?;
        return Ok(recovery_path);
    }

    Err(SettingsError::Io {
        operation: "创建坏设置恢复文件",
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "无法分配唯一的坏设置恢复文件名",
        ),
    })
}

fn create_temporary_file(destination: &Path) -> Result<(PathBuf, File), SettingsError> {
    let parent = destination
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let destination_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("settings");

    for _ in 0..TEMP_FILE_ATTEMPTS {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{destination_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match open_new_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(SettingsError::Io {
                    operation: "创建临时",
                    path,
                    source,
                })
            }
        }
    }

    Err(SettingsError::Io {
        operation: "创建临时",
        path: destination.to_path_buf(),
        source: io::Error::new(io::ErrorKind::AlreadyExists, "无法分配唯一的临时设置文件名"),
    })
}

fn open_new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows 路径包含 NUL 字符",
            ));
        }
        value.push(0);
        Ok(value)
    }

    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "huyanba-settings-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn partial_json_uses_defaults_and_accepts_frontend_aliases() {
        let settings: AppSettings = serde_json::from_value(json!({
            "restEnabled": false,
            "restMinutes": 45,
            "restDuration": 90,
            "colorTemp": 5_200,
            "activePreset": "办公",
            "allowEscExit": false,
            "autostart": true
        }))
        .expect("deserialize partial settings");

        assert!(!settings.rest_enabled);
        assert_eq!(settings.work_interval_minutes, 45);
        assert_eq!(settings.rest_duration_seconds, 90);
        assert_eq!(settings.color_temperature, 5_200);
        assert_eq!(settings.filter_preset, "办公");
        assert!(!settings.allow_esc);
        assert!(settings.filter_enabled);
        assert_eq!(settings.filter_strength, 30);

        let serialized = serde_json::to_value(settings).expect("serialize settings");
        assert!(serialized.get("autostart").is_none());
        assert!(serialized.get("autoStart").is_none());
        assert_eq!(serialized["workIntervalMinutes"], 45);
        assert_eq!(serialized["allowEsc"], false);
    }

    #[test]
    fn missing_file_returns_defaults_and_save_round_trips() {
        let directory = TestDirectory::new();
        let path = directory.0.join("nested").join(SETTINGS_FILE_NAME);
        let store = SettingsStore::new(&path);
        assert_eq!(store.load().expect("load defaults"), AppSettings::default());

        let expected = AppSettings {
            filter_enabled: false,
            work_interval_minutes: 50,
            ..AppSettings::default()
        };
        store.save(&expected).expect("save settings");
        assert_eq!(store.load().expect("reload settings"), expected);

        let entries: Vec<_> = fs::read_dir(path.parent().expect("settings parent"))
            .expect("read settings directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect();
        assert_eq!(entries, vec![SETTINGS_FILE_NAME]);
    }

    #[test]
    fn update_is_a_locked_read_modify_write() {
        let directory = TestDirectory::new();
        let store = SettingsStore::new(directory.0.join(SETTINGS_FILE_NAME));
        let updated = store
            .update(|settings| {
                settings.filter_strength = 55;
                settings.allow_esc = false;
            })
            .expect("update settings");

        assert_eq!(updated.filter_strength, 55);
        assert!(!updated.allow_esc);
        assert_eq!(store.load().expect("load updated settings"), updated);
    }

    #[test]
    fn concurrent_updates_do_not_lose_independent_changes() {
        let directory = TestDirectory::new();
        let store = Arc::new(SettingsStore::new(directory.0.join(SETTINGS_FILE_NAME)));
        store
            .save(&AppSettings::default())
            .expect("save initial settings");
        let barrier = Arc::new(Barrier::new(3));

        let first_store = Arc::clone(&store);
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_store
                .update(|settings| settings.filter_strength = 65)
                .expect("update filter strength");
        });
        let second_store = Arc::clone(&store);
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_store
                .update(|settings| settings.allow_esc = false)
                .expect("update allowEsc");
        });

        barrier.wait();
        first.join().expect("join first updater");
        second.join().expect("join second updater");
        let settings = store.load().expect("load concurrently updated settings");
        assert_eq!(settings.filter_strength, 65);
        assert!(!settings.allow_esc);
    }

    #[test]
    fn malformed_json_is_reported_without_replacing_the_file() {
        let directory = TestDirectory::new();
        let path = directory.0.join(SETTINGS_FILE_NAME);
        fs::write(&path, b"{not json").expect("write malformed settings");
        let store = SettingsStore::new(&path);

        assert!(matches!(store.load(), Err(SettingsError::Json { .. })));
        assert_eq!(fs::read(&path).expect("read original file"), b"{not json");
    }

    #[test]
    fn malformed_json_can_be_quarantined_and_replaced_with_defaults_in_memory() {
        let directory = TestDirectory::new();
        let path = directory.0.join(SETTINGS_FILE_NAME);
        fs::write(&path, b"{not json").expect("write malformed settings");
        let store = SettingsStore::new(&path);

        let outcome = store.load_or_recover().expect("recover malformed settings");
        assert_eq!(outcome.settings, AppSettings::default());
        assert!(outcome
            .recovery_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("解析设置 JSON 失败")));
        let recovered_from = outcome.recovered_from.expect("recovery path");
        assert!(!path.exists());
        assert_eq!(
            fs::read(&recovered_from).expect("read recovered file"),
            b"{not json"
        );
        assert_eq!(
            store.load().expect("load defaults after recovery"),
            AppSettings::default()
        );
    }

    #[test]
    fn invalid_settings_values_are_quarantined_too() {
        let directory = TestDirectory::new();
        let path = directory.0.join(SETTINGS_FILE_NAME);
        fs::write(
            &path,
            br#"{"workIntervalMinutes":0,"restDurationSeconds":60}"#,
        )
        .expect("write invalid settings");
        let store = SettingsStore::new(&path);

        let outcome = store.load_or_recover().expect("recover invalid settings");
        assert_eq!(outcome.settings, AppSettings::default());
        assert!(outcome
            .recovery_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("工作间隔分钟数必须大于 0")));
        let recovered_from = outcome.recovered_from.expect("recovery path");
        assert!(recovered_from.exists());
        assert!(!path.exists());
    }

    #[test]
    fn invalid_scheduler_durations_are_rejected() {
        let settings = AppSettings {
            work_interval_minutes: 0,
            ..AppSettings::default()
        };
        assert_eq!(
            settings.validate(),
            Err(SettingsValidationError::WorkIntervalIsZero)
        );
    }
}
