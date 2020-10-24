use crossbeam_channel::{bounded, Receiver, RecvTimeoutError};

use signal_hook::iterator::Signals;

use std::thread;
use std::time::Duration;

use anyhow::Result;

#[derive(Clone)] // so multiple threads could wait on signals
pub struct Waiter {
    rx: Receiver<i32>,
}

fn notify(signals: &[i32]) -> Receiver<i32> {
    let (tx, rx) = bounded(1);
    let signals = Signals::new(signals).expect("failed to register signal hook");
    thread::spawn(move || {
        for signal in signals.forever() {
            info!("notified via SIG{}", signal);
            tx.send(signal)
                .unwrap_or_else(|_| panic!("failed to send signal {}", signal));
        }
    });
    rx
}

impl Waiter {
    pub fn start() -> Waiter {
        Waiter {
            rx: notify(&[
                signal_hook::SIGINT,
                signal_hook::SIGTERM,
                signal_hook::SIGUSR1, // allow external triggering (e.g. via bitcoind `blocknotify`)
            ]),
        }
    }
    pub fn wait(&self, duration: Duration) -> Result<()> {
        match self.rx.recv_timeout(duration) {
            Ok(sig) => {
                if sig != signal_hook::SIGUSR1 {
                    bail!("Interrupted with SIG{}", sig);
                };
                Ok(())
            }
            Err(RecvTimeoutError::Timeout) => Ok(()),
            Err(RecvTimeoutError::Disconnected) => panic!("signal hook channel disconnected"),
        }
    }
    pub fn poll(&self) -> Result<()> {
        self.wait(Duration::from_secs(0))
    }
}
