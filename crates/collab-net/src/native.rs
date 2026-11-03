//! The WebSocket link on native platforms.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

use cadcraft_collab::message::{self, ClientMsg, MAX_MESSAGE_BYTES, ServerMsg};
use cadcraft_collab::{Error, Link, Result};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// How long a read waits before the thread looks for messages to send.
const POLL: Duration = Duration::from_millis(15);
/// How long reaching the server may take, and then the handshake, before connecting gives up.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
const HANDSHAKE_WAIT: Duration = Duration::from_secs(10);

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub(crate) fn connect(url: &str) -> Result<Box<dyn Link>> {
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE_BYTES)).max_frame_size(Some(MAX_MESSAGE_BYTES));
    let fail = |e: String| Error::Invalid(format!("can't connect to {url}: {e}"));
    let request = tungstenite::client::IntoClientRequest::into_client_request(url).map_err(|e| fail(e.to_string()))?;
    let uri = request.uri();
    let host = uri.host().ok_or_else(|| fail("no host name".into()))?.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
    let addrs: Vec<_> = (host.as_str(), port).to_socket_addrs().map_err(|e| fail(e.to_string()))?.collect();
    // A host that doesn't answer would otherwise hold the connection attempt for a minute or more.
    let mut last = fail("the host name has no address".into());
    let mut stream = None;
    for a in addrs.iter().take(8) {
        match TcpStream::connect_timeout(a, CONNECT_WAIT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = fail(e.to_string()),
        }
    }
    let stream = stream.ok_or(last)?;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(HANDSHAKE_WAIT)).and_then(|()| stream.set_write_timeout(Some(HANDSHAKE_WAIT))).map_err(|e| fail(e.to_string()))?;
    let (mut ws, _) = tungstenite::client_tls_with_config(request, stream, Some(config), None).map_err(|e| match e {
        tungstenite::HandshakeError::Failure(f) => fail(f.to_string()),
        tungstenite::HandshakeError::Interrupted(_) => fail("the server did not answer in time".into()),
    })?;
    poll_reads(&mut ws).map_err(|e| Error::Invalid(format!("can't set up the connection: {e}")))?;
    let (out_tx, out_rx) = mpsc::channel();
    let (in_tx, in_rx) = mpsc::channel();
    let open = Arc::new(AtomicBool::new(true));
    let flag = open.clone();
    std::thread::Builder::new()
        .name("collab-link".into())
        .spawn(move || {
            pump(ws, &out_rx, &in_tx);
            flag.store(false, Ordering::SeqCst);
        })
        .map_err(|e| Error::Invalid(format!("can't start the connection thread: {e}")))?;
    Ok(Box::new(WsLink { out: out_tx, inbox: in_rx, open }))
}

/// Make reads return after [`POLL`] so one thread can both read and write.
fn poll_reads(ws: &mut Socket) -> std::io::Result<()> {
    match ws.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(POLL)),
        MaybeTlsStream::Rustls(s) => s.get_ref().set_read_timeout(Some(POLL)),
        _ => Ok(()),
    }
}

/// Move messages until either side closes.
fn pump(mut ws: Socket, out: &Receiver<Vec<u8>>, inbox: &Sender<Result<ServerMsg>>) {
    loop {
        loop {
            match out.try_recv() {
                Ok(bytes) => {
                    if ws.send(Message::Binary(bytes.into())).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
        match ws.read() {
            Ok(Message::Binary(b)) => {
                if inbox.send(message::decode(&b)).is_err() {
                    return;
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                // Pongs to the server's pings go out with the next write or flush.
                let _ = ws.flush();
            }
            Err(_) => return,
        }
    }
}

struct WsLink {
    out: Sender<Vec<u8>>,
    inbox: Receiver<Result<ServerMsg>>,
    open: Arc<AtomicBool>,
}

impl Link for WsLink {
    fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        let bytes = message::encode(msg)?;
        self.out.send(bytes).map_err(|_| Error::Invalid("the connection is closed".into()))
    }

    fn recv(&mut self) -> Option<Result<ServerMsg>> {
        self.inbox.try_recv().ok()
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }
}
