use std::borrow::Cow;

pub(crate) fn escape_controls(value: &str) -> Cow<'_, str> {
    if !value.chars().any(char::is_control) {
        return Cow::Borrowed(value);
    }
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_terminal_control_characters_only_when_needed() {
        assert_eq!(escape_controls("owner\nname"), "owner\\nname");
        assert!(matches!(escape_controls("owner"), Cow::Borrowed("owner")));
    }
}
