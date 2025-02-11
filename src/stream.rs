use lamp::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use rustls::{ClientConfig, ClientConnection, Connection};
use rustls_pki_types::ServerName;
use std::io::{self, Read, Write};
use std::marker::Unpin;
use std::pin::Pin;
use std::sync::Arc;
use std::task::ready;
use std::task::{Context, Poll};

pub struct SyncAdapter<'adapter, 'cx, IO> {
    io: &'adapter mut IO,
    cx: &'adapter mut Context<'cx>,
}

impl<IO: Unpin> Unpin for SyncAdapter<'_, '_, IO> {}

impl<IO: AsyncRead + AsyncWrite + Unpin> Read for SyncAdapter<'_, '_, IO> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match Pin::new(&mut self.io).poll_read(self.cx, buf) {
            Poll::Ready(result) => return result,
            Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> Write for SyncAdapter<'_, '_, IO> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match Pin::new(&mut self.io).poll_write(self.cx, buf) {
            Poll::Ready(result) => return result,
            Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match Pin::new(&mut self.io).poll_flush(self.cx) {
            Poll::Ready(res) => return res,
            Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }
}

pub struct Stream<IO> {
    io: IO,
    conn: ClientConnection,
}

impl<IO: AsyncRead + AsyncWrite + Unpin> Stream<IO> {
    fn io_read(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut r = SyncAdapter {
            io: &mut self.io,
            cx,
        };

        let read = match self.conn.read_tls(&mut r) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Poll::Pending,
            Err(e) => return Poll::Ready(Err(e)),
        };

        match self.conn.process_new_packets() {
            Ok(_state) => {}
            Err(e) => {
                // Last ditch write
                //self.conn.write_tls(&mut r);

                let err = io::Error::new(io::ErrorKind::InvalidData, e);
                return Poll::Ready(Err(err));
            }
        }

        Poll::Ready(Ok(read))
    }

    fn io_write(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut w = SyncAdapter {
            io: &mut self.io,
            cx,
        };

        match self.conn.write_tls(&mut w) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Poll::Pending,
            res => return Poll::Ready(res),
        }
    }

    fn handshake(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<(usize, usize)>> {
        let mut write_len = 0;
        let mut read_len = 0;

        loop {
            let mut write_block = false;
            let mut read_block = false;
            let mut flush_required = false;

            let mut eof = false;

            // Write
            while self.conn.wants_write() {
                match self.io_write(cx) {
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),

                    Poll::Ready(Ok(wrlen)) => {
                        write_len += wrlen;
                        flush_required = true;
                    }

                    Poll::Pending => {
                        write_block = true;
                        break;
                    }
                }
            }

            // If we need a flush, do so
            if flush_required {
                match Pin::new(&mut self.io).poll_flush(cx) {
                    Poll::Ready(Ok(())) => (),
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                    Poll::Pending => write_block = true,
                }
            }

            // Read
            while self.conn.wants_read() && !eof {
                match self.io_read(cx) {
                    Poll::Ready(Ok(0)) => eof = true,
                    Poll::Ready(Ok(rdlen)) => read_len += rdlen,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => {
                        read_block = true;
                        break;
                    }
                }
            }

            match (eof, self.conn.is_handshaking()) {
                (true, true) => {
                    let error = io::Error::new(io::ErrorKind::InvalidData, "eof on tls handshake");

                    return Poll::Ready(Err(error));
                }
                (_, false) => return Poll::Ready(Ok((read_len, write_len))),
                (_, true) => return Poll::Pending,

                (..) => continue,
            }
        }
    }

    fn complete_io(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.conn.is_handshaking() {
            match self.handshake(cx) {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(_) => {}
                Poll::Pending => return Poll::Pending,
            }
        }

        if self.conn.wants_write() {
            match self.handshake(cx) {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(_) => {}
                Poll::Pending => return Poll::Pending,
            }
        }

        Poll::Ready(Ok(()))
    }

    fn prep_read(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let _ = ready!(self.complete_io(cx));

        while self.conn.wants_read() {
            let res = ready!(self.handshake(cx));
            if res?.0 == 0 {
                break;
            }
        }

        Poll::Ready(Ok(()))
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncRead for Stream<IO> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        ready!(self.prep_read(cx));
        match self.conn.reader().read(buf) {
            Ok(n) => return Poll::Ready(Ok(n)),
            Err(e) => return Poll::Ready(Err(e)),
        }
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

// impl<IO> Future for Connecting<IO> {
//     type Output = io::Result<Stream<IO>>;

//     fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
//         let mut output = Poll::Pending;

//         output
//     }
// }
