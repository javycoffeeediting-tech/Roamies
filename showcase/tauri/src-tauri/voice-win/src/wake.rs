// Wake phrases ("Hey Roamies", or whatever the user set): the general form of WakeWord in voice/main.swift. A
// phrase counts at the start of something said (the text's start, up to two stray words in, or after a sentence
// end), with near-misses Parakeet really writes ("Hey Romies", "Hey Roamie's", "Hi Roamies", "Hey roam ies").

/// Greetings Parakeet swaps for one another; in a phrase and in what was heard they all read as "hey".
const GREETINGS: [&str; 6] = ["hey", "hi", "hay", "heya", "hiya", "hei"];

#[derive(Clone, Debug, PartialEq)]
pub struct Phrase {
    /// As the user typed it (for the log and the app).
    pub text: String,
    words: Vec<String>,
    joined: String,
}

impl Phrase {
    /// None for a phrase with no letters or digits in it.
    pub fn new(text: &str) -> Option<Phrase> {
        let words: Vec<String> = tokens(text).into_iter().map(|t| t.word).collect();
        if words.is_empty() {
            return None;
        }
        let joined = words.concat();
        Some(Phrase { text: text.trim().to_owned(), words, joined })
    }

    /// How many letters may differ: none for a very short phrase, more for longer ones.
    fn allowed(&self) -> usize {
        match self.joined.chars().count() {
            0..=4 => 0,
            5..=7 => 1,
            8..=12 => 2,
            _ => 3,
        }
    }
}

/// The phrases from the user's list, blanks and repeats dropped.
pub fn phrases(list: &[String]) -> Vec<Phrase> {
    let mut out: Vec<Phrase> = Vec::new();
    for p in list.iter().filter_map(|s| Phrase::new(s)) {
        if !out.iter().any(|o| o.joined == p.joined) {
            out.push(p);
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub struct Hit {
    /// What was said after the phrase ("" if nothing yet).
    pub rest: String,
    /// Which phrase (its index in the list).
    pub phrase: usize,
    pub distance: usize,
}

struct Token {
    word: String,
    start: usize,
    end: usize,
}

/// Lowercased words without apostrophes ("Roamie's" -> "roamies"), greetings as "hey", with their byte range.
fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut chars = text.char_indices().peekable();
    loop {
        let next = chars.next();
        let in_word = next.is_some_and(|(_, c)| c.is_alphanumeric() || ((c == '\'' || c == '\u{2019}') && start.is_some()));
        match (in_word, start, next) {
            (true, None, Some((i, _))) => start = Some(i),
            (false, Some(s), _) => {
                let end = next.map_or(text.len(), |(i, _)| i);
                let w: String = text[s..end].to_lowercase().chars().filter(|c| *c != '\'' && *c != '\u{2019}').collect();
                if !w.is_empty() {
                    let word = if GREETINGS.contains(&w.as_str()) { "hey".to_owned() } else { w };
                    out.push(Token { word, start: s, end });
                }
                start = None;
            }
            _ => {}
        }
        if next.is_none() {
            break;
        }
    }
    out
}

fn edits(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return a.len().max(b.len());
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = prev.clone();
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The first phrase heard at the start of something said, and what came after it.
pub fn find(phrases: &[Phrase], text: &str) -> Option<Hit> {
    let words = tokens(text);
    let mut best: Option<(usize, usize, usize, usize)> = None; // (start token, end byte, phrase, distance)
    for (i, w) in words.iter().enumerate() {
        let before = &text[if i == 0 { 0 } else { words[i - 1].end }..w.start];
        let after_stop = before.contains(['.', '?', '!']);
        for (pi, p) in phrases.iter().enumerate() {
            let n = p.words.len();
            // Up to two stray words may come first ("um", "okay"), one before a one-word phrase ("I asked Jarvis"
            // is talk about it); or it follows a sentence end.
            if i > if n == 1 { 1 } else { 2 } && !after_stop {
                continue;
            }
            if !w.word.starts_with(p.joined.chars().next().unwrap_or(' ')) {
                continue;
            }
            // The phrase's own word count, one fewer (run together: "heyroamies"), or one more (split: "roam ies").
            for len in n.saturating_sub(1).max(1)..=n + 1 {
                if i + len > words.len() {
                    break;
                }
                // Only spaces and commas between its words ("Hey, Roamies"), never a sentence end.
                if words[i..i + len].windows(2).any(|pair| text[pair[0].end..pair[1].start].chars().any(|c| c != ' ' && c != ',')) {
                    continue;
                }
                let heard: String = words[i..i + len].iter().map(|t| t.word.as_str()).collect();
                let d = edits(&heard, &p.joined);
                // A split heard as more words than the phrase has must be a closer match ("hey room is" isn't it).
                let allowed = if len > n { p.allowed().saturating_sub(1) } else { p.allowed() };
                if d <= allowed && best.is_none_or(|b| i < b.0 || (i == b.0 && d < b.3)) {
                    best = Some((i, words[i + len - 1].end, pi, d));
                }
            }
        }
        if best.is_some_and(|b| b.0 == i) {
            break;
        }
    }
    let (_, end, phrase, distance) = best?;
    let rest = text[end..].trim_start_matches(|c: char| !c.is_alphanumeric()).trim_end().to_owned();
    Some(Hit { rest, phrase, distance })
}

/// What was said with the phrase taken off the front, or the text as it is when the phrase isn't there.
pub fn strip(phrases: &[Phrase], text: &str) -> String {
    find(phrases, text).map_or_else(|| text.to_owned(), |h| h.rest)
}

// ---------- end words ("over", "go"): said last, they send what came before them at once ----------

/// The user's end words (or short phrases), each as its words, lowercase; blanks and repeats dropped.
pub fn end_words(list: &[String]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for s in list {
        // Greetings stay as written here ("hi" as an end word means "hi").
        let words: Vec<String> = raw_tokens(s).into_iter().map(|t| t.word).collect();
        if !words.is_empty() && !out.contains(&words) {
            out.push(words);
        }
    }
    out
}

/// What was said before an end word that came last ("What time is it, over." -> "What time is it"), or None when
/// the text doesn't end with one, or is nothing but one.
pub fn cut_end_word(ends: &[Vec<String>], text: &str) -> Option<String> {
    let words = raw_tokens(text);
    for end in ends {
        let n = end.len();
        if words.len() <= n || !words[words.len() - n..].iter().zip(end).all(|(t, e)| &t.word == e) {
            continue;
        }
        // Only spaces or punctuation after it: "over" in "over there" isn't an end.
        let start = words[words.len() - n].start;
        let kept = text[..start].trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '-' | '\u{2014}'));
        if !kept.is_empty() {
            return Some(kept.to_owned());
        }
    }
    None
}

/// Whether the text ends with one of the words or phrases (alone counts too): "…, cancel." or just "Never mind."
pub fn ends_with_word(list: &[Vec<String>], text: &str) -> bool {
    let words = raw_tokens(text);
    list.iter().any(|end| {
        let n = end.len();
        n > 0 && words.len() >= n && words[words.len() - n..].iter().zip(end).all(|(t, e)| &t.word == e)
    })
}

/// Words as heard, lowercase and without apostrophes, greetings left as they are.
fn raw_tokens(text: &str) -> Vec<Token> {
    let mut out = tokens(text);
    for t in &mut out {
        t.word = text[t.start..t.end].to_lowercase().chars().filter(|c| *c != '\'' && *c != '\u{2019}').collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(v: &[&str]) -> Vec<Phrase> {
        phrases(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn rest(p: &[Phrase], text: &str) -> Option<String> {
        find(p, text).map(|h| h.rest)
    }

    // The same cases as --test-wake in voice/main.swift, for the default phrase.
    #[test]
    fn hey_roamies_and_its_near_misses() {
        let p = list(&["Hey Roamies"]);
        let yes = [
            ("Hey Roamies", ""),
            ("Hey Roamies, open Safari.", "open Safari."),
            ("hey roamies open safari", "open safari"),
            ("Hey, Roamies.", ""),
            ("Hi Roamies what time is it?", "what time is it?"),
            ("Hey Romies", ""),
            ("Hey Roamie's, open Notes", "open Notes"),
            ("Hey Roamie", ""),
            ("Hey Roomies, play some music", "play some music"),
            ("Hey Romi's", ""),
            ("Hey, Romees.", ""),
            ("Hey roam ies, hello", "hello"),
            ("That's it. Hey Roamies, open Mail", "open Mail"),
            ("HEY ROAMIES! What's on today?", "What's on today?"),
            ("Hey Roamies\u{2019} open notes", "open notes"),
            ("Um hey Roamies, open Mail", "open Mail"),
            ("Okay, hi Roamies", ""),
        ];
        for (text, want) in yes {
            assert_eq!(rest(&p, text).as_deref(), Some(want), "{text:?}");
        }
        let no = [
            "Hey Romy's",
            "Hey Ronnie",
            "Hey Robbie, come here",
            "Hey Rome",
            "Hey rooms",
            "Hey ramen",
            "Hey Remy",
            "Hey rhymes",
            "Hey room is ready",
            "Roamies is great",
            "I told him hey Roamies",
            "",
        ];
        for text in no {
            assert_eq!(rest(&p, text), None, "{text:?}");
        }
    }

    #[test]
    fn custom_phrases_of_any_length() {
        let p = list(&["Jarvis", "OK computer", "  ", "Hey Roamies", "hey, roamies!"]);
        assert_eq!(p.len(), 3, "blanks and repeats dropped");
        assert_eq!(find(&p, "Jarvis, what's the weather?"), Some(Hit { rest: "what's the weather?".into(), phrase: 0, distance: 0 }));
        assert_eq!(rest(&p, "Jarvi what time is it"), Some("what time is it".into()));
        assert_eq!(rest(&p, "I asked Jarvis yesterday"), None, "not at the start");
        assert_eq!(rest(&p, "Um, Jarvis, open Chrome"), Some("open Chrome".into()), "one stray word is fine");
        assert_eq!(rest(&p, "Never mind. Jarvis, stop"), Some("stop".into()), "after a sentence end");
        assert_eq!(find(&p, "Okay computer, open Chrome").map(|h| (h.rest, h.phrase)), Some(("open Chrome".into(), 1)));
        assert_eq!(rest(&p, "OK, computer."), Some(String::new()));
        assert_eq!(rest(&p, "Okay compute this sum"), None, "too far off the phrase");
        assert_eq!(rest(&p, "hey roamies remind me at five").as_deref(), Some("remind me at five"));
    }

    #[test]
    fn a_short_phrase_must_be_exact() {
        let p = list(&["Max"]);
        assert_eq!(rest(&p, "Max, stop"), Some("stop".into()));
        assert_eq!(rest(&p, "Mac stop"), None);
    }

    #[test]
    fn end_words_said_last_cut_the_take() {
        let ends = end_words(&["Over".into(), "go".into(), "send it".into(), " ".into(), "over".into()]);
        assert_eq!(ends.len(), 3, "blanks and repeats dropped");
        let cut = |t: &str| cut_end_word(&ends, t);
        assert_eq!(cut("What time is it, over."), Some("What time is it".into()));
        assert_eq!(cut("What time is it? Over"), Some("What time is it?".into()), "a question mark stays");
        assert_eq!(cut("open my email go"), Some("open my email".into()));
        assert_eq!(cut("Write it down, send it."), Some("Write it down".into()));
        assert_eq!(cut("Is it over there"), None, "not the last word");
        assert_eq!(cut("over"), None, "nothing before it");
        assert_eq!(cut("Over."), None);
        assert_eq!(cut("Let's go!"), Some("Let's".into()), "the user's words, taken at their word");
        assert_eq!(cut("Hey, go"), Some("Hey".into()));
        assert_eq!(cut_end_word(&[], "open my email go"), None, "no end words: nothing ends early");
    }

    #[test]
    fn cancel_words_count_last_or_alone() {
        let c = end_words(&["cancel".into(), "never mind".into()]);
        assert!(ends_with_word(&c, "Cancel."));
        assert!(ends_with_word(&c, "open my email, never mind"));
        assert!(ends_with_word(&c, "Oh, never mind."));
        assert!(!ends_with_word(&c, "cancel my three o'clock meeting"), "not the last word");
        assert!(!ends_with_word(&c, "never"));
        assert!(!ends_with_word(&[], "cancel"));
    }

    #[test]
    fn strip_takes_the_phrase_off_or_leaves_the_text() {
        let p = list(&["Hey Roamies"]);
        assert_eq!(strip(&p, "Hey Roamies, open Mail."), "open Mail.");
        assert_eq!(strip(&p, "open Mail."), "open Mail.");
        assert_eq!(strip(&p, "Hey Roamies."), "");
    }
}
