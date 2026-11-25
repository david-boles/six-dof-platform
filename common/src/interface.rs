//! Communication interface of the six-DOF test stand: postcard-rpc endpoints,
//! topics and message types.
//!
//! Shared by the firmware (`firmware/`, `no_std`) and the web UI (`web/`, wasm).
//! Every message type here is serialized with `postcard`. `postcard-rpc` identifies
//! each endpoint/topic by a hash of its path *and* its type's schema, so changing a
//! type here changes its key: a firmware/UI mismatch shows up as "unknown key"
//! instead of being silently mis-decoded.
//!
//! Also available without being listed here (postcard-rpc's standard interface,
//! `postcard_rpc::standard_icd`): a ping endpoint (`postcard-rpc/ping`) and
//! `LoggingTopic` (`postcard-rpc/logging`), on which the firmware sends free-text
//! log messages such as ADC error details.

use postcard::experimental::max_size::MaxSize;
use postcard_rpc::{Endpoint, Topic, TopicDirection, endpoints, topics};
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// Number of load cell channels (3 ADCs × 2 differential inputs).
pub const CHANNELS: usize = 6;
/// Number of wrench components: `[Fx, Fy, Fz, Mx, My, Mz]`.
pub const WRENCH_LEN: usize = 6;

/// Calibration matrix C in `w = C · (c − z)`: row = wrench component
/// (`[Fx, Fy, Fz, Mx, My, Mz]`), column = channel. Rows 0–2 in N/count, rows
/// 3–5 in N·m/count; torques about the origin (`reference/geometry.md`).
pub type CalibrationMatrix = [[f32; CHANNELS]; WRENCH_LEN];
/// Zero offsets z in `w = C · (c − z)`, in ADC counts per channel. Floating
/// point because they're averages (sub-count resolution).
pub type ZeroCounts = [f32; CHANNELS];
/// Reply to the `*/set` endpoints. (An alias because postcard-rpc's
/// `endpoints!` table only accepts single-token types.)
pub type SetResult = Result<(), SetError>;
/// Reply to `average` (alias: see `SetResult`).
pub type AverageResult = Result<AverageCounts, AverageError>;

/// How long `average` averages for.
pub const AVERAGE_DURATION_MS: u32 = 1000;

// Endpoints:
// - `interface/hash/get`: the firmware's `INTERFACE_HASH`. The UI asks for it
//   first, to detect firmware/UI version mismatches. NEVER CHANGE this endpoint
//   (path or types), or that check can't work across versions.
// - `info/get`, `adc/status/get`: see `DeviceInfo`, `AdcStatus`.
// - `time/get`: device time now, in µs since boot (same clock as
//   `Readings::timestamp_us`). The UI uses it after connecting to tell readings
//   that were queued before the connection (sent first, possibly much older)
//   from current ones.
// - `calibration/get|set`, `zero/get|set`: C and z used for the `wrench` topic.
//   Held in RAM only: after a reset the firmware uses `geometry::DEFAULT_C` and
//   z = 0 again. `set` refuses NaN and infinite values.
// - `average`: mean and standard deviation of the raw counts over the next
//   `AVERAGE_DURATION_MS` (e.g. for zeroing: then `zero/set` with the mean).
//   One at a time: a second request while one runs gets postcard-rpc's
//   `WireError::FailedToSpawn`.
endpoints! {
    list = ENDPOINT_LIST;
    | EndpointTy               | RequestTy         | ResponseTy         | Path                 |
    | ----------               | ---------         | ----------         | ----                 |
    | GetInterfaceHashEndpoint | ()                | u64                | "interface/hash/get" |
    | GetInfoEndpoint          | ()                | DeviceInfo         | "info/get"           |
    | GetAdcStatusEndpoint     | ()                | AdcStatus          | "adc/status/get"     |
    | GetTimeEndpoint          | ()                | u64                | "time/get"           |
    | GetCalibrationEndpoint   | ()                | CalibrationSetting | "calibration/get"    |
    | SetCalibrationEndpoint   | CalibrationMatrix | SetResult          | "calibration/set"    |
    | GetZeroEndpoint          | ()                | ZeroSetting        | "zero/get"           |
    | SetZeroEndpoint          | ZeroCounts        | SetResult          | "zero/set"           |
    | AverageEndpoint          | ()                | AverageResult      | "average"            |
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
    | WrenchTopic    | Wrench    | "wrench"       |
}

// ---------------------------------------------------------------------------
// Interface hash
// ---------------------------------------------------------------------------

/// Hash of the whole interface: all endpoint request/response keys and topic
/// keys, including postcard-rpc's standard ones. Each key is itself a hash of the
/// path and the type's wire structure (field and variant names, types, nesting;
/// not type names), so this changes whenever anything on the wire changes. It
/// doesn't change for changes in meaning that leave the bytes the same (units,
/// sign conventions); David decided that's acceptable (2026-11-24).
///
/// The firmware returns it from `interface/hash/get`; the UI compares it with its
/// own on connect.
pub const INTERFACE_HASH: u64 = interface_hash();

const fn interface_hash() -> u64 {
    // FNV-1a (64 bit), the same hash postcard-rpc uses for keys: for each byte,
    // XOR it into the state, then multiply by the FNV prime.
    const fn update(mut state: u64, bytes: [u8; 8]) -> u64 {
        let mut i = 0;
        while i < bytes.len() {
            state ^= bytes[i] as u64;
            state = state.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        state
    }

    let mut state: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    let endpoints = ENDPOINT_LIST.endpoints;
    let mut i = 0;
    while i < endpoints.len() {
        state = update(state, endpoints[i].1.to_bytes());
        state = update(state, endpoints[i].2.to_bytes());
        i += 1;
    }
    let topic_lists = [TOPICS_IN_LIST.topics, TOPICS_OUT_LIST.topics];
    let mut list = 0;
    while list < topic_lists.len() {
        let topics = topic_lists[list];
        let mut i = 0;
        while i < topics.len() {
            state = update(state, topics[i].1.to_bytes());
            i += 1;
        }
        list += 1;
    }
    state
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
        request_size::<GetInterfaceHashEndpoint>(),
        request_size::<GetInfoEndpoint>(),
        request_size::<GetAdcStatusEndpoint>(),
        request_size::<GetTimeEndpoint>(),
        request_size::<GetCalibrationEndpoint>(),
        request_size::<SetCalibrationEndpoint>(),
        request_size::<GetZeroEndpoint>(),
        request_size::<SetZeroEndpoint>(),
        request_size::<AverageEndpoint>(),
        // postcard-rpc's standard ping endpoint (`u32`).
        u32::POSTCARD_MAX_SIZE,
    ]);

/// Largest decoded frame (header + body) the device sends to the host.
pub const MAX_DEVICE_MESSAGE_SIZE: usize = MAX_HEADER_SIZE
    + max_of([
        response_size::<GetInterfaceHashEndpoint>(),
        response_size::<GetInfoEndpoint>(),
        response_size::<GetAdcStatusEndpoint>(),
        response_size::<GetTimeEndpoint>(),
        response_size::<GetCalibrationEndpoint>(),
        response_size::<SetCalibrationEndpoint>(),
        response_size::<GetZeroEndpoint>(),
        response_size::<SetZeroEndpoint>(),
        response_size::<AverageEndpoint>(),
        message_size::<ReadingsTopic>(),
        message_size::<AdcStatusTopic>(),
        message_size::<WrenchTopic>(),
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
        // f32 is always 4 bytes (not a varint).
        assert_eq!(Wrench::POSTCARD_MAX_SIZE, 10 + 6 * 4);
        assert_eq!(SetResult::POSTCARD_MAX_SIZE, 1 + 1);
        // Largest request: `calibration/set`, 36 × f32.
        assert_eq!(MAX_REQUEST_SIZE, 13 + 36 * 4);
        // Largest device message: the `calibration/get` reply (36 × f32 + bool);
        // next is the log message (2-byte length prefix + 128 bytes).
        assert_eq!(MAX_DEVICE_MESSAGE_SIZE, 13 + 36 * 4 + 1);
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

/// The wrench computed by the firmware for one set of readings:
/// `w = C · (c − z)` with the current calibration matrix and zero offsets.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Wrench {
    /// Same as `Readings::timestamp_us` of the readings it was computed from.
    pub timestamp_us: u64,
    /// `[Fx, Fy, Fz, Mx, My, Mz]` in N and N·m: the external wrench applied to
    /// the platform, torques about the origin (`reference/geometry.md`).
    pub values: [f32; WRENCH_LEN],
}

/// Reply to `calibration/get`.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CalibrationSetting {
    pub matrix: CalibrationMatrix,
    /// True until `calibration/set` is called (the firmware's built-in nominal
    /// matrix, `geometry::DEFAULT_C`, is in use).
    pub is_boot_default: bool,
}

/// Reply to `zero/get`.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ZeroSetting {
    pub counts: ZeroCounts,
    /// True until `zero/set` is called (zero offsets are all 0).
    pub is_boot_default: bool,
}

/// Reply to `average`: statistics of the raw counts per channel.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct AverageCounts {
    /// Mean, in ADC counts.
    pub mean: [f32; CHANNELS],
    /// Sample standard deviation (noise), in ADC counts.
    pub std_dev: [f32; CHANNELS],
    /// Number of readings averaged.
    pub n: u32,
}

/// Why `average` failed.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum AverageError {
    /// An ADC error happened during the averaging window (details in the log).
    AdcError,
    /// Not enough readings arrived in time (e.g. sampling has terminated).
    Timeout,
}

/// Why a `*/set` request was refused.
#[derive(Serialize, Deserialize, Schema, MaxSize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SetError {
    /// A value is NaN or infinite (would make every wrench NaN).
    NotFinite,
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
