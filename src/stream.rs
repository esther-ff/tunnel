use lamp::Executor;
use lamp::io::{AsyncRead, AsyncWrite};

use rustls::{ClientConfig, ClientConnection};
use rustls_pki_types::ServerName;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use webpki_roots::TLS_SERVER_ROOTS;

pub struct SyncAdapter<IO: AsyncRead + AsyncWrite> {
    io: IO
};

impl<IO: AsyncRead + AsyncWrite> io::Read for SyncAdapter<IO> {
    fn read(&mut self, buf: &mut [u8]) {
        let mut future = self.io.async_read(buf);

        Pin::new(&mut future).poll()
    }
}
pub struct Connector {
    config: Arc<ClientConfig>,
}

impl Connector {
    pub fn new(cfg: ClientConfig) -> Self {
        Self {
            config: Arc::new(cfg),
        }
    }

    pub fn connect<IO>(&self, url: ServerName<'static>, io: IO) -> Connecting<IO>
    where
        IO: AsyncWrite + AsyncRead,
    {
        let client = ClientConnection::new(Arc::clone(&self.config), url).unwrap();
        Connecting { client, io }
    }
}

pub struct Connecting<IO> {
    client: ClientConnection,
    io: IO,
}

impl<IO> Future for Connecting<IO> {
    type Output = io::Result<Stream<IO>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut output = Poll::Pending;

        output
    }
}

pub struct Stream<IO> {
    io: IO,
    conn: ClientConnection,
}
