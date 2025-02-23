use super::request::{HeaderList, ReqBuilder, RequestFuture};
use crate::tls_client::{Resolving, TlsClient};
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

pub(crate) struct Connecting<'c> {
    tls: Resolving<'c>,
    user_agent: Option<&'static str>,
    headers: Option<HeaderList<'c>>,
}

impl<'c> Future for Connecting<'c> {
    type Output = io::Result<Client<'c>>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = ready!(Pin::new(&mut self.tls).poll(cx));

        match res {
            Ok(tls) => {
                // Under normal circumstances this is 100% safe.
                let user_agent = self.user_agent.take().expect("no user_agent present");

                let headers = self.headers.take();
                let client = Client {
                    tls,
                    user_agent,
                    headers,
                };

                Poll::Ready(Ok(client))
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

pub struct Client<'c> {
    tls: TlsClient<'c>,
    user_agent: &'static str,
    headers: Option<HeaderList<'c>>,
}

#[derive(Debug)]
pub enum Method {
    GET,
    PUT,
    POST,
    HEAD,
    PATCH,
    OPTIONS,
    CONNECT,
}

impl Method {
    pub(crate) const fn bytes(&self) -> &'static [u8] {
        match *self {
            Method::GET => "GET ".as_bytes(),
            Method::PUT => "PUT ".as_bytes(),
            Method::POST => "POST ".as_bytes(),
            Method::HEAD => "HEAD ".as_bytes(),
            Method::PATCH => "PATCH ".as_bytes(),
            Method::OPTIONS => "OPTIONS ".as_bytes(),
            Method::CONNECT => "CONNECT ".as_bytes(),
        }
    }
}

impl<'c> Client<'c> {
    pub fn connect(
        url: &'static str,
        user_agent: &'static str,
        headers: Option<&'c HashMap<&'c str, String>>,
    ) -> io::Result<Connecting<'c>> {
        let tls = TlsClient::create(None, url)?;

        let hdr = match headers {
            None => None,
            Some(map) => {
                let mut hdrlist = HeaderList::new();
                map.into_iter()
                    .for_each(|(key, val)| hdrlist.put((key, &*val)));

                Some(hdrlist)
            }
        };

        Ok(Connecting {
            tls,
            user_agent: Some(user_agent),
            headers: hdr,
        })
    }

    pub(crate) fn get_header_slice(&self) -> Option<&[(&'c str, &'c str)]> {
        match self.headers.as_ref() {
            Some(h) => Some(h.header_slice()),
            None => None,
        }
    }

    fn execute(&self, req: ReqBuilder) {}
}
