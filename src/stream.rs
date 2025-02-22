use lamp::io::{AsyncRead, AsyncWrite, TokenBearer};

use rustls::{ClientConfig, ClientConnection};
use rustls_pki_types::ServerName;
use std::io::{self, Read, Write};
use std::marker::Unpin;
use std::pin::Pin;
use std::sync::Arc;
use std::task::ready;
use std::task::{Context, Poll};

use log::{debug, error, info};

#[cfg(debug_assertions)]
#[derive(Debug)]
struct HandshakeMetrics {
    write_len: usize,
    read_len: usize,
    write_blocked: bool,
    read_blocked: bool,
}

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
    pub fn create(
        io: IO,
        url: ServerName<'static>,
        cfg: Arc<ClientConfig>,
    ) -> io::Result<Ready<IO>> {
        let conn = match ClientConnection::new(cfg, url) {
            Ok(conn) => conn,
            Err(e) => {
                let err = io::Error::new(io::ErrorKind::Other, e);

                return Err(err);
            }
        };

        let stream = Self { io, conn };
        Ok(Ready::Handshaking(stream))
    }

    fn conn_fn<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&ClientConnection) -> T,
    {
        f(&self.conn)
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
                let _ = self.conn.write_tls(&mut r);

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

                    Poll::Ready(Ok(0)) => {
                        let err = io::Error::from(io::ErrorKind::WriteZero);
                        return Poll::Ready(Err(err));
                    }
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

            #[cfg(debug_assertions)]
            {
                let metrics = HandshakeMetrics {
                    write_len,
                    read_len,
                    write_blocked: write_block,
                    read_blocked: read_block,
                };

                debug!("handshake metrics before return:\n {:?}", metrics)
            };

            return match (eof, self.conn.is_handshaking()) {
                (true, true) => {
                    let error = io::Error::new(io::ErrorKind::InvalidData, "eof on tls handshake");

                    Poll::Ready(Err(error))
                }
                (_, false) => Poll::Ready(Ok((read_len, write_len))),
                (_, true) if write_block || read_block => {
                    if read_len != 0 || write_len != 0 {
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
        while self.conn.wants_read() {
            match self.io_read(cx) {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(_ln)) => {
                    debug!("read len: {}", _ln);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }

        return match self.conn.reader().read(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) => Poll::Ready(Err(e)),
        };
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Stream<IO> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut written = 0;

        while buf.len() != written {
            match self.conn.writer().write(buf) {
                Ok(wrlen) => written += wrlen,
                Err(e) => return Poll::Ready(Err(e)),
            };

            while self.conn.wants_write() {
                match self.io_write(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    _ => {} // we don't care if it's successful
                }
            }
        }

        Poll::Ready(Ok(written))
    }

    fn poll_flush<'f>(mut self: Pin<&mut Self>, cx: &mut Context<'f>) -> Poll<io::Result<()>> {
        match self.conn.writer().flush() {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Poll::Pending,
            Err(e) => return Poll::Ready(Err(e)),
        }

        let io = Pin::new(&mut self.io);
        io.poll_flush(cx)
    }
}

pub enum Ready<Rw> {
    Handshaking(Stream<Rw>),
    Done,
}

impl<Rw: AsyncRead + AsyncWrite + Unpin> Future for Ready<Rw> {
    type Output = io::Result<Stream<Rw>>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use std::mem;

        let me = self.get_mut();

        let mut stream = match mem::replace(me, Ready::Done) {
            Ready::Done => panic!("polled after completion"),
            Ready::Handshaking(stream) => stream,
        };

        while stream.conn.is_handshaking() {
            dbg!(stream.conn.is_handshaking());
            match stream.handshake(cx) {
                Poll::Ready(Ok(_l)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {
                    let _ = mem::replace(me, Ready::Handshaking(stream));
                    return Poll::Pending;
                }
            };
        }

        Poll::Ready(Ok(stream))
    }
}
