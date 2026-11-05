use std::fmt;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HexError {
    #[error("odd number of hex digits: {0}")]
    OddLength(usize),
    #[error("invalid hex digit at byte {index}: {ch:?}")]
    InvalidDigit { index: usize, ch: char },
}

pub fn parse(input: &str) -> Result<Vec<u8>, HexError> {
    let cleaned: String = input
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != ',' && *c != '-')
        .collect();

    if !cleaned.len().is_multiple_of(2) {
        return Err(HexError::OddLength(cleaned.len()));
    }

    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let bytes = cleaned.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = digit(bytes[i]).ok_or(HexError::InvalidDigit {
            index: i,
            ch: bytes[i] as char,
        })?;
        let lo = digit(bytes[i + 1]).ok_or(HexError::InvalidDigit {
            index: i + 1,
            ch: bytes[i + 1] as char,
        })?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub struct Dump<'a>(pub &'a [u8]);

impl fmt::Display for Dump<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (row, chunk) in self.0.chunks(16).enumerate() {
            let offset = row * 16;
            write!(f, "{:08x}  ", offset)?;
            for i in 0..16 {
                match chunk.get(i) {
                    Some(b) => write!(f, "{:02x} ", b)?,
                    None => write!(f, "   ")?,
                }
                if i == 7 {
                    write!(f, " ")?;
                }
            }
            write!(f, " |")?;
            for b in chunk {
                let c = *b as char;
                write!(
                    f,
                    "{}",
                    if c.is_ascii_graphic() || c == ' ' {
                        c
                    } else {
                        '.'
                    }
                )?;
            }
            writeln!(f, "|")?;
        }
        Ok(())
    }
}

pub fn dump(bytes: &[u8]) -> Dump<'_> {
    Dump(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain() {
        assert_eq!(parse("0102cdab").unwrap(), vec![0x01, 0x02, 0xcd, 0xab]);
    }

    #[test]
    fn parse_spaced() {
        assert_eq!(
            parse("0x01 02, cd_ab").unwrap(),
            vec![0x01, 0x02, 0xcd, 0xab]
        );
    }

    #[test]
    fn parse_empty() {
        assert_eq!(parse("  ").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn parse_odd() {
        assert_eq!(parse("abc"), Err(HexError::OddLength(3)));
    }

    #[test]
    fn parse_bad_char() {
        assert!(matches!(
            parse("zz"),
            Err(HexError::InvalidDigit { index: 0, .. })
        ));
    }

    #[test]
    fn dump_layout() {
        let text = dump(&[0x01, 0x02, 0xcd, 0xab]).to_string();
        assert!(text.starts_with("00000000  01 02 cd ab"));
        assert!(text.contains("|....|"));
    }
}
