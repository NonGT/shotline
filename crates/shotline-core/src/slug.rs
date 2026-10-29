/// Lowercase ASCII slug for file names. Non-ASCII text (Thai titles) drops out, so this can be empty.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Into the Forest!"), "into-the-forest");
        assert_eq!(slugify("  Dr. Kan  "), "dr-kan");
        assert_eq!(slugify("ป่าลึก"), "");
        assert_eq!(slugify("ป่า deep"), "deep");
    }
}
