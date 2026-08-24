#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use macos::{register, take_wake_requested};
#[cfg(not(target_os = "macos"))]
pub use unsupported::{register, take_wake_requested};

#[cfg(any(target_os = "macos", test))]
fn take_requested(flag: &AtomicBool) -> bool {
    flag.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    use super::take_requested;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn wake_requests_coalesce_and_are_consumed_once() {
        let flag = AtomicBool::new(false);

        assert!(!take_requested(&flag));
        flag.store(true, Ordering::Release);
        flag.store(true, Ordering::Release);
        assert!(take_requested(&flag));
        assert!(!take_requested(&flag));
    }
}
