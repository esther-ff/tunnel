use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use crate::tls_client::TlsClient;
use lamp::io::{AsyncRead, AsyncWrite};

pub(crate) struct RequestFuture<'a> {
    data: &'a [u8],
    tls: &'a mut TlsClient<'a>,
    buf: Option<&'a mut [u8]>,
}

macro_rules! out {
    ($expr: expr) => {
        match $expr {
            Ok(_) => {}
            Err(e) => return Poll::Ready(Err(e)),
        }
    };
}

impl Future for RequestFuture<'_> {
    type Output = io::Result<()>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let data = &*self.data;

        let res = ready!(Pin::new(&mut *self.tls).poll_write(cx, data));
        out!(res);

        let flush_res = ready!(Pin::new(&mut *self.tls).poll_flush(cx));
        out!(flush_res);

        let buf = self
            .buf
            .take()
            .expect("buf field for RequestFuture should not be empty");

        let pin = Pin::new(&mut *self.tls);

        match pin.poll_write(cx, buf) {
            Poll::Pending => {
                self.buf.replace(buf);
                Poll::Pending
            }

            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
        }
    }
}
