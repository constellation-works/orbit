//! Newest-first line reading for append-only logs.
//!
//! Tail-style readers only want the last few records of a file that may be
//! large. [`ReverseLines`] reads fixed-size blocks backwards from the end, so
//! the bytes read scale with how far back the caller walks, not the file size.

use std::io::{self, Read, Seek, SeekFrom};

/// Default block size for [`ReverseLines::new`].
pub const REVERSE_READ_BLOCK: usize = 64 * 1024;

/// Iterates a seekable reader's lines from last to first.
///
/// Lines are split on `\n`, with one trailing `\r` stripped. Each line must
/// be UTF-8; an invalid one yields [`io::ErrorKind::InvalidData`]. Blank lines
/// are yielded as empty strings; a final newline terminates the last line.
/// The bounded constructors skip lines exceeding their byte limit before
/// decoding them. Scanning and copying are linear in the bytes visited.
pub struct ReverseLines<R> {
    reader: R,
    block_size: usize,
    /// Start of the bytes not yet read.
    pos: u64,
    /// Current block and the end of its unconsumed prefix.
    block: Vec<u8>,
    block_end: usize,
    /// A line's bytes in reverse order, so earlier blocks append without
    /// moving bytes already collected. Reversed once when the line completes.
    carry: Vec<u8>,
    max_line_bytes: usize,
    skipping: bool,
    finished: bool,
    /// Whether the final newline has been handled.
    started: bool,
}

impl<R: Read + Seek> ReverseLines<R> {
    /// Reads backwards in [`REVERSE_READ_BLOCK`] blocks.
    pub fn new(reader: R) -> io::Result<Self> {
        Self::with_block_size(reader, REVERSE_READ_BLOCK)
    }

    /// Reads backwards in `block_size` blocks (`0` means the default), so
    /// tests can force a line to span blocks without a huge fixture.
    pub fn with_block_size(reader: R, block_size: usize) -> io::Result<Self> {
        Self::with_limits(reader, block_size, usize::MAX)
    }

    /// Reads default-size blocks, skipping lines longer than `max_line_bytes`.
    /// The limit excludes `\n` but includes a trailing `\r`.
    pub fn with_max_line_bytes(reader: R, max_line_bytes: usize) -> io::Result<Self> {
        Self::with_limits(reader, REVERSE_READ_BLOCK, max_line_bytes)
    }

    /// Reads `block_size` blocks (`0` means the default), buffering at most
    /// `max_line_bytes` per line. Oversized lines are skipped through their
    /// preceding newline without retaining more bytes. Memory is bounded by
    /// the block size plus the line limit, independent of an oversized line's
    /// length. The limit excludes `\n` but includes a trailing `\r`.
    pub fn with_limits(
        mut reader: R,
        block_size: usize,
        max_line_bytes: usize,
    ) -> io::Result<Self> {
        let pos = reader.seek(SeekFrom::End(0))?;
        Ok(Self {
            reader,
            block_size: if block_size == 0 {
                REVERSE_READ_BLOCK
            } else {
                block_size
            },
            pos,
            block: Vec::new(),
            block_end: 0,
            carry: Vec::new(),
            max_line_bytes,
            skipping: false,
            finished: pos == 0,
            started: false,
        })
    }

    fn read_block(&mut self) -> io::Result<()> {
        let len = u64::min(self.block_size as u64, self.pos);
        let start = self.pos - len;
        self.reader.seek(SeekFrom::Start(start))?;
        self.block.resize(len as usize, 0);
        self.reader.read_exact(&mut self.block)?;
        self.pos = start;
        self.block_end = self.block.len();
        if !self.started {
            self.started = true;
            if self.block.last() == Some(&b'\n') {
                self.block_end -= 1;
            }
        }
        Ok(())
    }

    fn next_raw(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if self.finished {
                return Ok(None);
            }
            if self.block_end == 0 {
                if self.pos == 0 {
                    self.finished = true;
                    return Ok(self.finish_line());
                }
                self.read_block()?;
            }

            let newline = self.block[..self.block_end]
                .iter()
                .rposition(|&byte| byte == b'\n');
            let start = newline.map_or(0, |index| index + 1);
            let fragment = &self.block[start..self.block_end];
            if !self.skipping {
                if fragment.len() > self.max_line_bytes.saturating_sub(self.carry.len()) {
                    self.carry.clear();
                    self.skipping = true;
                } else {
                    self.carry.extend(fragment.iter().rev());
                }
            }
            self.block_end = newline.unwrap_or(0);
            if newline.is_some()
                && let Some(line) = self.finish_line()
            {
                return Ok(Some(line));
            }
        }
    }

    fn finish_line(&mut self) -> Option<Vec<u8>> {
        if self.skipping {
            self.skipping = false;
            return None;
        }
        let mut line = std::mem::take(&mut self.carry);
        line.reverse();
        Some(line)
    }
}

impl<R: Read + Seek> Iterator for ReverseLines<R> {
    type Item = io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_raw() {
            Ok(Some(raw)) => Some(decode_line(raw)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

fn decode_line(mut raw: Vec<u8>) -> io::Result<String> {
    if raw.last() == Some(&b'\r') {
        raw.pop();
    }
    String::from_utf8(raw).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
