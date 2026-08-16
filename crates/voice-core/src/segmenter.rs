//! Incremental sentence segmenter for streaming LLM text → TTS. Port of
//! voice/src/core/segmenter.ts, using UAX #29 sentence boundaries in place of Intl.Segmenter.

use unicode_segmentation::UnicodeSegmentation;

fn is_cjk_term(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '；')
}
fn is_latin_term(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}
fn is_soft_break(c: char) -> bool {
    matches!(c, ',' | '，' | '、' | ';' | '；' | ':' | '：')
}

/// Remove common markdown decorations before TTS.
pub fn strip_markdown(text: &str) -> String {
    use std::sync::OnceLock;
    static RES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        vec![
            (regex::Regex::new(r"```[\s\S]*?```").unwrap(), " "),
            (regex::Regex::new(r"`([^`]*)`").unwrap(), "$1"),
            (regex::Regex::new(r"\*\*([^*]*)\*\*").unwrap(), "$1"),
            (regex::Regex::new(r"__([^_]*)__").unwrap(), "$1"),
            (regex::Regex::new(r"(^|\n)\s{0,3}#{1,6}\s+").unwrap(), "$1"),
            (regex::Regex::new(r"(^|\n)\s*[-*+]\s+").unwrap(), "$1"),
            (regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap(), "$1"),
        ]
    });
    let mut s = text.to_string();
    for (re, rep) in res {
        s = re.replace_all(&s, *rep).into_owned();
    }
    s
}

/// Rough "content weight": CJK ideographs count double since each carries a word's worth.
pub fn text_weight(text: &str) -> usize {
    text.trim().chars().filter(|c| !c.is_whitespace()).map(|c| if crate::echo_filter::is_cjk(c) { 2 } else { 1 }).sum()
}

#[derive(Debug, Clone)]
pub struct SegmenterOptions {
    /// Force a cut once the pending buffer exceeds this many chars.
    pub max_chars: usize,
    /// Don't emit sentences lighter than this — merge into the next one.
    pub min_chars: usize,
}

impl Default for SegmenterOptions {
    fn default() -> Self {
        Self { max_chars: 180, min_chars: 6 }
    }
}

#[derive(Debug, Default)]
pub struct SentenceSegmenter {
    buffer: String,
    opts: SegmenterOptions,
}

impl SentenceSegmenter {
    pub fn new(opts: SegmenterOptions) -> Self {
        Self { buffer: String::new(), opts }
    }

    /// Feed a text delta; returns zero or more complete sentences.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        // Leading whitespace only ever belongs to an already-emitted sentence.
        self.buffer.push_str(delta);
        self.buffer = self.buffer.trim_start().to_string();
        let mut out = Vec::new();
        let mut carry = String::new();
        loop {
            let Some(cut) = self.find_cut(&self.buffer) else { break };
            let sentence = self.buffer[..cut].to_string();
            self.buffer = self.buffer[cut..].trim_start().to_string();
            let sentence = format!("{carry}{sentence}");
            carry.clear();
            if text_weight(&sentence) < self.opts.min_chars {
                carry = sentence;
                continue;
            }
            out.push(sentence.trim().to_string());
        }
        if !carry.is_empty() {
            self.buffer = format!("{carry}{}", self.buffer);
        }
        out
    }

    /// End of stream: return whatever is left.
    pub fn flush(&mut self) -> Vec<String> {
        let rest = self.buffer.trim().to_string();
        self.buffer.clear();
        if rest.is_empty() { vec![] } else { vec![rest] }
    }

    /// Byte index to cut at, or None if the buffer has no safe cut point yet.
    fn find_cut(&self, text: &str) -> Option<usize> {
        if text.is_empty() {
            return None;
        }
        // Newline = paragraph break, always safe.
        if let Some(nl) = text.find('\n') {
            return Some(nl + 1);
        }
        // Everything but the last UAX#29 sentence is complete.
        let mut bounds = text.split_sentence_bound_indices();
        let first = bounds.next()?;
        if bounds.next().is_some() {
            return Some(first.0 + first.1.len());
        }
        // Single (possibly incomplete) segment: cut only on an unambiguous terminator.
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        for (i, &(idx, ch)) in chars.iter().enumerate() {
            let end = idx + ch.len_utf8();
            if is_cjk_term(ch) {
                return Some(end);
            }
            if is_latin_term(ch) {
                // Trailing whitespace confirms end of sentence (avoids "3.14", "e.g.x").
                if let Some(&(_, next)) = chars.get(i + 1) {
                    if next.is_whitespace() {
                        return Some(end);
                    }
                }
            }
        }
        // Run-on text: hard cut at the last soft break or space before max_chars.
        let max = self.opts.max_chars;
        if chars.len() >= max {
            let window = &chars[..max];
            let mut i = window.len() - 1;
            while i > max / 3 {
                let (idx, ch) = window[i];
                if is_soft_break(ch) {
                    return Some(idx + ch.len_utf8());
                }
                i -= 1;
            }
            if let Some(sp) = window.iter().rposition(|(_, c)| *c == ' ') {
                if sp > max / 3 {
                    return Some(window[sp].0 + 1);
                }
            }
            return Some(window[max - 1].0 + window[max - 1].1.len_utf8());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(deltas: &[&str], opts: SegmenterOptions) -> (Vec<String>, Vec<String>) {
        let mut seg = SentenceSegmenter::new(opts);
        let mut out = Vec::new();
        for d in deltas {
            out.extend(seg.push(d));
        }
        let rest = seg.flush();
        (out, rest)
    }

    #[test]
    fn latin_sentences_need_following_whitespace() {
        let mut seg = SentenceSegmenter::default();
        assert!(seg.push("Hello there.").is_empty());
        assert_eq!(seg.push(" How are"), vec!["Hello there."]);
        assert_eq!(seg.push(" you today? Fine."), vec!["How are you today?"]);
        assert_eq!(seg.flush(), vec!["Fine."]);
    }
    #[test]
    fn does_not_split_decimals() {
        let (out, rest) = run(&["Pi is 3.", "14 roughly. ", "Yes it is."], SegmenterOptions::default());
        assert_eq!(out, vec!["Pi is 3.14 roughly."]);
        assert_eq!(rest, vec!["Yes it is."]);
    }
    #[test]
    fn cjk_sentences() {
        let (out, rest) = run(&["你好，今天過得怎麼樣？我很好。", "那就好"], SegmenterOptions::default());
        assert_eq!(out, vec!["你好，今天過得怎麼樣？", "我很好。"]);
        assert_eq!(rest, vec!["那就好"]);
    }
    #[test]
    fn newlines_are_boundaries() {
        let (out, rest) = run(&["第一行\n\n第二行\n", "第三"], SegmenterOptions::default());
        assert_eq!(out, vec!["第一行", "第二行"]);
        assert_eq!(rest, vec!["第三"]);
    }
    #[test]
    fn merges_short_sentences() {
        let (out, _) = run(&["OK. Sure. Let's start with the plan. "], SegmenterOptions::default());
        assert_eq!(out, vec!["OK. Sure.", "Let's start with the plan."]);
        assert_eq!(run(&["好。那就這樣吧。"], SegmenterOptions::default()).0, vec!["好。那就這樣吧。"]);
    }
    #[test]
    fn hard_cuts_run_on_text() {
        let long = "這是一段沒有句號的話，".repeat(30);
        let (out, _) = run(&[&long], SegmenterOptions { max_chars: 60, min_chars: 6 });
        assert!(out.len() > 1);
        for s in &out {
            assert!(s.chars().count() <= 60);
        }
        assert!(out[0].ends_with('，'));
    }
    #[test]
    fn flush_empty() {
        let mut seg = SentenceSegmenter::default();
        assert!(seg.flush().is_empty());
        seg.push("   ");
        assert!(seg.flush().is_empty());
    }
    #[test]
    fn strip_markdown_works() {
        assert_eq!(strip_markdown("**bold** and `code` and [link](http://x)"), "bold and code and link");
        assert_eq!(strip_markdown("# Title\n- item one\n- item two"), "Title\nitem one\nitem two");
    }
}
