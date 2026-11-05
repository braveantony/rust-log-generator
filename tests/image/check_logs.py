#!/usr/bin/env python3
"""Independent check of the files rust-log-generator leaves in a directory.

It deliberately shares no code with the program: lines are judged by Python's
standard-library parsers (json, datetime) and by plain byte counting. Prints a
JSON summary; exits 1 if any promise checked here is violated.

usage: check_logs.py DIR --format json|text --max-file-bytes N --max-files N
"""
import argparse
import datetime
import json
import os
import re
import sys

LINE_BYTES = 256
# RFC 3339 "date-time" with exactly three fractional digits and a Z offset.
RFC3339_MS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
RUN_ID = re.compile(r"^[0-9a-f]{8}$")
LEVELS = {7: "DEBUG", 8: "WARN", 9: "ERROR"}
TEXT_LINE = re.compile(r"^(\S+) (\S+) run=(\S+) seq=(\d+) (.*)$")


def expected_level(seq):
    return LEVELS.get(seq % 10, "INFO")


def parse_ts(text, problems, where):
    if not RFC3339_MS.match(text):
        problems.append(f"{where}: ts {text!r} is not RFC 3339 with milliseconds and Z")
        return None
    try:
        return datetime.datetime.fromisoformat(text)
    except ValueError as err:
        problems.append(f"{where}: ts {text!r} is not a real date-time ({err})")
        return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("dir")
    parser.add_argument("--format", choices=["json", "text"], required=True)
    parser.add_argument("--max-file-bytes", type=int, required=True)
    parser.add_argument("--max-files", type=int, required=True)
    parser.add_argument("--allow-oversize", action="store_true",
                        help="do not fail on files larger than --max-file-bytes")
    args = parser.parse_args()

    problems = []
    names = sorted(os.listdir(args.dir))
    numbered = {}
    other = []
    for name in names:
        if name == "app.log":
            continue
        match = re.fullmatch(r"app\.log\.(\d+)", name)
        if match:
            numbered[int(match.group(1))] = name
        else:
            other.append(name)

    log_files = [numbered[i] for i in sorted(numbered, reverse=True)]  # oldest first
    if "app.log" in names:
        log_files.append("app.log")
    if len(log_files) > args.max_files:
        problems.append(f"{len(log_files)} log files, more than max_files={args.max_files}")
    if numbered and sorted(numbered) != list(range(1, len(numbered) + 1)):
        problems.append(f"rotated files are not numbered 1..N without gaps: {sorted(numbered)}")

    sizes = {}
    lines_total = 0
    runs = []            # [run id, first seq, last seq, line count]
    ts_backwards = 0
    prev_ts = None
    prev_run = None
    prev_seq = None
    level_counts = {}

    for name in log_files:
        path = os.path.join(args.dir, name)
        with open(path, "rb") as handle:
            data = handle.read()
        sizes[name] = len(data)
        if len(data) > args.max_file_bytes and not args.allow_oversize:
            problems.append(f"{name}: {len(data)} bytes, more than max_file_bytes={args.max_file_bytes}")
        if len(data) % LINE_BYTES:
            problems.append(f"{name}: {len(data)} bytes is not a multiple of {LINE_BYTES}")
        for offset in range(0, len(data) - len(data) % LINE_BYTES, LINE_BYTES):
            raw = data[offset:offset + LINE_BYTES]
            where = f"{name}@{offset}"
            if raw[-1:] != b"\n" or b"\n" in raw[:-1]:
                problems.append(f"{where}: not exactly one line of {LINE_BYTES} bytes")
                continue
            try:
                text = raw[:-1].decode("ascii")
            except UnicodeDecodeError:
                problems.append(f"{where}: not ASCII")
                continue
            if args.format == "json":
                try:
                    obj = json.loads(text)
                except json.JSONDecodeError as err:
                    problems.append(f"{where}: invalid JSON ({err})")
                    continue
                if not isinstance(obj, dict) or list(obj) != ["ts", "level", "run", "seq", "msg"]:
                    problems.append(f"{where}: unexpected fields {list(obj) if isinstance(obj, dict) else type(obj)}")
                    continue
                ts, level, run, seq, msg = obj["ts"], obj["level"], obj["run"], obj["seq"], obj["msg"]
                if not (isinstance(ts, str) and isinstance(level, str) and isinstance(run, str)
                        and isinstance(seq, int) and not isinstance(seq, bool) and isinstance(msg, str)):
                    problems.append(f"{where}: field of the wrong JSON type")
                    continue
            else:
                match = TEXT_LINE.match(text)
                if not match:
                    problems.append(f"{where}: text line does not match '<ts> <level> run=<id> seq=<n> <msg>'")
                    continue
                ts, level, run, seq, msg = match.group(1), match.group(2), match.group(3), int(match.group(4)), match.group(5)

            lines_total += 1
            level_counts[level] = level_counts.get(level, 0) + 1
            if not RUN_ID.match(run):
                problems.append(f"{where}: run {run!r} is not 8 lowercase hex digits")
            if level != expected_level(seq):
                problems.append(f"{where}: level {level} but seq {seq} calls for {expected_level(seq)}")
            parsed = parse_ts(ts, problems, where)
            if parsed is not None:
                if prev_ts is not None and parsed < prev_ts:
                    ts_backwards += 1
                prev_ts = parsed

            if run != prev_run:
                runs.append([run, seq, seq, 1])
                # A new run starts at 0, unless it is simply the oldest data left on disk.
                if prev_run is not None and seq != 0:
                    problems.append(f"{where}: run {run} starts at seq {seq}, not 0")
            else:
                if seq != prev_seq + 1:
                    problems.append(f"{where}: seq jumps from {prev_seq} to {seq} within run {run}")
                runs[-1][2] = seq
                runs[-1][3] += 1
            prev_run, prev_seq = run, seq

    summary = {
        "dir": args.dir,
        "files": sizes,
        "other_files": other,
        "total_bytes": sum(sizes.values()),
        "lines": lines_total,
        "runs": [{"run": r, "first_seq": a, "last_seq": b, "lines": n} for r, a, b, n in runs],
        "levels": level_counts,
        "ts_went_backwards": ts_backwards,
        "problems": problems[:20],
        "problem_count": len(problems),
        "ok": not problems,
    }
    print(json.dumps(summary, ensure_ascii=False))
    sys.exit(0 if not problems else 1)


if __name__ == "__main__":
    main()
