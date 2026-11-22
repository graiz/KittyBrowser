//! Private files and folders: kb's data directory, and what kb leaves in
//! the temporary directory. A page kb shows can be signed in, so its frames,
//! screenshots, profile and the control socket are for the user alone.

use std::io::{self, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Make `dir`, a folder of the user's own such as ~/.kittybrowser, if it
/// isn't there, readable only by the user, and tighten it to that if an
/// earlier kb left it open to others. A link in its place is followed, as
/// ssh follows one at ~/.ssh, and the folder it leads to must be the
/// user's. Some filesystems hold no Unix modes (some network and FUSE
/// mounts); there kb runs as it always has, and kb doctor says the folder
/// isn't private.
pub fn private_dir(dir: &Path) -> Result<(), String> {
    make_private(dir, true)
}

/// Like `private_dir`, for a folder in a directory others share, such as
/// /tmp, where whoever put a link or a folder there first would choose
/// where kb's private files go, or could read them: a link isn't followed.
pub fn private_shared_dir(dir: &Path) -> Result<(), String> {
    make_private(dir, false)
}

fn make_private(dir: &Path, follow: bool) -> Result<(), String> {
    let fail = |e: io::Error| format!("making {} private: {e}", dir.display());
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(fail(e)),
        _ => {}
    }
    let flags = if follow { libc::O_DIRECTORY } else { libc::O_DIRECTORY | libc::O_NOFOLLOW };
    let opened = std::fs::OpenOptions::new().read(true).custom_flags(flags).open(dir);
    let folder = match opened {
        Ok(folder) => folder,
        Err(_) if !follow && std::fs::symlink_metadata(dir).is_ok_and(|file| file.file_type().is_symlink()) => {
            return Err(format!("{} is a symbolic link, which kb won't follow in a shared folder", dir.display()))
        }
        Err(e) => return Err(fail(e)),
    };
    let file = folder.metadata().map_err(fail)?;
    let home = std::env::var_os("HOME").and_then(|home| std::fs::metadata(home).ok()).map(|home| home.uid());
    if !trusted(file.uid(), unsafe { libc::geteuid() }, home) {
        return Err(format!("{} belongs to another user, so kb won't keep private files in it", dir.display()));
    }
    if file.mode() & 0o777 != 0o700 {
        let _ = folder.set_permissions(std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Whether kb, running as user `me`, keeps private files in a folder
/// `owner` made: its own, and run as root with HOME kept (sudo kb), those
/// of HOME's owner, the user who ran it.
fn trusted(owner: u32, me: u32, home: Option<u32>) -> bool {
    owner == me || (me == 0 && home == Some(owner))
}

/// A new folder in the temporary directory, named `prefix` and six
/// characters no one can guess, that only the user can open.
pub fn private_temp_dir(prefix: &str) -> io::Result<PathBuf> {
    let template = std::env::temp_dir().join(format!("{prefix}XXXXXX"));
    let mut template = std::ffi::CString::new(template.into_os_string().into_vec())?.into_bytes_with_nul();
    // mkdtemp makes it 0700, whatever the umask.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    template.pop();
    Ok(std::ffi::OsString::from_vec(template).into())
}

/// Write `bytes` to a new file at `path` with `mode` (less the umask),
/// failing if anything, a link included, is there already, and leaving
/// nothing behind if the write fails.
pub fn write_new(path: &Path, bytes: &[u8], mode: u32) -> io::Result<std::fs::File> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(path)?;
    match file.write_all(bytes) {
        Ok(()) => Ok(file),
        Err(e) => {
            let _ = std::fs::remove_file(path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn whose_folders_are_trusted() {
        assert!(trusted(501, 501, Some(501)));
        assert!(!trusted(502, 501, Some(502)));
        // sudo kb, with HOME kept: the user's own folders, and root's.
        assert!(trusted(501, 0, Some(501)));
        assert!(trusted(0, 0, Some(501)));
        assert!(!trusted(502, 0, Some(501)));
        assert!(!trusted(502, 0, None));
    }

    #[test]
    fn data_folders_are_private() {
        let base = std::env::temp_dir().join(format!("kb-private-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();

        // A new one is made 0700.
        let new = base.join("new");
        private_dir(&new).unwrap();
        assert_eq!(mode(&new), 0o700);
        // One an earlier kb left 0755 is tightened, and its contents kept.
        let old = base.join("old");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("history"), "kept").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&old).unwrap();
        assert_eq!(mode(&old), 0o700);
        assert_eq!(std::fs::read_to_string(old.join("history")).unwrap(), "kept");
        // A link is followed, and the folder it leads to made private, as
        // for ~/.kittybrowser kept in a dotfiles folder.
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        private_dir(&link).unwrap();
        assert_eq!(mode(&elsewhere), 0o700);
        // In a shared folder, a link isn't followed, and what it points to
        // is left alone.
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = private_shared_dir(&link).unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        assert_eq!(mode(&elsewhere), 0o755);
        private_shared_dir(&base.join("shared")).unwrap();
        assert_eq!(mode(&base.join("shared")), 0o700);
        // Nor is a folder another user made, as one could in /tmp. Root
        // owns /; as root, the check would chmod it instead.
        if unsafe { libc::geteuid() } != 0 {
            let error = private_dir(Path::new("/")).unwrap_err();
            assert!(error.contains("belongs to another user"), "{error}");
            // Nor one a link leads to.
            let to_root = base.join("to-root");
            std::os::unix::fs::symlink("/", &to_root).unwrap();
            let error = private_dir(&to_root).unwrap_err();
            assert!(error.contains("belongs to another user"), "{error}");
        }
        // Nor is a file in its place taken for one.
        let file = base.join("file");
        std::fs::write(&file, "").unwrap();
        assert!(private_dir(&file).is_err());

        std::fs::remove_dir_all(&base).unwrap();
    }
}
