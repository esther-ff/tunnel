mod stream;
#[cfg(test)]
mod tests {

    use rustls::{ClientConfig, RootCertStore};

    use super::*;
    use lamp::io::TcpStream;
    use lamp::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use lamp::runtime::Executor;
    use stream::Stream;

    use webpki_roots::TLS_SERVER_ROOTS;
    #[test]

    fn connect_to_rust_lang_org() {
        let mut rt = Executor::new(4);
        rt.block_on(async {
            let url = "13.227.146.83:443";
            let dns_name = "www.rust-lang.org".try_into().unwrap();
            let certs = RootCertStore {
                roots: TLS_SERVER_ROOTS.into(),
            };

            let cfg = ClientConfig::builder()
                .with_root_certificates(certs)
                .with_no_client_auth();

            let io = TcpStream::new(url).unwrap();
            let mut stream = Stream::new(io, dns_name, cfg).unwrap();

            let req = concat!(
                "GET / HTTP/1.1\r\n",
                "Host: www.rust-lang.org\r\n",
                "User-Agent: testing/0.0.1\r\n",
                "\r\n"
            )
            .as_bytes();

            println!("Started write!");
            let write = stream.write(req).await;
            dbg!(write);
            let _ = stream.flush().await;
            println!("Finished write!");
            let mut buf: [u8; 1024] = [0u8; 1024];

            println!("Started reading!");
            let read = stream.read(&mut buf).await;
            dbg!(read);
            println!("Finished reading!");

            println!("{:?}", buf);
        });

        rt.shutdown();
    }
}
