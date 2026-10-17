//! Files that hold secrets or trust decisions: written `0600` inside `0700`
//! directories, never through a symlink, and only read back when they are
//! ours and nobody else can write them.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};
use tracing::info;

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Is `gid` this user's own private group: named after them, with nobody
/// else in it? (The default on most distributions, where the umask is often
/// 002, so files come out group-writable without anyone else being able to
/// write them.)
fn is_private_group(gid: u32) -> bool {
    use std::ffi::CStr;
    // SAFETY: the reentrant lookups write only into the buffers we pass, and
    // the strings we read point into those buffers.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut pbuf = vec![0 as libc::c_char; 16 * 1024];
        let mut pres = std::ptr::null_mut();
        if libc::getpwuid_r(euid(), &mut pwd, pbuf.as_mut_ptr(), pbuf.len(), &mut pres) != 0 || pres.is_null() {
            return false;
        }
        let user = CStr::from_ptr(pwd.pw_name);
        let mut grp: libc::group = std::mem::zeroed();
        let mut gbuf = vec![0 as libc::c_char; 64 * 1024];
        let mut gres = std::ptr::null_mut();
        if libc::getgrgid_r(gid, &mut grp, gbuf.as_mut_ptr(), gbuf.len(), &mut gres) != 0 || gres.is_null() {
            return false;
        }
        if CStr::from_ptr(grp.gr_name) != user {
            return false;
        }
        let mut member = grp.gr_mem;
        while !member.is_null() && !(*member).is_null() {
            if CStr::from_ptr(*member) != user {
                return false;
            }
            member = member.add(1);
        }
        true
    }
}

/// Create `dir` (and parents) and make the leaf `0700`.
pub fn private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    if meta.uid() != euid() {
        bail!("{} belongs to another user", dir.display());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restricting {}", dir.display()))?;
    }
    Ok(())
}

/// Replace `path` atomically with `bytes`, mode `0600`, in a private directory.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("refusing to write {}: it is a symlink", path.display());
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// Read a file written by [`write_private`]. `Ok(None)` means it doesn't
/// exist (first run); anything unexpected is an error, never "empty".
pub fn read_private(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut f = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            bail!("refusing to read {}: it is a symlink", path.display())
        }
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    let meta = f.metadata()?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if meta.uid() != euid() {
        bail!("{} belongs to another user", path.display());
    }
    let mode = meta.permissions().mode() & 0o777;
    let others_can_write = mode & 0o002 != 0 || (mode & 0o020 != 0 && !is_private_group(meta.gid()));
    if others_can_write {
        bail!("{} can be modified by other users; check it, then fix it with chmod 600", path.display());
    }
    if mode & 0o077 != 0 {
        // Ours, and nobody else could write it (an older version wrote it
        // with the default umask): make it private now.
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        info!("made {} private (was {mode:o})", path.display());
    }
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(bytes))
}

/// Like [`read_private`], as UTF-8 text.
pub fn read_private_text(path: &Path) -> Result<Option<String>> {
    match read_private(path)? {
        Some(b) => String::from_utf8(b)
            .map(Some)
            .with_context(|| format!("{} is not text", path.display())),
        None => Ok(None),
    }
}

/// Delete `path` if it exists.
pub fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("linuxplay-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn private_files_are_0600_in_0700_dirs() {
        let dir = scratch("perm");
        let path = dir.join("sub").join("secret");
        write_private(&path, b"hello").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(read_private(&path).unwrap().unwrap(), b"hello");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_is_none_but_writable_by_others_is_an_error() {
        let dir = scratch("other");
        assert!(read_private(&dir.join("nope")).unwrap().is_none());
        let path = dir.join("f");
        write_private(&path, b"x").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_private(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_from_a_loose_umask_are_tightened_only_when_safe() {
        let dir = scratch("umask");
        let path = dir.join("f");
        write_private(&path, b"x").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private(&path).is_ok());
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        let gid = fs::metadata(&path).unwrap().gid();
        let r = read_private(&path);
        if is_private_group(gid) {
            assert!(r.is_ok());
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        } else {
            assert!(r.is_err(), "a group others belong to could have changed it");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn symlinks_are_refused() {
        let dir = scratch("link");
        private_dir(&dir).unwrap();
        let target = dir.join("target");
        fs::write(&target, b"x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_private(&link).is_err());
        assert!(write_private(&link, b"y").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"x");
        fs::remove_dir_all(&dir).unwrap();
    }
}
