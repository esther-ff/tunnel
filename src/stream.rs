use lamp::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, TokenBearer};

use rustls::{ClientConfig, ClientConnection};
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
    pub fn new(io: IO, url: ServerName<'static>, cfg: ClientConfig) -> io::Result<Self> {
        let conn = match ClientConnection::new(Arc::new(cfg), url) {
            Ok(conn) => conn,
            Err(e) => {
                let err = io::Error::new(io::ErrorKind::Other, e);

                return Err(err);
            }
        };

        Ok(Self { io, conn })
    }
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
                self.conn.write_tls(&mut r);

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
                println!("handshake: started write");
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
            println!("handshake: finished write");

            // If we need a flush, do so
            if flush_required {
                println!("handshake: started flush");
                match Pin::new(&mut self.io).poll_flush(cx) {
                    Poll::Ready(Ok(())) => (),
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                    Poll::Pending => write_block = true,
                }

                println!("handshake: finished flush");
            }

            // Read
            while self.conn.wants_read() && !eof {
                println!("handshake: started read");
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

            println!("handshake: finished read");

            dbg!(eof, self.conn.is_handshaking());
            return match (eof, self.conn.is_handshaking()) {
                (true, true) => {
                    let error = io::Error::new(io::ErrorKind::InvalidData, "eof on tls handshake");

                    Poll::Ready(Err(error))
                }
                (_, false) => Poll::Ready(Ok((read_len, write_len))),
                (_, true) if write_block || read_block => {
                    if read_len == 0 || write_len == 0 {
                        Poll::Ready(Ok((read_len, write_len)))
                    } else {
                        Poll::Pending
                    }
                }

                (..) => continue,
            };
        }
    }

    fn complete_io(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        println!("complete_io: before handshake");
        if self.conn.is_handshaking() {
            match self.handshake(cx) {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(_) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
        println!("complete_io: after handshake");

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
        println!("hmm");
        let _ = ready!(self.complete_io(cx));
        println!("mmh");
        while self.conn.wants_read() {
            let res = ready!(self.handshake(cx));
            if res?.0 == 0 {
                break;
            }
        }

        Poll::Ready(Ok(()))
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin + TokenBearer> TokenBearer for Stream<IO> {
    fn get_token(&self) -> mio::Token {
        self.io.get_token()
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncRead for Stream<IO> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        println!("rustls prep read start");
        match ready!(self.prep_read(cx)) {
            Ok(_) => {}
            Err(e) => return Poll::Ready(Err(e)),
        };
        println!("rustls prep read end");

        println!("rustls read start");
        match self.conn.reader().read(buf) {
            Ok(n) => return Poll::Ready(Ok(n)),
            Err(e) => return Poll::Ready(Err(e)),
        }
        println!("rustls read end");
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Stream<IO> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let len = match self.conn.writer().write(buf) {
            Ok(n) => n,
            Err(e) => return Poll::Ready(Err(e)),
        };

        while self.conn.wants_write() {
            let out = ready!(self.handshake(cx));
            if out?.1 == 0 {
                break;
            }
        }

        Poll::Ready(Ok(len))
    }

    fn poll_flush<'f>(mut self: Pin<&mut Self>, cx: &mut Context<'f>) -> Poll<io::Result<()>> {
        match self.conn.writer().flush() {
            Ok(n) => return Poll::Ready(Ok(n)),
            Err(e) => return Poll::Ready(Err(e)),
        }

        let io = Pin::new(&mut self.io);
        io.poll_flush(cx)
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
