//! Process-level "no traces" measures (PRD §11).

/// Disables crash/core dumps where the OS lets a process do that for itself.
///
/// Windows: suppresses the crash dialog / WER UI for this process. System-wide
/// WER `LocalDumps` policy is not changed (that would modify system settings).
pub fn disable_crash_dumps() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
        };
        // SAFETY: SetErrorMode only changes this process's error-mode flags.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        }
    }
    #[cfg(unix)]
    {
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: plain syscall with a valid pointer to a stack value.
        unsafe {
            libc::setrlimit(libc::RLIMIT_CORE, &none);
        }
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: PR_SET_DUMPABLE takes integer arguments only.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        }
    }
}

/// Opt-in debug logging to stderr only (never to a file). Enabled by setting
/// `GHOSTCHAT_DEBUG` (`1` or a tracing filter such as `iroh=debug`).
/// GhostChat's own code never logs message content, keys or file content.
pub fn init_debug_logging() {
    let Ok(value) = std::env::var("GHOSTCHAT_DEBUG") else {
        return;
    };
    if value.is_empty() || value == "0" {
        return;
    }
    let filter = if value == "1" {
        "warn,iroh=info".to_string()
    } else {
        value
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .try_init();
}
