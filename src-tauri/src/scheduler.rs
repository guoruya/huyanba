use crate::settings::AppSettings;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MILLIS_PER_SECOND: u64 = 1_000;
const MILLIS_PER_MINUTE: u64 = 60 * MILLIS_PER_SECOND;
const MAX_CLOCK_RECHECK: Duration = Duration::from_secs(1);
const SUSPEND_GAP_THRESHOLD_MS: u64 = 3 * MILLIS_PER_SECOND;
const WAKE_RESET_DEDUP_WINDOW_MS: u64 = 2 * MILLIS_PER_SECOND;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct SchedulerConfig {
    pub rest_enabled: bool,
    pub work_interval_minutes: u32,
    pub rest_duration_seconds: u32,
    pub allow_esc: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self::from(&AppSettings::default())
    }
}

impl From<&AppSettings> for SchedulerConfig {
    fn from(settings: &AppSettings) -> Self {
        Self {
            rest_enabled: settings.rest_enabled,
            work_interval_minutes: settings.work_interval_minutes,
            rest_duration_seconds: settings.rest_duration_seconds,
            allow_esc: settings.allow_esc,
        }
    }
}

impl SchedulerConfig {
    fn validate(self) -> Result<Self, SchedulerError> {
        if self.work_interval_minutes == 0 {
            return Err(SchedulerError::InvalidConfig(
                "工作间隔分钟数必须大于 0".to_string(),
            ));
        }
        if self.rest_duration_seconds == 0 {
            return Err(SchedulerError::InvalidConfig(
                "休息时长秒数必须大于 0".to_string(),
            ));
        }
        Ok(self)
    }

    fn work_interval_ms(self) -> u64 {
        u64::from(self.work_interval_minutes).saturating_mul(MILLIS_PER_MINUTE)
    }

    fn rest_duration_ms(self) -> u64 {
        u64::from(self.rest_duration_seconds).saturating_mul(MILLIS_PER_SECOND)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SchedulerPhase {
    Disabled,
    Working,
    Resting,
    Sleeping,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerState {
    #[serde(flatten)]
    pub config: SchedulerConfig,
    pub phase: SchedulerPhase,
    pub next_rest_at_ms: Option<u64>,
    pub rest_end_at_ms: Option<u64>,
    pub paused: bool,
    pub paused_remaining_ms: Option<u64>,
    /// The wall-clock instant at which this snapshot was produced. Consumers
    /// can render countdowns from the absolute deadlines without owning timing.
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RestFinishedReason {
    Elapsed,
    User,
    CycleRestarted,
    Sleep,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StateChangeReason {
    Configured,
    Paused,
    Resumed,
    CycleRestarted,
    Sleep,
    Wake,
}

/// Events are deliberately platform-neutral. `lib.rs` can receive them on the
/// returned channel, invoke the existing lock-window functions, and emit the
/// same value to webviews without this module depending on private commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum SchedulerEvent {
    RestStarted {
        state: SchedulerState,
    },
    RestFinished {
        reason: RestFinishedReason,
        state: SchedulerState,
    },
    StateChanged {
        reason: StateChangeReason,
        state: SchedulerState,
    },
}

impl SchedulerEvent {
    pub fn state(&self) -> &SchedulerState {
        match self {
            Self::RestStarted { state }
            | Self::RestFinished { state, .. }
            | Self::StateChanged { state, .. } => state,
        }
    }
}

#[derive(Debug)]
pub enum SchedulerError {
    InvalidConfig(String),
    LockPoisoned,
    Shutdown,
    Sleeping,
    ThreadSpawn(io::Error),
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(formatter, "休息调度配置无效: {message}"),
            Self::LockPoisoned => formatter.write_str("休息调度状态锁已损坏"),
            Self::Shutdown => formatter.write_str("休息调度器已停止"),
            Self::Sleeping => formatter.write_str("系统睡眠期间不能立即开始休息"),
            Self::ThreadSpawn(error) => write!(formatter, "无法启动休息调度线程: {error}"),
        }
    }
}

impl Error for SchedulerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ThreadSpawn(error) => Some(error),
            _ => None,
        }
    }
}

struct SchedulerCore {
    config: SchedulerConfig,
    phase: SchedulerPhase,
    next_rest_at_ms: Option<u64>,
    rest_end_at_ms: Option<u64>,
    paused_remaining_ms: Option<u64>,
    last_observed_at_ms: u64,
    last_wake_reset_at_ms: Option<u64>,
    shutdown: bool,
}

impl SchedulerCore {
    fn new(config: SchedulerConfig, now_ms: u64) -> Self {
        let mut core = Self {
            config,
            phase: SchedulerPhase::Disabled,
            next_rest_at_ms: None,
            rest_end_at_ms: None,
            paused_remaining_ms: None,
            last_observed_at_ms: now_ms,
            last_wake_reset_at_ms: None,
            shutdown: false,
        };
        core.begin_new_work_cycle(now_ms);
        core
    }

    fn snapshot(&self, observed_at_ms: u64) -> SchedulerState {
        SchedulerState {
            config: self.config,
            phase: self.phase,
            next_rest_at_ms: self.next_rest_at_ms,
            rest_end_at_ms: self.rest_end_at_ms,
            paused: self.paused_remaining_ms.is_some(),
            paused_remaining_ms: self.paused_remaining_ms,
            observed_at_ms,
        }
    }

    fn next_deadline_ms(&self) -> Option<u64> {
        match self.phase {
            SchedulerPhase::Working => self.next_rest_at_ms,
            SchedulerPhase::Resting if self.paused_remaining_ms.is_none() => self.rest_end_at_ms,
            SchedulerPhase::Disabled | SchedulerPhase::Resting | SchedulerPhase::Sleeping => None,
        }
    }

    fn advance(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        match self.phase {
            SchedulerPhase::Working
                if self
                    .next_rest_at_ms
                    .is_some_and(|deadline| now_ms >= deadline) =>
            {
                self.begin_rest(now_ms);
                vec![SchedulerEvent::RestStarted {
                    state: self.snapshot(now_ms),
                }]
            }
            SchedulerPhase::Resting
                if self.paused_remaining_ms.is_none()
                    && self
                        .rest_end_at_ms
                        .is_some_and(|deadline| now_ms >= deadline) =>
            {
                self.begin_new_work_cycle(now_ms);
                vec![SchedulerEvent::RestFinished {
                    reason: RestFinishedReason::Elapsed,
                    state: self.snapshot(now_ms),
                }]
            }
            _ => Vec::new(),
        }
    }

    fn observe_time(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        let clock_moved_backwards = now_ms < self.last_observed_at_ms;
        let tick_gap_ms = now_ms.saturating_sub(self.last_observed_at_ms);
        self.last_observed_at_ms = now_ms;
        if clock_moved_backwards || tick_gap_ms > SUSPEND_GAP_THRESHOLD_MS {
            self.reset_after_tick_gap(now_ms)
        } else {
            self.advance(now_ms)
        }
    }

    fn configure(&mut self, config: SchedulerConfig, now_ms: u64) -> Vec<SchedulerEvent> {
        if self.config == config {
            return Vec::new();
        }

        let previous = self.config;
        self.config = config;
        match self.phase {
            SchedulerPhase::Working => {
                if !config.rest_enabled {
                    self.phase = SchedulerPhase::Disabled;
                    self.clear_deadlines();
                } else if !previous.rest_enabled
                    || previous.work_interval_minutes != config.work_interval_minutes
                {
                    self.begin_new_work_cycle(now_ms);
                }
            }
            SchedulerPhase::Disabled => {
                if config.rest_enabled {
                    self.begin_new_work_cycle(now_ms);
                }
            }
            SchedulerPhase::Resting => {
                if previous.rest_duration_seconds != config.rest_duration_seconds {
                    if self.paused_remaining_ms.is_some() {
                        self.paused_remaining_ms = Some(config.rest_duration_ms());
                        self.rest_end_at_ms = None;
                    } else {
                        self.rest_end_at_ms =
                            Some(now_ms.saturating_add(config.rest_duration_ms()));
                    }
                }
            }
            SchedulerPhase::Sleeping => {}
        }

        vec![SchedulerEvent::StateChanged {
            reason: StateChangeReason::Configured,
            state: self.snapshot(now_ms),
        }]
    }

    fn pause(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        if self.phase != SchedulerPhase::Resting || self.paused_remaining_ms.is_some() {
            return Vec::new();
        }
        let Some(deadline) = self.rest_end_at_ms else {
            return Vec::new();
        };

        self.paused_remaining_ms = Some(deadline.saturating_sub(now_ms));
        self.rest_end_at_ms = None;
        vec![SchedulerEvent::StateChanged {
            reason: StateChangeReason::Paused,
            state: self.snapshot(now_ms),
        }]
    }

    fn resume(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        if self.phase != SchedulerPhase::Resting {
            return Vec::new();
        }
        let Some(remaining_ms) = self.paused_remaining_ms.take() else {
            return Vec::new();
        };

        self.rest_end_at_ms = Some(now_ms.saturating_add(remaining_ms));
        vec![SchedulerEvent::StateChanged {
            reason: StateChangeReason::Resumed,
            state: self.snapshot(now_ms),
        }]
    }

    fn finish(&mut self, now_ms: u64, reason: RestFinishedReason) -> Vec<SchedulerEvent> {
        if self.phase != SchedulerPhase::Resting {
            return Vec::new();
        }
        self.begin_new_work_cycle(now_ms);
        vec![SchedulerEvent::RestFinished {
            reason,
            state: self.snapshot(now_ms),
        }]
    }

    fn start_now(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        if self.phase == SchedulerPhase::Resting {
            return Vec::new();
        }
        self.begin_rest(now_ms);
        vec![SchedulerEvent::RestStarted {
            state: self.snapshot(now_ms),
        }]
    }

    fn restart_cycle(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        let was_resting = self.phase == SchedulerPhase::Resting;
        self.begin_new_work_cycle(now_ms);
        if was_resting {
            vec![SchedulerEvent::RestFinished {
                reason: RestFinishedReason::CycleRestarted,
                state: self.snapshot(now_ms),
            }]
        } else {
            vec![SchedulerEvent::StateChanged {
                reason: StateChangeReason::CycleRestarted,
                state: self.snapshot(now_ms),
            }]
        }
    }

    #[cfg(test)]
    fn on_sleep(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        if self.phase == SchedulerPhase::Sleeping {
            return Vec::new();
        }
        let was_resting = self.phase == SchedulerPhase::Resting;
        self.phase = SchedulerPhase::Sleeping;
        self.clear_deadlines();
        if was_resting {
            vec![SchedulerEvent::RestFinished {
                reason: RestFinishedReason::Sleep,
                state: self.snapshot(now_ms),
            }]
        } else {
            vec![SchedulerEvent::StateChanged {
                reason: StateChangeReason::Sleep,
                state: self.snapshot(now_ms),
            }]
        }
    }

    #[cfg(test)]
    fn on_wake(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        if self.phase != SchedulerPhase::Sleeping {
            return Vec::new();
        }
        self.begin_new_work_cycle(now_ms);
        vec![SchedulerEvent::StateChanged {
            reason: StateChangeReason::Wake,
            state: self.snapshot(now_ms),
        }]
    }

    fn reset_after_tick_gap(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        let was_resting = self.phase == SchedulerPhase::Resting;
        self.last_wake_reset_at_ms = Some(now_ms);
        self.begin_new_work_cycle(now_ms);
        if was_resting {
            vec![SchedulerEvent::RestFinished {
                reason: RestFinishedReason::Sleep,
                state: self.snapshot(now_ms),
            }]
        } else {
            vec![SchedulerEvent::StateChanged {
                reason: StateChangeReason::Wake,
                state: self.snapshot(now_ms),
            }]
        }
    }

    fn reset_after_wake(&mut self, now_ms: u64) -> Vec<SchedulerEvent> {
        // An explicit platform wake must win over an already-expired deadline.
        // Updating the observation first also prevents the worker's next tick
        // from treating the same suspension as a second long-gap wake.
        self.last_observed_at_ms = now_ms;
        if self.last_wake_reset_at_ms.is_some_and(|previous| {
            now_ms >= previous && now_ms - previous <= WAKE_RESET_DEDUP_WINDOW_MS
        }) {
            return Vec::new();
        }
        self.reset_after_tick_gap(now_ms)
    }

    fn begin_rest(&mut self, now_ms: u64) {
        self.phase = SchedulerPhase::Resting;
        self.next_rest_at_ms = None;
        self.paused_remaining_ms = None;
        self.rest_end_at_ms = Some(now_ms.saturating_add(self.config.rest_duration_ms()));
    }

    fn begin_new_work_cycle(&mut self, now_ms: u64) {
        self.clear_deadlines();
        if self.config.rest_enabled {
            self.phase = SchedulerPhase::Working;
            self.next_rest_at_ms = Some(now_ms.saturating_add(self.config.work_interval_ms()));
        } else {
            self.phase = SchedulerPhase::Disabled;
        }
    }

    fn clear_deadlines(&mut self) {
        self.next_rest_at_ms = None;
        self.rest_end_at_ms = None;
        self.paused_remaining_ms = None;
    }
}

struct SharedScheduler {
    core: Mutex<SchedulerCore>,
    changed: Condvar,
    events: Sender<SchedulerEvent>,
}

/// Handle intended for Tauri managed state. `spawn` owns the timing thread and
/// returns a single receiver for an application-level event bridge.
pub struct RestScheduler {
    shared: Arc<SharedScheduler>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl RestScheduler {
    pub fn spawn(
        config: SchedulerConfig,
    ) -> Result<(Self, Receiver<SchedulerEvent>), SchedulerError> {
        let config = config.validate()?;
        let (event_sender, event_receiver) = mpsc::channel();
        let shared = Arc::new(SharedScheduler {
            core: Mutex::new(SchedulerCore::new(config, epoch_millis())),
            changed: Condvar::new(),
            events: event_sender,
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("rest-scheduler".to_string())
            .spawn(move || run_worker(worker_shared))
            .map_err(SchedulerError::ThreadSpawn)?;

        Ok((
            Self {
                shared,
                worker: Mutex::new(Some(worker)),
            },
            event_receiver,
        ))
    }

    pub fn configure(&self, config: SchedulerConfig) -> Result<SchedulerState, SchedulerError> {
        let config = config.validate()?;
        self.mutate(|core, now_ms| core.configure(config, now_ms))
    }

    pub fn query(&self) -> Result<SchedulerState, SchedulerError> {
        self.mutate(|_, _| Vec::new())
    }

    pub fn pause(&self) -> Result<SchedulerState, SchedulerError> {
        self.mutate(SchedulerCore::pause)
    }

    pub fn resume(&self) -> Result<SchedulerState, SchedulerError> {
        self.mutate(SchedulerCore::resume)
    }

    pub fn finish(&self) -> Result<SchedulerState, SchedulerError> {
        self.mutate(|core, now_ms| core.finish(now_ms, RestFinishedReason::User))
    }

    /// Starts a rest even when periodic rests are disabled. Repeated calls while
    /// already resting are idempotent and do not extend or retrigger the rest.
    pub fn start_now(&self) -> Result<SchedulerState, SchedulerError> {
        self.mutate(|core, now_ms| {
            if core.phase == SchedulerPhase::Sleeping {
                return Vec::new();
            }
            core.start_now(now_ms)
        })
        .and_then(|state| {
            if state.phase == SchedulerPhase::Sleeping {
                Err(SchedulerError::Sleeping)
            } else {
                Ok(state)
            }
        })
    }

    /// Discards any active rest/deadline and starts a full work interval from
    /// now. This is also the desired application-restart semantic.
    pub fn restart_cycle(&self) -> Result<SchedulerState, SchedulerError> {
        self.mutate(SchedulerCore::restart_cycle)
    }

    /// Resets all deadlines after a platform wake without first advancing an
    /// expired work/rest deadline. This handles short sleeps that are below the
    /// worker's long-tick-gap fallback threshold.
    pub fn reset_after_wake(&self) -> Result<SchedulerState, SchedulerError> {
        let now_ms = epoch_millis();
        let state = {
            let mut core = self
                .shared
                .core
                .lock()
                .map_err(|_| SchedulerError::LockPoisoned)?;
            if core.shutdown {
                return Err(SchedulerError::Shutdown);
            }

            let events = core.reset_after_wake(now_ms);
            let state = core.snapshot(now_ms);
            publish_events(&self.shared.events, events);
            state
        };
        self.shared.changed.notify_all();
        Ok(state)
    }

    pub fn shutdown(&self) {
        {
            let mut core = self
                .shared
                .core
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            core.shutdown = true;
        }
        self.shared.changed.notify_all();
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }

    fn mutate(
        &self,
        mutation: impl FnOnce(&mut SchedulerCore, u64) -> Vec<SchedulerEvent>,
    ) -> Result<SchedulerState, SchedulerError> {
        let now_ms = epoch_millis();
        let state = {
            let mut core = self
                .shared
                .core
                .lock()
                .map_err(|_| SchedulerError::LockPoisoned)?;
            if core.shutdown {
                return Err(SchedulerError::Shutdown);
            }

            let mut events = core.observe_time(now_ms);
            events.extend(mutation(&mut core, now_ms));
            let state = core.snapshot(now_ms);
            publish_events(&self.shared.events, events);
            state
        };
        self.shared.changed.notify_all();
        Ok(state)
    }
}

impl Drop for RestScheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(shared: Arc<SharedScheduler>) {
    let mut core = shared
        .core
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if core.shutdown {
            break;
        }

        let now_ms = epoch_millis();
        let events = core.observe_time(now_ms);
        if !events.is_empty() {
            publish_events(&shared.events, events);
            continue;
        }

        // A timed wait is intentional even while disabled, paused, or marked
        // sleeping: otherwise the worker cannot notice that the process was
        // suspended and reset the cycle after wake without a platform event.
        let wait = core
            .next_deadline_ms()
            .map(|deadline_ms| {
                Duration::from_millis(deadline_ms.saturating_sub(now_ms).max(1))
                    .min(MAX_CLOCK_RECHECK)
            })
            .unwrap_or(MAX_CLOCK_RECHECK);
        let (next_core, _) = shared
            .changed
            .wait_timeout(core, wait)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        core = next_core;
    }
}

fn publish_events(sender: &Sender<SchedulerEvent>, events: Vec<SchedulerEvent>) {
    for event in events {
        let _ = sender.send(event);
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::TryRecvError;

    fn test_config() -> SchedulerConfig {
        SchedulerConfig {
            rest_enabled: true,
            work_interval_minutes: 1,
            rest_duration_seconds: 60,
            allow_esc: true,
        }
    }

    #[test]
    fn absolute_work_deadline_starts_rest_exactly_once() {
        let mut core = SchedulerCore::new(test_config(), 1_000);
        assert!(core.advance(60_999).is_empty());

        let events = core.advance(61_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::RestStarted { .. }]
        ));
        assert_eq!(core.phase, SchedulerPhase::Resting);
        assert_eq!(core.rest_end_at_ms, Some(121_000));
        assert!(core.advance(61_000).is_empty());
        assert!(core.advance(90_000).is_empty());
    }

    #[test]
    fn elapsed_rest_finishes_once_and_starts_a_fresh_work_cycle() {
        let mut core = SchedulerCore::new(test_config(), 0);
        assert_eq!(core.advance(60_000).len(), 1);
        let events = core.advance(120_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::RestFinished {
                reason: RestFinishedReason::Elapsed,
                ..
            }]
        ));
        assert_eq!(core.phase, SchedulerPhase::Working);
        assert_eq!(core.next_rest_at_ms, Some(180_000));
        assert!(core.advance(120_000).is_empty());
    }

    #[test]
    fn pause_freezes_remaining_time_and_resume_builds_a_new_deadline() {
        let mut core = SchedulerCore::new(test_config(), 0);
        core.start_now(10_000);
        let pause_events = core.pause(25_000);
        assert_eq!(pause_events.len(), 1);
        assert_eq!(core.paused_remaining_ms, Some(45_000));
        assert_eq!(core.rest_end_at_ms, None);
        assert!(core.advance(1_000_000).is_empty());

        let resume_events = core.resume(1_000_000);
        assert_eq!(resume_events.len(), 1);
        assert_eq!(core.rest_end_at_ms, Some(1_045_000));
        assert!(core.advance(1_044_999).is_empty());
        assert_eq!(core.advance(1_045_000).len(), 1);
        assert!(core.advance(1_045_000).is_empty());
    }

    #[test]
    fn finish_and_start_now_are_idempotent() {
        let mut core = SchedulerCore::new(test_config(), 0);
        assert_eq!(core.start_now(10).len(), 1);
        assert!(core.start_now(20).is_empty());
        assert_eq!(core.rest_end_at_ms, Some(60_010));

        assert_eq!(core.finish(1_000, RestFinishedReason::User).len(), 1);
        assert!(core.finish(1_001, RestFinishedReason::User).is_empty());
        assert_eq!(core.next_rest_at_ms, Some(61_000));
    }

    #[test]
    fn configuration_changes_only_reset_the_affected_deadline() {
        let mut core = SchedulerCore::new(test_config(), 0);
        let mut changed = test_config();
        changed.allow_esc = false;
        core.configure(changed, 10_000);
        assert_eq!(core.next_rest_at_ms, Some(60_000));

        changed.work_interval_minutes = 2;
        core.configure(changed, 10_000);
        assert_eq!(core.next_rest_at_ms, Some(130_000));

        assert!(core.configure(changed, 20_000).is_empty());
        assert_eq!(core.next_rest_at_ms, Some(130_000));
    }

    #[test]
    fn sleep_suppresses_deadlines_and_wake_resets_a_full_work_cycle() {
        let mut core = SchedulerCore::new(test_config(), 0);
        assert_eq!(core.on_sleep(30_000).len(), 1);
        assert_eq!(core.phase, SchedulerPhase::Sleeping);
        assert!(core.advance(600_000).is_empty());

        assert_eq!(core.on_wake(600_000).len(), 1);
        assert_eq!(core.phase, SchedulerPhase::Working);
        assert_eq!(core.next_rest_at_ms, Some(660_000));
        assert!(core.on_wake(601_000).is_empty());
        assert_eq!(core.next_rest_at_ms, Some(660_000));
    }

    #[test]
    fn long_worker_gap_closes_a_rest_and_resets_the_work_cycle() {
        let mut core = SchedulerCore::new(test_config(), 0);
        core.start_now(1_000);
        let events = core.reset_after_tick_gap(600_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::RestFinished {
                reason: RestFinishedReason::Sleep,
                ..
            }]
        ));
        assert_eq!(core.phase, SchedulerPhase::Working);
        assert_eq!(core.next_rest_at_ms, Some(660_000));
        assert!(core.advance(600_000).is_empty());
    }

    #[test]
    fn eight_second_worker_gap_resets_before_an_expired_deadline_can_fire() {
        let mut core = SchedulerCore::new(test_config(), 0);
        core.last_observed_at_ms = 55_000;

        let events = core.observe_time(63_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::StateChanged {
                reason: StateChangeReason::Wake,
                ..
            }]
        ));
        assert_eq!(core.phase, SchedulerPhase::Working);
        assert_eq!(core.next_rest_at_ms, Some(123_000));
    }

    #[test]
    fn normal_one_second_worker_ticks_do_not_reset_the_cycle() {
        let mut core = SchedulerCore::new(test_config(), 0);

        assert!(core.observe_time(1_000).is_empty());
        assert!(core.observe_time(2_050).is_empty());
        assert_eq!(core.phase, SchedulerPhase::Working);
        assert_eq!(core.next_rest_at_ms, Some(60_000));
        assert_eq!(core.last_wake_reset_at_ms, None);
    }

    #[test]
    fn backwards_wall_clock_change_resets_the_work_cycle() {
        let mut core = SchedulerCore::new(test_config(), 100_000);
        core.last_observed_at_ms = 100_000;

        let events = core.observe_time(40_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::StateChanged {
                reason: StateChangeReason::Wake,
                ..
            }]
        ));
        assert_eq!(core.next_rest_at_ms, Some(100_000));
    }

    #[test]
    fn explicit_short_wake_resets_before_an_expired_work_deadline_can_fire() {
        let mut core = SchedulerCore::new(test_config(), 0);
        core.last_observed_at_ms = 55_000;

        let events = core.reset_after_wake(63_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::StateChanged {
                reason: StateChangeReason::Wake,
                ..
            }]
        ));
        assert_eq!(core.phase, SchedulerPhase::Working);
        assert_eq!(core.next_rest_at_ms, Some(123_000));
        assert!(core.advance(63_000).is_empty());
        assert!(core.advance(60_000).is_empty());
    }

    #[test]
    fn explicit_wake_during_a_rest_closes_it_and_starts_fresh_work() {
        let mut core = SchedulerCore::new(test_config(), 0);
        core.start_now(10_000);
        core.pause(15_000);

        let events = core.reset_after_wake(20_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::RestFinished {
                reason: RestFinishedReason::Sleep,
                ..
            }]
        ));
        assert_eq!(core.phase, SchedulerPhase::Working);
        assert_eq!(core.next_rest_at_ms, Some(80_000));
        assert_eq!(core.rest_end_at_ms, None);
        assert_eq!(core.paused_remaining_ms, None);
    }

    #[test]
    fn explicit_wake_does_not_repeat_a_recent_worker_gap_reset() {
        let mut core = SchedulerCore::new(test_config(), 0);

        assert_eq!(core.observe_time(20_000).len(), 1);
        assert_eq!(core.next_rest_at_ms, Some(80_000));
        assert!(core.reset_after_wake(20_500).is_empty());
        assert_eq!(core.next_rest_at_ms, Some(80_000));
        assert!(core.observe_time(20_501).is_empty());
    }

    #[test]
    fn explicit_wake_and_worker_gap_share_one_observation() {
        let mut core = SchedulerCore::new(test_config(), 0);
        core.on_sleep(1_000);
        core.last_observed_at_ms = 1_000;

        let events = core.observe_time(600_000);
        assert!(matches!(
            events.as_slice(),
            [SchedulerEvent::StateChanged {
                reason: StateChangeReason::Wake,
                ..
            }]
        ));
        assert!(core.on_wake(600_000).is_empty());
        assert!(core.observe_time(600_001).is_empty());
        assert_eq!(core.next_rest_at_ms, Some(660_000));
    }

    #[test]
    fn disabled_schedule_still_allows_manual_rest() {
        let mut config = test_config();
        config.rest_enabled = false;
        let mut core = SchedulerCore::new(config, 0);
        assert_eq!(core.phase, SchedulerPhase::Disabled);
        assert_eq!(core.start_now(100).len(), 1);
        assert_eq!(core.phase, SchedulerPhase::Resting);
        core.finish(200, RestFinishedReason::User);
        assert_eq!(core.phase, SchedulerPhase::Disabled);
    }

    #[test]
    fn state_and_events_serialize_with_frontend_field_names() {
        let mut core = SchedulerCore::new(test_config(), 1_000);
        let event = core.start_now(2_000).remove(0);
        let value = serde_json::to_value(event).expect("serialize scheduler event");
        assert_eq!(value["event"], "restStarted");
        assert_eq!(value["state"]["restEnabled"], true);
        assert_eq!(value["state"]["phase"], "resting");
        assert_eq!(value["state"]["restEndAtMs"], 62_000);
        assert_eq!(value["state"]["allowEsc"], true);
    }

    #[test]
    fn background_channel_does_not_duplicate_manual_start_or_finish() {
        let mut config = test_config();
        config.rest_enabled = false;
        let (scheduler, receiver) = RestScheduler::spawn(config).expect("spawn scheduler");

        scheduler.start_now().expect("start rest");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(SchedulerEvent::RestStarted { .. })
        ));
        scheduler.start_now().expect("repeat start rest");
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));

        scheduler.finish().expect("finish rest");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(SchedulerEvent::RestFinished {
                reason: RestFinishedReason::User,
                ..
            })
        ));
        scheduler.finish().expect("repeat finish rest");
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn public_wake_reset_closes_an_active_rest_through_the_event_channel() {
        let mut config = test_config();
        config.rest_enabled = false;
        let (scheduler, receiver) = RestScheduler::spawn(config).expect("spawn scheduler");

        scheduler.start_now().expect("start rest");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(SchedulerEvent::RestStarted { .. })
        ));

        let state = scheduler.reset_after_wake().expect("reset after wake");
        assert_eq!(state.phase, SchedulerPhase::Disabled);
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(SchedulerEvent::RestFinished {
                reason: RestFinishedReason::Sleep,
                ..
            })
        ));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    }
}
