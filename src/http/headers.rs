#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MimeType {
    AppJson,
    TextPlain,
    TextHtml,
    ImagePng,
    ImageJpeg,
    ImageGif,

    Unimplemented,
}

impl MimeType {
    pub fn recognize(line: &str) -> MimeType {
        use MimeType::*;

        dbg!(&line);

        match line {
            "application/json" => AppJson,
            "text/plain" => TextPlain,
            "text/html" => TextHtml,
            "image/png" => ImagePng,
            "image/jpeg" => ImageJpeg,
            "image/jpg" => ImageJpeg,
            "image/gif" => ImageGif,
            _ => Unimplemented,
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone)]
pub enum TrfrEncodingType {
    Gzip,
    Chunked,
    Deflate,
    GzipChunked,
    DeflateChunked,

    None,

    Unknown,
}

impl TrfrEncodingType {
    pub fn recognize(line: &str) -> TrfrEncodingType {
        use TrfrEncodingType::*;

        match line {
            "gzip" => Gzip,
            "chunked" => Chunked,
            "deflate" => Deflate,
            "gzip, chunked" | "chunked, gzip" => GzipChunked,
            "deflate, chunked" | "chunked, deflate" => DeflateChunked,

            _ => Unknown,
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Header {
    ContentLength(usize),
    ContentType(MimeType),
    ContentEncoding(String),
    ContentLanguage(String),
    TransferEncoding(TrfrEncodingType),

    Unimplemented((String, String)),
}

impl Header {
    pub fn serialize(line: &str) -> Result<Header, &'static str> {
        use Header::*;

        let mut split = line.split(": ");

        let (name, val) = match (split.next(), split.next()) {
            (Some(name), Some(val)) => (name, val),
            _ => return Err("invalid header"),
        };

        match name {
            "Content-Length" => {
                let len = match val.parse::<usize>() {
                    Err(_) => {
                        return Err("value couldn't be parsed as an integer for Content-Length");
                    }
                    Ok(length) => length,
                };

                Ok(ContentLength(len))
            }

            "Content-Type" => Ok(ContentType(MimeType::recognize(val))),

            // todo
            "Content-Encoding" => Ok(ContentEncoding(val.to_string())),

            // todo
            "Content-Language" => Ok(ContentLanguage(val.to_string())),

            "Transfer-Encoding" => Ok(TransferEncoding(TrfrEncodingType::recognize(val))),

            _ => Ok(Unimplemented((name.to_string(), val.to_string()))),
        }
    }
}
