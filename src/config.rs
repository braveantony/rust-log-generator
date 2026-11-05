use std::path::PathBuf;

#[derive(Debug, PartialEq)]
pub struct Config {
    pub rate_bytes_per_sec: f64,
    pub json: bool,
    pub dir: PathBuf,
    pub max_file_bytes: u64,
    pub max_files: usize,
}

impl Config {
    /// Builds the configuration from a variable lookup (normally the process
    /// environment). Unset variables fall back to defaults; invalid values are
    /// rejected with a message naming the offending variable.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        // Validated after converting to bytes: a huge but finite MiB value can
        // still overflow to infinity in the conversion. Below one byte per
        // second the pacer never asks for a whole line, so the program would
        // run without ever writing anything.
        let rate_bytes_per_sec = match lookup("LOG_RATE_MIB") {
            None => MIB as f64,
            Some(raw) => match raw.trim().parse::<f64>().map(|mib| mib * MIB as f64) {
                Ok(bytes) if bytes.is_finite() && bytes >= 1.0 => bytes,
                Ok(bytes) if bytes > 0.0 && bytes < 1.0 => {
                    return Err(format!(
                        "LOG_RATE_MIB must be at least 1 byte per second \
                         (about 0.00000096 MiB), got {raw:?}"
                    ))
                }
                _ => {
                    return Err(format!(
                        "LOG_RATE_MIB must be a positive number, got {raw:?}"
                    ))
                }
            },
        };

        let json = match lookup("LOG_JSON") {
            None => true,
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(format!("LOG_JSON must be true or false, got {raw:?}")),
            },
        };

        let dir = match lookup("LOG_DIR") {
            None => PathBuf::from("/logs"),
            Some(raw) if raw.is_empty() => return Err("LOG_DIR must not be empty".to_string()),
            Some(raw) => PathBuf::from(raw),
        };

        let max_file_mib = positive_integer(&lookup, "LOG_MAX_FILE_MIB", 100)?;
        let max_file_count = positive_integer(&lookup, "LOG_MAX_FILES", 5)?;

        // Rotation keeps at most max_file_count files of max_file_mib each, so
        // their product is the disk space the log files can take. It must be
        // capped here because the storage may not do it: a volume without an
        // enforced quota lets the files fill the disk it shares with others.
        // An overflowing product is certainly above any cap.
        let max_total_mib = positive_integer(&lookup, "LOG_MAX_TOTAL_MIB", 1024)?;
        let within_cap = max_file_mib
            .checked_mul(max_file_count)
            .is_some_and(|total_mib| total_mib <= max_total_mib);
        if !within_cap {
            return Err(format!(
                "LOG_MAX_FILE_MIB ({max_file_mib}) x LOG_MAX_FILES ({max_file_count}) \
                 exceeds LOG_MAX_TOTAL_MIB ({max_total_mib}), the cap on disk space \
                 used by log files"
            ));
        }

        let max_file_bytes = max_file_mib
            .checked_mul(MIB)
            .ok_or_else(|| "LOG_MAX_FILE_MIB is too large".to_string())?;

        let max_files = usize::try_from(max_file_count)
            .map_err(|_| "LOG_MAX_FILES is too large".to_string())?;

        Ok(Config {
            rate_bytes_per_sec,
            json,
            dir,
            max_file_bytes,
            max_files,
        })
    }
}

pub const MIB: u64 = 1024 * 1024;

fn positive_integer(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: u64,
) -> Result<u64, String> {
    match lookup(name) {
        None => Ok(default),
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(value) if value > 0 => Ok(value),
            _ => Err(format!("{name} must be a positive integer, got {raw:?}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_from(vars: &[(&str, &str)]) -> Result<Config, String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|key| map.get(key).cloned())
    }

    #[test]
    fn uses_defaults_when_nothing_is_set() {
        let config = config_from(&[]).unwrap();
        assert_eq!(
            config,
            Config {
                rate_bytes_per_sec: 1024.0 * 1024.0,
                json: true,
                dir: PathBuf::from("/logs"),
                max_file_bytes: 100 * 1024 * 1024,
                max_files: 5,
            }
        );
    }

    #[test]
    fn reads_every_variable() {
        let config = config_from(&[
            ("LOG_RATE_MIB", "10"),
            ("LOG_JSON", "false"),
            ("LOG_DIR", "/data/out"),
            ("LOG_MAX_FILE_MIB", "20"),
            ("LOG_MAX_FILES", "3"),
        ])
        .unwrap();
        assert_eq!(
            config,
            Config {
                rate_bytes_per_sec: 10.0 * 1024.0 * 1024.0,
                json: false,
                dir: PathBuf::from("/data/out"),
                max_file_bytes: 20 * 1024 * 1024,
                max_files: 3,
            }
        );
    }

    #[test]
    fn accepts_a_fractional_rate() {
        let config = config_from(&[("LOG_RATE_MIB", "0.5")]).unwrap();
        assert_eq!(config.rate_bytes_per_sec, 512.0 * 1024.0);
    }

    #[test]
    fn rejects_a_rate_that_is_not_a_positive_finite_number() {
        // 1e305 MiB is finite, but overflows to infinity once converted to bytes.
        for bad in ["abc", "", "0", "-1", "NaN", "inf", "1e305"] {
            let err = config_from(&[("LOG_RATE_MIB", bad)]).unwrap_err();
            assert!(err.contains("LOG_RATE_MIB"), "{bad:?} gave: {err}");
        }
    }

    #[test]
    fn rejects_a_rate_below_one_byte_per_second() {
        // Such a rate never adds up to a whole line: the program would run
        // without ever writing anything.
        for too_slow in ["0.0000005", "1e-300", "0.00000095"] {
            let err = config_from(&[("LOG_RATE_MIB", too_slow)]).unwrap_err();
            assert!(err.contains("LOG_RATE_MIB"), "{too_slow:?} gave: {err}");
            assert!(err.contains("1 byte per second"), "{too_slow:?} gave: {err}");
        }
    }

    #[test]
    fn accepts_a_rate_of_exactly_one_byte_per_second() {
        // 2^-20 MiB is one byte.
        let config = config_from(&[("LOG_RATE_MIB", "0.00000095367431640625")]).unwrap();
        assert_eq!(config.rate_bytes_per_sec, 1.0);
    }

    #[test]
    fn json_flag_ignores_case() {
        assert!(!config_from(&[("LOG_JSON", "FALSE")]).unwrap().json);
        assert!(config_from(&[("LOG_JSON", "True")]).unwrap().json);
    }

    #[test]
    fn rejects_a_json_flag_that_is_not_true_or_false() {
        let err = config_from(&[("LOG_JSON", "yes")]).unwrap_err();
        assert!(err.contains("LOG_JSON"), "{err}");
    }

    #[test]
    fn rejects_an_empty_dir() {
        let err = config_from(&[("LOG_DIR", "")]).unwrap_err();
        assert!(err.contains("LOG_DIR"), "{err}");
    }

    #[test]
    fn rejects_a_max_file_size_that_is_not_a_positive_integer() {
        for bad in ["0", "-5", "1.5", "big"] {
            let err = config_from(&[("LOG_MAX_FILE_MIB", bad)]).unwrap_err();
            assert!(err.contains("LOG_MAX_FILE_MIB"), "{bad:?} gave: {err}");
        }
    }

    #[test]
    fn rejects_a_max_file_count_that_is_not_a_positive_integer() {
        for bad in ["0", "-1", "two"] {
            let err = config_from(&[("LOG_MAX_FILES", bad)]).unwrap_err();
            assert!(err.contains("LOG_MAX_FILES"), "{bad:?} gave: {err}");
        }
    }

    #[test]
    fn rejects_rotation_settings_above_the_default_total_cap() {
        // 300 MiB x 4 files = 1200 MiB, above the default cap of 1024 MiB.
        let err =
            config_from(&[("LOG_MAX_FILE_MIB", "300"), ("LOG_MAX_FILES", "4")]).unwrap_err();
        assert!(err.contains("LOG_MAX_TOTAL_MIB"), "{err}");
    }

    #[test]
    fn accepts_rotation_settings_exactly_at_the_total_cap() {
        let config =
            config_from(&[("LOG_MAX_FILE_MIB", "256"), ("LOG_MAX_FILES", "4")]).unwrap();
        assert_eq!(config.max_file_bytes, 256 * 1024 * 1024);
        assert_eq!(config.max_files, 4);
    }

    #[test]
    fn the_total_cap_can_be_raised() {
        let config = config_from(&[
            ("LOG_MAX_FILE_MIB", "300"),
            ("LOG_MAX_FILES", "4"),
            ("LOG_MAX_TOTAL_MIB", "1200"),
        ])
        .unwrap();
        assert_eq!(config.max_files, 4);
    }

    #[test]
    fn the_total_cap_can_be_lowered_below_the_default_rotation_settings() {
        // The defaults keep 5 x 100 MiB = 500 MiB.
        let err = config_from(&[("LOG_MAX_TOTAL_MIB", "400")]).unwrap_err();
        assert!(err.contains("LOG_MAX_TOTAL_MIB"), "{err}");
    }

    #[test]
    fn rejects_a_total_cap_that_is_not_a_positive_integer() {
        for bad in ["0", "-1", "1.5", "lots"] {
            let err = config_from(&[("LOG_MAX_TOTAL_MIB", bad)]).unwrap_err();
            assert!(err.contains("LOG_MAX_TOTAL_MIB"), "{bad:?} gave: {err}");
        }
    }

    #[test]
    fn a_total_that_overflows_is_rejected_rather_than_wrapped() {
        // 2^32 MiB x 2^32 files wraps to 0 in 64-bit arithmetic.
        let err = config_from(&[
            ("LOG_MAX_FILE_MIB", "4294967296"),
            ("LOG_MAX_FILES", "4294967296"),
            ("LOG_MAX_TOTAL_MIB", "18446744073709551615"),
        ])
        .unwrap_err();
        assert!(err.contains("LOG_MAX_TOTAL_MIB"), "{err}");
    }
}
