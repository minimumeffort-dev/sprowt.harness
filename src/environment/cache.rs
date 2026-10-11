use crate::sandbox::Sandbox;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) const POLICY: &str = include_str!("../../sandbox/environment_cache.py");
const DIRECTORY: &str = "/opt/sprowt-environments";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Use {
    #[default]
    Miss,
    Retained,
    Restored {
        bytes: u64,
        dependencies: bool,
    },
    Rejected {
        reason: String,
    },
    Published,
}

impl Use {
    pub(super) fn dependencies(&self) -> bool {
        matches!(
            self,
            Self::Restored {
                dependencies: true,
                ..
            }
        )
    }
    pub(super) fn summary(&self) -> String {
        match self {
            Self::Miss => "No matching prepared assets.".into(),
            Self::Retained => {
                "Existing preparation: capability probes rerun in the same worker HOME.".into()
            }
            Self::Restored {
                bytes,
                dependencies,
            } => format!(
                "Prepared assets: copied {bytes} bytes into private paths{}.",
                if *dependencies {
                    "; locked dependencies restored"
                } else {
                    "; setup still runs"
                }
            ),
            Self::Rejected { reason } => {
                format!("Prepared assets unavailable; fresh setup: {reason}")
            }
            Self::Published => "Prepared assets saved for matching tasks.".into(),
        }
    }
}

pub(super) fn operation(
    vm: &mut Sandbox,
    action: &str,
    key: &str,
    dependencies: bool,
    cancelled: &AtomicBool,
) -> io::Result<Use> {
    let result_path = format!(
        "/opt/sprowt-transfer/environment-cache-{}.json",
        vm.runtime_home().rsplit('/').next().unwrap_or("controller")
    );
    let arguments = json!({"action":action,"key":key,"dependencies":dependencies,
        "home":vm.runtime_home(),"workspace":vm.task_folder(),"cache":DIRECTORY,"result":result_path});
    let result = (|| {
        vm.guest(&["/bin/rm", "-f", &result_path], cancelled)?;
        vm.guest(
            &[
                "/usr/bin/timeout",
                "90",
                "/usr/bin/python3",
                "-c",
                POLICY,
                &arguments.to_string(),
            ],
            cancelled,
        )?;
        let bytes = vm.read_guest(&result_path, cancelled)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    })();
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Environment cache stopped.",
        ));
    }
    match result {
        Ok(result) => Ok(result),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
        Err(error) => Ok(Use::Rejected {
            reason: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests;
