//! Bounded bencode for untrusted tracker replies and torrent metadata.
use anyhow::{Result, bail, ensure};
use std::collections::BTreeMap;

pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DEPTH: usize = 64;
pub const MAX_ITEMS: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Bytes(Vec<u8>),
    Int(i64),
    List(Vec<Value>),
    Dict(BTreeMap<Vec<u8>, Value>),
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    items: usize,
}

impl Parser<'_> {
    fn bytes(&mut self) -> Result<Vec<u8>> {
        let start = self.pos;
        while self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        ensure!(
            self.pos > start && self.input.get(self.pos) == Some(&b':'),
            "invalid byte-string length"
        );
        let digits = &self.input[start..self.pos];
        ensure!(
            digits.len() == 1 || digits[0] != b'0',
            "noncanonical byte-string length"
        );
        let len = digits
            .iter()
            .try_fold(0usize, |n, d| {
                n.checked_mul(10)?.checked_add((*d - b'0') as usize)
            })
            .ok_or_else(|| anyhow::anyhow!("byte-string length overflow"))?;
        ensure!(len <= MAX_INPUT_BYTES, "byte-string too large");
        self.pos += 1;
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| anyhow::anyhow!("length overflow"))?;
        ensure!(end <= self.input.len(), "truncated byte string");
        let result = self.input[self.pos..end].to_vec();
        self.pos = end;
        Ok(result)
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        ensure!(depth <= MAX_DEPTH, "bencode nesting too deep");
        self.items += 1;
        ensure!(self.items <= MAX_ITEMS, "too many bencode values");
        match self.input.get(self.pos).copied() {
            Some(b'0'..=b'9') => Ok(Value::Bytes(self.bytes()?)),
            Some(b'i') => {
                self.pos += 1;
                let start = self.pos;
                while self.input.get(self.pos).is_some_and(|b| *b != b'e') {
                    self.pos += 1;
                }
                ensure!(
                    self.input.get(self.pos) == Some(&b'e'),
                    "unterminated integer"
                );
                let s = &self.input[start..self.pos];
                ensure!(!s.is_empty() && s != b"-0", "invalid integer");
                let digits = s.strip_prefix(b"-").unwrap_or(s);
                ensure!(
                    !digits.is_empty()
                        && digits.iter().all(u8::is_ascii_digit)
                        && (digits.len() == 1 || digits[0] != b'0'),
                    "noncanonical integer"
                );
                let number = std::str::from_utf8(s)?.parse::<i64>()?;
                self.pos += 1;
                Ok(Value::Int(number))
            }
            Some(b'l') => {
                self.pos += 1;
                let mut values = Vec::new();
                while self.input.get(self.pos) != Some(&b'e') {
                    ensure!(self.pos < self.input.len(), "unterminated list");
                    values.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(values))
            }
            Some(b'd') => {
                self.pos += 1;
                let mut values = BTreeMap::new();
                while self.input.get(self.pos) != Some(&b'e') {
                    ensure!(self.pos < self.input.len(), "unterminated dictionary");
                    let key = self.bytes()?;
                    let value = self.value(depth + 1)?;
                    ensure!(
                        values.insert(key, value).is_none(),
                        "duplicate dictionary key"
                    );
                }
                self.pos += 1;
                Ok(Value::Dict(values))
            }
            _ => bail!("invalid bencode value"),
        }
    }
}

pub fn decode(input: &[u8]) -> Result<Value> {
    ensure!(input.len() <= MAX_INPUT_BYTES, "bencode input too large");
    let mut parser = Parser {
        input,
        pos: 0,
        items: 0,
    };
    let value = parser.value(0)?;
    ensure!(parser.pos == input.len(), "trailing bencode data");
    Ok(value)
}

pub fn encode(value: &Value) -> Vec<u8> {
    fn append(value: &Value, out: &mut Vec<u8>) {
        match value {
            Value::Bytes(bytes) => {
                out.extend_from_slice(bytes.len().to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(bytes);
            }
            Value::Int(n) => {
                out.push(b'i');
                out.extend_from_slice(n.to_string().as_bytes());
                out.push(b'e');
            }
            Value::List(values) => {
                out.push(b'l');
                for v in values {
                    append(v, out);
                }
                out.push(b'e');
            }
            Value::Dict(values) => {
                out.push(b'd');
                for (key, value) in values {
                    out.extend_from_slice(key.len().to_string().as_bytes());
                    out.push(b':');
                    out.extend_from_slice(key);
                    append(value, out);
                }
                out.push(b'e');
            }
        }
    }
    let mut out = Vec::new();
    append(value, &mut out);
    out
}
