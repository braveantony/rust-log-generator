use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// Returns a fresh identifier for this run of the program: 8 lowercase hex
/// digits. Sequence numbers restart at 0 on every start, so a line is only
/// unique by (run id, sequence number).
pub fn generate() -> String {
    // The standard library has no random number API, but every `RandomState`
    // is keyed from OS randomness, so hashing nothing yields a random value.
    let random = RandomState::new().build_hasher().finish();
    format!("{:08x}", random as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_is_eight_lowercase_hex_digits() {
        let id = generate();
        assert_eq!(id.len(), 8, "{id}");
        assert!(
            id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "{id}"
        );
    }

    #[test]
    fn each_run_id_is_different() {
        assert_ne!(generate(), generate());
    }
}
