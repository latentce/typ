//! Reads the JSON the store writes into `prompt_words`: arrays of pattern
//! texts and a probe's contamination object. Only what the writer produces
//! is understood; anything else is reported as corrupt.

use typ_rs_core::compose::Contamination;

use crate::{Error, Result};

/// The patterns in a JSON array of strings.
pub(crate) fn string_array(json: &str) -> Result<Vec<Box<str>>> {
    match Parser::new(json).value()? {
        Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Value::String(s) => Ok(s.into_boxed_str()),
                other => Err(corrupt(format!("expected a string, found {other:?}"))),
            })
            .collect(),
        other => Err(corrupt(format!("expected an array, found {other:?}"))),
    }
}

/// A probe's contamination from its stored object. The roles of the
/// neighbouring words are recorded there too but are not read back.
pub(crate) fn contamination(json: &str) -> Result<Contamination> {
    let Value::Object(members) = Parser::new(json).value()? else {
        return Err(corrupt("contamination is not an object"));
    };
    let mut recently_targeted_word = None;
    let mut recently_targeted_patterns = None;
    for (key, value) in members {
        match (key.as_str(), value) {
            ("recent_word", Value::Bool(b)) => recently_targeted_word = Some(b),
            ("recent_patterns", Value::Array(items)) => {
                recently_targeted_patterns = Some(
                    items
                        .into_iter()
                        .map(|item| match item {
                            Value::String(s) => Ok(s.into_boxed_str()),
                            other => Err(corrupt(format!("recent_patterns holds {other:?}"))),
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
            }
            ("recent_word" | "recent_patterns", other) => {
                return Err(corrupt(format!("{key} holds {other:?}")));
            }
            _ => {}
        }
    }
    Ok(Contamination {
        recently_targeted_word: recently_targeted_word
            .ok_or_else(|| corrupt("contamination has no recent_word"))?,
        recently_targeted_patterns: recently_targeted_patterns
            .ok_or_else(|| corrupt("contamination has no recent_patterns"))?,
    })
}

fn corrupt(what: impl Into<String>) -> Error {
    Error::Corrupt(format!("prompt_words JSON: {}", what.into()))
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

struct Parser<'a> {
    rest: &'a str,
}

impl<'a> Parser<'a> {
    fn new(json: &'a str) -> Parser<'a> {
        Parser { rest: json }
    }

    /// The one value the text holds, with nothing but whitespace after it.
    fn value(mut self) -> Result<Value> {
        let value = self.parse()?;
        self.skip_whitespace();
        if self.rest.is_empty() {
            Ok(value)
        } else {
            Err(corrupt(format!("trailing text {:?}", self.rest)))
        }
    }

    fn parse(&mut self) -> Result<Value> {
        self.skip_whitespace();
        match self.rest.chars().next() {
            None => Err(corrupt("unexpected end")),
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => self.string().map(Value::String),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(_) => self.literal(),
        }
    }

    fn object(&mut self) -> Result<Value> {
        self.expect('{')?;
        let mut members = Vec::new();
        self.skip_whitespace();
        if self.take('}') {
            return Ok(Value::Object(members));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(':')?;
            let value = self.parse()?;
            members.push((key, value));
            self.skip_whitespace();
            if self.take(',') {
                continue;
            }
            self.expect('}')?;
            return Ok(Value::Object(members));
        }
    }

    fn array(&mut self) -> Result<Value> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.take(']') {
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.parse()?);
            self.skip_whitespace();
            if self.take(',') {
                continue;
            }
            self.expect(']')?;
            return Ok(Value::Array(items));
        }
    }

    fn string(&mut self) -> Result<String> {
        self.expect('"')?;
        let mut out = String::new();
        let mut chars = self.rest.char_indices();
        loop {
            let Some((i, c)) = chars.next() else {
                return Err(corrupt("unterminated string"));
            };
            match c {
                '"' => {
                    self.rest = &self.rest[i + 1..];
                    return Ok(out);
                }
                '\\' => match chars.next() {
                    Some((_, '"')) => out.push('"'),
                    Some((_, '\\')) => out.push('\\'),
                    Some((_, '/')) => out.push('/'),
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, other)) => {
                        return Err(corrupt(format!("unsupported escape \\{other}")));
                    }
                    None => return Err(corrupt("unterminated string")),
                },
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Result<Value> {
        let end = self
            .rest
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')))
            .unwrap_or(self.rest.len());
        let (text, rest) = self.rest.split_at(end);
        self.rest = rest;
        text.parse()
            .map(Value::Number)
            .map_err(|_| corrupt(format!("{text:?} is not a number")))
    }

    fn literal(&mut self) -> Result<Value> {
        for (text, value) in [
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("null", Value::Null),
        ] {
            if let Some(rest) = self.rest.strip_prefix(text) {
                self.rest = rest;
                return Ok(value);
            }
        }
        Err(corrupt(format!("unexpected {:?}", self.rest)))
    }

    fn skip_whitespace(&mut self) {
        self.rest = self.rest.trim_start();
    }

    fn take(&mut self, c: char) -> bool {
        match self.rest.strip_prefix(c) {
            Some(rest) => {
                self.rest = rest;
                true
            }
            None => false,
        }
    }

    fn expect(&mut self, c: char) -> Result<()> {
        if self.take(c) {
            Ok(())
        } else {
            Err(corrupt(format!("expected {c:?} at {:?}", self.rest)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_arrays_read_back_with_escapes() {
        assert_eq!(string_array("[]").unwrap(), Vec::<Box<str>>::new());
        assert_eq!(
            string_array(r#"["th", " th", "a\"b", "c\\d"]"#).unwrap(),
            vec![
                Box::from("th"),
                Box::from(" th"),
                Box::from("a\"b"),
                Box::from("c\\d")
            ]
        );
        assert!(string_array(r#"["th""#).is_err());
        assert!(string_array(r#"[1]"#).is_err());
        assert!(string_array(r#"{"a":1}"#).is_err());
    }

    #[test]
    fn contamination_reads_the_two_fields_it_needs_and_ignores_the_rest() {
        let c = contamination(
            r#"{"before":"targeted","after":null,"recent_word":true,"recent_patterns":["og"]}"#,
        )
        .unwrap();
        assert_eq!(
            c,
            Contamination {
                recently_targeted_word: true,
                recently_targeted_patterns: vec!["og".into()],
            }
        );
        assert!(contamination(r#"{"recent_word":true}"#).is_err());
        assert!(contamination(r#"{"recent_word":1,"recent_patterns":[]}"#).is_err());
        assert!(contamination(r#"[true]"#).is_err());
        assert!(contamination(r#"{"recent_word":true,"recent_patterns":[]} x"#).is_err());
    }
}
