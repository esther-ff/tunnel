use super::request::{HeaderList, ReqBuilder, RequestFuture};
use crate::tls_client::{Resolving, TlsClient};
use futures::channel::oneshot;
use lamp::Executor;
use lamp::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::mpsc;
use std::task::{Context, Poll, ready};

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

pub(crate) struct Connecting<'c> {
    tls: Resolving<'c>,
    user_agent: Option<&'static str>,
    headers: Option<HeaderList<'c>>,
}

struct Envelope {
    data: Vec<u8>,
    oneshot: Option<oneshot::Sender<Vec<u8>>>,
}

pub struct HttpsConn<'h> {
    io: TlsClient<'h>,
    recv: mpsc::Receiver<Envelope>,
    chan: Option<Envelope>,
}

impl<'h> Future for HttpsConn<'h> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use lamp::io::AsyncWrite;
        use mpsc::TryRecvError::{Disconnected, Empty};

        let mut envl = if self.chan.is_some() {
            self.chan.take().unwrap()
        } else {
            match self.recv.try_recv() {
                Ok(envl) => envl,

                Err(e) => match e {
                    Empty => return Poll::Pending,

                    Disconnected => {
                        let err = io::Error::new(io::ErrorKind::Other, "chan disconnected");

                        return Poll::Ready(Err(err));
                    }
                },
            }
        };

        match Pin::new(&mut self.io).poll_write(cx, &envl.data) {
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(_size) => {}
            Poll::Pending => {
                self.chan.replace(envl);
                return Poll::Pending;
            }
        }

        match Pin::new(&mut self.io).poll_flush(cx) {
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(_size) => {}
            Poll::Pending => {
                self.chan.replace(envl);
                return Poll::Pending;
            }
        }

        let mut buf: [u8; 16800] = [0; 16800];

        match Pin::new(&mut self.io).poll_read(cx, &mut buf) {
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(_size) => {
                let channel = envl.oneshot.take().unwrap();

                // check for result?
                let _ = channel.send(buf.to_vec());
            }
            Poll::Pending => {
                self.chan.replace(envl);
                return Poll::Pending;
            }
        }

        Poll::Pending
    }
}

pub struct Client<'c> {
    user_agent: &'static str,
    headers: Option<HeaderList<'c>>,
    waker: std::task::Waker,
    sender: mpsc::Sender<Envelope>,
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
    pub async fn connect(
        url: &'static str,
        user_agent: &'static str,
        headers: Option<&'c HashMap<&'c str, String>>,
    ) -> io::Result<Client<'c>> {
        let io = TlsClient::create(None, url)?.await?;

        let hdr = match headers {
            None => None,
            Some(map) => {
                let mut hdrlist = HeaderList::new();
                map.into_iter()
                    .for_each(|(key, val)| hdrlist.put((key, &*val)));

                Some(hdrlist)
            }
        };

        let (sender, recv) = mpsc::channel();

        let conn = HttpsConn {
            io,
            recv,
            chan: None,
        };

        println!("hehehehai!, {:?}", std::thread::current().name());
        let handle = Executor::spawn(conn);

        Ok(Client {
            user_agent,
            headers: hdr,
            waker: unsafe { handle.expose_waker() },
            sender,
        })
    }

    pub fn execute(&mut self, req: ReqBuilder) -> oneshot::Receiver<Vec<u8>> {
        let (s, r) = oneshot::channel();

        let data = req.construct();
        let envl = Envelope {
            data,
            oneshot: Some(s),
        };

        // handle this later
        let _res = self.sender.send(envl);

        self.waker.wake_by_ref();

        r
    }

    pub(crate) fn get_header_slice(&self) -> Option<&[(&'c str, &'c str)]> {
        match self.headers.as_ref() {
            Some(h) => Some(h.header_slice()),
            None => None,
        }
    }

    // pub fn execute(&mut self, req: ReqBuilder) -> RequestFuture<'_> {
    //     let data = req.construct();

    //     RequestFuture::new(data, self)
    // }
}
