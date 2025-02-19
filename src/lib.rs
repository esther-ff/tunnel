#![allow(dead_code)]

mod stream;
#[cfg(test)]
mod tests {

    use rustls::{ClientConfig, RootCertStore};

    use super::*;
    use lamp::io::TcpStream;
    use lamp::io::{AsyncReadExt, AsyncWriteExt};
    use lamp::runtime::Executor;
    use log::info;
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
        log::set_logger(logger).map(|()| log::set_max_level(log::LevelFilter::Debug))
    }

    use webpki_roots::TLS_SERVER_ROOTS;
    #[test]

    fn connect_to_echo() {
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
            let mut stream = Stream::create(io, dns_name, cfg).unwrap().await.unwrap();

            let req = concat!(
                "GET / HTTP/1.1\r\n",
                "User-Agent: testing/0.0.1\r\n",
                "\r\n"
            )
            .as_bytes();

            let write = stream.write(req).await;
            match write {
                Ok(wrlen) => {
                    dbg!(wrlen);
                }
                Err(e) => {
                    dbg!(e);
                    assert!(false, "failed write");
                }
            }
            let _ = stream.flush().await;

            info!("reading");
            let mut buf: [u8; 4096] = [0u8; 4096];
            let read = stream.read(&mut buf).await;

            let rdlen = match read {
                Ok(rdlen) => {
                    dbg!(rdlen);
                    rdlen
                }
                Err(e) => {
                    dbg!(e);
                    panic!("failed read")
                }
            };

            let string = std::str::from_utf8(&buf[0..rdlen]);
            assert!(string.is_ok(), "buf is not correct utf-8");

            println!("response:\n {}", string.unwrap());
        });

        rt.shutdown();

        assert!(result.is_ok(), "runtime shutdown abruptly due to an error");
    }
}
