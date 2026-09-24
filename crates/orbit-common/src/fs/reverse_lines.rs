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
/// be UTF-8; an invalid one yields [`io::ErrorKind::InvalidData`]. Blank lines,
/// including the one after a final newline, are yielded as empty strings.
pub struct ReverseLines<R> {
    reader: R,
    block_size: usize,
    /// Start of the bytes not yet read.
    pos: u64,
    /// A line's leading bytes, whose start lies before `pos`.
    carry: Vec<u8>,
    /// Complete lines of the current block, oldest first.
    pending: Vec<Vec<u8>>,
    /// Whether the empty remainder after a final newline was skipped.
    started: bool,
}

impl<R: Read + Seek> ReverseLines<R> {
    /// Reads backwards in [`REVERSE_READ_BLOCK`] blocks.
    pub fn new(reader: R) -> io::Result<Self> {
        Self::with_block_size(reader, REVERSE_READ_BLOCK)
    }

    /// Reads backwards in `block_size` blocks (`0` means the default), so
    /// tests can force a line to span blocks without a huge fixture.
    pub fn with_block_size(mut reader: R, block_size: usize) -> io::Result<Self> {
        let pos = reader.seek(SeekFrom::End(0))?;
        Ok(Self {
            reader,
            block_size: if block_size == 0 {
                REVERSE_READ_BLOCK
            } else {
                block_size
            },
            pos,
            carry: Vec::new(),
            pending: Vec::new(),
            started: false,
        })
    }

    fn read_block(&mut self) -> io::Result<()> {
        let len = u64::min(self.block_size as u64, self.pos);
        let start = self.pos - len;
        self.reader.seek(SeekFrom::Start(start))?;
        let mut block = vec![0; len as usize];
        self.reader.read_exact(&mut block)?;
        block.append(&mut self.carry);
        self.pos = start;

        let mut segments = block.split(|&byte| byte == b'\n');
        // The first segment may continue in earlier bytes; hold it back.
        let first = segments.next().unwrap_or_default().to_vec();
        self.pending.extend(segments.map(<[u8]>::to_vec));
        if start == 0 {
            self.pending.insert(0, first);
        } else {
            self.carry = first;
        }
        Ok(())
    }

    fn next_raw(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(line) = self.pending.pop() {
                return Ok(Some(line));
            }
            if self.pos == 0 {
                return Ok(None);
            }
            self.read_block()?;
            if !self.started {
                self.started = true;
                // A trailing newline terminates the last line; it does not
                // start an empty one.
                if self.pending.last().is_some_and(Vec::is_empty) {
                    self.pending.pop();
                }
            }
        }
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
