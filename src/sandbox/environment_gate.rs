use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

#[derive(Default)]
struct State {
    readers: usize,
    writer: bool,
    waiting_writers: usize,
}
#[derive(Default)]
pub(super) struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}
pub(crate) struct Lease {
    gate: Arc<Gate>,
    writer: bool,
}

impl Gate {
    #[cfg(test)]
    pub(super) fn waiting_writer(&self) -> bool {
        self.state.lock().unwrap().waiting_writers > 0
    }
    pub fn read(self: &Arc<Self>, cancelled: &AtomicBool) -> io::Result<Lease> {
        self.acquire(false, cancelled)
    }
    pub fn write(self: &Arc<Self>, cancelled: &AtomicBool) -> io::Result<Lease> {
        self.acquire(true, cancelled)
    }
    pub fn try_read(self: &Arc<Self>) -> io::Result<Lease> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        if state.writer || state.waiting_writers > 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "System packages are changing; retry this controlled command after installation.",
            ));
        }
        state.readers += 1;
        Ok(Lease {
            gate: self.clone(),
            writer: false,
        })
    }
    pub fn try_write(self: &Arc<Self>) -> io::Result<Lease> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        if state.writer || state.readers > 0 || state.waiting_writers > 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Controlled commands are active; retry package installation after they finish.",
            ));
        }
        state.writer = true;
        Ok(Lease {
            gate: self.clone(),
            writer: true,
        })
    }
    fn acquire(self: &Arc<Self>, writer: bool, cancelled: &AtomicBool) -> io::Result<Lease> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        if writer {
            state.waiting_writers += 1;
        }
        loop {
            if cancelled.load(Ordering::Relaxed) {
                if writer {
                    state.waiting_writers -= 1;
                    self.changed.notify_all();
                }
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Environment wait stopped.",
                ));
            }
            if !state.writer
                && if writer {
                    state.readers == 0
                } else {
                    state.waiting_writers == 0
                }
            {
                if writer {
                    state.waiting_writers -= 1;
                    state.writer = true;
                } else {
                    state.readers += 1;
                }
                return Ok(Lease {
                    gate: self.clone(),
                    writer,
                });
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(25))
                .map_err(|e| io::Error::other(e.to_string()))?
                .0;
        }
    }
}
impl Lease {
    pub(super) fn reads(&self, gate: &Arc<Gate>) -> bool {
        !self.writer && Arc::ptr_eq(&self.gate, gate)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        if self.writer {
            state.writer = false;
        } else {
            state.readers -= 1;
        }
        self.gate.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waiting_writer_precedes_later_readers_and_cancelled_waits_release() {
        let gate = Arc::new(Gate::default());
        let cancelled = AtomicBool::new(false);
        let held = gate.read(&cancelled).unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                let _lease = gate.write(&cancelled).unwrap();
                sent.send("writer").unwrap();
            });
            while gate.state.lock().unwrap().waiting_writers == 0 {
                std::thread::yield_now();
            }
            let reader = scope.spawn(|| {
                let _lease = gate.read(&cancelled).unwrap();
                sent.send("reader").unwrap();
            });
            assert_eq!(
                gate.try_read().err().unwrap().kind(),
                io::ErrorKind::WouldBlock
            );
            drop(held);
            assert_eq!(
                received.recv_timeout(Duration::from_secs(1)).unwrap(),
                "writer"
            );
            assert_eq!(
                received.recv_timeout(Duration::from_secs(1)).unwrap(),
                "reader"
            );
            writer.join().unwrap();
            reader.join().unwrap();
        });
        let held = gate.read(&cancelled).unwrap();
        assert_eq!(
            gate.try_write().err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        let stop = AtomicBool::new(true);
        assert_eq!(
            gate.write(&stop).err().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(gate.state.lock().unwrap().waiting_writers, 0);
        drop(held);
        assert!(gate.write(&cancelled).is_ok());
    }
}
