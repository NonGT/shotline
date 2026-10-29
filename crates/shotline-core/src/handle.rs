//! @handles in prompts, like `@paka` or `@deep-forest`.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub handle: String,
    /// Byte range of the whole mention including the `@`.
    pub span: Range<usize>,
}

fn is_handle_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

pub fn is_valid(handle: &str) -> bool {
    handle
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && !handle.ends_with(['-', '_'])
        && handle.chars().all(is_handle_char)
}

pub fn find(text: &str) -> Vec<Mention> {
    let mut out = vec![];
    let mut prev: Option<char> = None;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // An ASCII letter or digit right before @ means an email address, not a mention.
        // Thai text can sit right against a handle, so only ASCII counts here.
        if c == '@' && !prev.is_some_and(|p| p.is_ascii_alphanumeric()) {
            let start = i + 1;
            let mut end = start;
            while let Some(&(j, h)) = chars.peek() {
                if !is_handle_char(h) {
                    break;
                }
                end = j + h.len_utf8();
                chars.next();
            }
            let handle = text[start..end].trim_end_matches(['-', '_']);
            if !handle.is_empty() {
                out.push(Mention {
                    handle: handle.to_string(),
                    span: i..start + handle.len(),
                });
            }
            prev = text[..end].chars().next_back();
            continue;
        }
        prev = Some(c);
    }
    out
}

/// Handles in order of first mention, without repeats.
pub fn unique(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for m in find(text) {
        if !out.contains(&m.handle) {
            out.push(m.handle);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handles(text: &str) -> Vec<String> {
        find(text).into_iter().map(|m| m.handle).collect()
    }

    #[test]
    fn finds_handles() {
        assert_eq!(
            handles("@mali films while @kan argues with @paka."),
            ["mali", "kan", "paka"]
        );
        assert_eq!(handles("into @deep-forest, then"), ["deep-forest"]);
    }

    #[test]
    fn trims_trailing_dashes() {
        assert_eq!(handles("@paka-- and @kan_"), ["paka", "kan"]);
    }

    #[test]
    fn skips_emails_and_bare_at() {
        assert!(handles("mail me at a@b.com or @ noon").is_empty());
    }

    #[test]
    fn works_next_to_thai() {
        assert_eq!(handles("ผกา@paka เดิน"), ["paka"]);
    }

    #[test]
    fn span_covers_the_mention() {
        let text = "hi @paka-, ok";
        let m = &find(text)[0];
        assert_eq!(&text[m.span.clone()], "@paka");
    }

    #[test]
    fn unique_keeps_first_order() {
        assert_eq!(unique("@b @a @b"), ["b", "a"]);
    }

    #[test]
    fn validity() {
        assert!(is_valid("deep-forest"));
        assert!(is_valid("kan2"));
        assert!(!is_valid(""));
        assert!(!is_valid("-x"));
        assert!(!is_valid("x-"));
        assert!(!is_valid("Paka"));
        assert!(!is_valid("ผกา"));
    }
}
