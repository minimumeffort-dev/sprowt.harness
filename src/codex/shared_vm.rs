use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{
    diagnostics::timing::{Phase, Timing},
    sandbox::Sandbox,
};

type Cell<T> = Mutex<Weak<Mutex<T>>>;

struct Registry<T>(Mutex<BTreeMap<PathBuf, Arc<Cell<T>>>>);

impl<T> Registry<T> {
    const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    fn prepare(
        &self,
        root: &Path,
        cancelled: &AtomicBool,
        timing: Timing<'_>,
        prepare: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<Arc<Mutex<T>>> {
        let cell = {
            let mut entries = self.0.lock().unwrap();
            entries.retain(|_, cell| {
                Arc::strong_count(cell) > 1 || cell.try_lock().is_ok_and(|vm| vm.strong_count() > 0)
            });
            entries
                .entry(root.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(Weak::new())))
                .clone()
        };
        let mut slot = timing.measure(Phase::StartupWait, || cancellable_lock(&cell, cancelled))?;
        if let Some(vm) = slot.upgrade() {
            return Ok(vm);
        }
        let vm = Arc::new(Mutex::new(prepare()?));
        *slot = Arc::downgrade(&vm);
        Ok(vm)
    }
}

pub(super) fn cancellable_lock<'a, T>(
    mutex: &'a Mutex<T>,
    cancelled: &AtomicBool,
) -> io::Result<MutexGuard<'a, T>> {
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Worker startup stopped. Ctrl+R retries.",
            ));
        }
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(io::Error::other(error.to_string())),
        }
    }
}

static VMS: Registry<Sandbox> = Registry::new();

pub(crate) fn prepare(
    root: &Path,
    tasks: &[i64],
    memory: Option<u64>,
    cancelled: &AtomicBool,
    progress: impl Fn(&str),
    timing: Timing<'_>,
) -> io::Result<super::SharedVm> {
    let vm = VMS.prepare(root, cancelled, timing, || {
        Sandbox::prepare_empty_with_timing(root, memory, cancelled, &progress, timing)
    })?;
    {
        let mut prepared = cancellable_lock(&vm, cancelled)?;
        prepared.bind(tasks, memory, cancelled, &progress, timing)?;
    }
    Ok(vm)
}

pub(crate) fn prewarm(
    root: &Path,
    cancelled: &AtomicBool,
    timing: Timing<'_>,
) -> io::Result<Option<super::SharedVm>> {
    if Sandbox::has_bound_source(root)? {
        return Ok(None);
    }
    match VMS.prepare(root, cancelled, timing, || {
        if Sandbox::has_bound_source(root)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Source is already bound.",
            ));
        }
        Sandbox::prepare_empty_with_timing(root, None, cancelled, &|_| {}, timing)
    }) {
        Ok(vm) => Ok(Some(vm)),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, mpsc};

    #[test]
    fn different_roots_prepare_concurrently() {
        let registry = Registry::<String>::new();
        let cancelled = AtomicBool::new(false);
        let release = AtomicBool::new(false);
        let (started, ready) = mpsc::channel();
        let prepare = |name: &str| {
            started.send(name.to_owned()).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !release.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(name.to_owned())
        };
        thread::scope(|scope| {
            let a = scope.spawn(|| {
                registry
                    .prepare(Path::new("a"), &cancelled, Timing::quiet(), || {
                        prepare("first")
                    })
                    .unwrap()
            });
            let b = scope.spawn(|| {
                registry
                    .prepare(Path::new("b"), &cancelled, Timing::quiet(), || {
                        prepare("second")
                    })
                    .unwrap()
            });
            let first = ready.recv_timeout(Duration::from_secs(1));
            let second = ready.recv_timeout(Duration::from_secs(1));
            release.store(true, Ordering::Relaxed);
            assert_eq!(
                std::collections::BTreeSet::from([first.unwrap(), second.unwrap()]),
                std::collections::BTreeSet::from(["first".into(), "second".into()])
            );
            assert_eq!(*a.join().unwrap().lock().unwrap(), "first");
            assert_eq!(*b.join().unwrap().lock().unwrap(), "second");
        });
    }

    #[test]
    fn same_root_waiters_share_one_preparation() {
        let registry = Registry::<String>::new();
        let count = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let (started, ready) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        thread::scope(|scope| {
            let registry = &registry;
            let count = &count;
            let cancelled = &cancelled;
            let first = scope.spawn(move || {
                registry
                    .prepare(Path::new("same"), cancelled, Timing::quiet(), || {
                        count.fetch_add(1, Ordering::Relaxed);
                        started.send(()).unwrap();
                        finish.recv_timeout(Duration::from_secs(2)).unwrap();
                        Ok("prepared".into())
                    })
                    .unwrap()
            });
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
            let (entering, entered) = mpsc::channel();
            let second = scope.spawn(move || {
                entering.send(()).unwrap();
                registry
                    .prepare(Path::new("same"), cancelled, Timing::quiet(), || {
                        count.fetch_add(1, Ordering::Relaxed);
                        Ok("wrong".into())
                    })
                    .unwrap()
            });
            entered.recv_timeout(Duration::from_secs(1)).unwrap();
            release.send(()).unwrap();
            let one = first.join().unwrap();
            let two = second.join().unwrap();
            assert!(Arc::ptr_eq(&one, &two));
            assert_eq!(*two.lock().unwrap(), "prepared");
            assert_eq!(count.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn cancellation_stops_a_waiter_without_stopping_its_owner() {
        let registry = Registry::<String>::new();
        let owner_cancelled = AtomicBool::new(false);
        let waiter_cancelled = AtomicBool::new(false);
        let (started, ready) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        thread::scope(|scope| {
            let registry = &registry;
            let owner_cancelled = &owner_cancelled;
            let owner = scope.spawn(move || {
                registry.prepare(Path::new("same"), owner_cancelled, Timing::quiet(), || {
                    started.send(()).unwrap();
                    finish.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok("retained".into())
                })
            });
            ready.recv().unwrap();
            let waiter = scope.spawn(|| {
                registry.prepare(
                    Path::new("same"),
                    &waiter_cancelled,
                    Timing::quiet(),
                    || panic!("waiter must not prepare"),
                )
            });
            waiter_cancelled.store(true, Ordering::Relaxed);
            assert_eq!(
                waiter.join().unwrap().unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            release.send(()).unwrap();
            assert_eq!(*owner.join().unwrap().unwrap().lock().unwrap(), "retained");
        });
    }
}
