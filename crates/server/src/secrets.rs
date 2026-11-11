//! Small secrets kept as files in the data folder, readable by this user only.

use std::path::Path;

pub fn read(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Writes the secret to a file that is this user's only from the moment it exists (not
/// readable first and locked after), then puts it in place, so a reader never sees half.
pub fn write(path: &Path, value: &str) -> std::io::Result<()> {
    use std::io::Write;
    let part = path.with_extension("part");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&part)?;
    // A leftover part file from a crash keeps the mode it had: lock it either way.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&part, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_secret_and_reads_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-key");
        write(&path, "first").unwrap();
        write(&path, "second").unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some("second"));
        assert!(!dir.path().join("host-key.part").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
