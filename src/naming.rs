use anyhow::{bail, Result};
use std::path::MAIN_SEPARATOR;

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name must not be empty");
    }

    if name.chars().any(char::is_whitespace) {
        bail!("name must not contain whitespace");
    }

    if name.contains('/') || name.contains('\\') || name.contains(MAIN_SEPARATOR) {
        bail!("name must not contain path separators");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_name;

    #[test]
    fn accepts_simple_names() {
        validate_name("feature-123").unwrap();
        validate_name("abc_def.1").unwrap();
    }

    #[test]
    fn rejects_invalid_names() {
        for name in ["", "has space", "tabs\there", "a/b", "a\\b"] {
            assert!(validate_name(name).is_err(), "{name:?} should be invalid");
        }
    }
}
