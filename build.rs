use std::{env, fs, io, path::Path};

fn files(root: &Path, paths: &mut Vec<std::path::PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files(&entry.path(), paths)?;
        } else if kind.is_file() {
            paths.push(entry.path());
        } else {
            return Err(io::Error::other(
                "Skill bundles must contain regular files.",
            ));
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let root = std::path::PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let skills = root.join("skills");
    println!("cargo:rerun-if-changed=skills");
    let mut paths = Vec::new();
    files(&skills, &mut paths)?;
    paths.sort();
    // This names the cache; runtime validation compares the full embedded bytes.
    let mut version = 0xcbf29ce484222325_u64;
    let mut source = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for path in paths {
        let relative = path.strip_prefix(&skills).unwrap().to_str().unwrap();
        let bytes = fs::read(&path)?;
        for byte in relative.bytes().chain([0]).chain(bytes).chain([0]) {
            version = (version ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        source.push_str(&format!("({relative:?}, include_bytes!({path:?})),\n"));
    }
    source.push_str(&format!(
        "];\npub const VERSION: &str = \"{version:016x}\";\n"
    ));
    fs::write(
        std::path::PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("skills.rs"),
        source,
    )
}
