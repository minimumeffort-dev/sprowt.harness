use super::*;
use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Allocation {
    pub cpus: u64,
    pub memory_gib: u64,
}

impl Allocation {
    pub const SMALL: Self = Self {
        cpus: 2,
        memory_gib: 2,
    };

    pub fn from_configuration(configuration: &Value) -> io::Result<Self> {
        let resources = &configuration["resources"];
        Ok(Self {
            cpus: resources["cpus"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| io::Error::other("VM CPU allocation is missing."))?,
            memory_gib: resources["memoryInBytes"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| io::Error::other("VM memory allocation is missing."))?
                .div_ceil(1 << 30),
        })
    }
}

fn profile(budget: Allocation, memory: Option<u64>) -> io::Result<Allocation> {
    let ordinary = if budget.cpus >= 4 && budget.memory_gib >= 4 {
        Allocation {
            cpus: 4,
            memory_gib: 4,
        }
    } else {
        Allocation::SMALL
    };
    let requested = Allocation {
        memory_gib: memory.unwrap_or(ordinary.memory_gib),
        ..ordinary
    };
    if requested.cpus > budget.cpus || requested.memory_gib > budget.memory_gib {
        return Err(io::Error::other(
            "This VM allocation exceeds half the host CPUs or memory. Use a host with more capacity.",
        ));
    }
    Ok(requested)
}

fn occupied(list: &[Value]) -> io::Result<Allocation> {
    let mut used = Allocation {
        cpus: 0,
        memory_gib: 0,
    };
    for vm in list.iter().filter(|vm| {
        vm["status"]["state"] == "running"
            && vm["id"].as_str().is_some_and(|name| {
                name.starts_with("sprowt-") && vm["configuration"]["labels"][OWNER] == name
            })
    }) {
        let allocation = Allocation::from_configuration(&vm["configuration"])?;
        used.cpus += allocation.cpus;
        used.memory_gib += allocation.memory_gib;
    }
    Ok(used)
}

fn fits(budget: Allocation, used: Allocation, pending: Allocation) -> bool {
    used.cpus.saturating_add(pending.cpus) <= budget.cpus
        && used.memory_gib.saturating_add(pending.memory_gib) <= budget.memory_gib
}

fn host_budget() -> io::Result<Allocation> {
    let memory = Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output()?;
    checked(&memory)?;
    let memory: u64 = String::from_utf8_lossy(&memory.stdout)
        .trim()
        .parse()
        .map_err(io::Error::other)?;
    Ok(Allocation {
        cpus: std::thread::available_parallelism()?.get() as u64 / 2,
        memory_gib: memory / (1 << 30) / 2,
    })
}

fn admission_lock(path: &Path, cancelled: &AtomicBool) -> io::Result<fs::File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    private(path, 0o600)?;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "VM admission stopped.",
            ));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

// The lock is the pending reservation until create/start confirms a running VM.
pub(super) fn admit(
    existing: Option<Allocation>,
    memory: Option<u64>,
    cancelled: &AtomicBool,
    timing: Timing<'_>,
) -> io::Result<(Allocation, fs::File)> {
    let budget = host_budget()?;
    let allocation = existing.map_or_else(|| profile(budget, memory), Ok)?;
    if !fits(
        budget,
        Allocation {
            cpus: 0,
            memory_gib: 0,
        },
        allocation,
    ) {
        return Err(io::Error::other(
            "The saved VM allocation exceeds this host's resource budget; use a host with more capacity.",
        ));
    }
    let directory = directories::ProjectDirs::from("", "", "sprowt-harness")
        .ok_or_else(|| io::Error::other("Cannot locate VM admission state."))?
        .data_local_dir()
        .to_owned();
    fs::create_dir_all(&directory)?;
    timing.measure(Phase::SlotWait, || {
        loop {
            let guard = admission_lock(&directory.join("vm-admission.lock"), cancelled)?;
            let output = container()
                .args(["list", "--all", "--format", "json"])
                .output()?;
            checked(&output)?;
            let list: Vec<Value> = serde_json::from_slice(&output.stdout)?;
            if fits(budget, occupied(&list)?, allocation) {
                return Ok((allocation, guard));
            }
            drop(guard);
            thread::sleep(Duration::from_millis(100));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_hosts_fall_back_but_occupied_hosts_wait_for_the_ordinary_profile() {
        let small = Allocation {
            cpus: 2,
            memory_gib: 4,
        };
        assert_eq!(profile(small, None).unwrap(), Allocation::SMALL);
        let budget = Allocation {
            cpus: 14,
            memory_gib: 48,
        };
        let requested = profile(budget, None).unwrap();
        assert_eq!(
            requested,
            Allocation {
                cpus: 4,
                memory_gib: 4
            }
        );
        assert!(!fits(
            budget,
            Allocation {
                cpus: 12,
                memory_gib: 12
            },
            requested
        ));
        assert!(fits(budget, Allocation::SMALL, requested));
        assert_eq!(profile(budget, Some(8)).unwrap().memory_gib, 8);
        assert!(profile(small, Some(8)).is_err());
    }

    #[test]
    fn running_legacy_allocations_count_and_unlabelled_or_stopped_vms_do_not() {
        let vm = |name: &str, state: &str, owned: bool, cpus: u64, memory: u64| {
            json!({
            "id":name,"status":{"state":state},"configuration":{
                "labels":{OWNER:if owned {name} else {"other"}},
                "resources":{"cpus":cpus,"memoryInBytes":memory * (1 << 30)}}})
        };
        let list = [
            vm("sprowt-legacy", "running", true, 2, 2),
            vm("sprowt-new", "running", true, 4, 8),
            vm("sprowt-stopped", "stopped", true, 4, 8),
            vm("buildkit", "running", false, 2, 2),
        ];
        assert_eq!(
            occupied(&list).unwrap(),
            Allocation {
                cpus: 6,
                memory_gib: 10
            }
        );
    }

    #[test]
    fn admission_lock_wait_is_cancellable_and_releases_its_reservation() {
        let data = crate::store::test_support::TestData::new();
        fs::create_dir_all(&data.0).unwrap();
        let path = data.0.join("admission.lock");
        let held = admission_lock(&path, &AtomicBool::new(false)).unwrap();
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            let (started, ready) = std::sync::mpsc::channel();
            let path = &path;
            let stopped = &cancelled;
            let waiter = scope.spawn(move || {
                started.send(()).unwrap();
                admission_lock(path, stopped)
            });
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
            thread::sleep(Duration::from_millis(50));
            assert!(!waiter.is_finished());
            cancelled.store(true, Ordering::Relaxed);
            assert_eq!(
                waiter.join().unwrap().unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
        });
        drop(held);
        let released = admission_lock(&path, &AtomicBool::new(false)).unwrap();
        assert!(released.metadata().unwrap().is_file());
    }
}
