use super::*;

pub(crate) fn results(root: &Path, id: i64, source: &str) -> io::Result<Vec<CheckResult>> {
    let bundle = load(root, id)?;
    let Some(receipt) = bundle.receipt else {
        return Ok(Vec::new());
    };
    if receipt.source != source
        || receipt.scripts_fingerprint != workspace::fingerprint(&script_snapshot(&bundle.scripts))?
    {
        return Ok(Vec::new());
    }
    let bytes = match fs::read(root.join(format!("task-drafts/{id}.json"))) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let snapshot = workspace::source_snapshot(root, serde_json::from_slice(&bytes)?)?;
    if receipt.fingerprint != workspace::fingerprint(&snapshot)? {
        return Ok(Vec::new());
    }
    Ok(receipt.results)
}
