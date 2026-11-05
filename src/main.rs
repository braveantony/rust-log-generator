mod config;
mod format;
mod pacer;
mod rotate;
mod run_id;

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use config::Config;
use pacer::Pacer;
use rotate::RotatingWriter;

const MIB: f64 = config::MIB as f64;
const BURST_BYTES: u64 = 256 * 1024;
const TICK: Duration = Duration::from_millis(100);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rust-log-generator: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let config = Config::from_lookup(|name| std::env::var(name).ok())?;

    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))
            .map_err(|err| format!("cannot install signal handler: {err}"))?;
    }

    let mut writer = RotatingWriter::new(&config.dir, config.max_file_bytes, config.max_files)
        .map_err(|err| format!("cannot open log file in {}: {err}", config.dir.display()))?;
    let write_failed = |err| format!("cannot write to {}: {err}", config.dir.display());

    let run_id = run_id::generate();
    println!(
        "rust-log-generator: run {run_id}: writing {} MiB/s of {} logs to {} (rotate at {} MiB, keep {} files)",
        config.rate_bytes_per_sec / MIB,
        if config.json { "JSON" } else { "text" },
        config.dir.join(rotate::FILE_NAME).display(),
        config.max_file_bytes as f64 / MIB,
        config.max_files,
    );

    let start = Instant::now();
    let mut pacer = Pacer::new(config.rate_bytes_per_sec);
    let mut next_tick = start;
    let mut written: u64 = 0;
    let mut seq: u64 = 0;
    let mut line = Vec::with_capacity(format::LINE_BYTES);
    let mut reported_at = start;
    let mut reported_bytes: u64 = 0;

    while !stop.load(Ordering::Relaxed) {
        // Each tick's writes get one tick of time. If the disk cannot keep up,
        // what is left carries over to later ticks (and the pacer forgives a
        // long backlog), so the stop flag and the report stay responsive.
        let target = pacer.target(start.elapsed(), written);
        let deadline = Instant::now() + TICK;
        while written < target && Instant::now() < deadline {
            let burst_end = target.min(written.saturating_add(BURST_BYTES));
            while written < burst_end {
                format::write_line(&mut line, config.json, &run_id, unix_millis(), seq);
                writer.write_line(&line).map_err(write_failed)?;
                written += line.len() as u64;
                seq += 1;
            }
        }
        writer.flush().map_err(write_failed)?;

        let now = Instant::now();
        let since_report = now - reported_at;
        if since_report >= REPORT_INTERVAL {
            println!(
                "rust-log-generator: {:.2} MiB/s (total {:.1} MiB)",
                (written - reported_bytes) as f64 / MIB / since_report.as_secs_f64(),
                written as f64 / MIB,
            );
            reported_at = now;
            reported_bytes = written;
        }

        next_tick += TICK;
        match next_tick.checked_duration_since(Instant::now()) {
            Some(wait) => thread::sleep(wait),
            // The tick overran: start the next one immediately.
            None => next_tick = Instant::now(),
        }
    }

    writer.flush().map_err(write_failed)?;
    println!("rust-log-generator: stopped after {:.1} MiB", written as f64 / MIB);
    Ok(())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_millis() as u64)
}
