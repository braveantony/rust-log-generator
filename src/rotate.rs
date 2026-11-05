use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
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
        self.file.flush()
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
