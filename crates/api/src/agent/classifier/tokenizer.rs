//! The e5-small (XLM-R SentencePiece Unigram) tokenizer, reimplemented on a
//! compact vocabulary map. The `tokenizers` crate builds a byte trie with a
//! hash map per node for the 250k pieces, ~265 MB resident on linux/aarch64;
//! this needs ~20 MB. Same pipeline as `tokenizer.json`, step for step:
//! 1. special tokens (`<s>`, `</s>`, `<pad>`, `<unk>`, `<mask>`, matched
//!    leftmost-longest on the raw text) map to their ids;
//! 2. every other segment: the precompiled charsmap normalizer
//!    (`spm_precompiled`, the code `tokenizers` itself uses), runs of 2+
//!    spaces collapsed to one, spaces to `▁`, `▁` prepended, split before
//!    each `▁`;
//! 3. Viterbi over piece log-probabilities, unknown characters scored
//!    `min_score - 10` and fused (`Unigram::encode_optimized`);
//! 4. `<s> … </s>`, truncated to 512 ids.
//!
//! The parity fixture checks the ids against the Python `tokenizers` on
//! every training goal and a set of edge strings.
use base64::Engine as _;
use serde::Deserialize;
use spm_precompiled::Precompiled;
use std::collections::HashMap;

const MAX_IDS: usize = 512;
const UNK_PENALTY: f64 = 10.0;
const SPACE: char = '▁';

#[derive(Deserialize)]
struct Json {
    added_tokens: Vec<AddedToken>,
    normalizer: NormalizerSequence,
    pre_tokenizer: MetaspaceJson,
    model: UnigramJson,
}

#[derive(Deserialize)]
struct AddedToken {
    id: u32,
    content: String,
    special: bool,
    normalized: bool,
    lstrip: bool,
    rstrip: bool,
    single_word: bool,
}

#[derive(Deserialize)]
struct NormalizerSequence {
    #[serde(rename = "type")]
    kind: String,
    normalizers: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct MetaspaceJson {
    #[serde(rename = "type")]
    kind: String,
    replacement: String,
    #[serde(default)]
    add_prefix_space: Option<bool>,
    #[serde(default)]
    prepend_scheme: Option<String>,
    #[serde(default)]
    split: Option<bool>,
}

#[derive(Deserialize)]
struct UnigramJson {
    #[serde(rename = "type")]
    kind: String,
    unk_id: u32,
    vocab: Vec<(String, f64)>,
    #[serde(default)]
    byte_fallback: bool,
}

pub(super) struct Tokenizer {
    pieces: HashMap<Box<str>, u32>,
    scores: Vec<f64>,
    unk_id: u32,
    min_score: f64,
    /// Longest piece, in chars.
    max_chars: usize,
    charsmap: Precompiled,
    specials: Vec<(String, u32)>,
    bos: u32,
    eos: u32,
}

impl Tokenizer {
    /// Parse `tokenizer.json`, refusing any shape other than the pinned
    /// e5-small one (so a different file cannot silently tokenize another way).
    pub(super) fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let json: Json = serde_json::from_slice(bytes).map_err(|e| format!("tokenizer: {e}"))?;
        let shape = |what: &str| format!("tokenizer: unexpected {what}");
        let [precompiled, replace] = json.normalizer.normalizers.as_slice() else {
            return Err(shape("normalizers"));
        };
        if json.normalizer.kind != "Sequence"
            || precompiled["type"] != "Precompiled"
            || replace["type"] != "Replace"
            || replace["pattern"]["Regex"] != " {2,}"
            || replace["content"] != " "
        {
            return Err(shape("normalizers"));
        }
        let prepends = json.pre_tokenizer.add_prefix_space == Some(true)
            || json.pre_tokenizer.prepend_scheme.as_deref() == Some("always");
        if json.pre_tokenizer.kind != "Metaspace"
            || json.pre_tokenizer.replacement != SPACE.to_string()
            || !prepends
            || json.pre_tokenizer.split == Some(false)
        {
            return Err(shape("pre-tokenizer"));
        }
        if json.model.kind != "Unigram" || json.model.byte_fallback || json.model.vocab.is_empty() {
            return Err(shape("model"));
        }
        let charsmap = base64::engine::general_purpose::STANDARD
            .decode(
                precompiled["precompiled_charsmap"]
                    .as_str()
                    .ok_or_else(|| shape("charsmap"))?,
            )
            .map_err(|_| shape("charsmap"))?;
        let charsmap = Precompiled::from(&charsmap).map_err(|_| shape("charsmap"))?;
        let mut specials = Vec::new();
        for token in &json.added_tokens {
            if !token.special
                || token.normalized
                || token.lstrip
                || token.rstrip
                || token.single_word
            {
                return Err(shape("added token"));
            }
            specials.push((token.content.clone(), token.id));
        }
        let id_of = |content: &str| {
            specials
                .iter()
                .find(|(c, _)| c == content)
                .map(|(_, id)| *id)
                .ok_or_else(|| shape("special tokens"))
        };
        let (bos, eos) = (id_of("<s>")?, id_of("</s>")?);
        let n = json.model.vocab.len();
        if json.model.unk_id as usize >= n {
            return Err(shape("unk id"));
        }
        let mut pieces = HashMap::with_capacity(n);
        let mut scores = Vec::with_capacity(n);
        let (mut min_score, mut max_chars) = (f64::INFINITY, 0);
        for (id, (piece, score)) in json.model.vocab.into_iter().enumerate() {
            min_score = min_score.min(score);
            max_chars = max_chars.max(piece.chars().count());
            scores.push(score);
            // A later duplicate wins, as in `tokenizers`.
            pieces.insert(piece.into_boxed_str(), id as u32);
        }
        Ok(Self {
            pieces,
            scores,
            unk_id: json.model.unk_id,
            min_score,
            max_chars,
            charsmap,
            specials,
            bos,
            eos,
        })
    }

    /// `<s>` ids `</s>`, at most 512 ids.
    pub(super) fn encode(&self, text: &str) -> Vec<u32> {
        let mut ids = vec![self.bos];
        let mut rest = text;
        while !rest.is_empty() {
            match self.next_special(rest) {
                Some((at, len, id)) => {
                    self.encode_segment(&rest[..at], &mut ids);
                    ids.push(id);
                    rest = &rest[at + len..];
                }
                None => {
                    self.encode_segment(rest, &mut ids);
                    rest = "";
                }
            }
        }
        ids.truncate(MAX_IDS - 1);
        ids.push(self.eos);
        ids
    }

    /// Leftmost, then longest, special token: (byte offset, length, id).
    fn next_special(&self, text: &str) -> Option<(usize, usize, u32)> {
        text.char_indices().find_map(|(at, _)| {
            self.specials
                .iter()
                .filter(|(content, _)| text[at..].starts_with(content.as_str()))
                .max_by_key(|(content, _)| content.len())
                .map(|(content, id)| (at, content.len(), *id))
        })
    }

    fn encode_segment(&self, segment: &str, ids: &mut Vec<u32>) {
        if segment.is_empty() {
            return;
        }
        let normalized = self.charsmap.normalize_string(segment);
        let mut collapsed = String::with_capacity(normalized.len() + 3);
        let mut spaces = 0usize;
        for c in normalized.chars() {
            if c == ' ' {
                spaces += 1;
                continue;
            }
            if spaces > 0 {
                collapsed.push(SPACE);
                spaces = 0;
            }
            collapsed.push(c);
        }
        if spaces > 0 {
            collapsed.push(SPACE);
        }
        if collapsed.is_empty() {
            return;
        }
        if !collapsed.starts_with(SPACE) {
            collapsed.insert(0, SPACE);
        }
        // Split before every `▁` (the delimiter starts the next word).
        let mut start = 0;
        for (at, c) in collapsed.char_indices().skip(1) {
            if c == SPACE {
                self.viterbi(&collapsed[start..at], ids);
                start = at;
            }
        }
        self.viterbi(&collapsed[start..], ids);
    }

    fn viterbi(&self, word: &str, ids: &mut Vec<u32>) {
        #[derive(Clone, Copy)]
        struct Best {
            score: f64,
            from: Option<usize>,
            id: u32,
        }
        let bounds: Vec<usize> = word
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(word.len()))
            .collect();
        let chars = bounds.len() - 1;
        let unk_score = self.min_score - UNK_PENALTY;
        let mut best = vec![
            Best {
                score: 0.0,
                from: None,
                id: 0,
            };
            chars + 1
        ];
        for start in 0..chars {
            let here = best[start].score;
            let mut single = false;
            for end in start + 1..=chars.min(start + self.max_chars) {
                let Some(&id) = self.pieces.get(&word[bounds[start]..bounds[end]]) else {
                    continue;
                };
                let candidate = here + self.scores[id as usize];
                let node = &mut best[end];
                if node.from.is_none() || candidate > node.score {
                    *node = Best {
                        score: candidate,
                        from: Some(start),
                        id,
                    };
                }
                single |= end == start + 1;
            }
            if !single {
                let candidate = here + unk_score;
                let node = &mut best[start + 1];
                if node.from.is_none() || candidate > node.score {
                    *node = Best {
                        score: candidate,
                        from: Some(start),
                        id: self.unk_id,
                    };
                }
            }
        }
        // Backtrack; consecutive unknown pieces fuse into one, which is then
        // looked up as a whole (unknown again unless it is a piece).
        let mut out: Vec<u32> = Vec::new();
        let mut end = chars;
        let mut unk_end: Option<usize> = None;
        let flush = |unk_end: &mut Option<usize>, start: usize, out: &mut Vec<u32>| {
            if let Some(stop) = unk_end.take() {
                let fused = &word[bounds[start]..bounds[stop]];
                out.push(self.pieces.get(fused).copied().unwrap_or(self.unk_id));
            }
        };
        while end > 0 {
            let node = best[end];
            let Some(start) = node.from else { break };
            if node.id == self.unk_id {
                unk_end.get_or_insert(end);
            } else {
                flush(&mut unk_end, end, &mut out);
                out.push(node.id);
            }
            end = start;
        }
        flush(&mut unk_end, end, &mut out);
        out.reverse();
        ids.extend(out);
    }
}
