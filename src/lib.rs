//! A reimplementation of `std::io::BufReader` with additional methods.
#![cfg_attr(feature = "nightly", feature(io))]

use std::io::prelude::*;
use std::io::SeekFrom;
use std::{cmp, fmt, io, iter, ptr};

#[cfg(test)]
mod tests;

const DEFAULT_BUF_SIZE: usize = 64 * 1024;
const MOVE_THRESHOLD: usize = 1024;

pub struct BufReader<R> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
    cap: usize,
}

impl<R> BufReader<R> { 
    pub fn new(inner: R) -> Self {
        BufReader::with_capacity(DEFAULT_BUF_SIZE, inner)
    }

    pub fn with_capacity(cap: usize, inner: R) -> Self {
        let mut self_ = BufReader {
            inner: inner,
            buf: Vec::new(),
            pos: 0,
            cap: 0,
        };

        // We've already implemented exact-ish reallocation, so DRY
        self_.grow(cap);

        self_
    } 

    /// Move data to the start of the buffer, making room at the end for more 
    /// reading.
    pub fn make_room(&mut self) {
        if self.pos == self.cap || self.pos == 0 {
            self.pos = 0;
            self.cap = 0;
            return;
        }

        let src = self.buf[self.pos..].as_ptr();
        let dest = self.buf.as_mut_ptr();

        // Using unsafe for guaranteed memmove.
        unsafe {
            ptr::copy(src, dest, self.cap - self.pos);
        }

        self.cap -= self.pos;
        self.pos = 0;
    }

    /// Grow the internal buffer by *at least* `additional` bytes. May not be
    /// quite exact due to implementation details of the buffer's allocator.
    /// 
    /// ##Note
    /// This should not be called frequently as each call will incur a 
    /// reallocation and a zeroing of the new memory.
    pub fn grow(&mut self, additional: usize) {
        // We're not expecting to grow frequently, so power-of-two growth is 
        // unnecessarily greedy.
        self.buf.reserve_exact(additional);
        // According to reserve_exact(), the allocator can still return more 
        // memory than requested; we might as well use all of it.
        let additional = cmp::max(additional, self.buf.capacity());
        self.buf.extend(iter::repeat(0).take(additional));
    }

    // RFC: pub fn shrink(&mut self, new_len: usize) ?

    /// Get the section of the buffer containing valid data; may be empty.
    ///
    /// Call `.consume()` to remove bytes from the beginning of this section.
    pub fn get_buf(&self) -> &[u8] {
        &self.buf[self.pos .. self.cap]
    }

    /// Get the current number of bytes available in the buffer.
    pub fn available(&self) -> usize {
        self.cap - self.pos
    }

    /// Get the total buffer capacity.
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Get an immutable reference to the underlying reader.
    pub fn get_ref(&self) -> &R { &self.inner }

    /// Get a mutable reference to the underlying reader.
    ///
    /// ## Note
    /// Reading directly from the underlying reader is not recommended.
    pub fn get_mut(&mut self) -> &mut R { &mut self.inner }

    /// Consumes `self` and returns the inner reader only.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Consumes `self` and returns both the underlying reader and the buffer, 
    /// with the data moved to the beginning and the length truncated to contain
    /// only valid data.
    ///
    /// See also: `BufReader::unbuffer()`
    pub fn into_inner_with_buf(mut self) -> (R, Vec<u8>) {
        self.make_room();
        self.buf.truncate(self.cap);
        (self.inner, self.buf)
    }

    /// Consumes `self` and returns an adapter which implements `Read` and will 
    /// empty the buffer before reading directly from the underlying reader.
    pub fn unbuffer(mut self) -> Unbuffer<R> {
        self.buf.truncate(self.cap);

        Unbuffer {
            inner: self.inner,
            buf: self.buf,
            pos: self.pos,
        }
    }
}

impl<R: Read> BufReader<R> {
    /// Unconditionally perform a read into the buffer, moving data to make room
    /// if necessary.
    ///
    /// If the read was successful, returns the number of bytes now available 
    /// in the buffer.
    pub fn read_into_buf(&mut self) -> io::Result<usize> {
        if self.pos == self.cap {
            self.cap = try!(self.inner.read(&mut self.buf));
            self.pos = 0;
        } else {
            // If there's more room at the beginning of the buffer
            // than at the end, move the data down.
            if self.buf.len() - self.cap < self.pos &&
                    self.pos > MOVE_THRESHOLD {
                self.make_room();
            }

            self.cap += try!(self.inner.read(&mut self.buf[self.cap..]));
        }

        Ok(self.cap)
    }
}

impl<R: Read> Read for BufReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If we don't have any buffered data and we're doing a massive read
        // (larger than our internal buffer), bypass our internal buffer
        // entirely.
        if self.pos == self.cap && buf.len() >= self.buf.len() {
            return self.inner.read(buf);
        }
        let nread = {
            let mut rem = try!(self.fill_buf());
            try!(rem.read(buf))
        };
        self.consume(nread);
        Ok(nread)
    }
}

impl<R: Read> BufRead for BufReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        // If we've reached the end of our internal buffer then we need to fetch
        // some more data from the underlying reader.
        if self.pos == self.cap {
            self.cap = try!(self.inner.read(&mut self.buf));
            self.pos = 0;
        }

        Ok(&self.buf[self.pos..self.cap])
    }

    fn consume(&mut self, amt: usize) {
        self.pos = cmp::min(self.pos + amt, self.cap);
    }
}

impl<R> fmt::Debug for BufReader<R> where R: fmt::Debug {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("buf_redux::BufReader")
            .field("reader", &self.inner)
            .field("available", &self.available())
            .field("capacity", &self.capacity())
            .finish()
    }
}

impl<R: Seek> Seek for BufReader<R> {
    /// Seek to an offset, in bytes, in the underlying reader.
    ///
    /// The position used for seeking with `SeekFrom::Current(_)` is the
    /// position the underlying reader would be at if the `BufReader` had no
    /// internal buffer.
    ///
    /// Seeking always discards the internal buffer, even if the seek position
    /// would otherwise fall within it. This guarantees that calling
    /// `.unwrap()` immediately after a seek yields the underlying reader at
    /// the same position.
    ///
    /// See `std::io::Seek` for more details.
    ///
    /// Note: In the edge case where you're seeking with `SeekFrom::Current(n)`
    /// where `n` minus the internal buffer length underflows an `i64`, two
    /// seeks will be performed instead of one. If the second seek returns
    /// `Err`, the underlying reader will be left at the same position it would
    /// have if you seeked to `SeekFrom::Current(0)`.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let result: u64;
        if let SeekFrom::Current(n) = pos {
            let remainder = (self.cap - self.pos) as i64;
            // it should be safe to assume that remainder fits within an i64 as the alternative
            // means we managed to allocate 8 ebibytes and that's absurd.
            // But it's not out of the realm of possibility for some weird underlying reader to
            // support seeking by i64::min_value() so we need to handle underflow when subtracting
            // remainder.
            if let Some(offset) = n.checked_sub(remainder) {
                result = try!(self.inner.seek(SeekFrom::Current(offset)));
            } else {
                // seek backwards by our remainder, and then by the offset
                try!(self.inner.seek(SeekFrom::Current(-remainder)));
                self.pos = self.cap; // empty the buffer
                result = try!(self.inner.seek(SeekFrom::Current(n)));
            }
        } else {
            // Seeking with Start/End doesn't care about our buffer length.
            result = try!(self.inner.seek(pos));
        }
        self.pos = self.cap; // empty the buffer
        Ok(result)
    }
}

/// A `Read` adapter for a consumed `BufReader` which will empty bytes from the buffer before reading from
/// `inner` directly. Frees the buffer when it has been emptied. 
pub struct Unbuffer<R> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
}

impl<R> Unbuffer<R> {
    /// Returns `true` if the buffer still has some bytes left, `false` otherwise.
    pub fn is_buf_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Returns the number of bytes remaining in the buffer.
    pub fn buf_len(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Return the underlying reader, finally letting the buffer die in peace and join its family
    /// in allocation-heaven.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for Unbuffer<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.buf.len() {
            // RFC: Is `try!()` necessary here since this shouldn't
            // really return an error, ever?
            let read = try!((&self.buf[self.pos..]).read(buf));
            self.pos += read;

            if self.pos == self.buf.len() {
                self.buf == Vec::new();
            }

            Ok(read)
        } else {
            self.inner.read(buf)
        }
    }
}

impl<R: fmt::Debug> fmt::Debug for Unbuffer<R> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("buf_redux::Unbuffer")
            .field("reader", &self.inner)
            .field("buffer", &format_args!("{}/{}", self.pos, self.buf.len()))
            .finish()
    }
}

// RFC: impl<R: BufRead> BufRead for Unbuffer<R> ?