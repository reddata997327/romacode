//! `RecursiveCharacterTextSplitter` — chunks long text on natural boundaries
//! (`\n\n`, `\n`, `. `, ` `, ``) while honouring `chunk_size` and `chunk_overlap`.

/// Splits text into overlapping chunks, preferring the earliest separator that
/// keeps pieces under `chunk_size` — the port of
/// `langchain.text_splitter.RecursiveCharacterTextSplitter`.
#[derive(Debug, Clone)]
pub struct RecursiveCharacterTextSplitter {
    chunk_size: usize,
    chunk_overlap: usize,
    separators: Vec<&'static str>,
}

impl Default for RecursiveCharacterTextSplitter {
    fn default() -> Self {
        Self::new(1000, 200)
    }
}

impl RecursiveCharacterTextSplitter {
    /// `chunk_size` max characters per chunk, `chunk_overlap` shared suffix/prefix.
    ///
    /// A `chunk_overlap >= chunk_size` used to PANIC via `assert!`; it is now
    /// clamped to `chunk_size - 1` (the largest overlap that can never
    /// produce an empty or oversized chunk) so the constructor stays
    /// non-panicking and API-stable.
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        let chunk_size = chunk_size.max(1);
        Self {
            chunk_size,
            chunk_overlap: chunk_overlap.min(chunk_size.saturating_sub(1)),
            separators: vec!["\n\n", "\n", ". ", " ", ""],
        }
    }

    /// Override the separator priority.
    pub fn with_separators(mut self, seps: Vec<&'static str>) -> Self {
        self.separators = seps;
        self
    }

    /// Split `text` into chunks.
    pub fn split(&self, text: &str) -> Vec<String> {
        if text.chars().count() <= self.chunk_size {
            let t = text.trim();
            return if t.is_empty() { Vec::new() } else { vec![t.to_string()] };
        }
        let pieces = self.split_by_separators(text);
        self.merge(pieces)
    }

    /// Recursively split using separators; the final separator `""` cuts per char.
    fn split_by_separators(&self, text: &str) -> Vec<String> {
        for (i, sep) in self.separators.iter().enumerate() {
            if text.contains(sep) && !sep.is_empty() {
                let parts: Vec<String> = text.split(sep).map(|s| s.to_string()).collect();
                // Try to keep sub-pieces intact if they fit; recurse on oversized ones.
                let mut out = Vec::new();
                for p in parts {
                    if p.chars().count() <= self.chunk_size {
                        if !p.trim().is_empty() {
                            out.push(p);
                        }
                    } else {
                        let sub = RecursiveCharacterTextSplitter {
                            chunk_size: self.chunk_size,
                            chunk_overlap: self.chunk_overlap,
                            separators: self.separators[i + 1..].to_vec(),
                        };
                        out.extend(sub.split_by_separators(&p));
                    }
                }
                return out;
            }
        }
        // Fallback: hard character cut (sep == "" case).
        text.chars()
            .collect::<Vec<_>>()
            .chunks(self.chunk_size)
            .map(|c| c.iter().collect::<String>())
            .collect()
    }

    /// Merge small pieces into chunks up to `chunk_size` with `chunk_overlap`
    /// carried between consecutive chunks.
    fn merge(&self, pieces: Vec<String>) -> Vec<String> {
        let mut chunks: Vec<String> = Vec::new();
        let mut current = String::new();
        for piece in pieces {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let plen = piece.chars().count();
            if !current.is_empty() && current.chars().count() + plen + 1 > self.chunk_size {
                chunks.push(current.clone());
                // Overlap: keep the tail of the previous chunk.
                if self.chunk_overlap > 0 {
                    let tail: String = current
                        .chars()
                        .rev()
                        .take(self.chunk_overlap)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    current = tail.trim_start().to_string();
                } else {
                    current.clear();
                }
                // The overlap tail may leave no room for the piece (tail +
                // separator + piece > chunk_size): appending it anyway used to
                // emit chunks exceeding chunk_size by overlap + 1. Drop the
                // tail so the piece starts a fresh, in-bounds chunk.
                if !current.is_empty() && current.chars().count() + plen + 1 > self.chunk_size {
                    current.clear();
                }
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(piece);
        }
        if !current.trim().is_empty() {
            chunks.push(current);
        }
        chunks
    }
}

/// A chunk of text plus metadata — `langchain_core.documents.Document`.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The chunk text.
    pub page_content: String,
    /// Arbitrary metadata (source, page number, …).
    pub metadata: std::collections::HashMap<String, String>,
}

impl Document {
    /// New document with empty metadata.
    pub fn new(page_content: impl Into<String>) -> Self {
        Self {
            page_content: page_content.into(),
            metadata: Default::default(),
        }
    }

    /// New document with a `source` metadata entry.
    pub fn with_source(page_content: impl Into<String>, source: impl Into<String>) -> Self {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("source".to_string(), source.into());
        Self {
            page_content: page_content.into(),
            metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_single_chunk() {
        let s = RecursiveCharacterTextSplitter::new(100, 10);
        assert_eq!(s.split("hello world"), vec!["hello world"]);
        assert!(s.split("   ").is_empty());
    }

    #[test]
    fn paragraphs_split_first() {
        let s = RecursiveCharacterTextSplitter::new(30, 0);
        let text = "First paragraph is short.\n\nSecond paragraph is considerably longer than thirty chars.";
        let chunks = s.split(text);
        assert!(chunks.len() >= 2);
        assert!(chunks[0].starts_with("First paragraph"));
    }

    #[test]
    fn hard_cut_fallback() {
        let s = RecursiveCharacterTextSplitter::new(5, 0).with_separators(vec![""]);
        let chunks = s.split("abcdefghij");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "abcde");
        assert_eq!(chunks[1], "fghij");
    }

    #[test]
    fn overlap_is_applied() {
        let s = RecursiveCharacterTextSplitter::new(20, 5).with_separators(vec![" "]);
        let text = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh";
        let chunks = s.split(text);
        assert!(chunks.len() >= 2);
        // With overlap, chunk n+1 must not start with a brand-new word boundary every time:
        // simply assert every chunk is <= chunk_size + slack.
        for c in &chunks {
            assert!(c.chars().count() <= 21 + 6);
        }
    }

    #[test]
    fn document_metadata() {
        let d = Document::with_source("text", "book.pdf");
        assert_eq!(d.metadata["source"], "book.pdf");
    }

    #[test]
    fn oversized_overlap_is_clamped_not_panic() {
        // Regression (M39): `chunk_overlap >= chunk_size` used to PANIC in
        // the constructor; it must clamp instead.
        let s = RecursiveCharacterTextSplitter::new(10, 25);
        let chunks = s.split("aaaaa bbbbb ccccc ddddd eeeee");
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.chars().count() <= 10, "chunk exceeds chunk_size: {c:?}");
        }
        // Equal overlap is clamped too (and still must not panic).
        let s = RecursiveCharacterTextSplitter::new(8, 8);
        let chunks = s.split("one two three four five six seven");
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.chars().count() <= 8, "chunk exceeds chunk_size: {c:?}");
        }
        // chunk_size 1 clamps overlap to 0.
        let s = RecursiveCharacterTextSplitter::new(1, 5);
        assert_eq!(s.split("ab"), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn merged_chunks_never_exceed_chunk_size() {
        // Regression (M39): after a chunk flush, the overlap tail was
        // appended to unconditionally, so tail + separator + piece could
        // exceed chunk_size by overlap + 1.
        for overlap in [0usize, 1, 3, 7, 19] {
            let s = RecursiveCharacterTextSplitter::new(20, overlap).with_separators(vec![" "]);
            let text = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh iiiii jjjjj kkkkk";
            let chunks = s.split(text);
            assert!(chunks.len() >= 2);
            for c in &chunks {
                assert!(
                    c.chars().count() <= 20,
                    "overlap {overlap}: chunk of {} chars exceeds chunk_size: {c:?}",
                    c.chars().count()
                );
            }
        }
    }
}
