use chan::{Receiver, Sender};
use chan_signal::Signal;
use std::time::Duration;

use crate::errors::*;

#[derive(Clone)] // so multiple threads could wait on signals
pub struct Waiter {
    term_signal: Receiver<Signal>,
    sync_signal: Receiver<Signal>,
    never_chan: (Sender<Signal>, Receiver<Signal>),
}

impl Waiter {
    pub fn start() -> Waiter {
        Waiter {
            term_signal: chan_signal::notify(&[Signal::INT, Signal::TERM]),
            sync_signal: chan_signal::notify(&[Signal::USR1]),
            never_chan: chan::sync(0),
        }
    }

    /// Wait for the timeout duration or until a termination signal comes in.
    pub fn wait(&self, duration: Duration) -> Result<()> {
        wait(duration, &self.term_signal, &self.never_chan.1)
    }

    /// Wait for the timeout duration, until a termination signal comes in, or until a
    /// SIGUSR1 signal comes to trigger a real-time sync (via bitcoind's blocknotify).
    pub fn wait_sync(&self, duration: Duration) -> Result<()> {
        wait(duration, &self.term_signal, &self.sync_signal)
    }

    pub fn poll(&self) -> Result<()> {
        self.wait(Duration::from_secs(0))
    }
}

fn wait(
    duration: Duration,
    term_signal: &Receiver<Signal>,
    sync_signal: &Receiver<Signal>,
) -> Result<()> {
    let timeout = chan::after(duration);
    chan_select! {
        term_signal.recv() -> s => {
            if let Some(sig) = s {
                bail!(ErrorKind::Interrupt(sig));
            }
        },
        sync_signal.recv() => {},
        timeout.recv() => {},
    }
    Ok(())
}
