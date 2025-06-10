#[cfg(not(windows))]
use anyhow::Context;
use crossbeam_channel::{unbounded, Receiver};
#[cfg(not(windows))]
use signal_hook::consts::signal::*;
#[cfg(not(windows))]
use signal_hook::iterator::Signals;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::{error, fmt};

#[cfg(not(windows))]
use crate::thread::spawn;

#[derive(Debug)]
pub struct ExitError;

impl fmt::Display for ExitError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "exiting due to signal")
    }
}

impl error::Error for ExitError {}

#[derive(Clone)]
pub(crate) struct ExitFlag {
    flag: Arc<AtomicBool>,
}

impl ExitFlag {
    fn new() -> Self {
        ExitFlag {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn poll(&self) -> Result<(), ExitError> {
        if self.flag.load(Ordering::Relaxed) {
            Err(ExitError)
        } else {
            Ok(())
        }
    }

    fn set(&self) {
        self.flag.store(true, Ordering::Relaxed)
    }
}

pub(crate) struct Signal {
    rx: Receiver<()>,
    exit: ExitFlag,
}

impl Signal {
    #[cfg(not(windows))]
    pub fn new() -> Signal {
        let ids = vec![
            SIGINT, SIGTERM,
            SIGUSR1, // allow external triggering (e.g. via bitcoind `blocknotify`)
        ];
        let (tx, rx) = unbounded();
        let result = Signal {
            rx,
            exit: ExitFlag::new(),
        };

        let exit_flag = result.exit.clone();
        let mut signals = Signals::new(ids).expect("failed to register signal hook");
        spawn("signal", move || {
            for id in &mut signals {
                info!("notified via SIG{}", id);
                match id {
                    SIGUSR1 => (),
                    _ => exit_flag.set(),
                };
                tx.send(()).context("failed to send signal")?;
            }
            Ok(())
        });
        result
    }

    #[cfg(windows)]
    pub fn new() -> Signal {
        let (tx, rx) = unbounded();
        let result = Signal {
            rx,
            exit: ExitFlag::new(),
        };

        let exit_flag = result.exit.clone();

        // Handle Ctrl-C
        ctrlc::set_handler(move || {
            info!("notified via Ctrl-C");
            exit_flag.set();
            let _ = tx.send(());
        })
        .expect("failed to set Ctrl-C handler");

        result
    }

    pub fn receiver(&self) -> &Receiver<()> {
        &self.rx
    }

    pub fn exit_flag(&self) -> &ExitFlag {
        &self.exit
    }
}
