//! Converts the CAD export (`web/assets/platform.gltf`) into the compact model
//! format the UI embeds (`app/src/model_format.rs`).
//!
//! - One part per glTF node with a mesh (Onshape: one per CAD body), named like
//!   the node. Onshape splits each body into one primitive per CAD face; they're
//!   merged here, so the UI draws 27 meshes instead of 5,300.
//! - Node transforms (including parents') are applied to the vertices, so parts
//!   end up in the stand frame (Onshape's export has none, but this keeps working
//!   if a later export does).
//! - Color: the material's `baseColorFactor`. glTF defines it as linear RGB, but
//!   Onshape writes the sRGB values (its UI colors / 255), so it's kept as sRGB.
//! - Dropped: texture coordinates (no textures), other material properties.

use std::path::Path;

#[path = "../../app/src/model_format.rs"]
pub mod model_format;

use model_format::Part;

/// 4×4 matrix, column-major (as glTF stores it): `m[column][row]`.
type Mat4 = [[f32; 4]; 4];

const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for (c, column) in out.iter_mut().enumerate() {
        for (r, value) in column.iter_mut().enumerate() {
            *value = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
}

/// Rotates a normal by the matrix's 3×3 part and renormalizes. Exact for
/// rotations and uniform scaling (which CAD exports use); a non-uniform scale
/// would need the inverse transpose.
fn transform_normal(m: &Mat4, n: [f32; 3]) -> [f32; 3] {
    let v: [f32; 3] = std::array::from_fn(|r| m[0][r] * n[0] + m[1][r] * n[1] + m[2][r] * n[2]);
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    v.map(|x| x / len)
}

/// Reads the glTF file and returns the encoded model.
pub fn convert(path: &Path) -> Result<Vec<u8>, String> {
    let (document, buffers, _images) =
        gltf::import(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or("the glTF file has no scene")?;
    let mut parts = Vec::new();
    for node in scene.nodes() {
        collect(&node, &IDENTITY, &buffers, &mut parts)?;
    }
    if parts.is_empty() {
        return Err("the glTF file has no meshes".into());
    }
    Ok(model_format::encode(&parts))
}

fn collect(
    node: &gltf::Node,
    parent: &Mat4,
    buffers: &[gltf::buffer::Data],
    parts: &mut Vec<Part>,
) -> Result<(), String> {
    let transform = mul(parent, &node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        let mut part = Part {
            name: node.name().unwrap_or("").to_string(),
            color: [0.8, 0.8, 0.8, 1.0],
            ..Default::default()
        };
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                return Err(format!("part {}: only triangles are supported", part.name));
            }
            part.color = primitive
                .material()
                .pbr_metallic_roughness()
                .base_color_factor();
            let reader = primitive.reader(|b| Some(&buffers[b.index()]));
            let offset = part.positions.len() as u32;
            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| format!("part {}: no positions", part.name))?
                .collect();
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .ok_or_else(|| format!("part {}: no normals", part.name))?
                .collect();
            if normals.len() != positions.len() {
                return Err(format!("part {}: normal count mismatch", part.name));
            }
            match reader.read_indices() {
                Some(indices) => part.indices.extend(indices.into_u32().map(|i| i + offset)),
                // Non-indexed: every three vertices form a triangle.
                None => part
                    .indices
                    .extend((0..positions.len() as u32).map(|i| i + offset)),
            }
            part.positions.extend(
                positions
                    .into_iter()
                    .map(|p| transform_point(&transform, p)),
            );
            part.normals
                .extend(normals.into_iter().map(|n| transform_normal(&transform, n)));
        }
        parts.push(part);
    }
    for child in node.children() {
        collect(&child, &transform, buffers, parts)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices() {
        // Translation by (1, 2, 3) after a 90° rotation about z.
        let rotate: Mat4 = [
            [0.0, 1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let translate: Mat4 = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [1.0, 2.0, 3.0, 1.0],
        ];
        let m = mul(&translate, &rotate);
        assert_eq!(transform_point(&m, [1.0, 0.0, 0.0]), [1.0, 3.0, 3.0]);
        assert_eq!(transform_normal(&m, [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]);
    }

    /// The model format round trip (the format file lives in the app crate,
    /// which can't run tests on the host).
    #[test]
    fn format_round_trip() {
        let part = Part {
            name: "LC0".into(),
            color: [0.8, 0.8, 0.8, 1.0],
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, -0.5]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2],
        };
        let encoded = model_format::encode(std::slice::from_ref(&part));
        assert_eq!(model_format::decode(&encoded).unwrap(), vec![part]);
        assert!(model_format::decode(b"nope").is_err());
        assert!(model_format::decode(&encoded[..20]).is_err());
    }
}
