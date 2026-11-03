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
    /// The built web app (`trunk build --release` in apps/cadcraft-web writes `dist/web`): then
    /// `http://ADDRESS/s/<session>` opens CADCraft in a browser, joined to that session.
    #[arg(long)]
    web: Option<std::path::PathBuf>,
    /// Folder for the sessions' drawings: saved as they change and reopened after a restart.
    /// Without it, a session ends when its last person leaves.
    #[arg(long)]
    data: Option<std::path::PathBuf>,
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
    let rooms = cadcraft_server::Rooms::new(cadcraft_server::Config { web: args.web, data: args.data });
    if let Err(e) = cadcraft_server::serve(listener, rooms) {
        eprintln!("cadcraft-server: {e}");
        std::process::exit(1);
    }
}
