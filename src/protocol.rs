//! Wire protocol for the Slidr Arduino firmware.
//!
//! Frame format (CSV, `\n`-terminated, 115200 8N1):
//!
//! `<board>,<badge>,<s1>,<s2>,<s3>,<s4>,<s5>,<s6>,<b1>,<b2>,<b3>,<b4>,<b5>,<b6>,<b7>,<b8>,<b9>,<b10>,<b11>\n`

use serde::{Deserialize, Serialize};

pub const NUM_SLIDERS: usize = 6;
pub const NUM_BUTTONS: usize = 11;
pub const FRAME_FIELDS: usize = 2 + NUM_SLIDERS + NUM_BUTTONS;
pub const BAUD: u32 = 115_200;

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BoardOrientation {
    #[default]
    None = -1,
    Left = 0,
    Right = 1,
}

impl BoardOrientation {
    pub fn from_int(v: i32) -> Self {
        match v {
            0 => Self::Left,
            1 => Self::Right,
            _ => Self::None,
        }
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Badge {
    #[default]
    None = 0,
    Supporter = 1,
    Premium = 2,
}

impl Badge {
    pub fn from_int(v: i32) -> Self {
        match v {
            1 => Self::Supporter,
            2 => Self::Premium,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub board: BoardOrientation,
    pub badge: Badge,
    pub sliders: [i32; NUM_SLIDERS],
    pub buttons: [u8; NUM_BUTTONS],
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("too few fields ({0}, need {})", FRAME_FIELDS)]
    Short(usize),
    #[error("invalid int in field {0}: {1:?}")]
    BadInt(usize, String),
}

pub fn parse_line(line: &str) -> Result<Frame, ParseError> {
    let mut it = line.trim_end_matches(['\r', '\n']).split(',');
    let mut idx = 0usize;
    let mut next = || -> Result<&str, ParseError> {
        let f = it.next().ok_or(ParseError::Short(idx))?;
        idx += 1;
        Ok(f)
    };

    let board = parse_i32(next()?, 0)?;
    let badge = parse_i32(next()?, 1)?;

    let mut sliders = [0i32; NUM_SLIDERS];
    for (i, s) in sliders.iter_mut().enumerate() {
        *s = parse_i32(next()?, 2 + i)?.clamp(0, super::curve::SLIDER_RAW_MAX);
    }

    let mut buttons = [0u8; NUM_BUTTONS];
    for (i, b) in buttons.iter_mut().enumerate() {
        let v = parse_i32(next()?, 2 + NUM_SLIDERS + i)?;
        *b = if v != 0 { 1 } else { 0 };
    }

    Ok(Frame {
        board: BoardOrientation::from_int(board),
        badge: Badge::from_int(badge),
        sliders,
        buttons,
    })
}

fn parse_i32(s: &str, field: usize) -> Result<i32, ParseError> {
    s.trim()
        .parse::<i32>()
        .map_err(|_| ParseError::BadInt(field, s.to_string()))
}

/// Streaming frame extractor — feed chunks, get complete frames.
#[derive(Default)]
pub struct FrameReader {
    buf: String,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push raw bytes from the serial port; returns 0-N parsed frames.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        // Treat as ASCII; non-UTF8 is rare on serial CSV and we lossy-convert.
        self.buf.push_str(&String::from_utf8_lossy(chunk));

        let mut out = Vec::new();
        loop {
            let Some(nl) = self.buf.find('\n') else { break };
            let line: String = self.buf.drain(..=nl).collect();
            if let Ok(frame) = parse_line(&line) {
                out.push(frame);
            }
        }
        // Guard against runaway accumulation when no newlines arrive.
        if self.buf.len() > 4096 {
            self.buf.clear();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_frame() {
        let line = "0,1,1,512,1024,256,128,64,0,1,0,1,0,0,0,0,0,0,0\n";
        let f = parse_line(line).unwrap();
        assert_eq!(f.board, BoardOrientation::Left);
        assert_eq!(f.badge, Badge::Supporter);
        assert_eq!(f.sliders, [1, 512, 1024, 256, 128, 64]);
        assert_eq!(f.buttons, [0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn streams_partial_frames() {
        let mut r = FrameReader::new();
        let frames = r.push(b"0,0,1,2,3,4,5,6,");
        assert!(frames.is_empty());
        let frames = r.push(b"1,0,0,1,0,0,0,0,0,0,0\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].sliders, [1, 2, 3, 4, 5, 6]);
        assert_eq!(frames[0].buttons, [1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn rejects_short_frame() {
        assert!(parse_line("0,0,1,2,3\n").is_err());
    }
}
