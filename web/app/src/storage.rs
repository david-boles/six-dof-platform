//! What the UI keeps in the browser and in files:
//! - the saved calibration matrix C and the display reference point, in the
//!   browser's `localStorage` (per origin; for `file://` pages Chromium uses one
//!   storage for all local files);
//! - calibration files: the saved calibration as JSON, for export/import.
//!
//! Only C is saved, not the zero offsets z: offsets drift (temperature, time,
//! what's mounted), and re-zeroing takes a second.

use js_sys::{Array, Date};
use serde::{Deserialize, Serialize};
use six_dof_platform_common::{calibration::all_finite, interface::CalibrationMatrix};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, HtmlInputElement, Storage, Url};

const CALIBRATION_KEY: &str = "six-dof-platform.calibration";
const REFERENCE_POINT_KEY: &str = "six-dof-platform.reference-point";

/// `format` value of calibration files (guards against importing other JSON).
const FORMAT: &str = "six-dof-platform-calibration";
/// `version` of the file format; bump when the fields change.
const VERSION: u32 = 1;

/// A calibration as saved in the browser and in calibration files.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedCalibration {
    pub format: String,
    pub version: u32,
    /// When it was saved (ISO 8601, UTC).
    pub saved_at: String,
    /// Free text, e.g. how it was made.
    pub note: String,
    /// C: rows `[Fx, Fy, Fz, Mx, My, Mz]`, columns = load cells; N/count and
    /// N·m/count, torques about the origin.
    pub matrix: CalibrationMatrix,
}

impl SavedCalibration {
    pub fn new(matrix: CalibrationMatrix, note: String) -> Self {
        Self {
            format: FORMAT.into(),
            version: VERSION,
            saved_at: String::from(Date::new_0().to_iso_string()),
            note,
            matrix,
        }
    }

    /// Parses and checks a calibration file.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let saved: Self =
            serde_json::from_str(text).map_err(|e| format!("not a calibration file: {e}"))?;
        if saved.format != FORMAT {
            return Err(format!(
                "not a calibration file (format \"{}\")",
                saved.format
            ));
        }
        if saved.version != VERSION {
            return Err(format!(
                "unsupported calibration file version {} (expected {VERSION})",
                saved.version
            ));
        }
        if !all_finite(saved.matrix.as_flattened()) {
            return Err("the calibration contains invalid numbers".into());
        }
        Ok(saved)
    }

    pub fn to_json(&self) -> String {
        // Can't fail: only strings, integers and finite floats.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// File name for exporting, e.g. `six-dof-calibration-2026-11-24.json`.
    pub fn file_name(&self) -> String {
        let date = self.saved_at.get(..10).unwrap_or("undated");
        format!("six-dof-calibration-{date}.json")
    }
}

fn local_storage() -> Result<Storage, String> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .ok_or_else(|| "this browser doesn't allow local storage for this page".into())
}

/// The calibration saved in this browser, if any.
pub fn load_calibration() -> Result<Option<SavedCalibration>, String> {
    match local_storage()?.get_item(CALIBRATION_KEY) {
        Ok(Some(text)) => SavedCalibration::from_json(&text)
            .map(Some)
            .map_err(|e| format!("the saved calibration can't be read: {e}")),
        _ => Ok(None),
    }
}

pub fn store_calibration(saved: &SavedCalibration) -> Result<(), String> {
    local_storage()?
        .set_item(CALIBRATION_KEY, &saved.to_json())
        .map_err(|_| "saving in the browser failed".into())
}

/// The display reference point (mm); the origin if none is saved.
pub fn load_reference_point() -> [f64; 3] {
    local_storage()
        .ok()
        .and_then(|s| s.get_item(REFERENCE_POINT_KEY).ok().flatten())
        .and_then(|text| serde_json::from_str::<[f64; 3]>(&text).ok())
        .filter(|p| p.iter().all(|v| v.is_finite()))
        .unwrap_or([0.0; 3])
}

pub fn store_reference_point(point: [f64; 3]) {
    if let Ok(storage) = local_storage() {
        // Not critical: if this fails, the point is just not remembered.
        let _ = storage.set_item(
            REFERENCE_POINT_KEY,
            &serde_json::to_string(&point).unwrap_or_default(),
        );
    }
}

/// Offers `text` as a file download named `file_name`.
///
/// How: wrap the text in a `Blob`, create a temporary URL for it, and click a
/// (never displayed) link to that URL with the `download` attribute set.
pub fn download(file_name: &str, text: &str) -> Result<(), String> {
    let js_err = |e: JsValue| format!("download failed: {e:?}");
    let parts = Array::of1(&JsValue::from_str(text));
    let options = BlobPropertyBag::new();
    options.set_type("application/json");
    let blob = Blob::new_with_str_sequence_and_options(&parts, &options).map_err(js_err)?;
    let url = Url::create_object_url_with_blob(&blob).map_err(js_err)?;
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let link: HtmlAnchorElement = document
        .create_element("a")
        .map_err(js_err)?
        .unchecked_into();
    link.set_href(&url);
    link.set_download(file_name);
    link.click();
    let _ = Url::revoke_object_url(&url);
    Ok(())
}

/// Reads the file selected in a file `<input>` as text.
pub async fn read_selected_file(input: &HtmlInputElement) -> Result<String, String> {
    let file = input
        .files()
        .and_then(|files| files.get(0))
        .ok_or("no file selected")?;
    let text = JsFuture::from(file.text())
        .await
        .map_err(|e| format!("reading the file failed: {e:?}"))?;
    text.as_string().ok_or_else(|| "the file isn't text".into())
}
