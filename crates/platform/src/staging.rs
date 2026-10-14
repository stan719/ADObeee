//! Staging files for atomic saves: a new, unpredictably named file beside the target, written and
//! then renamed over it. [`write_atomically`] does the whole save; [`StagedFile`] splits it in
//! two, so several files can all be written before any of them replaces what was there; and
//! [`create_staging`] only makes the file, leaving writing, renaming and cleaning up to the caller.

use std::path::{Path, PathBuf};

/// How many staging names [`create_staging`] tries. A random 64-bit name is only taken if someone
/// put a file there on purpose, so running out means refusing, not trying harder.
pub const STAGING_ATTEMPTS: usize = 16;

/// Where the suffix goes in a staging name. Each caller keeps the form it has always used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingName {
    /// `.{name}.{suffix}.pdfcraft-tmp`, used by the automation tools and the CLI.
    SuffixThenTag,
    /// `.{name}.pdfcraft-{suffix}.tmp`, used by the desktop app's save.
    TagThenSuffix,
}

/// Create a new, empty staging file in `dir` for the file `name`, one name per suffix. It is
/// opened with `create_new`, which fails if anything already has the name (a file, a hard link,
/// a symbolic link even when dangling, a folder), on Windows as everywhere else; such a name is
/// skipped, never opened, so a file planted at the staging path can't receive or redirect the
/// write. Pass [`staging_suffixes`]: fixed suffixes are for tests, and a predictable name can be
/// taken in advance to refuse every save.
pub fn create_staging(
    dir: &Path,
    name: &str,
    form: StagingName,
    suffixes: impl IntoIterator<Item = u64>,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    // At most 128 bytes of the target's name, cut between characters, so the staging name fits
    // the 255-byte (Linux, macOS) and 255-unit (Windows) limits however long that name is.
    let mut stem = String::new();
    for c in name.chars() {
        if stem.len() + c.len_utf8() > 128 {
            break;
        }
        stem.push(c);
    }
    for suffix in suffixes.into_iter().take(STAGING_ATTEMPTS) {
        let tmp = dir.join(match form {
            StagingName::SuffixThenTag => format!(".{stem}.{suffix:016x}.pdfcraft-tmp"),
            StagingName::TagThenSuffix => format!(".{stem}.pdfcraft-{suffix:016x}.tmp"),
        });
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Windows reports a folder at the name as "access denied"; it is taken all the same.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && tmp.symlink_metadata().is_ok() => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "every temporary file name tried is taken"))
}

/// Unpredictable staging-name suffixes, so a name can't be planted in advance. Each is the
/// standard library's keyed hash of a counter (`RandomState::hash_one`, currently SipHash-1-3),
/// keyed from the operating system's random source. That is all a staging name needs, but it is
/// not a CSPRNG: don't use these where a value must stay secret or unguessable after it has been
/// seen, such as a token or a key.
pub fn staging_suffixes() -> impl Iterator<Item = u64> {
    use std::hash::BuildHasher;
    let state = std::hash::RandomState::new();
    (0u64..).map(move |i| state.hash_one(i))
}

/// Replace `target` with `bytes` so that a crash, a full disk or a failed flush leaves either the
/// old file or the new one, never a partly written one: written beside it, flushed to the disk
/// and renamed over it. A folder at `target` is refused, not replaced.
pub fn write_atomically(target: &Path, bytes: &[u8], form: StagingName) -> std::io::Result<()> {
    StagedFile::write(target, bytes, form)?.commit()
}

/// A file written and flushed beside its target, waiting to replace it. Dropping it without
/// [`commit`](Self::commit) removes it, so a save that fails part way leaves only what was there.
#[derive(Debug)]
#[must_use = "a staged file is removed when dropped; call commit to replace the target"]
pub struct StagedFile {
    tmp: PathBuf,
    target: PathBuf,
    committed: bool,
}

impl StagedFile {
    /// Write `bytes` to a new staging file beside `target` and flush it to the disk. `target` is
    /// not touched until [`commit`](Self::commit).
    pub fn write(target: &Path, bytes: &[u8], form: StagingName) -> std::io::Result<Self> {
        Self::write_with(target, bytes, form, staging_suffixes(), std::fs::File::sync_all)
    }

    /// [`write`](Self::write), trying the staging names that `suffixes` give and flushing with
    /// `sync_all` (the standard library's outside tests, a failing one in them).
    fn write_with(
        target: &Path,
        bytes: &[u8],
        form: StagingName,
        suffixes: impl IntoIterator<Item = u64>,
        sync_all: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
    ) -> std::io::Result<Self> {
        use std::io::Write;
        // The rename would fail on a folder too, but only after earlier files of a set had
        // replaced theirs; refusing here keeps the whole set untouched.
        if target.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::IsADirectory, "is a folder, not a file"));
        }
        let name = target.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file name"))?;
        let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let (tmp, mut file) = create_staging(dir, &name.to_string_lossy(), form, suffixes)?;
        // From here on, an early return drops `staged`, which removes the staging file.
        let staged = Self { tmp, target: target.to_path_buf(), committed: false };
        file.write_all(bytes)?;
        sync_staged(&file, sync_all)?;
        // Closed before the rename: Windows can't rename a file that is still open.
        drop(file);
        Ok(staged)
    }

    /// Rename the staging file over the target. If that fails, the target is as it was and the
    /// staging file is removed.
    pub fn commit(mut self) -> std::io::Result<()> {
        std::fs::rename(&self.tmp, &self.target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// Flush the staging file to the disk before it replaces the original. On macOS `sync_all` asks
/// for a full flush (`F_FULLFSYNC`), which SMB and FUSE volumes, such as a mounted Cryptomator
/// vault, refuse as unsupported: there a plain `fsync` is the strongest flush the volume offers.
/// Any other failure still fails the save.
fn sync_staged(f: &std::fs::File, sync_all: impl FnOnce(&std::fs::File) -> std::io::Result<()>) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use rustix::io::Errno;
        match sync_all(f) {
            Err(e) if matches!(Errno::from_io_error(&e), Some(Errno::NOTSUP | Errno::NOTTY)) => Ok(rustix::fs::fsync(f)?),
            other => other,
        }
    }
    #[cfg(not(target_os = "macos"))]
    sync_all(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty folder for one test.
    fn test_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdfcraft-platform-staging-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_name(p: &Path) -> String {
        p.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// The callers' tests plant files at these exact names; if a name changed, those tests would
    /// plant where nothing writes and pass without testing anything.
    #[test]
    fn each_form_keeps_its_exact_name() {
        let dir = test_dir("forms");
        let (tools, _) = create_staging(&dir, "out.pdf", StagingName::SuffixThenTag, [0xab]).unwrap();
        assert_eq!(file_name(&tools), ".out.pdf.00000000000000ab.pdfcraft-tmp");
        let (desktop, _) = create_staging(&dir, "out.pdf", StagingName::TagThenSuffix, [0xab]).unwrap();
        assert_eq!(file_name(&desktop), ".out.pdf.pdfcraft-00000000000000ab.tmp");
        assert_eq!(std::fs::read(&tools).unwrap(), b"", "a new, empty file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_name_is_cut_to_128_bytes_between_characters() {
        let dir = test_dir("long");
        // One byte, then four-byte characters: the 128-byte limit falls inside the 32nd one.
        let name = format!("a{}.pdf", "\u{1F600}".repeat(40));
        let (tmp, _) = create_staging(&dir, &name, StagingName::SuffixThenTag, [1]).unwrap();
        assert_eq!(file_name(&tmp), format!(".a{}.0000000000000001.pdfcraft-tmp", "\u{1F600}".repeat(31)));
        // One-byte characters: exactly 128 of them are kept.
        let (tmp, _) = create_staging(&dir, &"x".repeat(200), StagingName::SuffixThenTag, [2]).unwrap();
        assert_eq!(file_name(&tmp), format!(".{}.0000000000000002.pdfcraft-tmp", "x".repeat(128)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_out_of_suffixes_refuses_and_creates_nothing() {
        let dir = test_dir("empty");
        let e = create_staging(&dir, "out.pdf", StagingName::TagThenSuffix, []).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_first_sixteen_names_are_tried() {
        let dir = test_dir("cap");
        let name = |s: u64| dir.join(format!(".out.pdf.{s:016x}.pdfcraft-tmp"));
        for s in 1..=STAGING_ATTEMPTS as u64 {
            std::fs::write(name(s), "PLANTED").unwrap();
        }
        assert!(create_staging(&dir, "out.pdf", StagingName::SuffixThenTag, 1..).is_err());
        assert!(!name(STAGING_ATTEMPTS as u64 + 1).exists(), "a 17th name was tried");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The desktop app's staging name for `out.pdf` with `suffix`.
    fn staged(dir: &Path, suffix: u64) -> PathBuf {
        dir.join(format!(".out.pdf.pdfcraft-{suffix:016x}.tmp"))
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    /// The names in a folder: what a test can see was left behind.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    /// [`write_atomically`] with fixed staging names.
    fn write_with(target: &Path, bytes: &[u8], suffixes: impl IntoIterator<Item = u64>) -> std::io::Result<()> {
        StagedFile::write_with(target, bytes, StagingName::TagThenSuffix, suffixes, std::fs::File::sync_all)?.commit()
    }

    /// A symbolic link to a file, where the system allows one (Windows needs Developer Mode or an
    /// administrator for it).
    fn file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(std::io::Error::other(format!("no symbolic links here: {} {}", target.display(), link.display())))
        }
    }

    #[test]
    fn saving_never_writes_through_a_file_planted_at_the_staging_name() {
        let dir = test_dir("planted");
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "PRECIOUS").unwrap();
        let target = dir.join("out.pdf");
        // A hard link needs no privileges on any system, and writing to it writes to `outside`.
        std::fs::hard_link(&outside, staged(&dir, 1)).unwrap();
        std::fs::write(staged(&dir, 2), "PLANTED").unwrap();
        write_with(&target, b"NEW", [1, 2, 3]).unwrap();
        assert_eq!(read(&target), "NEW");
        assert_eq!(read(&outside), "PRECIOUS");
        assert_eq!(read(&staged(&dir, 1)), "PRECIOUS");
        assert_eq!(read(&staged(&dir, 2)), "PLANTED");
        assert!(!staged(&dir, 3).exists(), "the staging file was renamed into place");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_never_writes_through_a_symbolic_link_at_the_staging_name() {
        let dir = test_dir("symlink");
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "PRECIOUS").unwrap();
        let target = dir.join("out.pdf");
        if let Err(e) = file_symlink(&outside, &staged(&dir, 1)) {
            eprintln!("symbolic links not checked: {e}");
            return;
        }
        write_with(&target, b"NEW", [1, 2]).unwrap();
        assert_eq!(read(&target), "NEW");
        assert_eq!(read(&outside), "PRECIOUS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_never_creates_a_file_through_a_dangling_link_at_the_staging_name() {
        let dir = test_dir("dangling");
        let unborn = dir.join("created-through-a-link.txt");
        let target = dir.join("out.pdf");
        if let Err(e) = file_symlink(&unborn, &staged(&dir, 1)) {
            eprintln!("symbolic links not checked: {e}");
            return;
        }
        write_with(&target, b"NEW", [1, 2]).unwrap();
        assert_eq!(read(&target), "NEW");
        assert!(!unborn.exists(), "nothing was created through the dangling link");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_at_the_staging_name_is_left_alone() {
        let dir = test_dir("staging-folder");
        std::fs::create_dir(staged(&dir, 1)).unwrap();
        std::fs::write(staged(&dir, 1).join("inside.txt"), "PLANTED").unwrap();
        let target = dir.join("out.pdf");
        write_with(&target, b"NEW", [1, 2]).unwrap();
        assert_eq!(read(&target), "NEW");
        assert_eq!(read(&staged(&dir, 1).join("inside.txt")), "PLANTED");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_gives_up_rather_than_reuse_a_taken_name() {
        let dir = test_dir("taken");
        let target = dir.join("out.pdf");
        std::fs::write(&target, "OLD").unwrap();
        for s in 1..=STAGING_ATTEMPTS as u64 {
            std::fs::write(staged(&dir, s), "PLANTED").unwrap();
        }
        assert!(write_with(&target, b"NEW", 1..).is_err());
        assert_eq!(read(&target), "OLD");
        for s in 1..=STAGING_ATTEMPTS as u64 {
            assert_eq!(read(&staged(&dir, s)), "PLANTED");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_a_long_name_leaves_only_the_saved_file() {
        assert_ne!(staging_suffixes().next(), staging_suffixes().next(), "each save draws new names");
        // 60 four-byte characters: a 244-byte name, within every system's limit. Its staging name
        // must be too (on Linux the whole name in it would be 275 bytes).
        let dir = test_dir("clean");
        let name = format!("{}.pdf", "\u{1F600}".repeat(60));
        let path = dir.join(&name);
        write_atomically(&path, b"NEW", StagingName::TagThenSuffix).unwrap();
        write_atomically(&path, b"NEWER", StagingName::SuffixThenTag).unwrap();
        assert_eq!(read(&path), "NEWER");
        assert_eq!(listing(&dir), [name], "no staging file is left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_without_a_file_name_is_refused() {
        let e = write_with(Path::new(""), b"NEW", [1]).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{e}");
    }

    #[test]
    fn a_folder_at_the_target_is_refused_before_anything_is_written() {
        let dir = test_dir("target-folder");
        let target = dir.join("out.pdf");
        std::fs::create_dir(&target).unwrap();
        let e = StagedFile::write(&target, b"NEW", StagingName::SuffixThenTag).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::IsADirectory, "{e}");
        assert!(target.is_dir());
        assert_eq!(listing(&dir), ["out.pdf"], "no staging file was made");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_staged_file_replaces_nothing_until_committed_and_is_removed_if_dropped() {
        let dir = test_dir("staged");
        let (a, b) = (dir.join("a.pdf"), dir.join("b.pdf"));
        std::fs::write(&a, "OLD A").unwrap();
        std::fs::write(&b, "OLD B").unwrap();
        let staged_a = StagedFile::write(&a, b"NEW A", StagingName::SuffixThenTag).unwrap();
        let staged_b = StagedFile::write(&b, b"NEW B", StagingName::SuffixThenTag).unwrap();
        assert_eq!((read(&a), read(&b)), ("OLD A".into(), "OLD B".into()), "staging touches no target");
        assert_eq!(listing(&dir).len(), 4, "both staging files exist beside their targets");
        staged_a.commit().unwrap();
        drop(staged_b);
        assert_eq!((read(&a), read(&b)), ("NEW A".into(), "OLD B".into()));
        assert_eq!(listing(&dir), ["a.pdf", "b.pdf"], "the dropped staging file was removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Windows refuses to replace a read-only file, so the rename fails: the staging file must not
    /// be left behind.
    #[cfg(windows)]
    #[test]
    fn a_failed_save_removes_the_staging_file() {
        let dir = test_dir("readonly");
        let target = dir.join("out.pdf");
        std::fs::write(&target, "OLD").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&target, perms.clone()).unwrap();
        let e = write_with(&target, b"NEW", [7]).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "the rename failed: {e}");
        assert_eq!(read(&target), "OLD");
        assert_eq!(listing(&dir), ["out.pdf"], "the staging file was removed");
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On macOS `sync_all` asks for a full flush, which SMB and FUSE volumes (a Cryptomator vault,
    /// #981) refuse as unsupported: ENOTSUP (45) or ENOTTY (25). The save still goes through.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_volume_without_full_flush_still_saves() {
        for errno in [45, 25] {
            let dir = test_dir(&format!("nofullsync-{errno}"));
            let target = dir.join("out.pdf");
            std::fs::write(&target, "OLD").unwrap();
            let asked = std::cell::Cell::new(false);
            let refuse = |_: &std::fs::File| {
                asked.set(true);
                Err(std::io::Error::from_raw_os_error(errno))
            };
            StagedFile::write_with(&target, b"NEW", StagingName::TagThenSuffix, [7], refuse).unwrap().commit().unwrap();
            assert!(asked.get(), "the full flush was tried");
            assert_eq!(read(&target), "NEW");
            assert_eq!(listing(&dir), ["out.pdf"], "no staging file is left behind");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Only "not supported" is let through: any other flush failure fails the save and keeps the
    /// original.
    #[test]
    fn a_failed_flush_keeps_the_original() {
        let dir = test_dir("syncfails");
        let target = dir.join("out.pdf");
        std::fs::write(&target, "OLD").unwrap();
        let e = StagedFile::write_with(&target, b"NEW", StagingName::TagThenSuffix, [7], |_| Err(std::io::Error::other("disk gone"))).unwrap_err();
        assert_eq!(e.to_string(), "disk gone");
        assert_eq!(read(&target), "OLD");
        assert_eq!(listing(&dir), ["out.pdf"], "the staging file was removed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
