use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(crate) fn user_state_root() -> io::Result<PathBuf> {
    if let Some(root) = std::env::var_os("HUMANIZE_STATE_ROOT").filter(|value| !value.is_empty()) {
        return validate_explicit_state_path("HUMANIZE_STATE_ROOT", PathBuf::from(root));
    }
    if let Some(root) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return validate_explicit_state_path(
            "XDG_STATE_HOME",
            PathBuf::from(root).join("humanize"),
        );
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is required"))?;
    validate_explicit_state_path("HOME", PathBuf::from(home).join(".humanize"))
}

pub(crate) fn private_runtime_root() -> io::Result<PathBuf> {
    user_state_root().map(|root| root.join("runtime"))
}

pub(crate) fn private_run_root(runtime_root: &Path, public_run_root: &Path) -> io::Result<PathBuf> {
    let public_run_root = resolved_path(public_run_root)?;
    Ok(runtime_root.join(format!(
        "r{:016x}",
        stable_hash(&public_run_root.to_string_lossy())
    )))
}

pub(crate) fn legacy_private_run_root(runtime_root: &Path, public_run_root: &Path) -> PathBuf {
    let public_run_root =
        lexical_absolute(public_run_root).unwrap_or_else(|_| public_run_root.to_path_buf());
    runtime_root.join(format!(
        "r{:016x}",
        stable_hash(&public_run_root.to_string_lossy())
    ))
}

fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(crate) fn validate_explicit_state_path(variable: &str, path: PathBuf) -> io::Result<PathBuf> {
    let project_root = project_root(&std::env::current_dir()?)?;
    let candidate = resolved_path(&path)?;
    if candidate == project_root || candidate.starts_with(&project_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{variable} must not point at the current project directory or one of its descendants"
            ),
        ));
    }
    Ok(path)
}

fn project_root(working_directory: &Path) -> io::Result<PathBuf> {
    let working_directory = resolved_path(working_directory)?;
    let mut directory = working_directory.as_path();
    loop {
        match std::fs::symlink_metadata(directory.join(".git")) {
            Ok(metadata) if metadata.file_type().is_dir() || metadata.file_type().is_file() => {
                return Ok(directory.to_path_buf());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let Some(parent) = directory.parent() else {
            return Ok(working_directory);
        };
        directory = parent;
    }
}

pub(crate) fn resolved_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = lexical_absolute(path)?;
    let mut existing = absolute.as_path();
    let mut suffix = Vec::<OsString>::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(mut canonical) => {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(error);
                };
                suffix.push(name.to_os_string());
                existing = existing.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn lexical_absolute(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported state path prefix",
                ));
            }
        }
    }
    Ok(normalized)
}
