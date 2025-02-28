#![allow(dead_code)]

mod http;
mod stream;
mod tls_client;

#[cfg(test)]
mod tests {

    use rustls::{ClientConfig, RootCertStore};

    use super::*;
    use lamp::io::TcpStream;
    use lamp::io::{AsyncReadExt, AsyncWriteExt};
    use lamp::runtime::Executor;
    use log::info;
    use std::sync::Arc;
    use webpki_roots::TLS_SERVER_ROOTS;

    use stream::Stream;

    use log::{Level, Metadata, Record};

    struct Logger;

    impl log::Log for Logger {
        fn enabled(&self, m: &Metadata) -> bool {
            m.level() <= Level::Trace
        }

        fn log(&self, r: &Record) {
            if self.enabled(r.metadata()) {
                println!(
                    "{} {}: {}",
                    r.level(),
                    r.file().map_or("undetected", |x| x),
                    r.args()
                )
            }
        }

        fn flush(&self) {}
    }

    fn log_init(logger: &'static Logger) -> Result<(), log::SetLoggerError> {
        log::set_logger(logger).map(|()| log::set_max_level(log::LevelFilter::Warn))
    }

    #[test]
    fn connect_to_rust_lang_via_stream() {
        static LOG: Logger = Logger;
        log_init(&LOG).expect("log fail");
        let mut rt = Executor::new(4);

        let result = rt.block_on(async {
            let url = "www.rust-lang.org:443";

            let dns_name = "www.rust-lang.org".try_into().unwrap();
            let certs = RootCertStore {
                roots: TLS_SERVER_ROOTS.into(),
            };

            let cfg = ClientConfig::builder()
                .with_root_certificates(certs)
                .with_no_client_auth();

            let sock = std::net::TcpStream::connect(url).unwrap();
            let io = TcpStream::from_std(sock).unwrap();
            let mut stream = Stream::create(io, dns_name, Arc::new(cfg))
                .unwrap()
                .await
                .unwrap();

            let req = concat!(
                "GET / HTTP/1.1\r\n",
                "User-Agent: testing/0.0.1\r\n",
                "\r\n"
            )
            .as_bytes();

            let write = stream.write(req).await;
            match write {
                Ok(_) => {}
                Err(e) => {
                    dbg!(e);
                    assert!(false, "failed write");
                }
            }
            let _ = stream.flush().await;

            let mut buf: [u8; 4096] = [0u8; 4096];
            let read = stream.read(&mut buf).await;

            match read {
                Ok(rdlen) => {
                    dbg!(rdlen);
                }
                Err(e) => {
                    dbg!(e);
                    panic!("failed read")
                }
            };
        });

        rt.shutdown();

        assert!(result.is_ok(), "runtime shutdown abruptly due to an error");
    }

    #[test]
    fn connect_to_rust_lang_via_client() {
        use tls_client::TlsClient;

        let mut rt = Executor::new(4);

        let req = concat!(
            "GET / HTTP/1.1\r\n",
            "User-Agent: testing/0.0.1\r\n",
            "\r\n"
        )
        .as_bytes();

        let task = async move {
            let mut client = match TlsClient::create(None, "www.rust-lang.org") {
                Ok(cl) => cl.await.expect("failure of client"),
                Err(e) => panic!("{}", e),
            };

            let mut buf: [u8; 4096] = [0u8; 4096];

            let _ = client.write(&req).await;
            let read = client.read(&mut buf).await.unwrap();

            // println!("{}", std::str::from_utf8(&buf[0..read]).unwrap());
        };

        let res = rt.block_on(task);

        rt.shutdown();

        assert!(res.is_ok(), "runtime shutdown abruptly due to an error");
    }

    #[test]
    fn connect_to_rust_lang_via_https_client() {
        use http::client::{Client, Method};
        use http::request::ReqBuilder;

        let mut rt = Executor::new(4);

        let res = rt.block_on(async {
            let mut req = ReqBuilder::new(Method::GET);

            let mut client = Client::connect("www.rust-lang.org", "tunnel-test/0.0.1", None)
                .await
                .unwrap();
            req.add_headers(vec![("Test-header", "Test-value")]);

            let (req_as_string, content) = req.clone().show_as_string();
            println!("Req: {req_as_string:?}");
            println!("content: {:#?}", content);

            let test = client.execute(req).await.unwrap();
            let _ = dbg!(std::str::from_utf8(&test));
        });

        rt.shutdown();

        assert!(res.is_ok(), "runtime shutdown abruptly due to an error");
    }
}
