#![allow(dead_code)]

mod stream;
#[cfg(test)]
mod tests {

    use rustls::{ClientConfig, RootCertStore};

    use super::*;
    use lamp::io::TcpStream;
    use lamp::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use lamp::runtime::Executor;
    use stream::Stream;

    use log::{Level, Metadata, Record};

    struct Logger;

    impl log::Log for Logger {
        fn enabled(&self, m: &Metadata) -> bool {
            m.level() <= Level::Info
        }

        fn log(&self, r: &Record) {
            if self.enabled(r.metadata()) {
                println!(
                    "[{}: {}] => {}",
                    r.level(),
                    r.file().map_or("undetected", |x| x),
                    r.args()
                )
            }
        }

        fn flush(&self) {}
    }

    fn log_init(logger: &'static Logger) -> Result<(), log::SetLoggerError> {
        log::set_logger(logger).map(|()| log::set_max_level(log::LevelFilter::Info))
    }

    use webpki_roots::TLS_SERVER_ROOTS;
    #[test]

    fn connect_to_echo() {
        static LOG: Logger = Logger;

        log_init(&LOG).expect("log fail");
        let mut rt = Executor::new(4);
        rt.block_on(async {
            let ip = "147.182.252.2:443";
            let url = "echo.free.beeceptor.com:443";

            let dns_name = "echo.free.beeceptor.com".try_into().unwrap();
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
            let _ = stream.flush().await;
            let mut buf: [u8; 128] = [0u8; 128];
            let read = stream.read(&mut buf).await.unwrap();

            println!("{:?}", buf);
        });

        rt.shutdown();
    }
}
