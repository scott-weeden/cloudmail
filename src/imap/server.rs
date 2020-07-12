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
use std::io::{self, BufRead, Read, Write};
use std::str;
use std::sync::{Arc, Mutex};

use lazy_static::lazy_static;
use regex::bytes::Regex;

use super::command_processor::CommandProcessor;
use super::lex::LexWriter;
use super::syntax as s;
use crate::support::error::Error;

const MAX_CMDLINE: usize = 65536;
const APPEND_SIZE_LIMIT: u32 = 64 * 1024 * 1024;

lazy_static! {
    static ref LITERAL_AT_EOL: Regex =
        Regex::new(r#"~?\{([0-9]+)\+?\}$"#).unwrap();
}

pub struct Server<R, W> {
    read: R,
    write: Arc<Mutex<W>>,
    processor: CommandProcessor,
    sent_bye: bool,
}

impl<R: BufRead + Send + Sync, W: Write + Send> Server<R, W> {
    pub fn new(read: R, write: W, processor: CommandProcessor) -> Self {
        Server {
            read,
            write: Arc::new(Mutex::new(write)),
            processor,
            sent_bye: false,
        }
    }

    /// Run the server.
    ///
    /// Blocks until an error occurs or a BYE response has been sent.
    pub fn run(&mut self) -> Result<(), Error> {
        self.send_response(self.processor.greet())?;

        let mut cmdline = Vec::<u8>::new();

        while !self.sent_bye && !self.processor.logged_out() {
            let nread = self.buffer_next_line(&mut cmdline, true)?;

            if let Some((before_literal, length, literal_plus)) =
                self.check_literal(&cmdline, nread)
            {
                if let Ok((b"", append)) =
                    s::AppendCommandStart::parse(before_literal)
                {
                    if length > APPEND_SIZE_LIMIT {
                        let tag = append.tag.into_owned();
                        self.append_limit_exceeded(
                            &mut cmdline,
                            Cow::Owned(tag),
                            length,
                            literal_plus,
                        )?;
                        continue;
                    }

                    self.accept_literal(literal_plus)?;

                    let tag = (*append.tag).to_owned();
                    let mut literal_reader =
                        self.read.by_ref().take(length.into());
                    if let Err(resp) = self.processor.cmd_append_start(
                        append,
                        length,
                        &mut literal_reader,
                    ) {
                        // cmd_append_start() may have ended prematurely, so
                        // ensure we read the entire literal.
                        let _ = io::copy(&mut literal_reader, &mut io::sink());
                        self.send_response(s::ResponseLine {
                            tag: Some(Cow::Owned(tag)),
                            response: resp,
                        })?;
                        self.discard_command(&mut cmdline, None)?;
                        self.processor.cmd_append_abort();
                        continue;
                    }

                    self.handle_append(&mut cmdline, tag)?;
                } else {
                    // Not an append; just add the literal to the command line.

                    cmdline.extend_from_slice(b"\r\n");
                    if length as usize + cmdline.len() > MAX_CMDLINE {
                        self.command_line_too_long(
                            &mut cmdline,
                            true,
                            true,
                            Some((length, literal_plus)),
                        )?;
                        continue;
                    }

                    self.accept_literal(literal_plus)?;
                    let nread = self
                        .read
                        .by_ref()
                        .take(length as u64)
                        .read_to_end(&mut cmdline)?;
                    if nread != length as usize {
                        return Err(Error::Io(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "EOF reading literal",
                        )));
                    }
                }
            } else {
                // No ending literal; this should be a complete command
                // TODO AUTHENTICATE flow
                if let Ok((b"", cmdline)) = s::CommandLine::parse(&cmdline) {
                    let r = self
                        .processor
                        .handle_command(cmdline, &response_sender(&self.write));
                    self.send_response(r)?;
                } else if let Ok((_, frag)) =
                    s::UnknownCommandFragment::parse(&cmdline)
                {
                    self.send_response(s::ResponseLine {
                        tag: Some(frag.tag),
                        response: s::Response::Cond(s::CondResponse {
                            cond: s::RespCondType::Bad,
                            code: Some(s::RespTextCode::Parse(())),
                            quip: Some(Cow::Borrowed(
                                "Unrecognised command syntax",
                            )),
                        }),
                    })?;
                } else {
                    self.send_response(s::ResponseLine {
                        tag: None,
                        response: s::Response::Cond(s::CondResponse {
                            cond: s::RespCondType::Bye,
                            code: Some(s::RespTextCode::Parse(())),
                            quip: Some(Cow::Borrowed(
                                "That doesn't look anything like \
                                 an IMAP command!",
                            )),
                        }),
                    })?;
                }

                cmdline.clear();
            }
        }

        Ok(())
    }

    /// Read the next line, appending it to `cmdline`.
    ///
    /// Returns the number of bytes added to `cmdline`.
    ///
    /// Both DOS newlines and sane newlines (THE HORROR!) are accepted. The
    /// line ending is removed from the buffer.
    ///
    /// If EOF is reached before the full line is read, returns an
    /// `UnexpectedEof` IO error.
    ///
    /// If the maximum command line length is exceeded, sends an appropriate
    /// response to the client, swallows the whole command, and returns
    /// successfully with `cmdline` clear.
    fn buffer_next_line(
        &mut self,
        cmdline: &mut Vec<u8>,
        initial: bool,
    ) -> Result<usize, Error> {
        let mut nread = self
            .read
            .by_ref()
            .take(MAX_CMDLINE as u64)
            .read_until(b'\n', cmdline)?;

        if 0 == nread {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "EOF reached before reading full line",
            )));
        }

        if cmdline.len() > MAX_CMDLINE || !cmdline.ends_with(b"\n") {
            self.command_line_too_long(cmdline, false, initial, None)?;
            return Ok(0);
        }

        // Drop ending LF
        cmdline.pop().expect("No LF at end of cmdline?");
        nread -= 1;
        // If there's an ending CR, drop that too
        if cmdline.ends_with(b"\r") {
            cmdline.pop().expect("No CR at end of cmdline?");
            nread -= 1;
        }

        Ok(nread)
    }

    /// Check whether the current command line ends with a literal.
    ///
    /// Only the last `nread` bytes of the command line are checked, so that
    /// this can consider only things added by the last read operation.
    fn check_literal<'a>(
        &self,
        cmdline: &'a [u8],
        nread: usize,
    ) -> Option<(&'a [u8], u32, bool)> {
        LITERAL_AT_EOL
            .captures(&cmdline[cmdline.len() - nread..])
            .and_then(|c| c.get(0).and_then(|m0| c.get(1).map(|m1| (m0, m1))))
            .and_then(|(m0, m1)| {
                str::from_utf8(m1.as_bytes())
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .map(|len| {
                        (
                            &cmdline[..m0.start()],
                            len,
                            m0.as_bytes().contains(&b'+'),
                        )
                    })
            })
    }

    /// Send the appropriate continuation for a literal.
    fn accept_literal(&self, literal_plus: bool) -> Result<(), Error> {
        if !literal_plus {
            self.write.lock().unwrap().write_all(b"+ go\r\n")?;
        }

        Ok(())
    }

    /// Handle command rejection due to the command line limit being exceeded.
    ///
    /// `recoverable` indicates whether it is expected that this condition can
    /// be repaired by following the basic lexical syntax until the end of the
    /// command is reached. This must be `false` if `cmdline` could potentially
    /// contain a partial literal.
    ///
    /// `initial` indicates whether `cmdline` is expected to contain a tag.
    ///
    /// `literal_info` gives details about a literal, if any, which is
    /// currently initiated at the end of `cmdline`.
    fn command_line_too_long(
        &mut self,
        cmdline: &mut Vec<u8>,
        recoverable: bool,
        initial: bool,
        literal_info: Option<(u32, bool)>,
    ) -> Result<(), Error> {
        if let (true, Ok((_, frag))) =
            (initial, s::UnknownCommandFragment::parse(&cmdline))
        {
            self.send_response(s::ResponseLine {
                // The RFC 3501 grammar doesn't allow tagged BYE, so if we're
                // going to send BYE, we need to ensure it is untagged.
                tag: if recoverable { Some(frag.tag) } else { None },
                response: s::Response::Cond(s::CondResponse {
                    cond: if recoverable {
                        s::RespCondType::No
                    } else {
                        s::RespCondType::Bye
                    },
                    code: None,
                    quip: Some(Cow::Borrowed("Command line too long")),
                }),
            })?;
            self.discard_command(cmdline, literal_info)?;
        } else {
            self.send_response(s::ResponseLine {
                tag: None,
                response: s::Response::Cond(s::CondResponse {
                    cond: s::RespCondType::Bye,
                    code: None,
                    quip: Some(Cow::Borrowed(if initial {
                        "That doesn't look anything like \
                             an IMAP command!"
                    } else {
                        "Command line continuation too long"
                    })),
                }),
            })?;
            cmdline.clear();
        }

        Ok(())
    }

    /// Discard data from the read stream until an error occurs, a BYE response
    /// is sent, or the end of the command is reached.
    ///
    /// This assumes that the current `cmdline` is incomplete, i.e., the caller
    /// knows there is at least one more line belonging to the command. The
    /// command is not parsed for literals on entry to this function, since in
    /// some cases (for example, during append), the literal may have been
    /// consumed already.
    ///
    /// `literal_info` gives details on any unconsumed literal currently at the
    /// end of `cmdline`.
    fn discard_command(
        &mut self,
        cmdline: &mut Vec<u8>,
        mut literal_info: Option<(u32, bool)>,
    ) -> Result<(), Error> {
        while !self.sent_bye {
            if let Some((len, literal_plus)) = literal_info.take() {
                // If not using LITERAL+, the No or Bad response we already
                // sent back to the client aborts the literal, so we are
                // consistent at this point.
                if !literal_plus {
                    break;
                }

                // Discard the literal
                io::copy(
                    &mut self.read.by_ref().take(len.into()),
                    &mut io::sink(),
                )?;
            }

            cmdline.clear();
            let nread = self.buffer_next_line(cmdline, false)?;
            if let Some((_, len, literal_plus)) =
                self.check_literal(cmdline, nread)
            {
                literal_info = Some((len, literal_plus));
            } else {
                // Reached end of line without literal; command is done
                break;
            }
        }

        cmdline.clear();
        Ok(())
    }

    /// Handle the APPEND command beyond the first item.
    ///
    /// This must be called immediately after the literal of the first item has
    /// been fully read.
    fn handle_append(
        &mut self,
        cmdline: &mut Vec<u8>,
        tag: String,
    ) -> Result<(), Error> {
        loop {
            cmdline.clear();
            let nread = self.buffer_next_line(cmdline, false)?;

            // End of append if the command line is empty and this isn't due to
            // reading a bunch of stuff and discarding it.
            if cmdline.is_empty() && nread <= 2 {
                let r = self.processor.cmd_append_commit(
                    Cow::Owned(tag),
                    &response_sender(&self.write),
                );
                self.send_response(r)?;
                return Ok(());
            }

            if let Some((before_literal, length, literal_plus)) =
                self.check_literal(cmdline, nread)
            {
                if length > APPEND_SIZE_LIMIT {
                    self.append_limit_exceeded(
                        cmdline,
                        Cow::Owned(tag),
                        length,
                        literal_plus,
                    )?;
                    self.processor.cmd_append_abort();
                    return Ok(());
                }

                if let Ok((b"", frag)) =
                    s::AppendFragment::parse(before_literal)
                {
                    self.accept_literal(literal_plus)?;

                    let mut literal_reader =
                        self.read.by_ref().take(length.into());
                    if let Err(resp) = self.processor.cmd_append_item(
                        frag,
                        length,
                        &mut literal_reader,
                    ) {
                        // cmd_append_start() may have ended prematurely, so
                        // ensure we read the entire literal.
                        let _ = io::copy(&mut literal_reader, &mut io::sink());
                        self.send_response(s::ResponseLine {
                            tag: Some(Cow::Owned(tag)),
                            response: resp,
                        })?;
                        self.discard_command(cmdline, None)?;
                        self.processor.cmd_append_abort();
                        return Ok(());
                    }
                } else {
                    self.send_response(s::ResponseLine {
                        tag: Some(Cow::Owned(tag)),
                        response: s::Response::Cond(s::CondResponse {
                            cond: s::RespCondType::Bad,
                            code: None,
                            quip: Some(Cow::Borrowed("Bad APPEND syntax")),
                        }),
                    })?;
                    self.discard_command(
                        cmdline,
                        Some((length, literal_plus)),
                    )?;
                    self.processor.cmd_append_abort();
                    return Ok(());
                }
            }

            // Not an understood append fragment
            self.send_response(s::ResponseLine {
                tag: Some(Cow::Owned(tag)),
                response: s::Response::Cond(s::CondResponse {
                    cond: s::RespCondType::Bad,
                    code: None,
                    quip: Some(Cow::Borrowed("Bad APPEND syntax")),
                }),
            })?;
            self.discard_command(cmdline, None)?;
            self.processor.cmd_append_abort();
            return Ok(());
        }
    }

    fn append_limit_exceeded(
        &mut self,
        cmdline: &mut Vec<u8>,
        tag: Cow<'_, str>,
        length: u32,
        literal_plus: bool,
    ) -> Result<(), Error> {
        self.send_response(s::ResponseLine {
            tag: Some(tag),
            response: s::Response::Cond(s::CondResponse {
                cond: s::RespCondType::Bad,
                code: None,
                quip: Some(Cow::Borrowed("APPEND size limit exceeded")),
            }),
        })?;
        self.discard_command(cmdline, Some((length, literal_plus)))?;
        self.processor.cmd_append_abort();
        Ok(())
    }

    fn send_response(
        &mut self,
        mut r: s::ResponseLine<'_>,
    ) -> Result<(), Error> {
        self.sent_bye |= matches!(
            r, s::ResponseLine { response: s::Response::Cond(s::CondResponse {
                cond: s::RespCondType::Bye,
                ..
            }), .. });

        let mut w = self.write.lock().unwrap();
        {
            let mut w = LexWriter::new(&mut *w, false, false);
            r.write_to(&mut w)?;
            w.verbatim_bytes(b"\r\n")?;
        }
        w.flush()?;
        Ok(())
    }
}

fn response_sender<'a>(
    w: &'a Arc<Mutex<impl Write + Send>>,
) -> impl Fn(s::Response<'_>) + Send + Sync + 'a {
    move |r| {
        let mut w = w.lock().unwrap();
        let mut w = LexWriter::new(&mut *w, false, false);
        let _ = s::ResponseLine {
            tag: None,
            response: r,
        }
        .write_to(&mut w);
        let _ = w.verbatim_bytes(b"\r\n");
    }
}
