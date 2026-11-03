//! The WebSocket link in the browser: the page's `WebSocket`, whose callbacks fill a queue the
//! engine drains each frame. Messages sent while the socket is still opening wait for it.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use cadcraft_collab::message::{self, ClientMsg, MAX_MESSAGE_BYTES, ServerMsg};
use cadcraft_collab::{Error, Link, Result};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, Event, MessageEvent, WebSocket};

#[derive(Default)]
struct State {
    inbox: VecDeque<Result<ServerMsg>>,
    /// Sent before the socket opened.
    outbox: Vec<Vec<u8>>,
    connected: bool,
    closed: bool,
}

type Shared = Rc<RefCell<State>>;

pub(crate) fn connect(url: &str) -> Result<Box<dyn Link>> {
    let ws = WebSocket::new(url).map_err(|e| Error::Invalid(format!("can't connect to {url}: {e:?}")))?;
    ws.set_binary_type(BinaryType::Arraybuffer);
    let state: Shared = Rc::default();

    let (s, sock) = (state.clone(), ws.clone());
    let on_open = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
        let Ok(mut st) = s.try_borrow_mut() else { return };
        st.connected = true;
        for bytes in std::mem::take(&mut st.outbox) {
            if sock.send_with_u8_array(&bytes).is_err() {
                st.closed = true;
            }
        }
    });
    let s = state.clone();
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let Ok(mut st) = s.try_borrow_mut() else { return };
        let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() else { return };
        let msg = if buf.byte_length() as usize > MAX_MESSAGE_BYTES {
            Err(Error::Invalid("a message from the server is too large".into()))
        } else {
            message::decode(&js_sys::Uint8Array::new(&buf).to_vec())
        };
        st.inbox.push_back(msg);
    });
    let s = state.clone();
    let on_close = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
        if let Ok(mut st) = s.try_borrow_mut() {
            st.closed = true;
        }
    });
    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    ws.set_onerror(Some(on_close.as_ref().unchecked_ref()));
    Ok(Box::new(WebLink { ws, state, _callbacks: (on_open, on_message, on_close) }))
}

struct WebLink {
    ws: WebSocket,
    state: Shared,
    /// Kept alive as long as the socket can call them.
    _callbacks: (Closure<dyn FnMut(Event)>, Closure<dyn FnMut(MessageEvent)>, Closure<dyn FnMut(Event)>),
}

impl Link for WebLink {
    fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        let bytes = message::encode(msg)?;
        let mut st = self.state.try_borrow_mut().map_err(|_| Error::Invalid("the connection is busy".into()))?;
        if st.closed {
            return Err(Error::Invalid("the connection is closed".into()));
        }
        if !st.connected {
            st.outbox.push(bytes);
            return Ok(());
        }
        self.ws.send_with_u8_array(&bytes).map_err(|e| Error::Invalid(format!("can't send: {e:?}")))
    }

    fn recv(&mut self) -> Option<Result<ServerMsg>> {
        self.state.try_borrow_mut().ok()?.inbox.pop_front()
    }

    fn is_open(&self) -> bool {
        self.state.try_borrow().is_ok_and(|st| !st.closed || !st.inbox.is_empty())
    }
}

impl Drop for WebLink {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        self.ws.set_onerror(None);
        let _ = self.ws.close();
    }
}
