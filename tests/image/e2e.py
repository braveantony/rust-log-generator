#!/usr/bin/env python3
"""End-to-end and fault-injection checks of a rust-log-generator image, driven
from the outside with podman. Run it against an image before publishing it.

It starts the image as a container would be started in production (settings
in the environment, a directory mounted at /logs), measures what lands on
disk, and breaks things on purpose: a read-only directory, a full disk, a
paused process, files deleted or replaced while it runs, SIGKILL.

Log content is judged by check_logs.py, which shares no code with the program
and only uses Python's standard parsers.

usage: e2e.py IMAGE [group ...]
       groups: config rate content rotate stop leak faults   (default: all)

Needs rootless podman 4.3 or newer and Python 3.11 or newer. Takes about seven
minutes and writes a few hundred MiB to a temporary directory; the rate group
needs an otherwise idle machine. Exits 1 if any check fails.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

IMAGE = None                                            # set from the command line
HERE = os.path.dirname(os.path.abspath(__file__))
BASE = None                                             # temporary directory for this run
USERNS = "--userns=keep-id:uid=65532,gid=65532"         # host user <-> 65532 in the container
MIB = 1024 * 1024
LINE = 256
RESULTS = []


def sh(*args, timeout=120, check=False):
    return subprocess.run(args, capture_output=True, text=True, timeout=timeout, check=check)


def record(test, ok, detail):
    """ok is True or False for a check, None for an observation that cannot fail."""
    status = {True: "PASS", False: "FAIL", None: "INFO"}[ok]
    RESULTS.append({"test": test, "status": status, "detail": detail})
    print(f"[{status}] {test}: {detail}", flush=True)


def fresh_dir(name):
    path = os.path.join(BASE, name)
    shutil.rmtree(path, ignore_errors=True)
    os.makedirs(path)
    return path


def start(name, env, directory, mount_opts="", extra=()):
    sh("podman", "rm", "-f", name)
    args = ["podman", "run", "-d", "--name", name, USERNS]
    for key, value in env.items():
        args += ["-e", f"{key}={value}"]
    if directory:
        args += ["-v", f"{directory}:/logs{mount_opts}"]
    args += list(extra) + [IMAGE]
    result = sh(*args)
    if result.returncode != 0:
        raise RuntimeError(result.stderr)


def wait(name, timeout=30):
    """Exit code of the container, or None if it did not stop in time and had to be killed."""
    try:
        return int(sh("podman", "wait", name, timeout=timeout).stdout.strip())
    except (subprocess.TimeoutExpired, ValueError):
        sh("podman", "kill", "-s", "KILL", name)
        return None


def logs(name):
    result = sh("podman", "logs", name)
    return result.stdout, result.stderr


def stop(name, signal="TERM"):
    """Sends the signal and returns (exit code, seconds until the container exited)."""
    began = time.monotonic()
    sh("podman", "kill", "-s", signal, name)
    code = wait(name)
    return code, time.monotonic() - began


def remove(name):
    sh("podman", "rm", "-f", name)


def run_to_exit(name, env, directory, mount_opts="", timeout=20):
    start(name, env, directory, mount_opts)
    code = wait(name, timeout)
    out, err = logs(name)
    remove(name)
    return code, out, err


def last_seq(directory, fmt):
    """Sequence number of the last line on disk, read from the outside."""
    for _ in range(50):
        for name in ("app.log", "app.log.1"):
            path = os.path.join(directory, name)
            try:
                size = os.path.getsize(path)
                if size < LINE:
                    continue
                with open(path, "rb") as handle:
                    handle.seek(size - size % LINE - LINE)
                    line = handle.read(LINE).decode("ascii")
                if fmt == "json":
                    return json.loads(line)["seq"]
                return int(re.search(r" seq=(\d+) ", line).group(1))
            except (OSError, ValueError, AttributeError):
                # The file was rotated between measuring it and reading it.
                continue
        time.sleep(0.02)
    raise RuntimeError(f"no complete line found in {directory}")


def check(directory, fmt, max_file_mib, max_files):
    args = [sys.executable, os.path.join(HERE, "check_logs.py"), directory, "--format", fmt,
            "--max-file-bytes", str(int(max_file_mib * MIB)), "--max-files", str(max_files)]
    return json.loads(sh(*args, timeout=600).stdout)


def cleanup():
    """Removes every container and volume this script may have left behind."""
    names = [name for name in sh("podman", "ps", "-a", "--format", "{{.Names}}").stdout.split() if name.startswith("rlg-")]
    if names:
        sh("podman", "rm", "-f", *names)
    sh("podman", "volume", "rm", "-f", "rlg-full")


def listing(directory):
    return {name: os.path.getsize(os.path.join(directory, name)) for name in sorted(os.listdir(directory))}


# ---------------------------------------------------------------- A. settings

def group_config():
    directory = fresh_dir("defaults")
    start("rlg-defaults", {}, directory)
    time.sleep(2.5)
    code, _ = stop("rlg-defaults")
    out, _ = logs("rlg-defaults")
    remove("rlg-defaults")
    first = out.splitlines()[0] if out else ""
    expected = "writing 1 MiB/s of JSON logs to /logs/app.log (rotate at 100 MiB, keep 5 files)"
    record("defaults", expected in first and code == 0, f"startup line: {first!r}")

    bad = [
        ("LOG_RATE_MIB", "abc"), ("LOG_RATE_MIB", "0"), ("LOG_RATE_MIB", "-1"), ("LOG_RATE_MIB", "NaN"),
        ("LOG_RATE_MIB", "inf"), ("LOG_RATE_MIB", "1e305"), ("LOG_RATE_MIB", ""), ("LOG_RATE_MIB", "0.0000005"),
        ("LOG_JSON", "yes"), ("LOG_JSON", "1"), ("LOG_DIR", ""),
        ("LOG_MAX_FILE_MIB", "0"), ("LOG_MAX_FILE_MIB", "1.5"), ("LOG_MAX_FILE_MIB", "-5"),
        ("LOG_MAX_FILES", "0"), ("LOG_MAX_FILES", "two"), ("LOG_MAX_TOTAL_MIB", "0"),
    ]
    failures, touched = [], []
    for variable, value in bad:
        directory = fresh_dir("badcfg")
        # Files that a valid start with LOG_MAX_FILES=5 would touch or delete.
        for name, content in (("app.log", b"keep\n"), ("app.log.9", b"keep\n")):
            with open(os.path.join(directory, name), "wb") as handle:
                handle.write(content)
        before = listing(directory)
        code, out, err = run_to_exit("rlg-badcfg", {variable: value}, directory)
        if not (code == 1 and variable in err and out == ""):
            failures.append(f"{variable}={value!r}: exit={code} stderr={err.strip()!r} stdout={out!r}")
        if listing(directory) != before:
            touched.append(f"{variable}={value!r}: {listing(directory)}")
    record("invalid settings are rejected", not failures,
           f"{len(bad)} invalid values tried; " + ("all exit 1 with the variable named on stderr" if not failures else "; ".join(failures)))
    record("invalid settings touch no file", not touched,
           "pre-existing app.log and app.log.9 unchanged in every case" if not touched else "; ".join(touched))

    cap_cases = [
        ({"LOG_MAX_FILE_MIB": "300", "LOG_MAX_FILES": "4"}, 1, "300x4 > default cap 1024"),
        ({"LOG_MAX_FILE_MIB": "256", "LOG_MAX_FILES": "4"}, None, "256x4 = cap exactly (must start)"),
        ({"LOG_MAX_TOTAL_MIB": "400"}, 1, "defaults 100x5 > cap 400"),
        ({"LOG_MAX_FILE_MIB": "4294967296", "LOG_MAX_FILES": "4294967296",
          "LOG_MAX_TOTAL_MIB": "18446744073709551615"}, 1, "product overflows u64"),
    ]
    failures = []
    for env, want, label in cap_cases:
        directory = fresh_dir("capcfg")
        start("rlg-cap", env, directory)
        time.sleep(1.0)
        state = sh("podman", "inspect", "-f", "{{.State.Running}} {{.State.ExitCode}}", "rlg-cap").stdout.split()
        _, err = logs("rlg-cap")
        remove("rlg-cap")
        running, code = state[0] == "true", int(state[1])
        good = (running if want is None else (not running and code == want and "LOG_MAX_TOTAL_MIB" in err and not os.listdir(directory)))
        if not good:
            failures.append(f"{label}: running={running} exit={code} stderr={err.strip()!r} files={os.listdir(directory)}")
    record("disk cap", not failures, f"{len(cap_cases)} cases; " + ("all as promised" if not failures else "; ".join(failures)))


# ------------------------------------------------------------------- B. rate

def measure_rate(name, rate, fmt, window, max_file_mib, max_files, warmup=3.0):
    directory = fresh_dir(name)
    env = {"LOG_RATE_MIB": str(rate), "LOG_JSON": "true" if fmt == "json" else "false",
           "LOG_MAX_FILE_MIB": str(max_file_mib), "LOG_MAX_FILES": str(max_files)}
    start("rlg-" + name, env, directory)
    time.sleep(warmup)
    t1, s1 = time.monotonic(), last_seq(directory, fmt)
    time.sleep(window)
    t2, s2 = time.monotonic(), last_seq(directory, fmt)
    code, stop_seconds = stop("rlg-" + name)
    out, err = logs("rlg-" + name)
    stamped = sh("podman", "logs", "-t", "rlg-" + name).stdout
    remove("rlg-" + name)
    lines = s2 - s1
    return {"directory": directory, "lines": lines, "seconds": t2 - t1, "actual_mib_s": lines * LINE / MIB / (t2 - t1),
            "expected_lines": rate * MIB * (t2 - t1) / LINE, "exit": code, "stop_seconds": stop_seconds,
            "stdout": out, "stderr": err, "stamped": stamped}


def reported_rates(stdout):
    return [float(m.group(1)) for m in re.finditer(r": ([0-9.]+) MiB/s \(total", stdout)]


def group_rate():
    for rate, window in ((0.001, 60), (1, 30), (100, 30)):
        m = measure_rate(f"rate-{rate}", rate, "json", window, max_file_mib=10, max_files=5)
        tolerance = 0.01 * m["expected_lines"] + 1
        ok = abs(m["lines"] - m["expected_lines"]) <= tolerance and m["exit"] == 0 and m["stderr"] == ""
        record(f"rate {rate} MiB/s", ok,
               f"{m['lines']} lines in {m['seconds']:.2f} s = {m['actual_mib_s']:.5f} MiB/s; expected {m['expected_lines']:.1f} "
               f"+-{tolerance:.1f} lines; exit {m['exit']}")
        summary = check(m["directory"], "json", 10, 5)
        record(f"content at {rate} MiB/s", summary["ok"],
               f"{summary['lines']} lines in {len(summary['files'])} files, {summary['problem_count']} problems"
               + (f": {summary['problems'][:3]}" if summary["problems"] else ""))
        if rate == 1:
            stamps = [line.split(" ", 1)[0] for line in m["stamped"].splitlines() if "MiB/s (total" in line]
            times = [time.mktime(time.strptime(s[:19], "%Y-%m-%dT%H:%M:%S")) + float("0." + re.sub(r"\D.*$", "", s[20:])) for s in stamps]
            gaps = [b - a for a, b in zip(times, times[1:])]
            rates = reported_rates(m["stdout"])
            ok = bool(gaps) and min(gaps) >= 0.95 and max(gaps) <= 1.25 and all(abs(r - 1.0) <= 0.02 for r in rates[1:])
            gap_text = f"min {min(gaps):.3f} s, max {max(gaps):.3f} s" if gaps else "too few reports to measure"
            rate_text = f"{min(rates):.2f}..{max(rates):.2f}" if rates else "none"
            record("report cadence and value", ok,
                   f"{len(gaps)} gaps between reports: {gap_text}; reported rates {rate_text}")

    m = measure_rate("rate-1000", 1000, "json", 30, max_file_mib=10, max_files=5)
    rates = reported_rates(m["stdout"])
    steady = rates[3:-1] or rates or [0.0]
    mean_reported = sum(steady) / len(steady)
    ok = m["exit"] == 0 and m["stderr"] == "" and abs(mean_reported - m["actual_mib_s"]) <= 0.05 * m["actual_mib_s"]
    record("rate above capacity (1000 MiB/s asked)", ok,
           f"on disk {m['actual_mib_s']:.1f} MiB/s over {m['seconds']:.1f} s; program reported mean {mean_reported:.1f} "
           f"(min {min(steady):.1f}, max {max(steady):.1f}); exit {m['exit']}; stderr {m['stderr']!r}")
    summary = check(m["directory"], "json", 10, 5)
    record("content at saturation", summary["ok"],
           f"{summary['lines']} lines, {summary['problem_count']} problems" + (f": {summary['problems'][:3]}" if summary["problems"] else ""))


# ---------------------------------------------------------- C/D. content, files

def group_content():
    m = measure_rate("text", 2, "text", 8, max_file_mib=1, max_files=4)
    summary = check(m["directory"], "text", 1, 4)
    record("text format", summary["ok"],
           f"{summary['lines']} lines, levels {summary['levels']}, {summary['problem_count']} problems"
           + (f": {summary['problems'][:3]}" if summary["problems"] else ""))
    m = measure_rate("json", 2, "json", 8, max_file_mib=1, max_files=4)
    summary = check(m["directory"], "json", 1, 4)
    record("JSON format", summary["ok"],
           f"{summary['lines']} lines, levels {summary['levels']}, ts went backwards {summary['ts_went_backwards']}x, "
           f"{summary['problem_count']} problems" + (f": {summary['problems'][:3]}" if summary["problems"] else ""))


def group_rotate():
    # D1: nested directory is created; a second run appends and restarts seq at 0.
    directory = fresh_dir("append")
    env = {"LOG_RATE_MIB": "1", "LOG_DIR": "/logs/a/b", "LOG_MAX_FILE_MIB": "100", "LOG_MAX_FILES": "5"}
    for attempt in (1, 2):
        start("rlg-append", env, directory)
        time.sleep(2.2)
        stop("rlg-append")
        remove("rlg-append")
    nested = os.path.join(directory, "a", "b")
    summary = check(nested, "json", 100, 5)
    ok = summary["ok"] and len(summary["runs"]) == 2 and list(summary["files"]) == ["app.log"]
    record("directory created, second run appends", ok,
           f"files {summary['files']}; runs {[(r['run'], r['first_seq'], r['last_seq']) for r in summary['runs']]}; "
           f"{summary['problem_count']} problems")

    # D3/D2: many rotations, file count and sizes stay within the settings.
    m = measure_rate("rotation", 50, "json", 10, max_file_mib=1, max_files=3)
    summary = check(m["directory"], "json", 1, 3)
    wrote_mib = (summary["runs"][-1]["last_seq"] + 1) * LINE / MIB
    ok = summary["ok"] and sorted(summary["files"]) == ["app.log", "app.log.1", "app.log.2"]
    record("rotation keeps max_files", ok,
           f"wrote {wrote_mib:.0f} MiB (about {wrote_mib:.0f} rotations); left {summary['files']}; {summary['problem_count']} problems")

    # LOG_MAX_FILES=1.
    m = measure_rate("single", 20, "json", 5, max_file_mib=1, max_files=1)
    summary = check(m["directory"], "json", 1, 1)
    record("a single file starts over", summary["ok"] and list(summary["files"]) == ["app.log"],
           f"left {summary['files']}; {summary['problem_count']} problems")

    # D5: startup clean-up.
    directory = fresh_dir("cleanup")
    for name in ("app.log.3", "app.log.5", "app.log.9", "app.log.bak", "app.log.9.gz", "other.log.9", "notes.txt"):
        with open(os.path.join(directory, name), "wb") as handle:
            handle.write(b"x" * LINE)
    start("rlg-cleanup", {"LOG_MAX_FILES": "3", "LOG_RATE_MIB": "0.001"}, directory)
    time.sleep(1.0)
    stop("rlg-cleanup")
    remove("rlg-cleanup")
    left = sorted(os.listdir(directory))
    want = sorted(["app.log", "app.log.bak", "app.log.9.gz", "other.log.9", "notes.txt"])
    record("startup removes numbered files beyond max_files", left == want, f"left {left}")

    # Lead: an existing app.log larger than the current limit.
    directory = fresh_dir("oversize")
    with open(os.path.join(directory, "app.log"), "wb") as handle:
        handle.write((b"x" * (LINE - 1) + b"\n") * (3 * MIB // LINE))
    start("rlg-oversize", {"LOG_MAX_FILE_MIB": "1", "LOG_MAX_FILES": "3", "LOG_RATE_MIB": "0.2"}, directory)
    time.sleep(2.0)
    files_then = listing(directory)
    stop("rlg-oversize")
    remove("rlg-oversize")
    record("existing file larger than the limit", None,
           f"after start: {files_then} (limit 1048576 per file, 3145728 in total)")


# ---------------------------------------------------------------- E. stopping

def group_stop():
    for signal in ("TERM", "INT"):
        directory = fresh_dir("stop-" + signal)
        start("rlg-stop", {"LOG_RATE_MIB": "20", "LOG_MAX_FILE_MIB": "200", "LOG_MAX_FILES": "5"}, directory)
        time.sleep(3.0)
        code, seconds = stop("rlg-stop", signal)
        out, err = logs("rlg-stop")
        remove("rlg-stop")
        summary = check(directory, "json", 200, 5)
        final = re.search(r"stopped after ([0-9.]+) MiB", out)
        on_disk_mib = summary["total_bytes"] / MIB
        ok = (code == 0 and err == "" and final is not None and summary["ok"]
              and abs(float(final.group(1)) - on_disk_mib) <= 0.051 and seconds < 1.5)
        record(f"SIG{signal} stops cleanly", ok,
               f"exit {code} in {seconds:.2f} s (includes podman overhead); says 'stopped after {final.group(1) if final else '?'} MiB'; "
               f"on disk {summary['total_bytes']} bytes = {on_disk_mib:.2f} MiB, {summary['lines']} whole lines, last seq {summary['runs'][-1]['last_seq']}")


def proc_stats(pid):
    with open(f"/proc/{pid}/status") as handle:
        status = handle.read()
    rss = int(re.search(r"VmRSS:\s+(\d+) kB", status).group(1))
    threads = int(re.search(r"Threads:\s+(\d+)", status).group(1))
    return rss, len(os.listdir(f"/proc/{pid}/fd")), threads


def group_leak():
    directory = fresh_dir("leak")
    start("rlg-leak", {"LOG_RATE_MIB": "200", "LOG_MAX_FILE_MIB": "1", "LOG_MAX_FILES": "5"}, directory)
    pid = sh("podman", "inspect", "-f", "{{.State.Pid}}", "rlg-leak").stdout.strip()
    time.sleep(5)
    s1, before = last_seq(directory, "json"), proc_stats(pid)
    time.sleep(50)
    s2, after = last_seq(directory, "json"), proc_stats(pid)
    stop("rlg-leak")
    remove("rlg-leak")
    rotations = (s2 - s1) * LINE // MIB
    ok = after[1] == before[1] and after[2] == before[2] == 1 and after[0] <= before[0] + 64
    record("no growth over many rotations", ok,
           f"{rotations} rotations in 50 s; RSS {before[0]} -> {after[0]} kB; open files {before[1]} -> {after[1]}; threads {before[2]} -> {after[2]}")


# ------------------------------------------------------------ fault injection

def group_faults():
    # Read-only directory.
    directory = fresh_dir("readonly")
    code, out, err = run_to_exit("rlg-ro", {}, directory, mount_opts=":ro")
    record("read-only directory", code == 1 and "cannot open log file in /logs" in err and out == "",
           f"exit {code}; stderr {err.strip()!r}")

    # Paused process: how much is caught up afterwards.
    rate = 10
    directory = fresh_dir("pause")
    start("rlg-pause", {"LOG_RATE_MIB": str(rate), "LOG_MAX_FILE_MIB": "200", "LOG_MAX_FILES": "5"}, directory)
    time.sleep(3)
    sh("podman", "pause", "rlg-pause")
    time.sleep(0.3)
    seq_when_paused = last_seq(directory, "json")
    time.sleep(8)
    sh("podman", "unpause", "rlg-pause")
    resumed = time.monotonic()
    time.sleep(4)
    unpaused_seconds, seq_after = time.monotonic() - resumed, last_seq(directory, "json")
    stop("rlg-pause")
    out, _ = logs("rlg-pause")
    remove("rlg-pause")
    written_mib = (seq_after - seq_when_paused) * LINE / MIB
    caught_up = written_mib - rate * unpaused_seconds
    summary = check(directory, "json", 200, 5)
    peak = max(reported_rates(out), default=0.0)
    ok = summary["ok"] and 0.5 * rate <= caught_up <= 1.25 * rate
    record("paused 8 s, then resumed", ok,
           f"wrote {written_mib:.1f} MiB in the {unpaused_seconds:.1f} s after resuming = {rate * unpaused_seconds:.0f} MiB at rate + {caught_up:.1f} MiB caught up "
           f"(1 s of output is {rate} MiB; the 8 s pause was worth {rate * 8} MiB); highest reported rate {peak:.2f} MiB/s; "
           f"{summary['problem_count']} problems in the files")

    # Active file deleted while running: a new one must appear within a tick or two.
    directory = fresh_dir("deleted")
    start("rlg-del", {"LOG_RATE_MIB": "1", "LOG_MAX_FILE_MIB": "4", "LOG_MAX_FILES": "5"}, directory)
    time.sleep(2.0)
    seq_at_delete = last_seq(directory, "json")
    os.remove(os.path.join(directory, "app.log"))
    time.sleep(1.0)
    back = os.path.exists(os.path.join(directory, "app.log"))
    code, _ = stop("rlg-del")
    out, err = logs("rlg-del")
    remove("rlg-del")
    summary = check(directory, "json", 4, 5)
    first_seq_back = summary["runs"][0]["first_seq"] if summary["runs"] else None
    lost = (first_seq_back - seq_at_delete - 1) if first_seq_back is not None else None
    # One 100 ms tick at 1 MiB/s is about 410 lines; allow two ticks and the sampling delay.
    ok = back and code == 0 and err == "" and summary["ok"] and lost is not None and lost <= 1024
    record("active file deleted while running", ok,
           f"app.log back 1 s later: {back}; exit {code}; last seq before deleting {seq_at_delete}, first seq in the new file "
           f"{first_seq_back}: {lost} lines written in between are in no file")

    # Someone moves app.log away and creates a new one, as an external log rotator does.
    directory = fresh_dir("replaced")
    start("rlg-rep", {"LOG_RATE_MIB": "1", "LOG_MAX_FILE_MIB": "4", "LOG_MAX_FILES": "5"}, directory)
    time.sleep(1.5)
    os.rename(os.path.join(directory, "app.log"), os.path.join(directory, "moved.log"))
    with open(os.path.join(directory, "app.log"), "w") as handle:
        handle.write("EXTERNAL FILE\n")
    time.sleep(0.5)
    moved_then = os.path.getsize(os.path.join(directory, "moved.log"))
    time.sleep(1.5)
    moved_grew = os.path.getsize(os.path.join(directory, "moved.log")) - moved_then
    code, _ = stop("rlg-rep")
    remove("rlg-rep")
    with open(os.path.join(directory, "app.log"), "rb") as handle:
        head = handle.read(14)
        size = handle.seek(0, 2)
    ok = code == 0 and moved_grew == 0 and head == b"EXTERNAL FILE\n" and size > 14 and (size - 14) % LINE == 0
    record("active file replaced while running", ok,
           f"exit {code}; the moved file grew by {moved_grew} bytes after the first half second; the new app.log starts with "
           f"{head!r} and holds {size} bytes")

    # Rotated file deleted while running.
    directory = fresh_dir("deleted-rotated")
    start("rlg-delrot", {"LOG_RATE_MIB": "4", "LOG_MAX_FILE_MIB": "1", "LOG_MAX_FILES": "5"}, directory)
    time.sleep(2.0)
    for name in ("app.log.1", "app.log.2"):
        try:
            os.remove(os.path.join(directory, name))
        except FileNotFoundError:
            pass
    time.sleep(3.0)
    running = sh("podman", "inspect", "-f", "{{.State.Running}}", "rlg-delrot").stdout.strip()
    code, _ = stop("rlg-delrot")
    _, err = logs("rlg-delrot")
    remove("rlg-delrot")
    record("rotated files deleted while running", running == "true" and code == 0 and err == "",
           f"still running: {running}; exit {code}; files afterwards {list(listing(directory))}")

    # SIGKILL: what is on disk afterwards.
    worst = []
    for attempt in range(10):
        directory = fresh_dir("kill")
        start("rlg-kill", {"LOG_RATE_MIB": "100", "LOG_MAX_FILE_MIB": "200", "LOG_MAX_FILES": "5"}, directory)
        time.sleep(1.5 + 0.137 * attempt)
        code, _ = stop("rlg-kill", "KILL")
        out, _ = logs("rlg-kill")
        remove("rlg-kill")
        summary = check(directory, "json", 200, 5)
        reported = [float(m.group(1)) for m in re.finditer(r"\(total ([0-9.]+) MiB\)", out)]
        last_reported = reported[-1] if reported else 0.0
        worst.append((summary["ok"], summary["total_bytes"] % LINE, summary["total_bytes"] / MIB - last_reported, code))
    all_intact = all(ok and remainder == 0 for ok, remainder, _, _ in worst)
    never_behind = all(delta >= -0.051 for _, _, delta, _ in worst)
    record("SIGKILL leaves whole, consecutive lines", all_intact and never_behind,
           f"10 kills at 100 MiB/s: files intact in {sum(1 for ok, r, _, _ in worst if ok and r == 0)}/10; "
           f"disk minus last reported total: {min(d for _, _, d, _ in worst):.2f}..{max(d for _, _, d, _ in worst):.2f} MiB; exit codes {sorted({c for *_, c in worst})}")

    # Disk full: a small tmpfs volume kept mounted by a helper container.
    sh("podman", "rm", "-f", "rlg-full", "rlg-full-helper")
    sh("podman", "volume", "rm", "-f", "rlg-full")
    made = sh("podman", "volume", "create", "--opt", "type=tmpfs", "--opt", "device=tmpfs",
              "--opt", "o=size=8m,uid=65532,gid=65532,mode=0755", "rlg-full")
    helper = sh("podman", "run", "-d", "--name", "rlg-full-helper", "-v", "rlg-full:/logs",
                "docker.io/library/busybox:1.37", "sleep", "600")
    if made.returncode != 0 or helper.returncode != 0:
        # Not being able to run the check is a failure, not something to pass over.
        record("disk full", False, f"could not set up a small volume: {made.stderr.strip()} {helper.stderr.strip()}")
        return
    env = ["-e", "LOG_RATE_MIB=20", "-e", "LOG_MAX_FILE_MIB=3", "-e", "LOG_MAX_FILES=5"]

    def run_on_full_volume():
        sh("podman", "rm", "-f", "rlg-full")
        sh("podman", "run", "-d", "--name", "rlg-full", *env, "-v", "rlg-full:/logs", IMAGE)
        code = wait("rlg-full", timeout=15)
        out, err = logs("rlg-full")
        return code, out, err

    def copy_out(name):
        directory = fresh_dir(name)
        names = sh("podman", "exec", "rlg-full-helper", "ls", "/logs").stdout.split()
        for item in names:
            data = subprocess.run(["podman", "exec", "rlg-full-helper", "cat", f"/logs/{item}"], capture_output=True).stdout
            with open(os.path.join(directory, item), "wb") as handle:
                handle.write(data)
        return directory

    code, out, err = run_on_full_volume()
    directory = copy_out("full-1")
    sizes = listing(directory)
    partial = {name: size % LINE for name, size in sizes.items() if size % LINE}
    record("disk full", code == 1 and "cannot write to /logs" in err,
           f"exit {code}; stderr {err.strip()!r}; files {sizes} (8 MiB volume)")
    record("what a full disk leaves behind", None,
           f"bytes beyond the last whole line: {partial or 'none'}")

    # Free some space and start again on the same volume.
    sh("podman", "exec", "rlg-full-helper", "rm", "-f", "/logs/app.log.1", "/logs/app.log.2")
    sh("podman", "rm", "-f", "rlg-full")
    sh("podman", "run", "-d", "--name", "rlg-full", "-e", "LOG_RATE_MIB=1", "-e", "LOG_MAX_FILE_MIB=3", "-e", "LOG_MAX_FILES=2",
       "-v", "rlg-full:/logs", IMAGE)
    time.sleep(2.0)
    sh("podman", "kill", "-s", "TERM", "rlg-full")
    code2 = wait("rlg-full")
    directory = copy_out("full-2")
    summary = check(directory, "json", 3, 2)
    record("restart after a full disk", None,
           f"exit {code2}; files {summary['files']}; checker: {summary['problem_count']} problems"
           + (f", first: {summary['problems'][:2]}" if summary["problems"] else ""))
    sh("podman", "rm", "-f", "rlg-full", "rlg-full-helper")
    sh("podman", "volume", "rm", "-f", "rlg-full")


GROUPS = {"config": group_config, "rate": group_rate, "content": group_content, "rotate": group_rotate,
          "stop": group_stop, "leak": group_leak, "faults": group_faults}


def main():
    global IMAGE, BASE
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help") or any(name not in GROUPS for name in sys.argv[2:]):
        print(__doc__)
        sys.exit(2)
    IMAGE = sys.argv[1]
    chosen = sys.argv[2:] or list(GROUPS)
    BASE = tempfile.mkdtemp(prefix="rust-log-generator-e2e-")
    began = time.time()
    try:
        for name in chosen:
            print(f"== {name}", flush=True)
            try:
                GROUPS[name]()
            except Exception as error:  # a check that cannot finish has failed
                record(f"{name} group", False, f"stopped by {type(error).__name__}: {error}")
            finally:
                cleanup()
    finally:
        shutil.rmtree(BASE, ignore_errors=True)
    checks = [r for r in RESULTS if r["status"] != "INFO"]
    failed = [r for r in checks if r["status"] == "FAIL"]
    print(f"== {IMAGE}: {len(checks)} checks, {len(failed)} failed, {round(time.time() - began)} s")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
