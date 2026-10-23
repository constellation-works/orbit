//  Public reverse-reader boundary and resource regression coverage.
//  This binary owns its allocator so allocation volume can detect repeated
//  carry copies deterministically, without timing limits or synthetic load.

thread_local! {
    static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) };
}

struct CountingAllocator;

fn count_allocation(size: usize) {
    let _ = ALLOCATED.try_with(|count| {
        if let Some(total) = count.get() {
            count.set(Some(total.saturating_add(size)));
        }
    });
}

// SAFETY: Every allocation operation delegates unchanged to System. The
// thread-local counter is allocation-free and does not inspect pointers.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: The caller supplies the layout required by System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: The caller supplies the layout required by System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The pointer and layout are forwarded to their allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation(new_size);
        // SAFETY: The original pointer, layout and new size are unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct Counting<'a> {
    inner: Cursor<&'a [u8]>,
    bytes_read: &'a Cell<usize>,
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes_read.set(self.bytes_read.get() + read);
        Ok(read)
    }
}

impl Seek for Counting<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn reverse_scans_preserve_lines_and_bound_reads_and_copy_allocations() {
    // Cross every delimiter, UTF-8 and cap boundary with different block sizes.
    for bytes in [
        "",
        "\n",
        "\n\n",
        "abc",
        "abc\n",
        "\nabc\n\n",
        "ab\r\ncd\n",
        "é中\nxy",
        "short\n0123456789\nlast",
        "0123456789\nshort\n",
        "short\n0123456789",
    ] {
        for block in [1, 2, 3, 4, 7, 16, 0] {
            for cap in [0, 1, 2, 3, 5, 10, usize::MAX] {
                let expected: Vec<_> = bytes
                    .split_terminator('\n')
                    .filter(|line| line.len() <= cap)
                    .map(|line| line.strip_suffix('\r').unwrap_or(line))
                    .rev()
                    .collect();
                let actual = ReverseLines::with_limits(Cursor::new(bytes), block, cap)
                    .unwrap()
                    .collect::<io::Result<Vec<_>>>()
                    .unwrap();
                assert_eq!(
                    actual, expected,
                    "input {bytes:?}, block {block}, cap {cap}"
                );
            }
        }
    }

    // Counting reads alone cannot catch quadratic copies: the old reader read
    // each byte once, but allocated progressively larger carry buffers.
    for record_bytes in [4 << 20, 8 << 20] {
        let bytes = format!("older\n{}\nnewer\n", "x".repeat(record_bytes));
        for cap in [1 << 20, usize::MAX] {
            let read = Cell::new(0);
            ALLOCATED.with(|count| count.set(Some(0)));
            let lines = ReverseLines::with_limits(
                Counting {
                    inner: Cursor::new(bytes.as_bytes()),
                    bytes_read: &read,
                },
                REVERSE_READ_BLOCK,
                cap,
            )
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
            let allocated = ALLOCATED.with(|count| count.replace(None).unwrap());
            assert_eq!(read.get(), bytes.len(), "each scanned byte is read once");
            assert_eq!(lines.first().unwrap(), "newer");
            assert_eq!(lines.last().unwrap(), "older");
            if cap == usize::MAX {
                assert_eq!(lines.len(), 3);
                assert_eq!(lines[1].len(), record_bytes);
                assert!(lines[1].bytes().all(|byte| byte == b'x'));
            } else {
                assert_eq!(lines, ["newer", "older"], "oversized record is skipped");
            }
            let retained = record_bytes.min(cap) + REVERSE_READ_BLOCK;
            assert!(
                allocated <= 8 * retained,
                "reverse scan must avoid quadratic carry copies: {allocated} allocation bytes for {record_bytes} record bytes, cap {cap}"
            );
        }
    }
}
