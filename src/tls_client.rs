use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use lamp::io::{AsyncRead, AsyncWrite, TcpStream, TokenBearer};

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::ServerName;

use super::stream::{Ready, Stream};

pub(crate) struct Resolving<'a> {
    io: Ready<TcpStream>,
    cfg: Arc<ClientConfig>,
    url: &'a str,
}

impl<'a> Future for Resolving<'a> {
    type Output = io::Result<TlsClient<'a>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = ready!(Pin::new(&mut self.io).poll(cx));

        let output = match res {
            Err(e) => Poll::Ready(Err(e)),

            Ok(stream) => {
                let client = TlsClient {
                    io: stream,
                    cfg: Arc::clone(&self.cfg),
                    url: self.url,
                };

                Poll::Ready(Ok(client))
            }
        };

        output
    }
}

pub(crate) struct TlsClient<'a> {
    io: Stream<TcpStream>,
    cfg: Arc<ClientConfig>,
    url: &'a str,
}

impl TlsClient<'_> {
    pub(crate) fn create(
        conf: Option<ClientConfig>,
        url: &'static str,
    ) -> io::Result<Resolving<'static>> {
        let cfg = match conf {
            None => {
                // make config
                let root_store = RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.into(),
                };

                let config = ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth();

                Arc::new(config)
            }

            Some(config) => Arc::new(config),
        };

        let dns_name: ServerName<'static> = match url.try_into() {
            Ok(name) => name,
            Err(e) => {
                let err = io::Error::new(io::ErrorKind::Other, e);

                return Err(err);
            }
        };

        let tcp = std::net::TcpStream::connect(&format!("{}:443", url))?;
        let lamp_tcp = TcpStream::from_std(tcp)?;
        let io = Stream::create(lamp_tcp, dns_name, Arc::clone(&cfg))?;

        Ok(Resolving { io, cfg, url })
    }
}

impl AsyncRead for TlsClient<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for TlsClient<'_> {
    fn poll_write<'w>(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'w>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush<'f>(mut self: Pin<&mut Self>, cx: &mut Context<'f>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
}

impl TokenBearer for TlsClient<'_> {
    fn get_token(&self) -> mio::Token {
        self.io.get_token()
    }
}
