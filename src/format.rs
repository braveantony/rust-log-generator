/// Every log line is exactly this many bytes, trailing newline included, so
/// the byte rate maps directly to a line rate.
pub const LINE_BYTES: usize = 256;

/// Formats milliseconds since the Unix epoch as an RFC 3339 UTC timestamp,
/// e.g. `2001-09-09T01:46:40.000Z`.
pub fn format_timestamp(unix_millis: u64) -> String {
    let secs = unix_millis / 1000;
    let millis = unix_millis % 1000;
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Converts days since 1970-01-01 to a (year, month, day) civil date in the
/// proleptic Gregorian calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Message padding. It contains no characters that need JSON escaping.
const FILLER: &[u8] = b"the quick brown fox jumps over the lazy dog ";

fn level_for(seq: u64) -> &'static str {
    match seq % 10 {
        7 => "DEBUG",
        8 => "WARN",
        9 => "ERROR",
        _ => "INFO",
    }
}

/// Replaces the contents of `buf` with one log line of exactly `LINE_BYTES`
/// bytes, either a JSON object or plain text. `run_id` is written verbatim, so
/// it must not contain characters that need JSON escaping.
pub fn write_line(buf: &mut Vec<u8>, json: bool, run_id: &str, unix_millis: u64, seq: u64) {
    use std::io::Write;

    buf.clear();
    let ts = format_timestamp(unix_millis);
    let level = level_for(seq);
    // Writing to a Vec cannot fail.
    let suffix: &[u8] = if json {
        write!(
            buf,
            r#"{{"ts":"{ts}","level":"{level}","run":"{run_id}","seq":{seq},"msg":""#
        )
        .unwrap();
        b"\"}\n"
    } else {
        write!(buf, "{ts} {level} run={run_id} seq={seq} ").unwrap();
        b"\n"
    };
    let filler_len = LINE_BYTES - buf.len() - suffix.len();
    buf.extend(FILLER.iter().cycle().take(filler_len));
    buf.extend_from_slice(suffix);
}

#[cfg(test)]
mod tests {
    use super::*;

    const BILLENNIUM_MILLIS: u64 = 1_000_000_000_000; // 2001-09-09T01:46:40Z
    const RUN_ID: &str = "a3f9c1b2";

    fn line(json: bool, unix_millis: u64, seq: u64) -> String {
        let mut buf = Vec::new();
        write_line(&mut buf, json, RUN_ID, unix_millis, seq);
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn timestamp_of_the_epoch() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn timestamp_keeps_milliseconds() {
        assert_eq!(
            format_timestamp(BILLENNIUM_MILLIS + 123),
            "2001-09-09T01:46:40.123Z"
        );
    }

    #[test]
    fn timestamp_on_a_leap_day() {
        assert_eq!(
            format_timestamp(1_709_208_000_000),
            "2024-02-29T12:00:00.000Z"
        );
    }

    #[test]
    fn json_line_is_exactly_line_bytes_and_newline_terminated() {
        let line = line(true, BILLENNIUM_MILLIS, 42);
        assert_eq!(line.len(), LINE_BYTES);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn json_line_parses_with_the_expected_fields() {
        let line = line(true, BILLENNIUM_MILLIS, 42);
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["ts"], "2001-09-09T01:46:40.000Z");
        assert_eq!(value["level"], "INFO");
        assert_eq!(value["run"], "a3f9c1b2");
        assert_eq!(value["seq"], 42);
        assert!(!value["msg"].as_str().unwrap().is_empty());
    }

    #[test]
    fn text_line_is_exactly_line_bytes_and_newline_terminated() {
        let line = line(false, BILLENNIUM_MILLIS, 42);
        assert_eq!(line.len(), LINE_BYTES);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn text_line_starts_with_timestamp_level_run_and_sequence() {
        let line = line(false, BILLENNIUM_MILLIS, 42);
        assert!(
            line.starts_with("2001-09-09T01:46:40.000Z INFO run=a3f9c1b2 seq=42 "),
            "{line}"
        );
    }

    #[test]
    fn text_line_is_not_json() {
        let line = line(false, BILLENNIUM_MILLIS, 42);
        assert!(serde_json::from_str::<serde_json::Value>(&line).is_err());
    }

    #[test]
    fn line_length_does_not_depend_on_sequence_width_or_level() {
        for seq in [0, 7, 8, 9, 1_000_000, u64::MAX] {
            assert_eq!(line(true, BILLENNIUM_MILLIS, seq).len(), LINE_BYTES);
            assert_eq!(line(false, BILLENNIUM_MILLIS, seq).len(), LINE_BYTES);
        }
    }

    #[test]
    fn levels_vary_with_the_sequence_number() {
        let levels: Vec<String> = (0..10)
            .map(|seq| {
                let value: serde_json::Value =
                    serde_json::from_str(&line(true, BILLENNIUM_MILLIS, seq)).unwrap();
                value["level"].as_str().unwrap().to_string()
            })
            .collect();
        for expected in ["INFO", "DEBUG", "WARN", "ERROR"] {
            assert!(levels.iter().any(|l| l == expected), "missing {expected}");
        }
    }

    #[test]
    fn write_line_replaces_previous_buffer_contents() {
        let mut buf = b"stale".to_vec();
        write_line(&mut buf, true, RUN_ID, BILLENNIUM_MILLIS, 1);
        assert_eq!(buf.len(), LINE_BYTES);
        assert_eq!(buf[0], b'{');
    }
}
