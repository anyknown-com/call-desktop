//! Classifies a transcript that arrived while the assistant was speaking.
//!
//! AEC is the first line of defence; this is the second: if what the mic "heard" is mostly
//! what the speaker was playing, it's echo, not the user. Also filters pure backchannels
//! ("嗯", "uh-huh") so they don't cut the AI off, while explicit stop words always count as a
//! real interruption. Port of voice/src/core/echo-filter.ts.

use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptionKind {
    Empty,
    Echo,
    Backchannel,
    Speech,
}

const STOP_WORDS: &[&str] = &[
    "stop", "wait", "hold on", "hang on", "shut up", "pause", "no no",
    "等一下", "等等", "停", "停一下", "暫停", "先別", "不對", "不是", "閉嘴", "夠了",
];

const BACKCHANNELS: &[&str] = &[
    "嗯", "嗯嗯", "喔", "哦", "噢", "對", "對對", "好", "好的", "是", "是的", "okay", "ok",
    "yeah", "yes", "yep", "uh", "uh huh", "uh-huh", "mm", "mhm", "mm hmm", "hmm", "right",
    "sure", "i see", "aha", "ah", "oh", "wow", "嘿", "欸", "呃", "啊", "哈", "哈哈",
];

fn backchannels() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| BACKCHANNELS.iter().copied().collect())
}

fn punct_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"[\p{P}\p{S}]").unwrap())
}

pub fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

/// Lowercase, punctuation/symbols → space, collapse whitespace, trim.
pub fn normalize(text: &str) -> String {
    let lower = text.to_lowercase();
    let no_punct = punct_re().replace_all(&lower, " ");
    no_punct.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Tokens: latin words as-is, each CJK char as its own token.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in normalize(text).split(' ') {
        if word.is_empty() {
            continue;
        }
        if word.chars().any(is_cjk) {
            let mut latin = String::new();
            for ch in word.chars() {
                if is_cjk(ch) {
                    if !latin.is_empty() {
                        out.push(std::mem::take(&mut latin));
                    }
                    out.push(ch.to_string());
                } else {
                    latin.push(ch);
                }
            }
            if !latin.is_empty() {
                out.push(latin);
            }
        } else {
            out.push(word.to_string());
        }
    }
    out
}

fn bigrams(tokens: &[String]) -> Vec<String> {
    if tokens.len() < 2 {
        return tokens.to_vec();
    }
    tokens.windows(2).map(|w| format!("{} {}", w[0], w[1])).collect()
}

/// Fraction of `a`'s bigrams (or unigrams if too short) present in `b`.
pub fn coverage(a: &str, b: &str) -> f64 {
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let ga = bigrams(&ta);
    let gb: HashSet<String> = bigrams(&tb).into_iter().collect();
    let hit = ga.iter().filter(|g| gb.contains(*g)).count();
    hit as f64 / ga.len() as f64
}

/// Explicit "stop" commands — always a real interruption. Only matched as short commands, so
/// an echo of the AI saying "這不是問題" doesn't trip on "不是".
pub fn is_stop_command(transcript: &str) -> bool {
    let norm = normalize(transcript);
    if norm.is_empty() {
        return false;
    }
    if tokenize(&norm).len() > 6 {
        return false;
    }
    STOP_WORDS.iter().any(|w| norm.contains(w))
}

pub const DEFAULT_ECHO_THRESHOLD: f64 = 0.6;

pub fn classify_interruption(transcript: &str, echo_horizon: &str, echo_threshold: f64) -> InterruptionKind {
    let norm = normalize(transcript);
    if norm.is_empty() {
        return InterruptionKind::Empty;
    }
    let tokens = tokenize(&norm);
    if tokens.is_empty() {
        return InterruptionKind::Empty;
    }
    if is_stop_command(transcript) {
        return InterruptionKind::Speech;
    }
    let bc = backchannels();
    if bc.contains(norm.as_str()) {
        return InterruptionKind::Backchannel;
    }
    // Repeated backchannel like "嗯嗯嗯" / "ok ok".
    if tokens.len() <= 3 && tokens.iter().all(|t| bc.contains(t.as_str())) {
        return InterruptionKind::Backchannel;
    }
    if !echo_horizon.is_empty() {
        let cov = coverage(&norm, echo_horizon);
        // Short transcripts: any overlap with what we were saying is almost surely echo.
        let threshold = if tokens.len() <= 3 { 0.5 } else { echo_threshold };
        if cov >= threshold {
            return InterruptionKind::Echo;
        }
    }
    InterruptionKind::Speech
}

#[cfg(test)]
mod tests {
    use super::*;
    use InterruptionKind::*;

    const H: &str = "今天的天氣非常好，我們可以去公園散步。The weather is lovely today.";
    fn c(t: &str) -> InterruptionKind {
        classify_interruption(t, H, DEFAULT_ECHO_THRESHOLD)
    }

    #[test]
    fn tokenize_splits_latin_words_and_cjk_chars() {
        assert_eq!(tokenize("Hello, 世界 world!"), vec!["hello", "世", "界", "world"]);
    }
    #[test]
    fn empty() {
        assert_eq!(c(""), Empty);
        assert_eq!(c(" ... "), Empty);
    }
    #[test]
    fn echo() {
        assert_eq!(c("我們可以去公園散步"), Echo);
        assert_eq!(c("the weather is lovely"), Echo);
        assert_eq!(c("天氣非常好"), Echo);
        assert_eq!(c("今天的天氣非常好我們可以去公"), Echo);
    }
    #[test]
    fn real_speech_sharing_words() {
        assert_eq!(c("我不想去公園，我想去海邊看夕陽"), Speech);
        assert_eq!(c("what about tomorrow's weather forecast"), Speech);
    }
    #[test]
    fn backchannels() {
        assert_eq!(c("嗯"), Backchannel);
        assert_eq!(c("uh huh"), Backchannel);
        assert_eq!(c("嗯嗯 好"), Backchannel);
    }
    #[test]
    fn stop_words() {
        assert_eq!(c("等一下"), Speech);
        assert_eq!(c("wait wait"), Speech);
        assert_eq!(c("停"), Speech);
    }
    #[test]
    fn no_horizon() {
        assert_eq!(classify_interruption("我們可以去公園散步", "", DEFAULT_ECHO_THRESHOLD), Speech);
    }
    #[test]
    fn coverage_identical_is_one() {
        assert_eq!(coverage("hello world again", "hello world again"), 1.0);
    }
}
