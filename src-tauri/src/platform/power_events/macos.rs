use super::take_requested;
use block2::RcBlock;
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
use objc2_foundation::NSNotification;
use std::ptr::NonNull;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

static WAKE_REQUESTED: AtomicBool = AtomicBool::new(false);
static REGISTERED: AtomicBool = AtomicBool::new(false);
static REGISTRATION_LOCK: Mutex<()> = Mutex::new(());

/// Registers a process-lifetime observer for macOS workspace wake notifications.
///
/// The first successful call must run on the application main thread. Later calls
/// are idempotent and may come from any thread.
pub fn register() -> Result<(), String> {
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }

    if MainThreadMarker::new().is_none() {
        return Err("macOS 唤醒通知必须在应用主线程注册".into());
    }

    let _registration_guard = REGISTRATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }

    let workspace = NSWorkspace::sharedWorkspace();
    let notification_center = workspace.notificationCenter();
    let callback: RcBlock<dyn Fn(NonNull<NSNotification>)> =
        RcBlock::new(|_notification: NonNull<NSNotification>| {
            WAKE_REQUESTED.store(true, Ordering::Release);
        });

    // SAFETY: The name is AppKit's public workspace-wake constant, no object
    // filter or operation queue is supplied, and the sendable callback captures
    // no thread-affine state: it only sets a process-lifetime AtomicBool.
    let observer = unsafe {
        notification_center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidWakeNotification),
            None,
            None,
            &callback,
        )
    };

    // The registration intentionally lasts for the process lifetime. Keeping the
    // returned +1 retain count also keeps the opaque observer token alive, while
    // NSNotificationCenter owns the copied block used by that registration.
    let _observer = Retained::into_raw(observer);
    REGISTERED.store(true, Ordering::Release);
    Ok(())
}

/// Returns whether a wake was observed since the previous call and clears it.
pub fn take_wake_requested() -> bool {
    take_requested(&WAKE_REQUESTED)
}
