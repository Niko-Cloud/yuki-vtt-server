//! Keeps the computer from sleeping while it hosts, so players aren't cut off when the DM
//! stops touching the laptop for a while. Each system's own way: `systemd-inhibit` on Linux,
//! `caffeinate` on macOS, the execution state on Windows. Let go when the host stops.

/// Held while hosting; the computer may sleep again once it is dropped.
pub struct Awake {
    #[cfg(not(windows))]
    holder: Option<std::process::Child>,
    #[cfg(windows)]
    holder: Option<std::sync::mpsc::Sender<()>>,
}

/// Asks the computer to stay awake. Where it can't (no systemd, say), it says so and the host
/// runs on anyway.
pub fn keep_awake() -> Awake {
    let holder = hold();
    if holder.is_none() {
        tracing::warn!("couldn't keep this computer awake; it may sleep while players are on");
    }
    Awake { holder }
}

#[cfg(target_os = "linux")]
fn hold() -> Option<std::process::Child> {
    std::process::Command::new("systemd-inhibit")
        .args([
            "--what=sleep:idle",
            "--who=Yuki VTT Server",
            "--why=Players are at a table",
            "--mode=block",
            "sleep",
            "infinity",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

#[cfg(target_os = "macos")]
fn hold() -> Option<std::process::Child> {
    // Held for as long as this program runs, and let go when it is killed.
    std::process::Command::new("caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .spawn()
        .ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn hold() -> Option<std::process::Child> {
    None
}

#[cfg(windows)]
fn hold() -> Option<std::sync::mpsc::Sender<()>> {
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    // The state belongs to the thread that sets it, so one thread holds it until told to stop.
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        // SAFETY: plain flags; the call only marks this thread as needing the system awake.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
        let _ = stopped.recv();
        // SAFETY: as above; this lets the system sleep again.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    });
    Some(stop)
}

impl Drop for Awake {
    fn drop(&mut self) {
        #[cfg(not(windows))]
        if let Some(mut child) = self.holder.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(windows)]
        if let Some(stop) = self.holder.take() {
            let _ = stop.send(());
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn holds_the_computer_awake_and_lets_go() {
        let awake = keep_awake();
        let Some(child) = awake.holder.as_ref() else {
            // No systemd here (a container): nothing to hold, and the host runs on.
            return;
        };
        let pid = child.id();
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        drop(awake);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let gone = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|stat| stat.contains(") Z "))
            .unwrap_or(true);
        assert!(gone, "the inhibitor is let go");
    }
}
