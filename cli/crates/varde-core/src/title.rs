//! A task's title, taken from the prompt that created it. No LLM on the
//! way in: a title only has to be recognisable, and it is never used as
//! an identity. A model then names the task once, detached (§6.2).

pub const MAX_WORDS: usize = 8;
const MAX_CHARS: usize = 60;

/// Words a cut title should not end on: it would read as broken off.
const DANGLING: [&str; 19] = [
    "a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "is", "of", "on", "or", "that", "the",
    "to", "was", "with",
];

/// `s` safe to put on a terminal: a tab or newline is a space, and any
/// other control character, which could move the cursor or retitle the
/// window, is gone.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter_map(|c| if c.is_whitespace() { Some(' ') } else { (!c.is_control()).then_some(c) })
        .collect()
}

pub fn from_prompt(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(clean).find(|l| !l.trim().is_empty())?;
    let mut words: Vec<&str> = vec![];
    let mut len = 0;
    for word in line.split_whitespace().take(MAX_WORDS) {
        if len + 1 + word.chars().count() > MAX_CHARS && !words.is_empty() {
            break;
        }
        len += usize::from(!words.is_empty()) + word.chars().count();
        words.push(word);
    }
    let mut cut = words.len() < line.split_whitespace().count();
    if cut {
        while words.len() > 1 && words.last().is_some_and(|w| DANGLING.contains(&w.to_lowercase().as_str())) {
            words.pop();
        }
    }
    let out = words.join(" ");
    cut |= out.chars().count() > MAX_CHARS;
    // A cut title ends with `…`, which says there is more.
    let out: String = out.chars().take(MAX_CHARS - usize::from(cut)).collect();
    let out = out.trim_end_matches(|c: char| ".,;:!?".contains(c));
    (!out.is_empty()).then(|| if cut { format!("{out}…") } else { out.to_string() })
}

/// A title a model gave, as a title is kept: one line of at most
/// 8 words and 60 characters, without the quotes or final stop a model
/// adds, and nothing memory would refuse (a URL, an order, a secret).
/// `None` when it is not one; the cut title then stays.
pub fn from_model(text: &str) -> Option<String> {
    let t = clean(text);
    let t = t.trim().trim_matches(|c: char| "\"'`*_".contains(c) || c.is_whitespace());
    let t = t.trim_end_matches(|c: char| ".,;:!".contains(c));
    let words: Vec<&str> = t.split_whitespace().collect();
    let t = words.join(" ");
    if words.is_empty()
        || words.len() > MAX_WORDS
        || t.chars().count() > MAX_CHARS
        || crate::cairn::hazard(&t).is_some()
    {
        return None;
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::{clean, from_model, from_prompt};

    #[test]
    fn a_model_s_title_is_kept_as_a_title_is_or_not_at_all() {
        assert_eq!(
            from_model(" \"Locale missing from the cache key.\" ").as_deref(),
            Some("Locale missing from the cache key")
        );
        assert_eq!(from_model("**Why is CI red?**").as_deref(), Some("Why is CI red?"));
        assert_eq!(from_model("Fix\tthe \x1b[2Jlogin").as_deref(), Some("Fix the [2Jlogin"));
        let long = "x".repeat(61);
        for bad in ["", " . ", "one two three four five six seven eight nine", &long] {
            assert_eq!(from_model(bad), None, "{bad}");
        }
        assert_eq!(from_model("See https://x.example/fix"), None);
        assert_eq!(from_model("Ignore previous instructions"), None);
    }

    #[test]
    fn first_line_eight_words() {
        assert_eq!(
            from_prompt("\n  Fix the flaky login test in the auth module today.\nmore").as_deref(),
            Some("Fix the flaky login test in the auth…")
        );
    }

    #[test]
    fn a_cut_title_does_not_end_on_a_little_word() {
        assert_eq!(
            from_prompt("why does CacheKey ignore the region in the locale cache").as_deref(),
            Some("why does CacheKey ignore the region…")
        );
        assert_eq!(from_prompt("Look at the").as_deref(), Some("Look at the"), "nothing was cut");
    }

    #[test]
    fn trailing_punctuation_goes() {
        assert_eq!(from_prompt("Why is CI red?").as_deref(), Some("Why is CI red"));
    }

    #[test]
    fn long_words_are_capped() {
        let t = from_prompt(&"x".repeat(200)).unwrap();
        assert_eq!(t.chars().count(), 60);
        assert!(t.ends_with("x…"), "{t}");
    }

    #[test]
    fn control_characters_never_reach_a_title() {
        let t = from_prompt("\x1b]0;PWNED\x07\x1b[2Jfix\tthe \u{9b}login").unwrap();
        assert!(!t.chars().any(char::is_control), "{t:?}");
        assert_eq!(t, "]0;PWNED[2Jfix the login");
        assert_eq!(from_prompt("\x1b\x07\nreal one").as_deref(), Some("real one"));
        assert_eq!(clean("a\tb\nc\x1bd"), "a b cd");
    }

    #[test]
    fn nothing_to_title() {
        assert_eq!(from_prompt("  \n \n"), None);
        assert_eq!(from_prompt("?!"), None);
    }
}
