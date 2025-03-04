use crate::http1::headers::{self, ConnectionState, Header};
use memchr::memchr;
use rustls_pki_types::SubjectPublicKeyInfoDer;
use std::io::{BufRead, Cursor};
use std::str;
use std::task::Poll;

const VEC_PREALLOC: usize = 16 * 1024;
pub type Result<T> = std::result::Result<T, HttpResErr>;

// helper function
fn str_to_usize(line: &[u8]) -> Option<usize> {
    if let Ok(string) = str::from_utf8(line) {
        usize::from_str_radix(string, 16).ok()
    } else {
        None
    }
}

#[derive(Debug)]
/// Errors related to parsing a response.
pub enum HttpResErr {
    Empty,
    InvalidHeader(String),
    InvalidFirstLine(String),

    // Body errors
    InvalidBody(&'static str),
}

impl std::fmt::Display for HttpResErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#?}", self)
    }
}

impl std::error::Error for HttpResErr {}

#[derive(Debug, Copy, Clone)]
pub(crate) struct StateSnapshot {
    pub conn_closed: bool,
    pub decoder_err: bool,

    // This should eventually be converted into an enum.
    pub upgrade: bool,

    // todo!
    pub upgrade_protocol: usize,
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            conn_closed: false,
            decoder_err: false,
            upgrade: false,
            upgrade_protocol: 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
/// Represents the current state of the `DataDecoder`
enum DecoderState {
    Headers,
    Content,
    ChunkedContent,
    Finished,
    Error,
}

#[derive(Debug)]
/// This is a decoder for HTTP 1.x responses.
pub(crate) struct DataDecoder {
    /// Detected encoding from headers
    encoding: headers::TrfrEncodingType,

    /// State
    state: DecoderState,

    /// Content in construction,
    content: Option<Vec<u8>>,

    /// Response in construction,
    resp: Option<Response>,

    /// Content Length registered
    content_len: Option<usize>,

    /// Snapshot
    snap: StateSnapshot,
}

impl DataDecoder {
    /// Creates a new `DataDecoder` with no encoding by default.
    pub(crate) fn new() -> Self {
        Self {
            encoding: headers::TrfrEncodingType::None,
            state: DecoderState::Headers,
            content: Some(Vec::with_capacity(VEC_PREALLOC)),
            resp: None,
            content_len: None,
            snap: StateSnapshot::default(),
        }
    }

    /// Returns a `bool` indicating whether it's finished (true) or not done yet (false)
    pub(crate) fn finished(&self) -> bool {
        self.state == DecoderState::Finished
    }

    /// Obtains a `Option<Response>` which either contains the `Response` or `None`
    pub(crate) fn get_resp(&mut self) -> Option<Response> {
        if self.finished() {
            let mut resp = self.resp.take().unwrap();

            let content = if self.content.as_ref().unwrap().len() == 0 {
                None
            } else {
                self.content.take()
            };

            resp.content = content;

            self.content = Some(Vec::with_capacity(VEC_PREALLOC));

            return Some(resp);
        };

        None
    }

    pub(crate) fn encoding(&self) -> headers::TrfrEncodingType {
        self.encoding
    }

    pub(crate) fn decode(&mut self, data: &[u8]) -> Result<()> {
        use headers::TrfrEncodingType::{Chunked, Gzip, GzipChunked};

        if self.state == DecoderState::Finished {
            return Ok(());
        }

        let bytes = if self.state == DecoderState::Headers {
            let cursor = Self::parse_headers(self, data)?;
            let pos = cursor.position() as usize;

            &cursor.into_inner()[pos..]
        } else {
            data
        };

        match self.encoding() {
            Chunked => self.chunked_decode(bytes)?,
            Gzip => todo!(),
            GzipChunked => todo!(),
            _ => {
                self.content.as_mut().unwrap().extend_from_slice(bytes);

                let ready = match self.content_len {
                    Some(len) => len == self.content.as_ref().unwrap().len(),
                    None => true,
                };

                if ready {
                    self.s_fin();
                }
            }
        };

        Ok(())
    }

    fn chunked_decode(&mut self, data: &[u8]) -> Result<()> {
        let mut cursor = Cursor::new(data);

        loop {
            let buf = cursor.fill_buf().unwrap();
            if buf.len() == 0 {
                break;
            }

            let index = match memchr(b'\r', buf) {
                None => {
                    // This should probably mean an error.
                    // let's make it as finished right now.
                    self.s_fin();
                    break;
                }

                Some(0) => {
                    self.s_fin();
                    break;
                }
                Some(num) => num,
            };

            let len = match str_to_usize(&buf[..index]) {
                Some(len) => len,
                None => {
                    let err = HttpResErr::InvalidBody("couldn't read hex length of chunk");
                    self.s_err();
                    return Err(err);
                }
            };

            if len == 0 {
                self.state = DecoderState::Finished;
                break;
            };

            self.content
                .as_mut()
                .unwrap()
                .extend_from_slice(&buf[index + 2..len + 3]);
            cursor.consume(index + len + 4);
        }

        Ok(())
    }

    fn parse_headers<'a, 'b>(me: &mut Self, data: &'b [u8]) -> Result<Cursor<&'b [u8]>>
    where
        'a: 'b,
    {
        me.s_content();

        let mut headers: Vec<Header> = Vec::with_capacity(24);
        let mut cursor = Cursor::new(data);

        let buf = cursor.fill_buf().unwrap();

        let status_code = match memchr(b'\r', buf) {
            None => {
                me.s_err();
                return Err(HttpResErr::Empty);
            }
            Some(num) => {
                let rdlen = num + 2;

                let line = &buf[..rdlen];

                let string_num = match str::from_utf8(&line[9..12]) {
                    Err(e) => return Err(HttpResErr::InvalidFirstLine(e.to_string())),
                    Ok(s) => s,
                };

                let code = match string_num.parse::<u16>() {
                    Err(e) => return Err(HttpResErr::InvalidFirstLine(e.to_string())),
                    Ok(num) => num,
                };

                cursor.consume(rdlen);

                code
            }
        };

        loop {
            let buf = cursor.fill_buf().unwrap();

            if buf.len() == 0 {
                break;
            };

            match memchr(b'\r', buf) {
                None => break,
                Some(num) => {
                    let string = match str::from_utf8(&buf[..num]) {
                        Err(_error) => {
                            return Err(HttpResErr::InvalidHeader(
                                "couldn't read first line of response".to_string(),
                            ));
                        }
                        Ok(s) => s,
                    };

                    if string.len() == 0 {
                        cursor.consume(2);
                        break;
                    };
                    dbg!(string);
                    match Header::serialize(string) {
                        Err(_) => {
                            cursor.consume(num + 2);
                            continue;
                        }

                        Ok(header) => {
                            cursor.consume(num + 2);

                            use Header::*;

                            match header {
                                TransferEncoding(tr) => {
                                    me.s_chk_content();

                                    me.encoding = tr;
                                }

                                ContentLength(len) => me.content_len = Some(len),

                                Connection(ref state) => {
                                    if state == &ConnectionState::Close {
                                        me.state_mut(|state| state.upgrade == true);
                                    }

                                    // detect later to what protocol to upgrade
                                }

                                Upgrade(ref _protocol) => {
                                    todo!();
                                }
                                _ => {} // todo for more stuffs.
                            };

                            headers.push(header);
                        }
                    };
                }
            };
        }

        let resp = Response {
            code: status_code,
            headers,
            content: None,
        };

        me.resp = Some(resp);

        Ok(cursor)
    }

    pub(crate) fn poll_response(&mut self) -> Poll<Response> {
        if self.finished() {
            let resp = self.get_resp().take().unwrap();

            return Poll::Ready(resp);
        };

        Poll::Pending
    }

    pub(crate) fn state(&self) -> &StateSnapshot {
        &self.snap
    }

    fn state_mut<F, T>(&mut self, f: F) -> T
    where
        F: FnOnce(&mut StateSnapshot) -> T,
    {
        f(&mut self.snap)
    }

    // Functions to set state
    fn s_content(&mut self) {
        self.state = DecoderState::Content
    }

    fn s_chk_content(&mut self) {
        self.state = DecoderState::ChunkedContent
    }

    fn s_fin(&mut self) {
        self.state = DecoderState::Finished
    }

    fn s_err(&mut self) {
        self.state = DecoderState::Error
    }
}

#[derive(Debug)]
/// Struct representing a HTTP 1.x response.
pub struct Response {
    code: u16,
    headers: Vec<Header>,
    content: Option<Vec<u8>>,
}

impl Response {
    pub(crate) fn dummy() -> Self {
        Self {
            code: 100,
            headers: vec![],
            content: None,
        }
    }

    pub fn code(&self) -> u16 {
        self.code
    }

    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    pub fn content(&self) -> Option<&[u8]> {
        self.content.as_ref().map(|vec| &**vec)
    }

    pub fn status(&self) -> ResponseType {
        use ResponseType::*;

        match self.code {
            code if code <= 199 => Informational,
            code if code <= 299 => Successful,
            code if code <= 399 => Redirection,
            code if code <= 499 => ClientError,
            code if code <= 599 => ServerError,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug)]
pub enum ResponseType {
    Informational,
    Successful,
    Redirection,
    ClientError,
    ServerError,
}

#[cfg(test)]
mod tests {
    use crate::http1::response::DataDecoder;

    #[test]
    fn resp_simple() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Length: 2\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "AS",
        )
        .as_bytes();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        let bytes = decoder.get_resp().unwrap();
        dbg!(bytes);
    }

    #[test]
    #[should_panic]
    fn resp_simple_panic() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Length: 200\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "Versa\r\nilles",
        )
        .as_bytes();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        let resp = decoder.get_resp().unwrap();

        let text = std::str::from_utf8(&resp.content.as_ref().unwrap()).unwrap();
        assert!(text == "Versa\r\nilles", "invalid string")
    }

    #[test]
    fn resp_simple_frag() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Length: 5\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "AB",
        )
        .as_bytes();

        let resp1 = "CDE".as_bytes();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        decoder.decode(&resp1).unwrap();
        let resp = decoder.get_resp().unwrap();

        let text = std::str::from_utf8(&resp.content.as_ref().unwrap()).unwrap();
        assert!(text == "ABCDE", "invalid string")
    }

    #[test]
    fn resp_simple_broken_up() {}

    #[test]
    fn resp_chunked_full() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "4\r\ntest\r\n",
            "5\r\ntest1\r\n",
            "5\r\ntest2\r\n",
            "0\r\n\r\n",
        )
        .as_bytes()
        .to_vec();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        let resp = decoder.get_resp().unwrap();
        let text = std::str::from_utf8(&resp.content.as_ref().unwrap()).unwrap();
        assert!(text == "testtest1test2", "invalid string")
    }

    #[test]
    fn resp_chunked_two_parts() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "4\r\ntest\r\n",
            "5\r\ntest1\r\n",
        )
        .as_bytes()
        .to_vec();

        let resp1 = concat!("5\r\ntest2\r\n", "0\r\n\r\n",).as_bytes();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        decoder.decode(&resp1).unwrap();
        let resp = decoder.get_resp().unwrap();
        let text = std::str::from_utf8(&resp.content.as_ref().unwrap()).unwrap();
        assert!(text == "testtest1test2", "invalid string")
    }
}
