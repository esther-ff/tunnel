use crate::http1::headers::{self, Header};
use memchr::memchr;
use std::io::{BufRead, Cursor};
use std::str;

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
        }
    }

    /// Returns a `bool` indicating whether it's finished (true) or not done yet (false)
    pub(crate) fn finished(&self) -> bool {
        dbg!(&self.state);
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

                //dbg!(&self.content);
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

        // dirty fix
        self.content.as_mut().unwrap().pop();

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
                            dbg!(&header);
                            match header {
                                Header::TransferEncoding(tr) => {
                                    me.s_chk_content();

                                    me.encoding = tr;
                                }

                                Header::ContentLength(len) => me.content_len = Some(len),
                                _ => {} // todo for more stuffs.
                            }
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
}

#[cfg(test)]
mod tests {
    use crate::http1::response::DataDecoder;

    #[test]
    fn parse_headers_simple() {
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
    fn parse_headers_simple_incorrect() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Length: 200\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "ASDASDA\r\nDADSADADAS",
        )
        .as_bytes();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        let resp = decoder.get_resp().unwrap();

        let _ = dbg!(std::str::from_utf8(&resp.content.as_ref().unwrap()));
    }

    #[test]
    fn parse_headers_fragmented() {
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
        dbg!(&decoder);
        decoder.decode(&resp1).unwrap();
        dbg!(&decoder);
        let resp = decoder.get_resp().unwrap();

        let _ = dbg!(std::str::from_utf8(&resp.content.as_ref().unwrap()));
    }

    #[test]
    fn parse_headers_transfer_enc_chunked() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "4\r\ntest\r\n",
            "5\r\ntest1\r\n",
            "6\r\ntest2\r\n",
            "0\r\n\r\n",
        )
        .as_bytes()
        .to_vec();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        let resp = decoder.get_resp().unwrap();
        dbg!(&resp);
        let _ = dbg!(std::str::from_utf8(&resp.content.as_ref().unwrap()));
    }
    #[test]
    fn parse_headers_transfer_enc_really_chunked() {
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

        let resp1 = concat!("6\r\ntest2\r\n", "0\r\n\r\n",).as_bytes();

        let mut decoder = DataDecoder::new();
        decoder.decode(&resp).unwrap();
        dbg!(&decoder);
        decoder.decode(&resp1).unwrap();
        dbg!(&decoder);
        let resp = decoder.get_resp().unwrap();
        let _ = dbg!(std::str::from_utf8(&resp.content.as_ref().unwrap()));
    }
}
