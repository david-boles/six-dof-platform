//! Interface control document (ICD) for the six-DOF test stand.
//!
//! Shared by the firmware (`firmware/`, `no_std`) and the web UI (`web/`, wasm).
//! Every message type here is serialized with `postcard`. `postcard-rpc` identifies
//! each endpoint/topic by a hash of its path *and* its type's schema, so changing a
//! type here changes its key: a firmware/UI mismatch shows up as "unknown key"
//! instead of being silently mis-decoded.
//!
//! Also available without being listed here (postcard-rpc's standard ICD,
//! `postcard_rpc::standard_icd`): a ping endpoint (`postcard-rpc/ping`) and
//! `LoggingTopic` (`postcard-rpc/logging`), on which the firmware sends free-text
//! log messages such as ADC error details.

#![cfg_attr(not(feature = "use-std"), no_std)]

use postcard::experimental::max_size::MaxSize;
use postcard_rpc::{Endpoint, Topic, TopicDirection, endpoints, topics};
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// Number of load cell channels (3 ADCs × 2 differential inputs).
pub const CHANNELS: usize = 6;

endpoints! {
    list = ENDPOINT_LIST;
    | EndpointTy           | RequestTy | ResponseTy | Path             |
    | ----------           | --------- | ---------- | ----             |
    | GetInfoEndpoint      | ()        | DeviceInfo | "info/get"       |
    | GetAdcStatusEndpoint | ()        | AdcStatus  | "adc/status/get" |
}

topics! {
    list = TOPICS_IN_LIST;
    direction = TopicDirection::ToServer;
    | TopicTy | MessageTy | Path |
    | ------- | --------- | ---- |
}

topics! {
    list = TOPICS_OUT_LIST;
    direction = TopicDirection::ToClient;
    | TopicTy        | MessageTy | Path           |
    | -------        | --------- | ----           |
    | ReadingsTopic  | Readings  | "adc/readings" |
    | AdcStatusTopic | AdcStatus | "adc/status"   |
}

// ---------------------------------------------------------------------------
// Maximum message sizes, for sizing buffers on both ends.
//
// KEEP IN SYNC with the tables above: every endpoint and topic must appear in
// the matching list below. A forgotten one fails quietly at run time: a request
// that doesn't fit the device's buffer is dropped (the host's call never
// returns); a device message that doesn't fit fails to serialize ("publish
// failed" in the firmware log).
// ---------------------------------------------------------------------------

/// Longest free-text log message the firmware sends on `LoggingTopic`, in bytes.
/// The firmware truncates longer messages.
pub const MAX_LOG_LEN: usize = 128;

/// Largest postcard-rpc header: 1 discriminant byte + key (up to 8 bytes) +
/// sequence number (up to 4 bytes). See postcard-rpc's `header.rs`.
pub const MAX_HEADER_SIZE: usize = 1 + 8 + 4;

/// Largest decoded frame (header + body) the host sends to the device.
pub const MAX_REQUEST_SIZE: usize = MAX_HEADER_SIZE
    + max_of([
        request_size::<GetInfoEndpoint>(),
        request_size::<GetAdcStatusEndpoint>(),
        // postcard-rpc's standard ping endpoint (`u32`).
        u32::POSTCARD_MAX_SIZE,
    ]);

/// Largest decoded frame (header + body) the device sends to the host.
pub const MAX_DEVICE_MESSAGE_SIZE: usize = MAX_HEADER_SIZE
    + max_of([
        response_size::<GetInfoEndpoint>(),
        response_size::<GetAdcStatusEndpoint>(),
        message_size::<ReadingsTopic>(),
        message_size::<AdcStatusTopic>(),
        // `LoggingTopic` text: varint length prefix + bytes.
        varint_size(MAX_LOG_LEN) + MAX_LOG_LEN,
        // postcard-rpc's standard ping reply (`u32`).
        u32::POSTCARD_MAX_SIZE,
        // postcard-rpc's `WireError` replies; largest variant is
        // `FrameTooLong { len: u32, max: u32 }`: 1 + 5 + 5. A foreign type, so
        // `MaxSize` can't be derived for it.
        1 + 2 * u32::POSTCARD_MAX_SIZE,
    ]);

const fn request_size<E: Endpoint>() -> usize
where
    E::Request: MaxSize,
{
    E::Request::POSTCARD_MAX_SIZE
}

const fn response_size<E: Endpoint>() -> usize
where
    E::Response: MaxSize,
{
    E::Response::POSTCARD_MAX_SIZE
}

const fn message_size<T: Topic>() -> usize
where
    T::Message: MaxSize,
{
    T::Message::POSTCARD_MAX_SIZE
}

const fn max_of<const N: usize>(sizes: [usize; N]) -> usize {
    let mut max = 0;
    let mut i = 0;
    while i < N {
        if sizes[i] > max {
            max = sizes[i];
        }
        i += 1;
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the computed sizes so changes to them are noticed (update the
    /// expected values deliberately).
    #[test]
    fn max_sizes() {
        assert_eq!(Readings::POSTCARD_MAX_SIZE, 10 + 6 * 5);
        assert_eq!(DeviceInfo::POSTCARD_MAX_SIZE, 10 + 5);
        assert_eq!(MAX_REQUEST_SIZE, 13 + 5);
        // Largest is the log message: 2-byte length prefix + 128 bytes.
        assert_eq!(MAX_DEVICE_MESSAGE_SIZE, 13 + 2 + 128);
    }

    #[test]
    fn varint_sizes() {
        assert_eq!(varint_size(0), 1);
        assert_eq!(varint_size(127), 1);
        assert_eq!(varint_size(128), 2);
        assert_eq!(varint_size(16_383), 2);
        assert_eq!(varint_size(16_384), 3);
    }
}

/// Bytes postcard uses to encode `value` as a varint (7 bits per byte).
const fn varint_size(value: usize) -> usize {
    let mut bytes = 1;
    let mut rest = value >> 7;
    while rest > 0 {
        bytes += 1;
        rest >>= 7;
    }
    bytes
}

/// Static information about the device.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct DeviceInfo {
    /// Derived from the chip's factory MAC address.
    pub unique_id: u64,
    /// Time between complete sets of readings, in microseconds.
    pub sample_period_us: u32,
}

/// One complete set of raw load cell readings.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Readings {
    /// Device time (since boot) when the set was completed, in microseconds.
    /// Sets are produced every `DeviceInfo::sample_period_us` by a hardware timer, so
    /// a larger gap between consecutive timestamps means readings are missing:
    /// dropped by the firmware (e.g. nobody was reading the USB port) or on the
    /// UI side, or not taken (ADC re-initialization after an error).
    pub timestamp_us: u64,
    /// Signed 24-bit ADC counts, sign-extended. Index = 2 × ADC index + channel.
    pub raw: [i32; CHANNELS],
}

/// State of the ADC sampling task since the device started. Details of each error
/// are sent as text on postcard-rpc's `LoggingTopic`.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum AdcStatus {
    /// No errors so far.
    Ok,
    /// At least one error occurred; the firmware re-initialized the ADCs and
    /// sampling continues.
    Errored,
    /// Too many errors in a short time; sampling has stopped until the device is reset.
    Terminated,
}
