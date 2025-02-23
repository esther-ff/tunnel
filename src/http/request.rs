use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use super::client::{Client, Method};
use crate::tls_client::TlsClient;
use lamp::io::{AsyncRead, AsyncWrite};
use std::mem::MaybeUninit;

const HEADER_MAX: usize = 24;

pub(crate) struct RequestFuture<'a> {
    data: Option<Vec<u8>>,
    tls: &'a mut TlsClient<'a>,
    buf: Option<Vec<u8>>,
}

macro_rules! out {
    ($expr: expr) => {
        match $expr {
            Ok(_) => {}
            Err(e) => return Poll::Ready(Err(e)),
        }
    };
}

impl<'a> RequestFuture<'a> {
    pub(crate) fn new(data: Vec<u8>, tls: &'a mut TlsClient<'a>) -> Self {
        RequestFuture {
            data: Some(data),
            tls,
            buf: Some(Vec::with_capacity(8192)),
        }
    }
}

impl Future for RequestFuture<'_> {
    type Output = io::Result<()>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let data = self.data.take().unwrap();
        let tls = &mut *self.tls;
        let res = match Pin::new(tls).poll_write(cx, &data) {
            Poll::Pending => {
                self.data.replace(data);

                return Poll::Pending;
            }

            Poll::Ready(res) => res,
        };
        out!(res);

        let flush_res = ready!(Pin::new(&mut *self.tls).poll_flush(cx));
        out!(flush_res);

        let mut buf = self
            .buf
            .take()
            .expect("buf field for RequestFuture should not be empty");

        let pin = Pin::new(&mut *self.tls);

        match pin.poll_write(cx, &mut buf) {
            Poll::Pending => {
                self.buf.replace(buf);
                Poll::Pending
            }

            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
        }
    }
}

#[derive(Debug)]
pub struct HeaderList<'h> {
    hdr: [MaybeUninit<(&'h str, &'h str)>; HEADER_MAX],
    cursor: usize,
}

impl<'h> HeaderList<'h> {
    pub(crate) fn new() -> Self {
        Self {
            hdr: [const { MaybeUninit::uninit() }; HEADER_MAX],
            cursor: 0,
        }
    }

    fn can_push(&self) -> bool {
        self.cursor >= HEADER_MAX
    }

    pub(crate) fn put(&mut self, hdr: (&'h str, &'h str)) {
        // Ensures we don't overflow.
        // This function should be coupled with `can_push`.
        if self.can_push() {
            return;
        }

        let slot = unsafe { self.hdr.get_unchecked_mut(self.cursor) };
        slot.write(hdr);

        self.cursor += 1;
    }

    pub(crate) fn header_slice(&self) -> &[(&'h str, &'h str)] {
        if self.cursor == 0 {
            return &[];
        }

        let slice = &self.hdr[0..self.cursor - 1];
        unsafe {
            &*(slice as *const [MaybeUninit<(&'h str, &'h str)>] as *const [(&'h str, &'h str)])
        }
    }

    fn dealloc(&mut self, index: usize) {
        unsafe {
            let slot = self.hdr.get_unchecked_mut(index);
            slot.assume_init_drop();
        }
    }

    fn get(&self, index: usize) -> Option<(&'h str, &'h str)> {
        if index >= self.cursor {
            return None;
        }

        unsafe {
            let item = self.hdr.get_unchecked(index);

            Some(item.assume_init())
        }
    }

    fn iter(&'h self) -> impl Iterator<Item = (&'h str, &'h str)> {
        HdrListIter { pos: 0, hdrs: self }
    }
}

pub struct HdrListIter<'i> {
    pos: usize,
    hdrs: &'i HeaderList<'i>,
}

impl<'i> Iterator for HdrListIter<'i> {
    type Item = (&'i str, &'i str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos == self.hdrs.cursor {
            println!("returning none");
            return None;
        }

        let item = self.hdrs.get(self.pos);
        self.pos += 1;
        item
    }
}

impl std::ops::Drop for HeaderList<'_> {
    fn drop(&mut self) {
        if self.cursor == 0 {
            return;
        };

        self.cursor -= 1;
        while self.cursor != 0 {
            self.dealloc(self.cursor);
            self.cursor -= 1;
        }

        self.dealloc(0);
    }
}

#[derive(Debug)]
pub struct ReqBuilder<'b> {
    method: Method,
    route: Option<&'b str>,
    headers: Option<&'b [(&'b str, &'b str)]>,
    extra_headers: Option<HeaderList<'b>>,
    content: Option<&'b [u8]>,
}

impl<'b> ReqBuilder<'b> {
    const CRLF: &'static [u8] = &[b'\r', b'\n'];

    pub fn new(method: Method) -> Self {
        Self {
            method,
            route: None,
            headers: None,
            extra_headers: None,
            content: None,
        }
    }

    pub fn default_headers(&mut self, client: &'b Client) -> &mut Self {
        self.headers = client.get_header_slice();
        self
    }

    pub fn set_route(&mut self, route: &'b str) -> &mut Self {
        self.route.replace(route);
        self
    }

    pub fn set_content(&mut self, content: &'b [u8]) -> &mut Self {
        self.content.replace(content);
        self
    }

    pub fn add_headers(&mut self, iter: impl IntoIterator<Item = (&'b str, &'b str)>) -> &mut Self {
        if self.extra_headers.is_none() {
            self.extra_headers = Some(HeaderList::new())
        };

        let mut hdrlist = self.extra_headers.take().unwrap();

        iter.into_iter().for_each(|tuple| {
            if !hdrlist.can_push() {
                return;
            }

            hdrlist.put(tuple);
        });

        self.extra_headers.replace(hdrlist);

        self
    }

    pub(crate) fn construct(self) -> Vec<u8> {
        let mut req = Vec::with_capacity(8192);

        // First HTTP line.
        req.extend_from_slice(self.method.bytes());

        match self.route {
            None => {
                req.push(b'/');
            }

            Some(route) => {
                req.extend_from_slice(route.as_bytes());
            }
        };

        req.extend_from_slice(" HTTP/1.1\r\n".as_bytes());

        // Headers
        match self.headers {
            None => {}
            Some(slice) => {
                slice.iter().for_each(|(k, v)| {
                    req.extend_from_slice(k.as_bytes());
                    req.extend_from_slice(": ".as_bytes());
                    req.extend_from_slice(v.as_bytes());
                    req.extend_from_slice(Self::CRLF);
                });
            }
        }

        // Extra headers
        match self.extra_headers {
            None => {}
            Some(hdrs) => {
                hdrs.iter().for_each(|(k, v)| {
                    req.extend_from_slice(k.as_bytes());
                    req.extend_from_slice(": ".as_bytes());
                    req.extend_from_slice(v.as_bytes());
                    req.extend_from_slice(Self::CRLF);
                });
            }
        }

        // Content if any
        req.extend_from_slice(Self::CRLF);

        match self.content {
            Some(content) => {
                req.extend(content.iter());
            }

            None => {}
        };

        req
    }
}

#[cfg(test)]
mod tests {
    use super::{HeaderList, Method, ReqBuilder};

    #[test]
    fn create_header_list() {
        HeaderList::new();
    }

    #[test]
    fn use_header_list() {
        let mut hdrs = HeaderList::new();

        hdrs.put(("User-Agent", "Versailles"));

        let item = hdrs.get(0).expect("empty header list");

        assert!("User-Agent" == item.0);
        assert!("Versailles" == item.1);

        drop(hdrs);
    }

    #[test]
    fn try_overflow_header_list() {
        let mut hdrs = HeaderList::new();

        // HeaderLists are preallocated arrays of 24 `MaybeUninit<(&str, &str)>`
        for _ in 0..48 {
            hdrs.put(("dummy", "dummy"));
        }
    }

    #[test]
    fn req_builder() {
        let mut hdrs = HeaderList::new();
        hdrs.put(("User-Agent", "Versailles"));

        let mut req = ReqBuilder::new(Method::GET);

        req.set_route("/droit/humain")
            .set_content("pas de dieu, pas de maitre".as_bytes())
            .add_headers(hdrs.iter());

        dbg!(&req);

        let bytes = req.construct();

        let str_req = std::str::from_utf8(&bytes);
        assert!(str_req.is_ok());

        println!("{}", str_req.unwrap());
    }
}
