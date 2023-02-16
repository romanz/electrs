use crossbeam_channel as channel;
use crossbeam_channel::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};

use crate::errors::*;

#[derive(Clone)] // so multiple threads could wait on signals
pub struct Waiter {
    receiver: channel::Receiver<i32>,
}

fn notify(signals: &[i32]) -> channel::Receiver<i32> {
    let (s, r) = channel::bounded(1);
    let mut signals =
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
            receiver: notify(&[
                SIGINT, SIGTERM,
                SIGUSR1, // allow external triggering (e.g. via bitcoind `blocknotify`)
            ]),
        }
    }

    pub fn wait(&self, duration: Duration, accept_sigusr: bool) -> Result<()> {
        // Determine the deadline time based on the duration, so that it doesn't
        // get pushed back when wait_deadline() recurses
        self.wait_deadline(Instant::now() + duration, accept_sigusr)
    }

    fn wait_deadline(&self, deadline: Instant, accept_sigusr: bool) -> Result<()> {
        match self.receiver.recv_deadline(deadline) {
            Ok(sig) if sig == SIGUSR1 => {
                trace!("notified via SIGUSR1");
                if accept_sigusr {
                    Ok(())
                } else {
                    self.wait_deadline(deadline, accept_sigusr)
                }
            }
            Ok(sig) => bail!(ErrorKind::Interrupt(sig)),
            Err(RecvTimeoutError::Timeout) => Ok(()),
            Err(RecvTimeoutError::Disconnected) => bail!("signal hook channel disconnected"),
        }
    }
}
