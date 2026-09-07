//! Minimal dependency-free JSON value type, parser and serializer.
//!
//! Deliberately tiny: this project must build with **zero** third-party
//! crates so it can be cross-compiled into a fully static musl binary.
//! Performance is irrelevant here (payloads are a few KB).

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(BTreeMap<String, J>),
}

impl J {
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(m) => m.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            J::Num(n) => Some(*n),
            J::Str(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Integers are used for byte counters; f64 keeps enough precision for
    /// traffic volumes far beyond any realistic quota.
    pub fn as_i64(&self) -> i64 {
        match self {
            J::Num(n) => *n as i64,
            J::Str(s) => s.parse::<f64>().map(|v| v as i64).unwrap_or(0),
            J::Bool(b) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            J::Bool(b) => *b,
            J::Num(n) => *n != 0.0,
            J::Str(s) => s == "true" || s == "1",
            _ => false,
        }
    }

    pub fn as_arr(&self) -> Vec<&J> {
        match self {
            J::Arr(a) => a.iter().collect(),
            _ => Vec::new(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, J::Null)
    }
}

impl fmt::Display for J {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            J::Null => write!(f, "null"),
            J::Bool(b) => write!(f, "{}", if *b { "true" } else { "false" }),
            J::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{}", n)
                }
            }
            J::Str(s) => write!(f, "{}", escape_json(s)),
            J::Arr(a) => {
                write!(f, "[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            J::Obj(m) => {
                write!(f, "{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}:{}", escape_json(k), v)?;
                }
                write!(f, "}}")
            }
        }
    }
}

pub fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------- parsing

pub fn parse(s: &str) -> Result<J, String> {
    let b = s.as_bytes();
    let mut p = Parser { b, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!(
                "expected '{}' at {}",
                c as char,
                self.i
            ))
        }
    }

    fn value(&mut self) -> Result<J, String> {
        match self.peek() {
            Some(b'{') => self.obj(),
            Some(b'[') => self.arr(),
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b't') => self.lit("true", J::Bool(true)),
            Some(b'f') => self.lit("false", J::Bool(false)),
            Some(b'n') => self.lit("null", J::Null),
            Some(_) => self.num(),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn lit(&mut self, word: &str, v: J) -> Result<J, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(format!("invalid literal at {}", self.i))
        }
    }

    fn obj(&mut self) -> Result<J, String> {
        self.expect(b'{')?;
        let mut m = BTreeMap::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(J::Obj(m));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            self.expect(b':')?;
            self.ws();
            let v = self.value()?;
            m.insert(k, v);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(J::Obj(m));
                }
                _ => return Err(format!("expected ',' or '}}' at {}", self.i)),
            }
        }
    }

    fn arr(&mut self) -> Result<J, String> {
        self.expect(b'[')?;
        let mut a = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(J::Arr(a));
        }
        loop {
            self.ws();
            a.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(J::Arr(a));
                }
                _ => return Err(format!("expected ',' or ']' at {}", self.i)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = self.peek().ok_or("bad escape")?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            let hex: String = (0..4)
                                .map(|_| {
                                    let c = self.peek().unwrap_or(b'0');
                                    self.i += 1;
                                    c as char
                                })
                                .collect();
                            let cp = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        other => out.push(other as char),
                    }
                }
                Some(c) => {
                    // Copy the raw UTF-8 byte; the source is valid UTF-8.
                    self.i += 1;
                    out.push(c as char);
                }
            }
        }
    }

    fn num(&mut self) -> Result<J, String> {
        let start = self.i;
        while self.i < self.b.len()
            && matches!(
                self.b[self.i],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.i += 1;
        }
        let txt = std::str::from_utf8(&self.b[start..self.i]).unwrap_or("0");
        txt.parse::<f64>()
            .map(J::Num)
            .map_err(|_| format!("bad number: {}", txt))
    }
}

// ---------------------------------------------------------------- helpers

/// Convenience builder for an object literal.
#[macro_export]
macro_rules! jobj {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut m = ::std::collections::BTreeMap::new();
        $( m.insert($k.to_string(), $v); )*
        $crate::json::J::Obj(m)
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let src = r#"{"a":1,"b":[1,2,{"c":"x\ny"}],"d":null,"e":true,"f":1.5}"#;
        let v = parse(src).unwrap();
        assert_eq!(v.get("a").unwrap().as_i64(), 1);
        assert_eq!(v.get("b").unwrap().as_arr().len(), 3);
        assert_eq!(v.get("e").unwrap().as_bool(), true);
        assert!((v.get("f").unwrap().as_f64().unwrap() - 1.5).abs() < 1e-9);
        assert!(v.get("d").unwrap().is_null());
        let out = v.to_string();
        assert!(parse(&out).is_ok(), "reserialized must re-parse: {}", out);
    }

    #[test]
    fn utf8_chinese() {
        let v = parse(r#"{"n":"香港"}"#).unwrap();
        assert_eq!(v.get("n").unwrap().as_str().unwrap(), "香港");
    }
}
