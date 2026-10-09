//! Windows sleeps on its idle timer even while a download or console transfer is moving bytes,
//! which drops every connection. While a job is working the system stays awake; the display can
//! still turn off, and paused, queued or finished jobs never hold it.
use super::*;

fn working(p: &Progress) -> bool { !p.paused && !terminal_stage(&p.stage) && p.stage != "queued" }

pub(super) fn keep_awake_while_working(app: AppHandle) {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" { fn SetThreadExecutionState(flags: u32) -> u32; }
        const ES_CONTINUOUS: u32 = 0x8000_0000;
        const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
        // The request belongs to the thread that makes it, so one thread holds and releases it.
        let started = std::thread::Builder::new().name("keep-awake".into()).spawn(move || {
            let mut held = false;
            loop {
                let busy = app.try_state::<AppState>()
                    .and_then(|state| state.jobs.lock().ok().map(|jobs| jobs.values().any(working)))
                    .unwrap_or(false);
                if busy != held {
                    unsafe { SetThreadExecutionState(if busy { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS }); }
                    session_log::write("power", if busy { "Keeping Windows awake while jobs work" } else { "Windows may sleep again" });
                    held = busy;
                }
                std::thread::sleep(Duration::from_secs(10));
            }
        });
        if let Err(error) = started { session_log::write("power", &format!("Keep-awake thread failed: {error}")); }
    }
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_running_jobs_keep_windows_awake() {
        let job = |stage: &str, paused: bool| Progress { stage: stage.into(), paused, ..Default::default() };
        assert!(working(&job("downloading", false)));
        assert!(working(&job("installing", false)));
        assert!(!working(&job("downloading", true)));
        assert!(!working(&job("queued", false)));
        for stage in ["complete", "failed", "cancelled", "monitoring-ended", "delivered"] { assert!(!working(&job(stage, false))); }
    }
}
