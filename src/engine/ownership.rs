use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    path::Path,
};

#[derive(Debug)]
pub(crate) struct Ownership(File);

impl Drop for Ownership {
    fn drop(&mut self) {
        // Forked Git commands can briefly inherit this descriptor before exec.
        let _ = self.0.unlock();
    }
}

pub fn acquire(database: &Path, project: &Path) -> io::Result<Ownership> {
    let directory = database
        .parent()
        .ok_or_else(|| io::Error::other("Invalid data directory."))?;
    std::fs::create_dir_all(directory)?;
    let identity = format!(
        "{:x}",
        Sha256::digest(project.as_os_str().as_encoded_bytes())
    );
    let path = directory.join(format!("controller-{identity}.lock"));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(io::ErrorKind::WouldBlock,
            "This project is already open in another Sprowt CLI or desktop window. Close it there, then reopen it here."),
        TryLockError::Error(error) => error,
    })?;
    Ok(Ownership(file))
}
