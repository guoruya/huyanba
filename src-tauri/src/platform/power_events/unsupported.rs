/// No native workspace wake source is needed on non-macOS platforms.
pub fn register() -> Result<(), String> {
    Ok(())
}

pub fn take_wake_requested() -> bool {
    false
}
