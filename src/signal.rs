use crossbeam_channel as channel;
use crossbeam_channel::RecvTimeoutError;
use signal_hook;
use std::thread;
use std::time::Duration;

use crate::errors::*;

#[derive(Clone)] // so multiple threads could wait on signals
pub struct Waiter {
    receiver: channel::Receiver<i32>,
}

fn notify(signals: &[i32]) -> channel::Receiver<i32> {
    let (s, r) = channel::bounded(1);
    let signals =
        signal_hook::iterator::Signals::new(signals).expect("failed to register signal hook");
    thread::spawn(move || {
        for signal in signals.forever() {
            s.send(signal)
                .unwrap_or_else(|_| panic!("failed to send signal {}", signal));
        }
    });
    r
}

impl Waiter {
    pub fn start() -> Waiter {
        Waiter {
            receiver: notify(&[signal_hook::SIGINT, signal_hook::SIGTERM]),
        }
    }
    pub fn wait(&self, duration: Duration) -> Result<()> {
        match self.receiver.recv_timeout(duration) {
            Ok(sig) => bail!(ErrorKind::Interrupt(sig)),
            Err(RecvTimeoutError::Timeout) => Ok(()),
            Err(RecvTimeoutError::Disconnected) => bail!("signal hook channel disconnected"),
        }
    }
    pub fn poll(&self) -> Result<()> {
        self.wait(Duration::from_secs(0))
    }
}
