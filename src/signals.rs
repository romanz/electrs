use anyhow::Context;
use crossbeam_channel::{unbounded, Receiver};
use signal_hook::consts::signal::*;
use signal_hook::iterator::Signals;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::thread::spawn;

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

    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
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
    pub fn new() -> Signal {
        let ids = [
            SIGINT, SIGTERM,
            SIGUSR1, // allow external triggering (e.g. via bitcoind `blocknotify`)
        ];
        let (tx, rx) = unbounded();
        let result = Signal {
            rx,
            exit: ExitFlag::new(),
        };

        let exit_flag = result.exit.clone();
        let mut signals = Signals::new(&ids).expect("failed to register signal hook");
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

    pub fn receiver(&self) -> &Receiver<()> {
        &self.rx
    }

    pub fn exit_flag(&self) -> &ExitFlag {
        &self.exit
    }
}
