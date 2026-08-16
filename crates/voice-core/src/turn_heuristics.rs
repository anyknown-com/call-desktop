//! Cheap, local first pass at "has the user finished?". Port of voice/src/core/turn-heuristics.ts.

use crate::echo_filter::{is_cjk, normalize, tokenize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnVerdict {
    Complete,
    Incomplete,
}

/// Utterances ending in these are almost never a finished thought.
const TRAILING_INCOMPLETE: &[&str] = &[
    // zh
    "然後", "就是", "所以", "因為", "但是", "不過", "而且", "還有", "或是", "或者", "的話", "那個", "這個",
    "呃", "嗯", "欸", "那", "就", "跟", "和", "是", "在", "把", "如果", "雖然", "可是", "然後就是",
    // en
    "and", "but", "so", "because", "or", "um", "uh", "like", "then", "which", "that", "if", "when",
    "to", "the", "a", "an", "with", "for", "of", "in", "on", "at", "i", "it's", "is", "are", "was", "i'm",
    "you know", "i mean", "kind of", "sort of",
];

fn ends_with_any(s: &str, chars: &[char]) -> bool {
    s.trim_end().chars().last().is_some_and(|c| chars.contains(&c))
}

/// Returns `None` when the LLM should decide.
/// - trailing filler/conjunction → incomplete
/// - explicit question → complete
/// - very short bare utterance → let the LLM decide
pub fn heuristic_turn_verdict(utterance: &str) -> Option<TurnVerdict> {
    let raw = utterance.trim();
    if raw.is_empty() {
        return Some(TurnVerdict::Incomplete);
    }
    if ends_with_any(raw, &['?', '？']) {
        return Some(TurnVerdict::Complete);
    }
    let norm = normalize(raw);
    let tokens: Vec<&str> = norm.split(' ').filter(|t| !t.is_empty()).collect();
    let last_two = tokens.iter().rev().take(2).rev().copied().collect::<Vec<_>>().join(" ");
    let last = tokens.last().copied().unwrap_or("");
    for w in TRAILING_INCOMPLETE {
        let hit = if w.contains(' ') { last_two == *w } else { last == *w || (w.chars().any(is_cjk) && norm.ends_with(w)) };
        if hit {
            return Some(TurnVerdict::Incomplete);
        }
    }
    if ends_with_any(raw, &['。', '！', '!']) && tokenize(&norm).len() >= 4 {
        return Some(TurnVerdict::Complete);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use TurnVerdict::*;

    #[test]
    fn trailing_conjunctions_incomplete() {
        for u in ["我覺得這樣做然後", "所以我就想說，就是", "It's like, um", "I want to build something that", "因為"] {
            assert_eq!(heuristic_turn_verdict(u), Some(Incomplete), "{u}");
        }
    }
    #[test]
    fn questions_complete() {
        assert_eq!(heuristic_turn_verdict("你覺得呢？"), Some(Complete));
        assert_eq!(heuristic_turn_verdict("what do you think?"), Some(Complete));
    }
    #[test]
    fn firm_punctuation_complete() {
        assert_eq!(heuristic_turn_verdict("我今天想把這個功能做完。"), Some(Complete));
    }
    #[test]
    fn ambiguous_none() {
        assert_eq!(heuristic_turn_verdict("我在想那個功能"), None);
        assert_eq!(heuristic_turn_verdict("maybe we should refactor the queue"), None);
    }
    #[test]
    fn empty_incomplete() {
        assert_eq!(heuristic_turn_verdict("  "), Some(Incomplete));
    }
}
