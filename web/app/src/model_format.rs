//! Compact binary format of the 3D model embedded in the UI.
//!
//! Written at build time by `xtask` (from `web/assets/platform.gltf`) and read
//! by the app (`view3d.rs`); `xtask` includes this same file with `#[path]`.
//!
//! Why not embed the glTF itself: the export is 8.6 MB of JSON + base64 with
//! 5,300 primitives (one per CAD face) and texture coordinates nobody uses. This
//! format has one mesh per part, quantized normals and 16-bit indices where
//! possible, and needs no glTF parser in the app.
//!
//! Layout (little-endian):
//! ```text
//! "SDPM", version: u32, part count: u32, then per part:
//!   name length: u16, name: UTF-8
//!   color: 4 × f32 (sRGB, 0..1, as Onshape writes it; see `xtask`)
//!   vertex count: u32, index count: u32, index size: u8 (2 or 4)
//!   positions: vertex count × 3 × f32 (m, stand frame: origin at the center of
//!     the top triangle, z up)
//!   normals: vertex count × 3 × i8 (component × 127)
//!   indices: index count × index size bytes (triangles)
//! ```

#![allow(dead_code)] // Each user (app, xtask) uses only one direction.

pub const MAGIC: &[u8; 4] = b"SDPM";
pub const VERSION: u32 = 1;

/// One part of the model (one CAD body).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub name: String,
    /// sRGB + alpha, 0..1.
    pub color: [f32; 4],
    pub positions: Vec<[f32; 3]>,
    /// Unit vectors (after decoding: within about 1 % of unit length,
    /// renormalized).
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

pub fn encode(parts: &[Part]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(parts.len() as u32).to_le_bytes());
    for part in parts {
        let name = part.name.as_bytes();
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        for c in part.color {
            out.extend_from_slice(&c.to_le_bytes());
        }
        let wide = part.positions.len() > usize::from(u16::MAX);
        out.extend_from_slice(&(part.positions.len() as u32).to_le_bytes());
        out.extend_from_slice(&(part.indices.len() as u32).to_le_bytes());
        out.push(if wide { 4 } else { 2 });
        for p in &part.positions {
            for v in p {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        for n in &part.normals {
            for v in n {
                out.push(((v.clamp(-1.0, 1.0) * 127.0).round() as i8) as u8);
            }
        }
        for &i in &part.indices {
            if wide {
                out.extend_from_slice(&i.to_le_bytes());
            } else {
                out.extend_from_slice(&(i as u16).to_le_bytes());
            }
        }
    }
    out
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Part>, String> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(4)? != MAGIC {
        return Err("not a model file".into());
    }
    let version = r.u32()?;
    if version != VERSION {
        return Err(format!(
            "model format version {version}, expected {VERSION}"
        ));
    }
    let count = r.u32()?;
    let mut parts = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name_len = usize::from(u16::from_le_bytes(r.array()?));
        let name =
            String::from_utf8(r.take(name_len)?.to_vec()).map_err(|_| "invalid part name")?;
        let color = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
        let vertices = r.u32()? as usize;
        let index_count = r.u32()? as usize;
        let index_size = r.take(1)?[0];
        let mut positions = Vec::with_capacity(vertices);
        for _ in 0..vertices {
            positions.push([r.f32()?, r.f32()?, r.f32()?]);
        }
        let mut normals = Vec::with_capacity(vertices);
        for _ in 0..vertices {
            let n = r.take(3)?.iter().map(|&b| f32::from(b as i8) / 127.0);
            let n: Vec<f32> = n.collect();
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
            normals.push([n[0] / len, n[1] / len, n[2] / len]);
        }
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indices.push(match index_size {
                2 => u32::from(u16::from_le_bytes(r.array()?)),
                4 => r.u32()?,
                other => return Err(format!("invalid index size {other}")),
            });
        }
        if indices.iter().any(|&i| i as usize >= vertices) {
            return Err(format!("part {name}: index out of range"));
        }
        parts.push(Part {
            name,
            color,
            positions,
            normals,
            indices,
        });
    }
    Ok(parts)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let end = end.ok_or("model data ends too early")?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().expect("take returns N bytes"))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.array()?))
    }
}

// Tests: in `xtask/src/model.rs` (this crate can't run tests on the host).
