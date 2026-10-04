use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use clap::Args;

use crate::error::Error;

#[derive(Args, Debug)]
pub struct HealthArgs {
    /// Address the server listens on (the port is what matters)
    #[arg(long, env = "RUSTYBOX_BIND", default_value = "0.0.0.0:8080")]
    pub bind: SocketAddr,
}

pub fn run(args: HealthArgs) -> Result<(), Error> {
    let addr = SocketAddr::from(([127, 0, 0, 1], args.bind.port()));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    s.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    if out.starts_with("HTTP/1.0 200") || out.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(Error::backend("health check failed"))
    }
}
