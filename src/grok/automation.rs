//! One thread owns Windows UI Automation for Grok Bot. Callers on the HID and
//! roster threads queue work here instead of blocking on a slow window.

use std::{
    sync::{
        LazyLock,
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use uiautomation::UIAutomation;
use windows::{
    Win32::{
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx},
        UI::Accessibility::{IUIAutomation, IUIAutomation2},
    },
    core::Interface,
};

type Job = Box<dyn FnOnce(Result<&UIAutomation>) + Send>;

const CONNECTION_TIMEOUT_MS: u32 = 1000;
const TRANSACTION_TIMEOUT_MS: u32 = 3000;
const WAIT: Duration = Duration::from_secs(10);

static JOBS: LazyLock<Option<Sender<Job>>> = LazyLock::new(|| {
    let (send, jobs) = mpsc::channel();
    std::thread::Builder::new()
        .name("DeckX accessibility".into())
        .spawn(move || serve(jobs))
        .ok()?;
    Some(send)
});

/// Queues work on the accessibility thread and returns at once.
pub(super) fn spawn(job: impl FnOnce(Result<&UIAutomation>) + Send + 'static) -> Result<()> {
    JOBS.as_ref()
        .context("Windows accessibility is unavailable")?
        .send(Box::new(job))
        .ok()
        .context("Windows accessibility stopped")
}

/// Runs work on the accessibility thread and waits for its result.
pub(super) fn run<T: Send + 'static>(
    job: impl FnOnce(&UIAutomation) -> Result<T> + Send + 'static,
) -> Result<T> {
    let (send, receive) = mpsc::sync_channel(1);
    spawn(move |automation| {
        let _ = send.send(automation.and_then(job));
    })?;
    receive
        .recv_timeout(WAIT)
        .context("Windows accessibility did not respond")?
}

fn serve(jobs: Receiver<Job>) {
    // This thread lives as long as the process, so COM stays initialized.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok();
    let mut automation = Err(String::new());
    for job in jobs {
        if automation.is_err() {
            automation = match &com {
                Ok(()) => open().map_err(|error| format!("{error:#}")),
                Err(error) => Err(format!(
                    "Could not initialize Windows accessibility: {error}"
                )),
            };
        }
        job(automation
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}")));
    }
}

fn open() -> Result<UIAutomation> {
    let automation = UIAutomation::new_direct().context("Could not open Windows accessibility")?;
    // Bound how long an unresponsive window can hold the thread.
    let client: &IUIAutomation = automation.as_ref();
    if let Ok(timeouts) = client.cast::<IUIAutomation2>() {
        unsafe {
            let _ = timeouts.SetConnectionTimeout(CONNECTION_TIMEOUT_MS);
            let _ = timeouts.SetTransactionTimeout(TRANSACTION_TIMEOUT_MS);
        }
    }
    Ok(automation)
}
