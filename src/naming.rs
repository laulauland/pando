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
    use proptest::prelude::*;

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

    fn is_valid_name_spec(name: &str) -> bool {
        !name.is_empty()
            && !name.chars().any(char::is_whitespace)
            && !name.contains('/')
            && !name.contains('\\')
    }

    fn typical_valid_name() -> impl Strategy<Value = String> {
        "[A-Za-z0-9._-]{1,31}"
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

        #[test]
        fn typical_valid_names_are_accepted(name in typical_valid_name()) {
            prop_assert!(validate_name(&name).is_ok(), "{name:?} should be valid");
        }

        #[test]
        fn arbitrary_strings_are_accepted_iff_they_match_the_name_spec(name in any::<String>()) {
            prop_assert_eq!(validate_name(&name).is_ok(), is_valid_name_spec(&name), "name: {:?}", name);
        }
    }
}
