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
        // still overflow to infinity in the conversion.
        let rate_bytes_per_sec = match lookup("LOG_RATE_MIB") {
            None => MIB as f64,
            Some(raw) => match raw.trim().parse::<f64>().map(|mib| mib * MIB as f64) {
                Ok(bytes) if bytes.is_finite() && bytes > 0.0 => bytes,
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

        let max_file_bytes = positive_integer(&lookup, "LOG_MAX_FILE_MIB", 100)?
            .checked_mul(MIB)
            .ok_or_else(|| "LOG_MAX_FILE_MIB is too large".to_string())?;

        let max_files = usize::try_from(positive_integer(&lookup, "LOG_MAX_FILES", 5)?)
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
}
