//! `postcard-rpc` transport over the browser's Web Serial API.
//!
//! postcard-rpc's host client is transport-agnostic: it needs a `WireTx` (send one
//! frame), a `WireRx` (receive one frame) and a `WireSpawn` (run background tasks).
//! On a serial port, frames are COBS-encoded and terminated by a 0 byte, the same
//! framing the firmware's embedded-io server uses.

use std::collections::VecDeque;
use std::future::Future;

use js_sys::{Reflect, Uint8Array};
use postcard_rpc::{
    accumulator::raw::{CobsAccumulator, FeedResult},
    header::VarSeqKind,
    host_client::{HostClient, WireRx, WireSpawn, WireTx},
    standard_icd::{ERROR_PATH, WireError},
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, SerialOptions, SerialPort, WritableStreamDefaultWriter,
};

/// The client type the rest of the app uses.
pub type Client = HostClient<WireError>;

/// The accumulator holds one COBS-encoded device message including its terminating 0.
const MAX_FRAME: usize =
    cobs::max_encoding_length(six_dof_platform_common::interface::MAX_DEVICE_MESSAGE_SIZE) + 1;

#[derive(thiserror::Error, Debug, Clone)]
pub enum SerialError {
    #[error("browser error: {0}")]
    Js(String),
    #[error("serial port closed")]
    Closed,
}

impl From<JsValue> for SerialError {
    fn from(v: JsValue) -> Self {
        SerialError::Js(
            v.as_string()
                .or_else(|| {
                    v.dyn_ref::<js_sys::Error>()
                        .map(|e| String::from(e.message()))
                })
                .unwrap_or_else(|| format!("{v:?}")),
        )
    }
}

/// An open serial port with a postcard-rpc client running on it.
pub struct Connection {
    pub client: Client,
    port: SerialPort,
    // JS handles are reference-counted on the JS side; these are the same objects
    // the wire structs use. Kept here to tear the connection down.
    reader: ReadableStreamDefaultReader,
    writer: WritableStreamDefaultWriter,
}

impl Connection {
    /// Stops the client and closes the port so it can be opened again.
    pub async fn close(self) {
        self.client.close();
        // Cancelling the reader ends a pending `read()` (it resolves with done = true).
        // A stream must be unlocked (lock released) before the port can be closed.
        // Errors are ignored: after an unplug these may fail, and there's nothing to do.
        let _ = JsFuture::from(self.reader.cancel()).await;
        self.reader.release_lock();
        let _ = JsFuture::from(self.writer.close()).await;
        self.writer.release_lock();
        let _ = JsFuture::from(self.port.close()).await;
    }
}

/// Asks the user to pick a serial port (must be called from a user gesture, e.g. a
/// click handler), opens it and starts a postcard-rpc client on it.
pub async fn connect() -> Result<Connection, SerialError> {
    let serial = web_sys::window()
        .ok_or_else(|| SerialError::Js("no window".into()))?
        .navigator()
        .serial();
    if serial.is_undefined() {
        return Err(SerialError::Js(
            "this browser doesn't support Web Serial".into(),
        ));
    }
    let port: SerialPort = JsFuture::from(serial.request_port())
        .await?
        .unchecked_into();

    // The baud rate is required by the API but meaningless for the ESP32-C6's
    // USB CDC-ACM port: data moves at USB speed regardless.
    JsFuture::from(port.open(&SerialOptions::new(115_200))).await?;

    let reader: ReadableStreamDefaultReader = port.readable().get_reader().unchecked_into();
    let writer: WritableStreamDefaultWriter = port.writable().get_writer()?;

    let client = HostClient::new_with_wire(
        SerialTx {
            writer: writer.clone(),
        },
        SerialRx {
            reader: reader.clone(),
            acc: Box::new(CobsAccumulator::new()),
            pending: VecDeque::new(),
        },
        LocalSpawn,
        VarSeqKind::Seq4,
        ERROR_PATH,
        8,
    );
    Ok(Connection {
        client,
        port,
        reader,
        writer,
    })
}

struct SerialTx {
    writer: WritableStreamDefaultWriter,
}

impl WireTx for SerialTx {
    type Error = SerialError;

    fn send(&mut self, data: Vec<u8>) -> impl Future<Output = Result<(), Self::Error>> {
        let mut frame = cobs::encode_vec(&data);
        frame.push(0);
        let chunk = Uint8Array::from(frame.as_slice());
        let promise = self.writer.write_with_chunk(&chunk);
        async move {
            JsFuture::from(promise).await?;
            Ok(())
        }
    }
}

struct SerialRx {
    reader: ReadableStreamDefaultReader,
    acc: Box<CobsAccumulator<MAX_FRAME>>,
    /// Complete frames already decoded from earlier reads.
    pending: VecDeque<Vec<u8>>,
}

impl WireRx for SerialRx {
    type Error = SerialError;

    fn receive(&mut self) -> impl Future<Output = Result<Vec<u8>, Self::Error>> {
        self.receive_inner()
    }
}

impl SerialRx {
    async fn receive_inner(&mut self) -> Result<Vec<u8>, SerialError> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(frame);
            }

            // `read()` resolves to `{ value: Uint8Array, done: bool }`.
            let result = JsFuture::from(self.reader.read()).await?;
            if Reflect::get(&result, &"done".into())?.is_truthy() {
                return Err(SerialError::Closed);
            }
            let chunk = Uint8Array::new(&Reflect::get(&result, &"value".into())?).to_vec();

            // One read can contain several frames, or end in the middle of one.
            let mut window = chunk.as_slice();
            while !window.is_empty() {
                window = match self.acc.feed(window) {
                    FeedResult::Consumed => break,
                    // Corrupt or oversized frames are skipped; the next 0 byte resyncs.
                    FeedResult::OverFull(rest) | FeedResult::DeserError(rest) => {
                        leptos::logging::warn!("dropped a malformed frame");
                        rest
                    }
                    FeedResult::Success { data, remaining } => {
                        self.pending.push_back(data.to_vec());
                        remaining
                    }
                };
            }
        }
    }
}

/// Runs the client's background tasks on the browser's event loop.
struct LocalSpawn;

impl WireSpawn for LocalSpawn {
    fn spawn(&mut self, fut: impl Future<Output = ()> + 'static) {
        wasm_bindgen_futures::spawn_local(fut);
    }
}
