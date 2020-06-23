//-
// Copyright (c) 2020, Jason Lingle
//
// This file is part of Crymap.
//
// Crymap is free software: you can  redistribute it and/or modify it under the
// terms of  the GNU General Public  License as published by  the Free Software
// Foundation, either version  3 of the License, or (at  your option) any later
// version.
//
// Crymap is distributed  in the hope that  it will be useful,  but WITHOUT ANY
// WARRANTY; without  even the implied  warranty of MERCHANTABILITY  or FITNESS
// FOR  A PARTICULAR  PURPOSE.  See the  GNU General  Public  License for  more
// details.
//
// You should have received a copy of the GNU General Public License along with
// Crymap. If not, see <http://www.gnu.org/licenses/>.

use std::borrow::Cow;
use std::fmt;
use std::io::{self, BufRead, Read};
use std::mem;
use std::str;

use super::header::{self, ContentType};
use crate::account::model::*;
use crate::support::error::Error;

/// A visitor which receives events from the push parser.
///
/// In general, the visitor is permitted to emit an output at any step, at
/// which point parsing is considered complete. Once a visitor returns `Err`,
/// no more methods will be called on it. Methods return `Result<(), Output>`
/// instead of `Option<Output>` to enable use of the `?` operator and to get
/// warnings if results are ignored.
///
/// Methods are declared in the order they are usually called.
#[allow(unused_variables)]
pub trait Visitor: fmt::Debug {
    type Output;

    /// Receives the UID of the message being processed.
    fn uid(&mut self, uid: Uid) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Receives the last modified `Modseq` of the message.
    fn last_modified(&mut self, modseq: Modseq) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Indicates whether loading the flags for the message would be useful.
    fn want_flags(&self) -> bool {
        false
    }

    /// Indicates the current message has the given flags.
    fn flags(&mut self, flags: &[Flag]) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Indicates that the current message is marked \Recent.
    fn recent(&mut self) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Indicates that there are no more flags on the current message,
    /// including not \Recent.
    fn end_flags(&mut self) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Receives the `MessageMetadata` of the message being processed.
    fn metadata(
        &mut self,
        metadata: &MessageMetadata,
    ) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Called once for every line which passes through the parser, in its raw
    /// form.
    ///
    /// `line` typically ends with "\r\n", but may also end with a bare "\n" or
    /// nothing at all when scanning binary payloads or over-long lines.
    ///
    /// When scanning multipart bodies, the final line includes a line ending
    /// which is not actually part of this part. Use `content` for things that
    /// are sensitive to that.
    ///
    /// This is called before more specific methods relating to the line.
    fn raw_line(&mut self, line: &[u8]) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Called for each header found.
    ///
    /// `name` and `value` are in their raw form.
    ///
    /// Only called for headers that pass rudimentary validity checks
    /// (splittable, not too long, name is valid UTF-8).
    fn header(&mut self, name: &str, value: &[u8]) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Called for the content type once it has been parsed.
    ///
    /// This is the only header passed this way since the `Groveller` needs to
    /// parse it itself anyway.
    fn content_type(
        &mut self,
        ct: &ContentType<'_>,
    ) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Indicates that the start of "content" (and the end of the headers) has
    /// been reached and will run to the end of this segment.
    ///
    /// Multipart segments also have "content", which is simply their raw
    /// representation.
    fn start_content(&mut self) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Called as raw data which is strictly part of this part is encountered.
    ///
    /// `data` will usually either be a line ending itself or have no line
    /// ending.
    fn content(&mut self, data: &[u8]) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Indicates that the start of a multipart part has been encountered.
    ///
    /// If the visitor wishes to receive details of what is inside, it can
    /// return a new instance of itself.
    ///
    /// While scanning a part, raw lines are still fed to the "parent" visitor,
    /// while details such as headers or further nested parts are fed to the
    /// "child".
    fn start_part(
        &mut self,
    ) -> Option<Box<dyn Visitor<Output = Self::Output>>> {
        None
    }

    /// Called when the child created by `start_part` completes with a result.
    fn child_result(
        &mut self,
        child_result: Self::Output,
    ) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Indicates that a multipart part has completed.
    fn end_part(&mut self) -> Result<(), Self::Output> {
        Ok(())
    }

    /// Indicates that the end of the segment has been reached.
    ///
    /// This is always the last method to be called. It takes `&mut self` to
    /// keep the trait object-safe.
    fn end(&mut self) -> Self::Output;
}

/// Used by `Groveller` to access information about a message.
pub trait MessageAccessor {
    type Reader: BufRead;

    fn uid(&self) -> Uid;
    fn last_modified(&self) -> Modseq;
    fn is_recent(&self) -> bool;
    fn flags(&self) -> Vec<Flag>;
    fn open(&self) -> Result<(MessageMetadata, Self::Reader), Error>;
}

#[cfg(test)]
pub struct SimpleAccessor {
    pub uid: Uid,
    pub last_modified: Modseq,
    pub recent: bool,
    pub flags: Vec<Flag>,
    pub metadata: MessageMetadata,
    pub data: Vec<u8>,
}

#[cfg(test)]
impl Default for SimpleAccessor {
    fn default() -> Self {
        use chrono::prelude::*;

        SimpleAccessor {
            uid: Uid::MIN,
            last_modified: Modseq::MIN,
            recent: false,
            flags: vec![],
            metadata: MessageMetadata {
                size: 0,
                internal_date: Utc.timestamp_millis(0),
            },
            data: vec![],
        }
    }
}

#[cfg(test)]
impl MessageAccessor for SimpleAccessor {
    type Reader = io::Cursor<Vec<u8>>;

    fn uid(&self) -> Uid {
        self.uid
    }

    fn last_modified(&self) -> Modseq {
        self.last_modified
    }

    fn is_recent(&self) -> bool {
        self.recent
    }

    fn flags(&self) -> Vec<Flag> {
        self.flags.clone()
    }

    fn open(&self) -> Result<(MessageMetadata, Self::Reader), Error> {
        Ok((self.metadata.clone(), io::Cursor::new(self.data.clone())))
    }
}

pub fn grovel<V: Visitor + 'static>(
    accessor: &impl MessageAccessor,
    visitor: V,
) -> Result<V::Output, Error> {
    Groveller::new(Box::new(visitor)).grovel(accessor)
}

/// A push-parser which descends through a MIME message.
///
/// It is designed to be robust moreso than strictly correct. That is, it will
/// accept wildly malformed data but will still do its best to carry on, even
/// in the face of things that are invalid per the standard. It is also very
/// conservative as to what quantity of data it will load into memory.
///
/// The implementation does not know much about the subtleties of headers
/// except Content-Type (the one header it needs to know about to do its job)
/// and does not handle character encoding or transfer encoding. Any 8-bit
/// characters are required to be UTF-8 to be considered as text.
#[derive(Debug)]
struct Groveller<V> {
    visitor: Box<dyn Visitor<Output = V>>,
    /// Whether we are currently in the header part of the message.
    in_headers: bool,
    /// Whether any Content-Type header has been seen.
    seen_content_type: bool,
    /// Whether any multipart delimiter has been seen yet.
    seen_multipart_delim: bool,

    /// The current header, buffered until it reaches maximum size or we hit a
    /// non-continuation.
    buffered_header: Vec<u8>,
    /// When processing multipart data, the ending at the previous "line", so
    /// that we can do strictly correct boundary detection.
    ///
    /// That is, the line ending before a multipart boundary marker is not
    /// considered part of the binary content of the part it delimits.
    buffered_line_ending: &'static [u8],

    /// The Content-Type to use if we don't see one here.
    default_content_type: ContentType<'static>,
    /// The `default_content_type` for any new children.
    child_default_content_type: ContentType<'static>,

    /// If we are processing an inner part in detail, the child groveller.
    child: Option<Box<Self>>,
    /// If this is a multipart body, the delimiter, including leading `--`.
    boundary: Option<Vec<u8>>,
    /// If this has a message/rfc822 body.
    ///
    /// In this case, we treat the body as a sort of unbounded multipart with a
    /// single part since IMAP requires us to be able to recursively produce
    /// the content.
    is_message_rfc822: bool,

    recursion_depth: u32,
}

const CT_TEXT_PLAIN: ContentType<'static> = ContentType {
    typ: Cow::Borrowed(b"text"),
    subtype: Cow::Borrowed(b"plain"),
    parms: vec![],
};

const CT_MESSAGE_RFC822: ContentType<'static> = ContentType {
    typ: Cow::Borrowed(b"message"),
    subtype: Cow::Borrowed(b"rfc822"),
    parms: vec![],
};

const MAX_BUFFER: usize = 65536;
const MAX_RECURSION: u32 = 20;

impl<V> Groveller<V> {
    /// Create a new `Groveller` which will operate on the given visitor.
    fn new(visitor: Box<dyn Visitor<Output = V>>) -> Self {
        Groveller {
            visitor,
            in_headers: true,
            seen_content_type: false,
            seen_multipart_delim: false,

            buffered_header: vec![],
            buffered_line_ending: b"",

            default_content_type: CT_TEXT_PLAIN,
            child_default_content_type: CT_TEXT_PLAIN,

            child: None,
            boundary: None,
            is_message_rfc822: false,
            recursion_depth: 0,
        }
    }

    /// Process the message produced by the given accessor.
    fn grovel(mut self, accessor: &impl MessageAccessor) -> Result<V, Error> {
        if let Err(result) = self.check_info(accessor) {
            return Ok(result);
        }

        let (metadata, reader) = accessor.open()?;
        if let Err(result) = self.visitor.metadata(&metadata) {
            return Ok(result);
        }

        self.read_through(reader)
    }

    fn check_info(&mut self, accessor: &impl MessageAccessor) -> Result<(), V> {
        self.visitor.uid(accessor.uid())?;
        self.visitor.last_modified(accessor.last_modified())?;

        if self.visitor.want_flags() {
            if accessor.is_recent() {
                self.visitor.recent()?;
            }

            let flags = accessor.flags();
            self.visitor.flags(&flags)?;
            self.visitor.end_flags()?;
        }

        Ok(())
    }

    fn read_through(mut self, mut r: impl BufRead) -> Result<V, Error> {
        let mut buf = Vec::new();

        loop {
            let direct_consumed = {
                // See if we can get a full line at once without copying
                let r_buf = r.fill_buf()?;
                if r_buf.is_empty() {
                    // EOF
                    break;
                }

                let lf = memchr::memchr(b'\n', r_buf);
                if let Some(lf) = lf {
                    // Peek at whether we *know* the next line won't be a
                    // continuation so that we can do zero-copy parsing in the
                    // common case where there is no unfolding to be done.
                    let tail = &r_buf[lf + 1..];
                    let could_be_continuation = !tail.is_empty()
                        && (tail.starts_with(b" ") || tail.starts_with(b"\t"));
                    if let Err(output) =
                        self.push(&r_buf[..=lf], could_be_continuation)
                    {
                        return Ok(output);
                    }
                }
                lf
            };

            if let Some(direct_consumed) = direct_consumed {
                r.consume(direct_consumed + 1);
                continue;
            }

            // Nope, need to buffer the line
            buf.clear();
            let nread = r
                .by_ref()
                .take(MAX_BUFFER as u64)
                .read_until(b'\n', &mut buf)?;
            if 0 == nread {
                // EOF
                break;
            }

            if let Err(output) = self.push(&buf, true) {
                return Ok(output);
            }
            if let Err(output) = self.push_content(&buf) {
                return Ok(output);
            }
        }

        Ok(self.end())
    }

    fn push(
        &mut self,
        line: &[u8],
        next_could_be_continuation: bool,
    ) -> Result<(), V> {
        self.visitor.raw_line(line)?;

        if self.in_headers {
            let is_continuation =
                line.starts_with(b" ") || line.starts_with(b"\t");

            if !is_continuation && !self.buffered_header.is_empty() {
                self.process_buffered_header()?;
            }

            if line.is_empty() {
                self.end_headers()?;
            } else if is_continuation {
                if !self.buffered_header.is_empty() {
                    self.buffered_header.extend_from_slice(line);
                    if self.buffered_header.len() > MAX_BUFFER {
                        self.process_buffered_header()?;
                    }
                }
            } else {
                assert!(self.buffered_header.is_empty());
                if next_could_be_continuation {
                    self.buffered_header.extend_from_slice(line);
                } else {
                    self.process_header(line)?;
                }
            }
        } else {
            // Multipart boundary can occur anywhere after a line ending.
            //
            // (Strictly, it's supposed to only be after DOS line endings, but
            // we handle UNIX here too since it's unlikely any agent will ever
            // pick a boundary which exists in a binary payload but only after
            // a sane line ending.)
            if !self.buffered_line_ending.is_empty() {
                let (at_boundary, is_final) = self
                    .boundary
                    .as_ref()
                    .map(|boundary| {
                        if line.starts_with(boundary) {
                            (true, line[boundary.len()..].starts_with(b"--"))
                        } else {
                            (false, false)
                        }
                    })
                    .unwrap_or((false, false));

                if at_boundary {
                    self.buffered_line_ending = b"";
                    if self.seen_multipart_delim {
                        self.end_multipart_part()?;
                        self.seen_multipart_delim = true;
                    }

                    if !is_final {
                        self.start_multipart_part()?;
                    }

                    // Nothing else to do if we recognised the boundary
                    return Ok(());
                }

                // The buffered line ending is part of the child
                let ble = self.buffered_line_ending;
                self.on_child(|child| child.push_content(ble))?;
            }

            // Nothing special to do at this level, push down to the child, if
            // present.
            let content = if line.ends_with(b"\r\n") {
                self.buffered_line_ending = b"\r\n";
                &line[..line.len() - 2]
            } else if line.ends_with(b"\n") {
                self.buffered_line_ending = b"\n";
                &line[..line.len() - 1]
            } else {
                self.buffered_line_ending = b"";
                line
            };

            self.on_child(|child| child.push_content(content))?;
        }

        Ok(())
    }

    fn push_content(&mut self, content: &[u8]) -> Result<(), V> {
        if !self.in_headers {
            self.visitor.content(content)
        } else {
            Ok(())
        }
    }

    fn process_buffered_header(&mut self) -> Result<(), V> {
        let mut bh = mem::replace(&mut self.buffered_header, Vec::new());
        let ret = self.process_header(&bh);
        bh.clear();
        mem::replace(&mut self.buffered_header, bh);
        ret
    }

    fn process_header(&mut self, header: &[u8]) -> Result<(), V> {
        let mut split = header.splitn(2, |&b| b':' == b);

        let (name, value) = match (split.next(), split.next()) {
            (Some(name), Some(value)) => (name, value),
            _ => return Ok(()),
        };

        let name = match str::from_utf8(name) {
            Err(_) => return Ok(()),
            Ok(name) => name.trim(),
        };

        self.visitor.header(name, value)?;

        if "Content-Type".eq_ignore_ascii_case(name) {
            if let Some(ct) = header::parse_content_type(value) {
                self.content_type(&ct)?;
            }
        }

        Ok(())
    }

    fn content_type(&mut self, ct: &ContentType<'_>) -> Result<(), V> {
        // Ignore extra Content-Type headers
        if self.seen_content_type {
            return Ok(());
        }

        self.seen_content_type = true;

        self.visitor.content_type(&ct)?;

        if ct.is_type("multipart") {
            if let Some(bound) = ct.parm("boundary") {
                let mut boundary = Vec::with_capacity(bound.len() + 2);
                boundary.extend_from_slice(b"--");
                boundary.extend_from_slice(bound);
                self.boundary = Some(boundary);
            }

            if ct.is_subtype("digest") {
                self.child_default_content_type = CT_MESSAGE_RFC822;
            }
        } else if ct.is_type("message") && ct.is_subtype("rfc822") {
            self.is_message_rfc822 = true;
        }

        Ok(())
    }

    fn end_headers(&mut self) -> Result<(), V> {
        assert!(self.buffered_header.is_empty());

        if !self.seen_content_type {
            let dct = self.default_content_type.clone();
            self.content_type(&dct)?;
        }

        self.in_headers = false;
        self.visitor.start_content()?;

        if self.is_message_rfc822 {
            self.start_multipart_part()?;
        }

        Ok(())
    }

    fn do_multipart_bookkeeping(&self) -> bool {
        self.recursion_depth < MAX_RECURSION
            && (self.boundary.is_some() || self.is_message_rfc822)
    }

    fn start_multipart_part(&mut self) -> Result<(), V> {
        if !self.do_multipart_bookkeeping() {
            return Ok(());
        }

        assert!(self.child.is_none());

        if let Some(child_visitor) = self.visitor.start_part() {
            let mut child = Self::new(child_visitor);
            child.default_content_type =
                self.child_default_content_type.clone();
            child.recursion_depth = self.recursion_depth + 1;
            self.child = Some(Box::new(child));
        }

        Ok(())
    }

    fn on_child(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<(), V>,
    ) -> Result<(), V> {
        let child_result = self.child.as_mut().and_then(|c| f(c).err());
        if let Some(child_result) = child_result {
            self.child = None;
            self.visitor.child_result(child_result)
        } else {
            Ok(())
        }
    }

    fn end_multipart_part(&mut self) -> Result<(), V> {
        if !self.do_multipart_bookkeeping() {
            return Ok(());
        }

        if let Some(child) = self.child.take() {
            self.visitor.child_result(child.end())?;
        }
        self.visitor.end_part()
    }

    fn end(mut self) -> V {
        if let Err(output) = self.end_multipart_part() {
            return output;
        }

        self.visitor.end()
    }
}
