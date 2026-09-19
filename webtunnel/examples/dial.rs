//! Dial one bridge line and report what happened — for poking at real bridges and for
//! capturing this client's ClientHello with a fingerprinting listener.
//!
//!   cargo run -p webtunnel-client --example dial -- 'url=https://host/path;addr=…'

use webtunnel_client::{connect, ClientConfig, PtArgs};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let line = std::env::args()
        .nth(1)
        .expect("usage: dial '<k=v;k=v bridge options>'");
    let config = ClientConfig::from_args(&PtArgs::parse(&line).expect("argument list"))
        .expect("bridge options");
    match connect(&config).await {
        Ok(_) => println!("tunnel open"),
        Err(e) => println!("failed: {e}"),
    }
}
