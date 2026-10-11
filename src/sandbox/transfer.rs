use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const HELPER: &str = include_str!("../../sandbox/source_transfer.py");
const MAX_FILES: usize = 100_000;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    hash: String,
    size: u64,
    mode: u32,
}
type Manifest = BTreeMap<PathBuf, Entry>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delta {
    version: u64,
    basis: Manifest,
    files: Manifest,
    deleted: Vec<PathBuf>,
}

fn manifest(snapshot: &Snapshot) -> Manifest {
    snapshot
        .iter()
        .map(|(path, bytes, mode)| {
            (
                path.clone(),
                Entry {
                    hash: format!("{:x}", Sha256::digest(bytes)),
                    size: bytes.len() as u64,
                    mode: mode & 0o777,
                },
            )
        })
        .collect()
}
fn valid(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.to_str().is_some_and(|path| {
            path.len() <= 4096
                && !path.contains('\0')
                && path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
        })
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn apply(bytes: &[u8], basis: &Snapshot) -> io::Result<Snapshot> {
    let fail = || io::Error::other("Invalid source delta. The previous source is retained.");
    if bytes.len() > EXPORT_LIMIT {
        return Err(fail());
    }
    let mut entries = BTreeMap::new();
    let mut archive = tar::Archive::new(bytes);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !valid(&path)
            || !entry.header().entry_type().is_file()
            || entry.size() > 64 * 1024 * 1024
            || entries.len() > MAX_FILES
            || entry.header().mode()? > 0o777
        {
            return Err(fail());
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        if entries
            .insert(path, (data, entry.header().mode()? & 0o777))
            .is_some()
        {
            return Err(fail());
        }
    }
    let (metadata, _) = entries
        .remove(Path::new("manifest.json"))
        .ok_or_else(fail)?;
    let delta: Delta = serde_json::from_slice(&metadata)?;
    if delta.version != 1
        || delta.basis != manifest(basis)
        || delta.files.len() > MAX_FILES
        || delta.files.keys().any(|p| !valid(p))
        || delta
            .files
            .values()
            .any(|e| e.size > 64 * 1024 * 1024 || e.mode > 0o777 || e.hash.len() != 64)
    {
        return Err(fail());
    }
    let deleted: std::collections::BTreeSet<_> = delta.deleted.iter().cloned().collect();
    let expected: std::collections::BTreeSet<_> = delta
        .basis
        .keys()
        .filter(|p| !delta.files.contains_key(*p))
        .cloned()
        .collect();
    if deleted != expected || deleted.len() != delta.deleted.len() {
        return Err(fail());
    }
    let mut result = Vec::new();
    let mut total = 0usize;
    let basis_files: BTreeMap<_, _> = basis
        .iter()
        .map(|(path, bytes, mode)| (path, (bytes, mode)))
        .collect();
    for (path, expected) in &delta.files {
        let payload_path = Path::new("payload").join(path);
        let (data, mode) = if let Some(payload) = entries.remove(&payload_path) {
            payload
        } else if delta.basis.get(path) == Some(expected) {
            basis_files
                .get(path)
                .map(|(data, mode)| ((*data).clone(), **mode & 0o777))
                .ok_or_else(fail)?
        } else {
            return Err(fail());
        };
        total = total.checked_add(data.len()).ok_or_else(fail)?;
        if total > EXPORT_LIMIT
            || data.len() as u64 != expected.size
            || mode != expected.mode
            || format!("{:x}", Sha256::digest(&data)) != expected.hash
        {
            return Err(fail());
        }
        result.push((path.clone(), data, mode));
    }
    if !entries.is_empty() || manifest(&result) != delta.files {
        return Err(fail());
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn snapshot(
    name: &str,
    root: &Path,
    scope: &session::CommandScope,
    rpc: &mut Rpc,
    prefix: &str,
    basis: Option<&Snapshot>,
    cancelled: &AtomicBool,
) -> io::Result<Snapshot> {
    let empty = Vec::new();
    let basis = basis.unwrap_or(&empty);
    let host = root.join(format!(
        "transfer-{}.json",
        PROCESS.fetch_add(1, Ordering::Relaxed)
    ));
    let input = format!("{prefix}.json");
    let archive = format!("{prefix}.tar");
    let log = host.with_extension("log");
    let result = (|| {
        for attempt in 0..3 {
            // Only a changing tree gets retries. The last retry sends a full verified tree.
            let basis = if attempt == 2 { &empty } else { basis };
            fs::write(&host, serde_json::to_vec(&manifest(basis))?)?;
            private(&host, 0o600)?;
            fs::write(&log, [])?;
            control(
                &["cp", &host.to_string_lossy(), &format!("{name}:{input}")],
                &log,
                cancelled,
                120,
            )?;
            let exported = control(
                &[
                    "exec",
                    name,
                    "/usr/bin/python3",
                    "-c",
                    HELPER,
                    &scope.cwd,
                    &input,
                    &archive,
                ],
                &log,
                cancelled,
                120,
            );
            if let Err(error) = exported {
                let evidence = fs::read(&log).unwrap_or_default();
                let evidence =
                    String::from_utf8_lossy(&evidence[evidence.len().saturating_sub(8192)..]);
                if attempt < 2
                    && evidence.contains("RuntimeError: SOURCE_CHANGED:")
                    && !cancelled.load(Ordering::Relaxed)
                {
                    continue;
                }
                return Err(io::Error::new(
                    error.kind(),
                    format!("{error}\n{}", evidence),
                ));
            }
            let bytes = session::read_guest(rpc, scope, &archive, cancelled)?;
            fs::write(&log, [])?;
            if let Err(error) = control(
                &[
                    "exec",
                    name,
                    "/usr/bin/python3",
                    "-c",
                    HELPER,
                    "--verify",
                    &scope.cwd,
                    &archive,
                ],
                &log,
                cancelled,
                120,
            ) {
                let evidence = fs::read(&log).unwrap_or_default();
                let evidence =
                    String::from_utf8_lossy(&evidence[evidence.len().saturating_sub(8192)..]);
                if attempt < 2
                    && evidence.contains("RuntimeError: SOURCE_CHANGED:")
                    && !cancelled.load(Ordering::Relaxed)
                {
                    continue;
                }
                return Err(io::Error::new(error.kind(), format!("{error}\n{evidence}")));
            }
            return workspace::source_snapshot(root, apply(&bytes, basis)?);
        }
        unreachable!()
    })();
    let _ = fs::remove_file(host);
    let _ = control(
        &["exec", name, "/bin/rm", "-f", &input, &archive],
        &log,
        &AtomicBool::new(false),
        15,
    );
    let _ = fs::remove_file(log);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn delta(work: &Path, basis: &Snapshot) -> Vec<u8> {
        let input = work.parent().unwrap().join("basis.json");
        let archive = work.parent().unwrap().join("delta.tar");
        fs::write(&input, serde_json::to_vec(&manifest(basis)).unwrap()).unwrap();
        let output = Command::new("python3")
            .args(["-c", HELPER])
            .arg(work)
            .arg(&input)
            .arg(&archive)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read(archive).unwrap()
    }
    #[test]
    fn credential_files_remain_excluded_while_templates_are_source() {
        let data = crate::store::test_support::TestData::new();
        let work = data.0.join("work");
        fs::create_dir_all(work.join("nested")).unwrap();
        fs::write(work.join(".env.example"), "EXAMPLE=placeholder").unwrap();
        fs::write(work.join("nested/.env.sample"), "SAMPLE=placeholder").unwrap();
        for path in [".env", "nested/.env.local", ".npmrc", ".netrc", ".pypirc"] {
            fs::write(work.join(path), "private value").unwrap();
        }
        std::os::unix::fs::symlink("../private", work.join(".env.secret")).unwrap();
        assert_eq!(
            apply(&delta(&work, &vec![]), &vec![]).unwrap(),
            vec![
                (
                    PathBuf::from(".env.example"),
                    b"EXAMPLE=placeholder".to_vec(),
                    0o644
                ),
                (
                    PathBuf::from("nested/.env.sample"),
                    b"SAMPLE=placeholder".to_vec(),
                    0o644
                ),
            ]
        );
    }

    #[test]
    fn mutation_after_archive_creation_is_rejected_before_acceptance() {
        let data = crate::store::test_support::TestData::new();
        let work = data.0.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("source"), "before").unwrap();
        let bytes = delta(&work, &vec![]);
        let saved = apply(&bytes, &vec![]).unwrap();
        fs::write(work.join("source"), "after").unwrap();
        let output = Command::new("python3")
            .args(["-c", HELPER, "--verify"])
            .arg(&work)
            .arg(data.0.join("delta.tar"))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("SOURCE_CHANGED: source changed during transfer")
        );
        assert_eq!(saved[0].1, b"before");
        let fresh = apply(&delta(&work, &saved), &saved).unwrap();
        assert_eq!(fresh[0].1, b"after");
    }

    #[test]
    fn archive_paths_links_and_extra_payloads_are_hard_errors() {
        let basis = vec![];
        let metadata =
            serde_json::to_vec(&json!({"version":1,"basis":{},"files":{},"deleted":[]})).unwrap();
        for (path, kind) in [
            ("payload/../escape", tar::EntryType::Regular),
            ("payload/link", tar::EntryType::Symlink),
            ("payload/extra", tar::EntryType::Regular),
        ] {
            let mut tar = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(metadata.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            tar.append_data(&mut header, "manifest.json", metadata.as_slice())
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_mode(0o600);
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            tar.append(&header, &[][..]).unwrap();
            assert!(apply(&tar.into_inner().unwrap(), &basis).is_err(), "{path}");
        }
    }

    #[test]
    fn verified_deltas_preserve_binary_modes_deletes_and_empty_source() {
        use std::os::unix::fs::PermissionsExt;
        let data = crate::store::test_support::TestData::new();
        let work = data.0.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("one"), vec![0u8; 128 * 1024]).unwrap();
        fs::write(work.join("delete"), b"old").unwrap();
        let basis = apply(&delta(&work, &vec![]), &vec![]).unwrap();
        let unchanged = delta(&work, &basis);
        assert!(unchanged.len() < 16 * 1024);
        assert_eq!(apply(&unchanged, &basis).unwrap(), basis);
        fs::write(work.join("one"), [0, 255, 128]).unwrap();
        fs::set_permissions(work.join("one"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(work.join("delete")).unwrap();
        let updated = apply(&delta(&work, &basis), &basis).unwrap();
        assert_eq!(
            updated,
            vec![(PathBuf::from("one"), vec![0, 255, 128], 0o755)]
        );
        fs::remove_file(work.join("one")).unwrap();
        assert!(apply(&delta(&work, &updated), &updated).unwrap().is_empty());
        let mut corrupt = unchanged.clone();
        let at = corrupt
            .windows(64)
            .position(|bytes| bytes.iter().all(u8::is_ascii_hexdigit))
            .unwrap();
        corrupt[at] = b'z';
        assert!(apply(&corrupt, &basis).is_err());
        assert_eq!(apply(&unchanged, &basis).unwrap(), basis);
        std::os::unix::fs::symlink("../basis.json", work.join("link")).unwrap();
        let input = data.0.join("basis.json");
        let result = Command::new("python3")
            .args(["-c", HELPER])
            .arg(&work)
            .arg(input)
            .arg(data.0.join("bad.tar"))
            .output()
            .unwrap();
        assert!(!result.status.success());
        fs::remove_file(work.join("link")).unwrap();
        fs::write(work.join("original"), "linked source").unwrap();
        fs::hard_link(work.join("original"), work.join("hardlink")).unwrap();
        let result = Command::new("python3")
            .args(["-c", HELPER])
            .arg(&work)
            .arg(data.0.join("basis.json"))
            .arg(data.0.join("bad.tar"))
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("without links"));
    }
}
