#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use clap::Parser;

/// Host shared CADCraft drawing sessions. Clients join `ws://ADDRESS/s/<session>`; put a
/// TLS-terminating proxy in front for `wss://`.
#[derive(Parser)]
#[command(name = "cadcraft-server", version)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:9010")]
    listen: String,
}

fn main() {
    let args = Args::parse();
    let listener = match std::net::TcpListener::bind(&args.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cadcraft-server: can't listen on {}: {e}", args.listen);
            std::process::exit(1);
        }
    };
    eprintln!("cadcraft-server: listening on ws://{}/s/<session>", args.listen);
    if let Err(e) = cadcraft_server::serve(listener, cadcraft_server::Rooms::default()) {
        eprintln!("cadcraft-server: {e}");
        std::process::exit(1);
    }
}
