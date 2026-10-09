//! A small, tolerant parser for Valve's text KeyValues format (`.vdf` / `.acf`).
//!
//! Steam writes `libraryfolders.vdf` and `appmanifest_<appid>.acf` in this format:
//!
//! ```text
//! "AppState"
//! {
//!     "appid"      "278360"
//!     "InstalledDepots"
//!     {
//!         "278362" { "manifest" "7137994883443283717" }
//!     }
//! }
//! ```
//!
//! Supported syntax (written from the format as observed in real Steam files, not from
//! any Valve source):
//!
//! - quoted tokens with the escape sequences `\\`, `\"`, `\n`, `\t` (`\r` is also accepted);
//!   an unknown escape such as `\S` is kept verbatim (backslash included), which is what
//!   un-escaped Windows paths in hand-edited files need;
//! - unquoted tokens (a run of characters that are not whitespace, `"`, `{` or `}`);
//! - nested `{ ... }` blocks, duplicate keys (order preserved);
//! - `//` comments to end of line, CRLF/LF line endings, a leading UTF-8 BOM;
//! - `[$WIN32]`-style conditional tags after a key or value are skipped.
//!
//! The input is treated as hostile: there is no recursion (an explicit stack is used),
//! nesting depth and input size are capped, and every malformed input returns a
//! [`VdfError`] with a line/column instead of panicking.

use std::fmt;

/// Maximum nesting depth of `{}` blocks accepted by [`parse`].
pub const MAX_DEPTH: usize = 64;

/// Maximum input size accepted by [`parse`] (Steam's files are a few KiB).
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// A KeyValues value: either a string or an ordered list of child pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A leaf string value.
    Str(String),
    /// A `{ ... }` block. Order and duplicate keys are preserved.
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// The string value, if this is a leaf.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    /// The children, if this is a block.
    pub fn as_obj(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Obj(children) => Some(children),
            Value::Str(_) => None,
        }
    }

    /// First child whose key matches `key` case-insensitively (KeyValues keys are
    /// case-insensitive: Steam has written both `installdir` and `InstallDir`).
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_obj()?
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    /// Convenience: `get(key)` as a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    /// Follow a path of keys (each matched case-insensitively).
    pub fn get_path(&self, path: &[&str]) -> Option<&Value> {
        let mut current = self;
        for key in path {
            current = current.get(key)?;
        }
        Some(current)
    }
}

/// A parsed document: the top-level pairs (normally exactly one, e.g. `"AppState" {...}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Top-level key/value pairs in file order.
    pub pairs: Vec<(String, Value)>,
}

impl Document {
    /// First top-level value whose key matches case-insensitively.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.pairs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    /// The first top-level pair, whatever its key.
    pub fn root(&self) -> Option<(&str, &Value)> {
        self.pairs.first().map(|(k, v)| (k.as_str(), v))
    }
}

/// What went wrong while parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VdfErrorKind {
    /// The input exceeds [`MAX_INPUT_BYTES`].
    TooLarge,
    /// A quoted string was not closed before end of input.
    UnterminatedString,
    /// A `}` appeared where no block was open.
    UnexpectedCloseBrace,
    /// A `{` appeared where a key was expected.
    UnexpectedOpenBrace,
    /// A key was followed by end of input or `}` instead of a value.
    MissingValue,
    /// End of input inside an open `{` block.
    UnclosedBlock,
    /// Blocks nested deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for VdfErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            VdfErrorKind::TooLarge => "input too large",
            VdfErrorKind::UnterminatedString => "unterminated quoted string",
            VdfErrorKind::UnexpectedCloseBrace => "unexpected '}'",
            VdfErrorKind::UnexpectedOpenBrace => "unexpected '{' (expected a key)",
            VdfErrorKind::MissingValue => "key without a value",
            VdfErrorKind::UnclosedBlock => "unclosed '{' block at end of input",
            VdfErrorKind::TooDeep => "blocks nested too deeply",
        };
        f.write_str(text)
    }
}

/// A parse error with a 1-based source position.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} at line {line}, column {column}")]
pub struct VdfError {
    /// The error category.
    pub kind: VdfErrorKind,
    /// 1-based line.
    pub line: usize,
    /// 1-based column (in characters).
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Str(String),
    Open,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    line: usize,
    column: usize,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Lexer {
            chars: input.chars().peekable(),
            line: 1,
            column: 1,
        }
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.column = 1;
        } else {
            self.column = self.column.saturating_add(1);
        }
        Some(c)
    }

    fn error(&self, kind: VdfErrorKind) -> VdfError {
        VdfError {
            kind,
            line: self.line,
            column: self.column,
        }
    }

    /// Skip whitespace and `//` comments. A lone `/` is part of an unquoted token.
    fn skip_trivia(&mut self) {
        loop {
            match self.chars.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') => {
                    let mut lookahead = self.chars.clone();
                    lookahead.next();
                    if lookahead.peek() == Some(&'/') {
                        while let Some(c) = self.bump() {
                            if c == '\n' {
                                break;
                            }
                        }
                    } else {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    /// Next token, or `None` at end of input. Conditional tags (`[$X]`) are skipped.
    fn next_token(&mut self) -> Result<Option<Token>, VdfError> {
        loop {
            self.skip_trivia();
            let (line, column) = (self.line, self.column);
            let Some(&c) = self.chars.peek() else {
                return Ok(None);
            };
            let kind = match c {
                '{' => {
                    self.bump();
                    TokenKind::Open
                }
                '}' => {
                    self.bump();
                    TokenKind::Close
                }
                '"' => {
                    self.bump();
                    TokenKind::Str(self.quoted(line, column)?)
                }
                '[' => {
                    // Conditional such as [$WIN32] or [!$X360]: skip to the closing bracket
                    // (or end of line) and continue.
                    while let Some(c) = self.bump() {
                        if c == ']' || c == '\n' {
                            break;
                        }
                    }
                    continue;
                }
                _ => TokenKind::Str(self.unquoted()),
            };
            return Ok(Some(Token { kind, line, column }));
        }
    }

    fn quoted(&mut self, line: usize, column: usize) -> Result<String, VdfError> {
        let mut out = String::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(VdfError {
                    kind: VdfErrorKind::UnterminatedString,
                    line,
                    column,
                });
            };
            match c {
                '"' => return Ok(out),
                '\\' => match self.chars.peek().copied() {
                    Some('\\') => {
                        self.bump();
                        out.push('\\');
                    }
                    Some('"') => {
                        self.bump();
                        out.push('"');
                    }
                    Some('n') => {
                        self.bump();
                        out.push('\n');
                    }
                    Some('t') => {
                        self.bump();
                        out.push('\t');
                    }
                    Some('r') => {
                        self.bump();
                        out.push('\r');
                    }
                    // Unknown escape (or end of input): keep the backslash verbatim.
                    _ => out.push('\\'),
                },
                other => out.push(other),
            }
        }
    }

    fn unquoted(&mut self) -> String {
        let mut out = String::new();
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() || c == '"' || c == '{' || c == '}' {
                break;
            }
            if c == '/' {
                let mut lookahead = self.chars.clone();
                lookahead.next();
                if lookahead.peek() == Some(&'/') {
                    break;
                }
            }
            out.push(c);
            self.bump();
        }
        out
    }
}

/// Parse a KeyValues text document.
///
/// Never panics; malformed input yields a [`VdfError`].
pub fn parse(input: &str) -> Result<Document, VdfError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(VdfError {
            kind: VdfErrorKind::TooLarge,
            line: 1,
            column: 1,
        });
    }
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut lexer = Lexer::new(input);

    // Explicit stack of open blocks: (key that opened the block, children so far).
    let mut stack: Vec<(String, Vec<(String, Value)>)> = Vec::new();
    let mut top: Vec<(String, Value)> = Vec::new();

    loop {
        let Some(token) = lexer.next_token()? else {
            if stack.is_empty() {
                return Ok(Document { pairs: top });
            }
            return Err(lexer.error(VdfErrorKind::UnclosedBlock));
        };
        let key = match token.kind {
            TokenKind::Str(key) => key,
            TokenKind::Close => {
                let Some((block_key, children)) = stack.pop() else {
                    return Err(VdfError {
                        kind: VdfErrorKind::UnexpectedCloseBrace,
                        line: token.line,
                        column: token.column,
                    });
                };
                let parent = match stack.last_mut() {
                    Some((_, siblings)) => siblings,
                    None => &mut top,
                };
                parent.push((block_key, Value::Obj(children)));
                continue;
            }
            TokenKind::Open => {
                return Err(VdfError {
                    kind: VdfErrorKind::UnexpectedOpenBrace,
                    line: token.line,
                    column: token.column,
                });
            }
        };

        let Some(value_token) = lexer.next_token()? else {
            return Err(lexer.error(VdfErrorKind::MissingValue));
        };
        match value_token.kind {
            TokenKind::Str(value) => {
                let parent = match stack.last_mut() {
                    Some((_, siblings)) => siblings,
                    None => &mut top,
                };
                parent.push((key, Value::Str(value)));
            }
            TokenKind::Open => {
                if stack.len() >= MAX_DEPTH {
                    return Err(VdfError {
                        kind: VdfErrorKind::TooDeep,
                        line: value_token.line,
                        column: value_token.column,
                    });
                }
                stack.push((key, Vec::new()));
            }
            TokenKind::Close => {
                return Err(VdfError {
                    kind: VdfErrorKind::MissingValue,
                    line: value_token.line,
                    column: value_token.column,
                });
            }
        }
    }
}

/// Parse raw bytes (lossy UTF-8; Steam writes UTF-8).
pub fn parse_bytes(bytes: &[u8]) -> Result<Document, VdfError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(VdfError {
            kind: VdfErrorKind::TooLarge,
            line: 1,
            column: 1,
        });
    }
    parse(&String::from_utf8_lossy(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }

    #[test]
    fn parses_nested_appmanifest_shape() {
        let text = "\"AppState\"\n{\n\t\"appid\"\t\t\"278360\"\n\t\"InstalledDepots\"\n\t{\n\t\t\"278362\"\n\t\t{\n\t\t\t\"manifest\"\t\t\"7137994883443283717\"\n\t\t}\n\t}\n}\n";
        let doc = parse(text).unwrap();
        let app = doc.get("appstate").unwrap();
        assert_eq!(app.get_str("APPID"), Some("278360"));
        assert_eq!(
            app.get_path(&["installeddepots", "278362", "manifest"])
                .and_then(Value::as_str),
            Some("7137994883443283717")
        );
    }

    #[test]
    fn escapes_are_decoded() {
        let doc = parse(r#""k" "C:\\Program Files (x86)\\Steam" "q" "say \"hi\"\n\tok""#).unwrap();
        assert_eq!(doc.pairs[0].1, s(r"C:\Program Files (x86)\Steam"));
        assert_eq!(doc.pairs[1].1, s("say \"hi\"\n\tok"));
    }

    #[test]
    fn unknown_escape_is_kept_verbatim() {
        let doc = parse(r#""path" "D:\SteamLibrary""#).unwrap();
        assert_eq!(doc.pairs[0].1, s(r"D:\SteamLibrary"));
        // Trailing backslash before the closing quote of the *document* is an escaped quote,
        // which leaves the string unterminated.
        assert!(parse(r#""path" "D:\""#).is_err());
    }

    #[test]
    fn unquoted_tokens_comments_and_crlf() {
        let text = "// header comment\r\nroot\r\n{\r\n  key value // trailing\r\n  path /usr/lib/x\r\n  \"sub\" { a b }\r\n}\r\n";
        let doc = parse(text).unwrap();
        let root = doc.get("root").unwrap();
        assert_eq!(root.get_str("key"), Some("value"));
        assert_eq!(root.get_str("path"), Some("/usr/lib/x"));
        assert_eq!(
            root.get_path(&["sub", "a"]).and_then(Value::as_str),
            Some("b")
        );
    }

    #[test]
    fn bom_conditionals_and_duplicates() {
        let text = "\u{feff}\"r\" { \"k\" \"1\" [$WIN32] \"k\" \"2\" [!$OSX] }";
        let doc = parse(text).unwrap();
        let children = doc.get("r").unwrap().as_obj().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(doc.get("r").unwrap().get_str("k"), Some("1"));
    }

    #[test]
    fn empty_and_whitespace_documents() {
        assert!(parse("").unwrap().pairs.is_empty());
        assert!(parse("  \n\t// only a comment").unwrap().pairs.is_empty());
        assert_eq!(parse("\"a\" {}").unwrap().pairs[0].1, Value::Obj(vec![]));
    }

    #[test]
    fn malformed_inputs_return_errors() {
        let cases: &[(&str, VdfErrorKind)] = &[
            ("\"a", VdfErrorKind::UnterminatedString),
            ("\"a\" \"b", VdfErrorKind::UnterminatedString),
            ("}", VdfErrorKind::UnexpectedCloseBrace),
            ("\"a\" \"b\" }", VdfErrorKind::UnexpectedCloseBrace),
            ("{", VdfErrorKind::UnexpectedOpenBrace),
            ("\"a\"", VdfErrorKind::MissingValue),
            ("\"a\" { \"b\" }", VdfErrorKind::MissingValue),
            ("\"a\" {", VdfErrorKind::UnclosedBlock),
            ("\"a\" { \"b\" { \"c\" \"d\" }", VdfErrorKind::UnclosedBlock),
        ];
        for (input, kind) in cases {
            let err = parse(input).unwrap_err();
            assert_eq!(&err.kind, kind, "input {input:?}");
        }
    }

    #[test]
    fn error_positions_are_reported() {
        let err = parse("\"a\"\n{\n  }\n}").unwrap_err();
        assert_eq!(err.kind, VdfErrorKind::UnexpectedCloseBrace);
        assert_eq!((err.line, err.column), (4, 1));
    }

    #[test]
    fn depth_is_capped_without_recursion() {
        let mut text = String::new();
        for _ in 0..(MAX_DEPTH + 10) {
            text.push_str("k { ");
        }
        assert_eq!(parse(&text).unwrap_err().kind, VdfErrorKind::TooDeep);
        let mut ok = String::new();
        for _ in 0..MAX_DEPTH {
            ok.push_str("k { ");
        }
        ok.push_str("leaf v ");
        for _ in 0..MAX_DEPTH {
            ok.push_str("} ");
        }
        assert!(parse(&ok).is_ok());
    }

    #[test]
    fn lone_slash_is_part_of_token() {
        let doc = parse("a/b c/d").unwrap();
        assert_eq!(doc.pairs[0], ("a/b".to_string(), s("c/d")));
        // An unquoted token directly followed by a comment.
        let doc = parse("a b//c\n").unwrap();
        assert_eq!(doc.pairs[0], ("a".to_string(), s("b")));
    }

    #[test]
    fn non_utf8_bytes_do_not_panic() {
        let bytes = b"\"k\" \"\xff\xfe\x00v\"";
        let doc = parse_bytes(bytes).unwrap();
        assert_eq!(doc.pairs.len(), 1);
    }

    #[test]
    fn fuzz_style_truncations_never_panic() {
        let text = "\"libraryfolders\"\r\n{\r\n\t\"0\"\r\n\t{\r\n\t\t\"path\"\t\t\"C:\\\\Steam\"\r\n\t\t\"apps\"\r\n\t\t{\r\n\t\t\t\"278360\"\t\t\"0\"\r\n\t\t}\r\n\t}\r\n\t// c\r\n\t\"1\"\t\"[$X] x\"\r\n}\r\n";
        for end in 0..=text.len() {
            if let Some(prefix) = text.get(..end) {
                let _ = parse(prefix);
            }
        }
        // Byte-level corruption: flip every byte to a few interesting values.
        let bytes = text.as_bytes();
        for i in 0..bytes.len() {
            for replacement in [b'{', b'}', b'"', b'\\', b'/', 0u8, 0xff] {
                let mut copy = bytes.to_vec();
                copy[i] = replacement;
                let _ = parse_bytes(&copy);
            }
        }
    }

    #[test]
    fn unterminated_quote_reports_its_opening_position() {
        let err = parse("\"a\"\r\n{\r\n\t\"b\"\t\"c\r\n}\r\n").unwrap_err();
        assert_eq!(err.kind, VdfErrorKind::UnterminatedString);
        assert_eq!((err.line, err.column), (3, 6));
        // A lone backslash right before end of input is not an escape of anything.
        assert_eq!(
            parse("\"k\" \"abc\\").unwrap_err().kind,
            VdfErrorKind::UnterminatedString
        );
        // Unterminated conditional tag: skipped to end of line, then the key lacks a value.
        assert_eq!(
            parse("\"k\" [$WIN32").unwrap_err().kind,
            VdfErrorKind::MissingValue
        );
    }

    #[test]
    fn positions_count_characters_and_crlf_lines() {
        // Multi-byte characters count as one column; CRLF counts as one line break.
        let err = parse("\"é\" \"ü\"\r\n}").unwrap_err();
        assert_eq!(err.kind, VdfErrorKind::UnexpectedCloseBrace);
        assert_eq!((err.line, err.column), (2, 1));
        let err = parse("\"é\" \"ü\" }").unwrap_err();
        assert_eq!((err.line, err.column), (1, 9));
        // Old Mac-style lone CR is whitespace (no line break is counted, nothing breaks).
        let doc = parse("\"a\"\r{\r\"b\" \"c\"\r}\r").unwrap();
        assert_eq!(doc.get("a").unwrap().get_str("b"), Some("c"));
    }

    #[test]
    fn windows_library_paths_with_escaped_backslashes() {
        let doc = parse(concat!(
            "\"libraryfolders\"\r\n{\r\n",
            "\t\"1\"\t\t\"D:\\\\Games\\\\Steam Library\\\\\"\r\n",
            "\t\"2\"\t\t\"\\\\\\\\nas\\\\share\\\\Steam\"\r\n",
            "\t\"3\"\t\t\"E:\\SteamLibrary\"\r\n",
            "}\r\n"
        ))
        .unwrap();
        let root = doc.get("LIBRARYFOLDERS").unwrap();
        assert_eq!(root.get_str("1"), Some(r"D:\Games\Steam Library\"));
        assert_eq!(root.get_str("2"), Some(r"\\nas\share\Steam"));
        // Hand-written, unescaped single backslash: unknown escape kept verbatim.
        assert_eq!(root.get_str("3"), Some(r"E:\SteamLibrary"));
    }

    #[test]
    fn bom_with_crlf_and_errors_after_it() {
        let doc = parse("\u{feff}\"AppState\"\r\n{\r\n\t\"appid\"\t\t\"278360\"\r\n}\r\n").unwrap();
        assert_eq!(
            doc.get("appstate").unwrap().get_str("appid"),
            Some("278360")
        );
        // The BOM is not counted as a column.
        let err = parse("\u{feff}}").unwrap_err();
        assert_eq!((err.line, err.column), (1, 1));
        // BOM through the bytes path, too.
        let doc = parse_bytes(b"\xEF\xBB\xBF\"k\" \"v\"").unwrap();
        assert_eq!(doc.pairs, vec![("k".to_string(), s("v"))]);
    }

    #[test]
    fn depth_error_reports_the_offending_brace() {
        let mut text = String::new();
        for _ in 0..=MAX_DEPTH {
            text.push_str("k\n{\n");
        }
        let err = parse(&text).unwrap_err();
        assert_eq!(err.kind, VdfErrorKind::TooDeep);
        assert_eq!(err.line, (MAX_DEPTH + 1) * 2);
        // Very deep input far beyond the cap is still rejected without recursion.
        let huge = "k{".repeat(1_000_000);
        assert_eq!(parse(&huge).unwrap_err().kind, VdfErrorKind::TooDeep);
    }

    #[test]
    fn pseudo_random_inputs_never_panic() {
        // Deterministic xorshift generator over an alphabet of syntax-relevant characters.
        let alphabet: &[char] = &[
            '"', '{', '}', '\\', '/', '[', ']', '$', ' ', '\t', '\r', '\n', 'a', '0', 'é',
            '\u{feff}', '\u{0}',
        ];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = usize::try_from(next() % 48).unwrap();
            let text: String = (0..len)
                .map(|_| alphabet[usize::try_from(next() % alphabet.len() as u64).unwrap()])
                .collect();
            if let Err(e) = parse(&text) {
                assert!(e.line >= 1 && e.column >= 1, "{text:?}: {e:?}");
            }
        }
    }

    #[test]
    fn too_large_input_is_rejected() {
        let big = vec![b' '; MAX_INPUT_BYTES + 1];
        assert_eq!(parse_bytes(&big).unwrap_err().kind, VdfErrorKind::TooLarge);
    }
}
