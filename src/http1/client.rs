use super::request::{HeaderList, ReqBuilder};
use super::response::{DataDecoder, Response};
use crate::tls_client::{Resolving, TlsClient};
use futures::channel::oneshot;
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::mpsc;
use std::task::{Context, Poll};

#[derive(Debug, Clone, Copy)]
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
            Method::GET => const { "GET ".as_bytes() },
            Method::PUT => const { "PUT ".as_bytes() },
            Method::POST => const { "POST ".as_bytes() },
            Method::HEAD => const { "HEAD ".as_bytes() },
            Method::PATCH => const { "PATCH ".as_bytes() },
            Method::OPTIONS => const { "OPTIONS ".as_bytes() },
            Method::CONNECT => const { "CONNECT ".as_bytes() },
        }
    }
}

pub(crate) struct Connecting<'c> {
    tls: Resolving<'c>,
    user_agent: Option<&'static str>,
    headers: Option<HeaderList<'c>>,
}

#[derive(Debug)]
struct Envelope {
    data: Vec<u8>,
    oneshot: Option<oneshot::Sender<io::Result<Response>>>,
}

impl Envelope {
    fn chan_fn<F, T>(&mut self, f: F) -> T
    where
        F: FnOnce(oneshot::Sender<io::Result<Response>>) -> T,
    {
        let chan = self.oneshot.take().expect("chan should be here");
        let val = f(chan);
        val
    }
}

// todo
struct State;

pub struct HttpsConn<'h> {
    /// TLS connection,
    io: TlsClient<'h>,

    /// Receiver
    recv: mpsc::Receiver<Envelope>,

    /// Slot for currently processed request
    chan: Option<Envelope>,

    /// Internal state
    state: State,

    /// Decoder for data.
    decoder: DataDecoder,

    /// Reciever for a notification to shutdown
    shutdown: mpsc::Receiver<()>,
}

impl<'h> Future for HttpsConn<'h> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use lamp::io::{AsyncRead, AsyncWrite};
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

        if let Ok(()) = self.shutdown.try_recv() {
            return Poll::Ready(Ok(()));
        }

        for _ in 0..8 {
            let _ = Pin::new(&mut self.io).poll_write(cx, &envl.data);
            let _ = Pin::new(&mut self.io).poll_flush(cx);

            let mut buf: [u8; 16800] = [0; 16800];
            println!("Reading!");
            match Pin::new(&mut self.io).poll_read(cx, &mut buf) {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(size)) => {
                    println!("read ready!");

                    if let Err(e) = self.decoder.decode(&buf[0..size]) {
                        let err = io::Error::new(io::ErrorKind::Other, e);

                        let _ = envl.chan_fn(|ch| ch.send(Err(err)));
                    }

                    if self.decoder.finished() {
                        let resp = self
                            .decoder
                            .get_resp()
                            .expect("there should always be a response in slot");

                        match resp.status() {
                            _ => {} // todo:
                        }

                        let _ = envl.chan_fn(|ch| ch.send(Ok(resp)));
                    }
                    // let _ = dbg!(channel.send(Response::dummy()));
                }

                _ => {}
            }
        }

        self.chan.replace(envl);

        Poll::Pending
    }
}

pub struct Client<'c> {
    user_agent: &'static str,
    headers: Option<HeaderList<'c>>,
    shutdown: mpsc::Sender<()>,
    sender: mpsc::Sender<Envelope>,
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

        let (sender1, recv1) = mpsc::channel();

        let conn = HttpsConn {
            io,
            recv,
            chan: None,
            state: State,
            decoder: DataDecoder::new(),
            shutdown: recv1,
        };

        use lamp::Executor;
        let _ = Executor::spawn(conn);

        Ok(Client {
            user_agent,
            headers: hdr,
            shutdown: sender1,
            sender,
        })
    }

    pub fn execute(&mut self, req: ReqBuilder) -> oneshot::Receiver<io::Result<Response>> {
        let (s, r) = oneshot::channel();

        let data = req.construct();
        let envl = Envelope {
            data,
            oneshot: Some(s),
        };

        // handle this later
        let _res = self.sender.send(envl);

        r
    }

    pub fn shutdown(&self) -> Result<(), mpsc::SendError<()>> {
        self.shutdown.send(())
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
