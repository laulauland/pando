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

    fn valid_name() -> impl Strategy<Value = String> {
        prop::collection::vec(
            any::<char>().prop_filter("valid name character", |c| {
                !c.is_whitespace() && *c != '/' && *c != '\\'
            }),
            1..32,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        #[test]
        fn valid_generated_names_are_accepted(name in valid_name()) {
            prop_assert!(validate_name(&name).is_ok(), "{name:?} should be valid");
        }

        #[test]
        fn arbitrary_strings_are_accepted_iff_they_match_the_name_spec(name in any::<String>()) {
            let expected = !name.is_empty()
                && !name.chars().any(char::is_whitespace)
                && !name.contains('/')
                && !name.contains('\\');

            prop_assert_eq!(validate_name(&name).is_ok(), expected, "name: {:?}", name);
        }
    }
}
