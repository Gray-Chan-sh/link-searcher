use std::sync::LazyLock;

use jieba_rs::Jieba;
use tantivy::schema::*;
use tantivy::tokenizer::{LowerCaser, SimpleTokenizer, TextAnalyzer, Token, TokenStream, Tokenizer};
use tantivy::Index;

/// Name for the jieba Chinese segmentation tokenizer registered with tantivy.
pub const JIEBA_TOKENIZER_NAME: &str = "jieba";

/// Name for the suggest/autocomplete tokenizer that n-grams substrings.
pub const SUGGEST_TOKENIZER_NAME: &str = "suggest";

/// Global jieba instance, lazy-initialized with the built-in dictionary.
static JIEBA: LazyLock<Jieba> = LazyLock::new(|| Jieba::new());

/// Register custom tokenizers (jieba, suggest) on the index.
///
/// Must be called once after index creation and before any indexing or
/// querying that uses these tokenizers.
pub fn register_tokenizers(index: &Index) {
    // Jieba Chinese word segmentation tokenizer.
    let jieba_tokenizer = TextAnalyzer::builder(JiebaTokenizer).build();
    index
        .tokenizers()
        .register(JIEBA_TOKENIZER_NAME, jieba_tokenizer);

    // Suggest tokenizer: split on whitespace, lowercase, for prefix queries.
    let suggest_tokenizer = TextAnalyzer::builder(SimpleTokenizer::default()).filter(LowerCaser).build();
    index
        .tokenizers()
        .register(SUGGEST_TOKENIZER_NAME, suggest_tokenizer);
}

/// Build the tantivy schema used by the search index.
///
/// Fields:
/// - `file_id` (STRING | STORED) — UUID linking to file_tracking
/// - `file_name` (TEXT | STORED) — Base filename (indexed with jieba)
/// - `file_ext` (STRING | STORED) — Lowercase extension
/// - `dir_id` (STRING | STORED) — Directory ID for scoped search
/// - `content` (TEXT) — Full extracted text (tokenized with jieba, NOT stored)
/// - `content_suggest` (TEXT) — Same content with suggest tokenizer for autocomplete
/// - `mtime` (DATE | INDEXED | STORED) — Modification time for date filtering
/// - `file_size` (U64 | INDEXED | STORED) — File size for sorting
pub fn build_schema() -> Schema {
    let mut builder = Schema::builder();

    // Full-text fields using jieba tokenizer for CJK support.
    let content_indexing = TextFieldIndexing::default()
        .set_tokenizer(JIEBA_TOKENIZER_NAME)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let content_options = TextOptions::default().set_indexing_options(content_indexing);

    let name_indexing = TextFieldIndexing::default()
        .set_tokenizer(JIEBA_TOKENIZER_NAME)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let name_options = TextOptions::default()
        .set_indexing_options(name_indexing)
        .set_stored();

    // Suggest field uses raw + lowercaser for prefix queries.
    let suggest_indexing = TextFieldIndexing::default()
        .set_tokenizer(SUGGEST_TOKENIZER_NAME)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let suggest_options = TextOptions::default().set_indexing_options(suggest_indexing);

    builder.add_text_field("file_id", STRING | STORED);
    builder.add_text_field("file_name", name_options);
    builder.add_text_field("file_ext", STRING | STORED);
    builder.add_text_field("dir_id", STRING | STORED);
    builder.add_text_field("path", STRING | STORED);
    builder.add_text_field("content", content_options);
    builder.add_text_field("content_suggest", suggest_options);
    builder.add_date_field("mtime", INDEXED | STORED | FAST);
    builder.add_u64_field("file_size", INDEXED | STORED | FAST);

    builder.build()
}

// ---------------------------------------------------------------------------
// Custom Jieba Tokenizer
// ---------------------------------------------------------------------------

/// A tantivy [`Tokenizer`] that delegates to jieba-rs for Chinese word
/// segmentation, producing tokens at unicode-character positions.
///
/// Mixed English/Chinese text is handled naturally: jieba passes ASCII words
/// through unsegmented.
#[derive(Clone, Default)]
pub struct JiebaTokenizer;

impl Tokenizer for JiebaTokenizer {
    type TokenStream<'a> = JiebaTokenStream<'a>;

    fn token_stream<'a>(&mut self, text: &'a str) -> JiebaTokenStream<'a> {
        let jieba_tokens = JIEBA.tokenize(text, jieba_rs::TokenizeMode::Search, true);
        let token = jieba_tokens.first().map(|t| Token {
            offset_from: t.word.as_ptr() as usize - text.as_ptr() as usize,
            offset_to: t.word.as_ptr() as usize - text.as_ptr() as usize + t.word.len(),
            text: t.word.to_string(),
            position: t.start,
            position_length: t.end - t.start,
        });
        JiebaTokenStream {
            text,
            tokens: jieba_tokens,
            index: 0,
            token: token.unwrap_or_default(),
        }
    }
}

/// Token stream produced by [`JiebaTokenizer`].
pub struct JiebaTokenStream<'a> {
    text: &'a str,
    tokens: Vec<jieba_rs::Token<'a>>,
    index: usize,
    token: Token,
}

impl TokenStream for JiebaTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if self.index >= self.tokens.len() {
            return false;
        }
        let t = &self.tokens[self.index];
        self.token.offset_from = t.word.as_ptr() as usize - self.text.as_ptr() as usize;
        self.token.offset_to = self.token.offset_from + t.word.len();
        self.token.position = t.start;
        self.token.position_length = t.end - t.start;
        self.token.text.clear();
        self.token.text.push_str(t.word);
        self.index += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

// ---------------------------------------------------------------------------
// Raw tokenizer (for suggest field – emits the entire text as one token)
// ---------------------------------------------------------------------------

/// A tokenizer that emits the entire text as a single token.
/// Used together with [`LowerCaser`] for the suggest/autocomplete field.
#[derive(Clone, Default)]
pub struct RawTokenizer;

impl Tokenizer for RawTokenizer {
    type TokenStream<'a> = RawTokenStream<'a>;

    fn token_stream<'a>(&mut self, text: &'a str) -> RawTokenStream<'a> {
        RawTokenStream {
            text,
            done: false,
            token: Token::default(),
        }
    }
}

/// Token stream produced by [`RawTokenizer`].
pub struct RawTokenStream<'a> {
    text: &'a str,
    done: bool,
    token: Token,
}

impl TokenStream for RawTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if self.done {
            return false;
        }
        self.done = true;
        self.token.offset_from = 0;
        self.token.offset_to = self.text.len();
        self.token.position = 0;
        self.token.position_length = 1;
        self.token.text.clear();
        self.token.text.push_str(self.text);
        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jieba_tokenizer_segments_chinese() {
        let mut tokenizer = JiebaTokenizer;
        let mut stream = tokenizer.token_stream("张华考上了北京大学");
        let mut tokens = Vec::new();
        while let Some(t) = stream.next() {
            tokens.push(t.text.clone());
        }
        // "张华考上了北京大学" should be segmented by jieba.
        assert!(tokens.contains(&"张华".to_string()));
        assert!(tokens.contains(&"考上".to_string()));
        assert!(tokens.contains(&"北京大学".to_string()));
    }

    #[test]
    fn test_jieba_tokenizer_handles_ascii() {
        let mut tokenizer = JiebaTokenizer;
        let mut stream = tokenizer.token_stream("hello world");
        let mut tokens = Vec::new();
        while let Some(t) = stream.next() {
            tokens.push(t.text.clone());
        }
        assert_eq!(tokens, vec!["hello", " ", "world"]);
    }

    #[test]
    fn test_raw_tokenizer_emits_one_token() {
        let mut tokenizer = RawTokenizer;
        let mut stream = tokenizer.token_stream("some text for suggest");
        let tokens: Vec<String> = std::iter::from_fn(|| stream.next().map(|t| t.text.clone())).collect();
        assert_eq!(tokens, vec!["some text for suggest"]);
    }

    #[test]
    fn test_build_schema_contains_expected_fields() {
        let schema = build_schema();
        assert!(schema.get_field("file_id").is_ok());
        assert!(schema.get_field("file_name").is_ok());
        assert!(schema.get_field("file_ext").is_ok());
        assert!(schema.get_field("dir_id").is_ok());
        assert!(schema.get_field("path").is_ok());
        assert!(schema.get_field("content").is_ok());
        assert!(schema.get_field("content_suggest").is_ok());
        assert!(schema.get_field("mtime").is_ok());
        assert!(schema.get_field("file_size").is_ok());
    }
}
