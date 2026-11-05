//! End-to-end tests: they run the built program the way a container does, with
//! its settings in the environment and a directory to write to, and judge it
//! by its exit code, its output and the files it leaves behind.
//!
//! The unit tests cannot reach `main.rs`; these do. Timing is kept loose on
//! purpose so that a slow build machine does not fail them. How precisely the
//! rate is held is checked against the image by `tests/image/e2e.py`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

const PROGRAM: &str = env!("CARGO_BIN_EXE_rust-log-generator");
const LINE_BYTES: usize = 256;
const MIB: u64 = 1024 * 1024;

/// A unique scratch directory that is removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "rust-log-generator-e2e-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
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

fn program(log_dir: &Path, settings: &[(&str, &str)]) -> Command {
    let mut command = Command::new(PROGRAM);
    // Settings of the machine running the tests must not leak in. The rest of
    // the environment stays: coverage tooling passes its settings through it.
    for name in [
        "LOG_RATE_MIB",
        "LOG_JSON",
        "LOG_DIR",
        "LOG_MAX_FILE_MIB",
        "LOG_MAX_FILES",
        "LOG_MAX_TOTAL_MIB",
    ] {
        command.env_remove(name);
    }
    command.env("LOG_DIR", log_dir);
    for (name, value) in settings {
        command.env(name, value);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Finished {
    fn from(output: Output) -> Finished {
        Finished {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

/// Waits for the program to end by itself. A program that does not is killed,
/// and the test fails instead of hanging.
fn finish(mut child: Child) -> Finished {
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the program was still running 10 seconds after it should have ended");
        }
        sleep(Duration::from_millis(10));
    }
    Finished::from(child.wait_with_output().unwrap())
}

fn send_signal(child: &Child, signal: &str) {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success(), "kill -{signal} failed");
}

fn run_until_signal(
    log_dir: &Path,
    settings: &[(&str, &str)],
    run_for: Duration,
    signal: &str,
) -> Finished {
    let child = program(log_dir, settings).spawn().unwrap();
    sleep(run_for);
    send_signal(&child, signal);
    finish(child)
}

fn run_to_end(log_dir: &Path, settings: &[(&str, &str)]) -> Finished {
    finish(program(log_dir, settings).spawn().unwrap())
}

/// Splits a log file into its lines, insisting that each is exactly
/// `LINE_BYTES` long including the newline.
fn lines_of(path: &Path) -> Vec<String> {
    let data = fs::read(path).unwrap();
    assert_eq!(
        data.len() % LINE_BYTES,
        0,
        "{} holds {} bytes, not whole lines",
        path.display(),
        data.len()
    );
    data.chunks(LINE_BYTES)
        .map(|chunk| {
            assert_eq!(chunk[LINE_BYTES - 1], b'\n');
            assert!(!chunk[..LINE_BYTES - 1].contains(&b'\n'));
            String::from_utf8(chunk[..LINE_BYTES - 1].to_vec()).unwrap()
        })
        .collect()
}

fn json_lines_of(path: &Path) -> Vec<serde_json::Value> {
    lines_of(path)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn level_for(seq: u64) -> &'static str {
    match seq % 10 {
        7 => "DEBUG",
        8 => "WARN",
        9 => "ERROR",
        _ => "INFO",
    }
}

/// Asserts that the lines carry consecutive sequence numbers from one run,
/// with the level that goes with each number. Returns the first number.
fn assert_consecutive(lines: &[serde_json::Value]) -> u64 {
    let first = lines[0]["seq"].as_u64().unwrap();
    let run = lines[0]["run"].as_str().unwrap();
    assert_eq!(run.len(), 8, "run id {run:?}");
    assert!(run.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "run id {run:?}");
    for (offset, line) in lines.iter().enumerate() {
        let seq = first + offset as u64;
        assert_eq!(line["seq"].as_u64(), Some(seq));
        assert_eq!(line["run"].as_str(), Some(run));
        assert_eq!(line["level"].as_str(), Some(level_for(seq)));
        assert_eq!(line.as_object().unwrap().len(), 5);
    }
    first
}

#[test]
fn writes_json_lines_reports_once_a_second_and_stops_on_sigterm() {
    let dir = TempDir::new();
    let finished = run_until_signal(
        &dir.0,
        &[("LOG_RATE_MIB", "1")],
        Duration::from_millis(1500),
        "TERM",
    );
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    assert_eq!(finished.stderr, "");
    let stdout: Vec<&str> = finished.stdout.lines().collect();
    assert!(
        stdout[0].contains("writing 1 MiB/s of JSON logs to")
            && stdout[0].contains("(rotate at 100 MiB, keep 5 files)"),
        "{}",
        stdout[0]
    );
    // About once a second, so one report in 1.5 seconds; a slow machine may
    // fit in a second, but never one per tick.
    let reports = stdout.iter().filter(|line| line.contains(" MiB/s (total ")).count();
    assert!((1..=2).contains(&reports), "{reports} rate reports in {stdout:?}");
    assert!(
        stdout.last().unwrap().starts_with("rust-log-generator: stopped after "),
        "{stdout:?}"
    );

    assert_eq!(dir.file_names(), ["app.log"]);
    let lines = json_lines_of(&dir.path("app.log"));
    assert_eq!(assert_consecutive(&lines), 0);
    // Timestamps are the current time and move on. The format is fixed-width,
    // so comparing the text compares the times.
    let first_ts = lines[0]["ts"].as_str().unwrap();
    let last_ts = lines[lines.len() - 1]["ts"].as_str().unwrap();
    assert!(first_ts > "2025-01-01T00:00:00.000Z", "first timestamp {first_ts}");
    assert!(last_ts > first_ts, "timestamps {first_ts} .. {last_ts}");
    // 1.5 seconds at 1 MiB/s. The bounds are wide: this is about the program
    // pacing itself at all, not about how exactly.
    let bytes = (lines.len() * LINE_BYTES) as u64;
    assert!(
        (MIB / 2..=MIB * 5 / 2).contains(&bytes),
        "wrote {bytes} bytes in about 1.5 s at 1 MiB/s"
    );
}

#[test]
fn sleeps_between_ticks_instead_of_spinning() {
    let dir = TempDir::new();
    let child = program(&dir.0, &[("LOG_RATE_MIB", "0.01")]).spawn().unwrap();
    sleep(Duration::from_millis(1000));
    // Fields 14 and 15 of /proc/<pid>/stat: CPU time in user and kernel mode,
    // in clock ticks. The command name before them is in parentheses and may
    // contain spaces, so count from the closing one.
    let cpu_ticks = fs::read_to_string(format!("/proc/{}/stat", child.id()))
        .ok()
        .and_then(|stat| {
            let fields: Vec<u64> = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .skip(11)
                .take(2)
                .filter_map(|field| field.parse().ok())
                .collect();
            (fields.len() == 2).then(|| fields[0] + fields[1])
        });
    send_signal(&child, "TERM");
    let finished = finish(child);
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    match cpu_ticks {
        // A tick is at least a millisecond everywhere, and usually ten. A
        // program that never slept would have used a whole second of CPU.
        Some(ticks) => assert!(ticks <= 30, "used {ticks} clock ticks of CPU in one second"),
        None => eprintln!("skipped: /proc/<pid>/stat is not readable on this system"),
    }
}

#[test]
fn stops_on_sigint_too() {
    let dir = TempDir::new();
    let finished = run_until_signal(&dir.0, &[], Duration::from_millis(300), "INT");
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    assert!(finished.stdout.contains("stopped after "), "{}", finished.stdout);
    json_lines_of(&dir.path("app.log"));
}

#[test]
fn writes_plain_text_when_json_is_off() {
    let dir = TempDir::new();
    let finished = run_until_signal(
        &dir.0,
        &[("LOG_JSON", "false")],
        Duration::from_millis(300),
        "TERM",
    );
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    assert!(finished.stdout.contains("of text logs to"), "{}", finished.stdout);
    let lines = lines_of(&dir.path("app.log"));
    assert!(lines[0].contains(" INFO run=") && lines[0].contains(" seq=0 "), "{}", lines[0]);
    assert!(!lines[0].starts_with('{'));
}

#[test]
fn rotates_and_keeps_only_the_allowed_files() {
    let dir = TempDir::new();
    let finished = run_until_signal(
        &dir.0,
        &[("LOG_RATE_MIB", "8"), ("LOG_MAX_FILE_MIB", "1"), ("LOG_MAX_FILES", "2")],
        Duration::from_millis(1000),
        "TERM",
    );
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    assert_eq!(dir.file_names(), ["app.log", "app.log.1"]);
    assert_eq!(fs::metadata(dir.path("app.log.1")).unwrap().len(), MIB);
    assert!(fs::metadata(dir.path("app.log")).unwrap().len() <= MIB);
    // The older file ends where the newer one begins.
    let mut lines = json_lines_of(&dir.path("app.log.1"));
    lines.extend(json_lines_of(&dir.path("app.log")));
    assert!(assert_consecutive(&lines) > 0, "nothing was rotated away");
}

#[test]
fn keeps_running_when_the_rate_is_out_of_reach() {
    let dir = TempDir::new();
    let finished = run_until_signal(
        &dir.0,
        &[("LOG_RATE_MIB", "1000000"), ("LOG_MAX_FILE_MIB", "4"), ("LOG_MAX_FILES", "2")],
        Duration::from_millis(600),
        "TERM",
    );
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    assert_eq!(finished.stderr, "");
    let lines = json_lines_of(&dir.path("app.log"));
    assert_consecutive(&lines);
}

#[test]
fn rejects_an_invalid_setting_and_touches_no_file() {
    let dir = TempDir::new();
    // A valid start would append to the first and delete the second.
    fs::write(dir.path("app.log"), "keep\n").unwrap();
    fs::write(dir.path("app.log.9"), "keep\n").unwrap();
    let finished = run_to_end(&dir.0, &[("LOG_RATE_MIB", "abc")]);
    assert_eq!(finished.code, Some(1));
    assert_eq!(finished.stdout, "");
    assert!(finished.stderr.contains("LOG_RATE_MIB"), "{}", finished.stderr);
    assert_eq!(dir.file_names(), ["app.log", "app.log.9"]);
    assert_eq!(fs::read_to_string(dir.path("app.log")).unwrap(), "keep\n");
}

#[test]
fn rejects_a_rate_too_low_to_ever_write_a_line() {
    let dir = TempDir::new();
    let finished = run_to_end(&dir.0, &[("LOG_RATE_MIB", "0.0000005")]);
    assert_eq!(finished.code, Some(1));
    assert!(finished.stderr.contains("1 byte per second"), "{}", finished.stderr);
    assert!(dir.file_names().is_empty());
}

#[test]
fn reports_a_directory_it_cannot_create() {
    let dir = TempDir::new();
    fs::write(dir.path("a-file"), "").unwrap();
    let finished = run_to_end(&dir.path("a-file").join("logs"), &[]);
    assert_eq!(finished.code, Some(1));
    assert_eq!(finished.stdout, "");
    assert!(finished.stderr.contains("cannot open log file in"), "{}", finished.stderr);
}

#[test]
fn reports_a_write_that_fails() {
    // /dev/full accepts being opened and refuses every write, like a full disk.
    if !Path::new("/dev/full").exists() {
        eprintln!("skipped: this system has no /dev/full");
        return;
    }
    let dir = TempDir::new();
    std::os::unix::fs::symlink("/dev/full", dir.path("app.log")).unwrap();
    let finished = run_to_end(&dir.0, &[("LOG_RATE_MIB", "100")]);
    assert_eq!(finished.code, Some(1));
    assert!(finished.stderr.contains("cannot write to"), "{}", finished.stderr);
}

#[test]
fn starts_a_new_file_soon_after_the_active_one_is_deleted() {
    let dir = TempDir::new();
    let child = program(&dir.0, &[("LOG_RATE_MIB", "1")]).spawn().unwrap();
    sleep(Duration::from_millis(500));
    fs::remove_file(dir.path("app.log")).unwrap();
    sleep(Duration::from_millis(600));
    let reappeared = fs::metadata(dir.path("app.log")).map(|file| file.len()).unwrap_or(0);
    send_signal(&child, "TERM");
    let finished = finish(child);
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    assert!(reappeared > 0, "app.log had not come back 600 ms after it was deleted");
    // What the new file holds continues the same run, without a gap inside it.
    let lines = json_lines_of(&dir.path("app.log"));
    assert!(assert_consecutive(&lines) > 0);
}
