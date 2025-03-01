use crate::http::headers::{self, Header};
// use crate::http::request::HeaderList;
// use bytes::{Bytes, BytesMut};
use core::panic;
use memchr::memchr;
use std::io::BufRead;
use std::io::Cursor;
use std::str;

fn str_to_usize(line: &[u8]) -> Option<usize> {
    if let Ok(string) = str::from_utf8(line) {
        usize::from_str_radix(string, 16).ok()
    } else {
        None
    }
}

#[derive(Debug)]
pub enum HttpResErr {
    Empty,
    InvalidHeader(String),
    InvalidFirstLine(String),

    // Body errors
    InvalidBody,
}

impl std::fmt::Display for HttpResErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#?}", self)
    }
}

impl std::error::Error for HttpResErr {}

#[derive(Debug)]
struct DataDecoder {
    encoding: headers::TrfrEncodingType,
    cursor: Cursor<Vec<u8>>,
}

impl DataDecoder {
    fn decode(self) -> Option<Vec<u8>> {
        use headers::TrfrEncodingType::{Chunked, Gzip, GzipChunked};
        match self.encoding {
            Chunked => self.chunked_decode(),
            Gzip => todo!(),
            GzipChunked => todo!(),
            _ => Some(self.cursor.into_inner()),
        }
    }

    fn chunked_decode(mut self) -> Option<Vec<u8>> {
        let mut content: Vec<u8> = Vec::with_capacity(16386);

        loop {
            let buf = self.cursor.fill_buf().unwrap();
            dbg!(buf);
            let index = match memchr(b'\r', buf) {
                None => panic!("impl this!"),

                Some(0) => break,
                Some(num) => num,
            };

            let len = match str_to_usize(&buf[..index]) {
                Some(len) => len,
                None => return None,
            };

            if len == 0 {
                break;
            };

            content.extend_from_slice(&buf[index + 2..len + 3]);
            dbg!(&content);
            self.cursor.consume(index + len + 4);
        }

        // dirty fix
        content.pop();

        Some(content)
    }
}

fn parse_headers(
    mut cursor: Cursor<Vec<u8>>,
) -> Result<(u16, Vec<Header>, DataDecoder), HttpResErr> {
    let mut headers: Vec<Header> = Vec::with_capacity(24);

    let buf = cursor.fill_buf().unwrap();
    let status_code = match memchr(b'\r', buf) {
        None => return Err(HttpResErr::Empty),
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

    let mut tr_encoding: headers::TrfrEncodingType = headers::TrfrEncodingType::None;

    loop {
        let buf = cursor.fill_buf().unwrap();

        if buf.len() == 0 {
            break;
        };

        match memchr(b'\r', buf) {
            None => break,
            Some(num) => {
                let string = match std::str::from_utf8(&buf[..num]) {
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
                            Header::TransferEncoding(tr) => tr_encoding = tr,
                            _ => {} // todo for more stuffs.
                        }
                        headers.push(header);
                    }
                };
            }
        };
    }

    Ok((
        status_code,
        headers,
        DataDecoder {
            encoding: tr_encoding,
            cursor,
        },
    ))
}

#[derive(Debug)]
pub struct Response {
    code: u16,
    headers: Vec<Header>,
    content: Vec<u8>,
}

impl Response {
    pub fn new(data: Vec<u8>) -> Result<Self, HttpResErr> {
        let cursor = Cursor::new(data);
        let (code, headers, decoder) = match parse_headers(cursor) {
            Ok((code, headers, decoder)) => (code, headers, decoder),
            Err(e) => return Err(e),
        };

        let content = match decoder.decode() {
            None => return Err(HttpResErr::InvalidBody),
            Some(c) => c,
        };

        Ok(Response {
            code,
            headers,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::parse_headers;

    #[test]
    fn parse_headers_simple() {
        let resp = concat!(
            "HTTP/1.1 201 Created\r\n",
            "Content-Length: 200\r\n",
            "Content-Language: en\r\n",
            "Content-Encoding: none\r\n",
            "Test-Noimplement: Test\r\n",
            "\r\n",
            "ASDASDA\r\nDADSADADAS",
        )
        .as_bytes()
        .to_vec();

        let cursor = Cursor::new(resp);
        let res = parse_headers(cursor);
        let bytes = res.unwrap().2.decode();
        dbg!(bytes);
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

        let cursor = Cursor::new(resp);
        let res = parse_headers(cursor);
        let bytes = res.unwrap().2.decode().unwrap();
        dbg!(std::str::from_utf8(&bytes));
    }
}
