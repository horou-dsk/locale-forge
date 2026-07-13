use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use tempfile::Builder;

pub fn write(path: &Path, contents: &[u8], _private: bool) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let mut temporary = Builder::new()
        .prefix(".locale-forge-")
        .tempfile_in(parent)?;
    temporary.write_all(contents)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;

    #[cfg(unix)]
    if _private {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }

    let temporary_path = temporary.into_temp_path();
    replace(&temporary_path, path)?;

    #[cfg(unix)]
    if _private {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::{iter, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from_wide: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(iter::once(0)).collect();
    // SAFETY: Both buffers are owned, NUL-terminated UTF-16 paths and remain alive for the call.
    let result = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.json");
        fs::write(&path, "old").unwrap();

        write(&path, b"new", false).unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "new");
    }
}
