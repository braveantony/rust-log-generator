use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::mem;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "app.log";

const BUFFER_BYTES: usize = 256 * 1024;

/// Appends lines to `<dir>/app.log`. When the next line would push the file
/// past `max_file_bytes` the file is rotated to `app.log.1` (older files shift
/// to `.2`, `.3`, ...) and at most `max_files` files are kept, the active one
/// included.
pub struct RotatingWriter {
    dir: PathBuf,
    max_file_bytes: u64,
    max_files: usize,
    file: BufWriter<File>,
    size: u64,
}

impl RotatingWriter {
    pub fn new(dir: &Path, max_file_bytes: u64, max_files: usize) -> io::Result<RotatingWriter> {
        fs::create_dir_all(dir)?;
        remove_rotated_files_from(dir, max_files)?;
        let (file, size) = open_active(dir)?;
        Ok(RotatingWriter {
            dir: dir.to_path_buf(),
            max_file_bytes,
            max_files,
            file,
            size,
        })
    }

    pub fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let len = line.len() as u64;
        if self.size > 0 && self.size + len > self.max_file_bytes {
            self.rotate()?;
        }
        self.file.write_all(line)?;
        self.size += len;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.follow_replaced_file()?;
        self.file.flush()
    }

    /// If someone deleted `app.log` or put another file in its place, the open
    /// file is no longer the one readers see, and lines would keep going to a
    /// file without a name until the next rotation. This switches to whatever
    /// is at the path now, creating the file if it is gone, and takes along
    /// the lines that are still buffered.
    fn follow_replaced_file(&mut self) -> io::Result<()> {
        let ours = self.file.get_ref().metadata()?;
        let still_ours = match fs::metadata(self.dir.join(FILE_NAME)) {
            Ok(at_path) => at_path.dev() == ours.dev() && at_path.ino() == ours.ino(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => return Err(err),
        };
        if still_ours {
            return Ok(());
        }
        let (file, size) = open_active(&self.dir)?;
        let (_, buffered) = mem::replace(&mut self.file, file).into_parts();
        // The buffer is only unavailable after a panic inside a write, and a
        // panic does not come back here.
        let buffered = buffered.unwrap_or_default();
        self.file.write_all(&buffered)?;
        self.size = size + buffered.len() as u64;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        // The active file may have been deleted or moved by someone else; the
        // writer then simply starts a new one.
        let active = self.dir.join(FILE_NAME);
        if self.max_files == 1 {
            remove_if_exists(&active)?;
        } else {
            // Only the unbroken run app.log.1..=N has to move, so the cost
            // depends on the files present, not on max_files. The run stops
            // one short of the oldest slot: renaming replaces the destination,
            // which is what drops the oldest file.
            let mut shifting = 0;
            while shifting < self.max_files - 2 && self.numbered(shifting + 1).exists() {
                shifting += 1;
            }
            for index in (1..=shifting).rev() {
                rename_if_exists(&self.numbered(index), &self.numbered(index + 1))?;
            }
            rename_if_exists(&active, &self.numbered(1))?;
        }
        let (file, size) = open_active(&self.dir)?;
        self.file = file;
        self.size = size;
        Ok(())
    }

    fn numbered(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{FILE_NAME}.{index}"))
    }
}

fn open_active(dir: &Path) -> io::Result<(BufWriter<File>, u64)> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE_NAME))?;
    let size = file.metadata()?.len();
    Ok((BufWriter::with_capacity(BUFFER_BYTES, file), size))
}

/// Deletes `app.log.<n>` for every `n >= first_index`. These are left behind
/// when a volume is reused with a lower `max_files` than before; rotation
/// alone never touches them.
fn remove_rotated_files_from(dir: &Path, first_index: usize) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let index = name
            .to_str()
            .and_then(|name| name.strip_prefix(FILE_NAME))
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|digits| digits.parse::<usize>().ok());
        if index.is_some_and(|index| index >= first_index) {
            remove_if_exists(&dir.join(name))?;
        }
    }
    Ok(())
}

fn rename_if_exists(from: &Path, to: &Path) -> io::Result<()> {
    ignore_not_found(fs::rename(from, to))
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    ignore_not_found(fs::remove_file(path))
}

fn ignore_not_found(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A unique scratch directory that is removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "rust-log-generator-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.0.join(name)).unwrap()
        }

        fn file_names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_all(writer: &mut RotatingWriter, lines: &[&str]) {
        for line in lines {
            writer.write_line(line.as_bytes()).unwrap();
        }
        writer.flush().unwrap();
    }

    #[test]
    fn writes_lines_to_app_log() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 1024, 5).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n"]);
        assert_eq!(dir.read("app.log"), "aaa\nbbb\n");
        assert_eq!(dir.file_names(), ["app.log"]);
    }

    #[test]
    fn creates_the_directory_if_missing() {
        let dir = TempDir::new();
        let nested = dir.0.join("a").join("b");
        let mut writer = RotatingWriter::new(&nested, 1024, 5).unwrap();
        write_all(&mut writer, &["aaa\n"]);
        assert_eq!(fs::read_to_string(nested.join("app.log")).unwrap(), "aaa\n");
    }

    #[test]
    fn rotates_before_a_line_would_exceed_the_max_size() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 10, 5).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n", "ccc\n"]);
        assert_eq!(dir.read("app.log.1"), "aaa\nbbb\n");
        assert_eq!(dir.read("app.log"), "ccc\n");
        assert_eq!(dir.file_names(), ["app.log", "app.log.1"]);
    }

    #[test]
    fn a_file_filled_exactly_to_the_max_is_not_rotated_early() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 8, 5).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n"]);
        assert_eq!(dir.read("app.log"), "aaa\nbbb\n");
        assert_eq!(dir.file_names(), ["app.log"]);
    }

    #[test]
    fn older_files_shift_to_higher_numbers() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 4, 5).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n", "ccc\n"]);
        assert_eq!(dir.read("app.log"), "ccc\n");
        assert_eq!(dir.read("app.log.1"), "bbb\n");
        assert_eq!(dir.read("app.log.2"), "aaa\n");
    }

    #[test]
    fn keeps_at_most_max_files_and_drops_the_oldest() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 4, 3).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n", "ccc\n", "ddd\n", "eee\n"]);
        assert_eq!(dir.file_names(), ["app.log", "app.log.1", "app.log.2"]);
        assert_eq!(dir.read("app.log"), "eee\n");
        assert_eq!(dir.read("app.log.1"), "ddd\n");
        assert_eq!(dir.read("app.log.2"), "ccc\n");
    }

    #[test]
    fn keeps_exactly_max_files_when_more_than_four_are_allowed() {
        for max_files in [5, 6] {
            let dir = TempDir::new();
            let mut writer = RotatingWriter::new(&dir.0, 4, max_files).unwrap();
            let lines: Vec<String> = (0..10).map(|number| format!("{number:03}\n")).collect();
            for line in &lines {
                writer.write_line(line.as_bytes()).unwrap();
            }
            writer.flush().unwrap();
            // One line per file: the newest max_files lines remain, newest first.
            assert_eq!(dir.file_names().len(), max_files, "max_files {max_files}");
            assert_eq!(dir.read("app.log"), lines[9]);
            for slot in 1..max_files {
                assert_eq!(dir.read(&format!("app.log.{slot}")), lines[9 - slot]);
            }
        }
    }

    #[test]
    fn a_line_longer_than_the_max_still_goes_into_an_empty_file() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 2, 5).unwrap();
        write_all(&mut writer, &["aaa\n"]);
        assert_eq!(dir.read("app.log"), "aaa\n");
        assert_eq!(dir.file_names(), ["app.log"]);
    }

    #[test]
    fn a_rotation_that_cannot_move_a_file_reports_the_error() {
        // A directory where app.log.1 should go. With two files allowed
        // nothing is shifted first, so the active file is renamed straight
        // onto the directory: that fails, and not because something is missing.
        let dir = TempDir::new();
        fs::create_dir(dir.0.join("app.log.1")).unwrap();
        fs::write(dir.0.join("app.log.1").join("in-the-way"), "x").unwrap();
        let mut writer = RotatingWriter::new(&dir.0, 4, 2).unwrap();
        writer.write_line(b"aaa\n").unwrap();
        let err = writer.write_line(b"bbb\n").unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::NotFound, "{err}");
    }

    #[test]
    fn with_a_single_file_rotation_starts_the_file_over() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 4, 1).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n", "ccc\n"]);
        assert_eq!(dir.file_names(), ["app.log"]);
        assert_eq!(dir.read("app.log"), "ccc\n");
    }

    #[test]
    fn appends_to_an_existing_file_and_counts_its_size() {
        let dir = TempDir::new();
        fs::write(dir.0.join("app.log"), "old\nold\n").unwrap();
        let mut writer = RotatingWriter::new(&dir.0, 10, 5).unwrap();
        write_all(&mut writer, &["new\n"]);
        assert_eq!(dir.read("app.log.1"), "old\nold\n");
        assert_eq!(dir.read("app.log"), "new\n");
    }

    #[test]
    fn keeps_writing_when_the_active_file_was_deleted_externally() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 8, 5).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n"]);
        fs::remove_file(dir.0.join("app.log")).unwrap();
        write_all(&mut writer, &["ccc\n"]);
        assert_eq!(dir.read("app.log"), "ccc\n");
        assert_eq!(dir.file_names(), ["app.log"]);
    }

    #[test]
    fn keeps_writing_with_a_single_file_deleted_externally() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 8, 1).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n"]);
        fs::remove_file(dir.0.join("app.log")).unwrap();
        write_all(&mut writer, &["ccc\n"]);
        assert_eq!(dir.read("app.log"), "ccc\n");
    }

    #[test]
    fn a_deleted_active_file_is_recreated_at_the_next_flush() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 1024, 5).unwrap();
        write_all(&mut writer, &["aaa\n"]);
        fs::remove_file(dir.0.join("app.log")).unwrap();
        write_all(&mut writer, &["bbb\n", "ccc\n"]);
        assert_eq!(dir.read("app.log"), "bbb\nccc\n");
    }

    #[test]
    fn follows_a_file_that_replaced_the_active_one() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 1024, 5).unwrap();
        write_all(&mut writer, &["aaa\n"]);
        fs::rename(dir.0.join("app.log"), dir.0.join("moved.log")).unwrap();
        fs::write(dir.0.join("app.log"), "external\n").unwrap();
        write_all(&mut writer, &["bbb\n"]);
        assert_eq!(dir.read("moved.log"), "aaa\n");
        assert_eq!(dir.read("app.log"), "external\nbbb\n");
    }

    #[test]
    fn counts_the_size_of_a_file_that_replaced_the_active_one() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 16, 5).unwrap();
        write_all(&mut writer, &["aaa\n"]);
        fs::remove_file(dir.0.join("app.log")).unwrap();
        fs::write(dir.0.join("app.log"), "ext\next\n").unwrap();
        // 8 bytes already there plus this line: 12 of the 16 allowed.
        write_all(&mut writer, &["bbb\n"]);
        // Exactly full, so not rotated yet.
        write_all(&mut writer, &["ccc\n"]);
        assert_eq!(dir.read("app.log"), "ext\next\nbbb\nccc\n");
        assert_eq!(dir.file_names(), ["app.log"]);
        // One more line does not fit.
        write_all(&mut writer, &["ddd\n"]);
        assert_eq!(dir.read("app.log.1"), "ext\next\nbbb\nccc\n");
        assert_eq!(dir.read("app.log"), "ddd\n");
    }

    #[test]
    fn startup_removes_rotated_files_beyond_max_files() {
        let dir = TempDir::new();
        for name in ["app.log", "app.log.1", "app.log.2", "app.log.3", "app.log.4"] {
            fs::write(dir.0.join(name), "old\n").unwrap();
        }
        let _writer = RotatingWriter::new(&dir.0, 1024, 2).unwrap();
        assert_eq!(dir.file_names(), ["app.log", "app.log.1"]);
    }

    #[test]
    fn startup_leaves_unrelated_files_alone() {
        let dir = TempDir::new();
        for name in ["app.log.bak", "app.log.9.gz", "other.log.9", "notes.txt"] {
            fs::write(dir.0.join(name), "keep\n").unwrap();
        }
        let _writer = RotatingWriter::new(&dir.0, 1024, 2).unwrap();
        assert_eq!(
            dir.file_names(),
            ["app.log", "app.log.9.gz", "app.log.bak", "notes.txt", "other.log.9"]
        );
    }

    #[test]
    fn rotation_cost_does_not_grow_with_a_huge_max_files() {
        let dir = TempDir::new();
        let mut writer = RotatingWriter::new(&dir.0, 4, usize::MAX).unwrap();
        write_all(&mut writer, &["aaa\n", "bbb\n", "ccc\n"]);
        assert_eq!(dir.read("app.log"), "ccc\n");
        assert_eq!(dir.read("app.log.1"), "bbb\n");
        assert_eq!(dir.read("app.log.2"), "aaa\n");
    }

    #[test]
    fn appends_to_an_existing_file_with_room_left() {
        let dir = TempDir::new();
        fs::write(dir.0.join("app.log"), "old\n").unwrap();
        let mut writer = RotatingWriter::new(&dir.0, 10, 5).unwrap();
        write_all(&mut writer, &["new\n"]);
        assert_eq!(dir.read("app.log"), "old\nnew\n");
    }
}
