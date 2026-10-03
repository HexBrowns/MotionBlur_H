//! シェーダーに渡す定数。shaders/blur.hlsl の cbuffer と同じ並び（すべて float4 単位なので詰め物は要らない）。

use crate::pose::{Object, Vec2};

pub const MAX_LINKS: usize = 16;

pub static SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/blur.cso"));

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub tr0: [f32; 4],
    pub tr1: [f32; 4],
    pub origin_texel: [f32; 4],
    pub mix_falloff: [f32; 4],
    pub misc: [f32; 4],
    pub pivot: [f32; 4],
    pub counts: [f32; 4],
    pub link_pos: [[f32; 4]; MAX_LINKS],
    pub link_cmp: [[f32; 4]; MAX_LINKS],
    pub link_rot: [[f32; 4]; MAX_LINKS],
}

pub struct Shading {
    /// Compositing::Mix（0〜1）
    pub mix: f32,
    /// Shutter::Falloff::Amount（0〜1）
    pub falloff: f32,
    /// 0 = Trailing / 1 = Leading / 2 = Symmetric
    pub edge: u32,
    pub samples: u32,
    pub map_width: u32,
    pub alpha_hashed: bool,
}

/// 原作 Apply の renderer::Parameter を組み立てる部分。`origin` は出力画像の左上が元画像のどこに当たるか。
pub fn build(object: &Object, origin: Vec2, resolution: Vec2, s: &Shading) -> Params {
    let t = &object.transform.m;
    let mix = s.mix.clamp(0.0, 1.0) * 2.0;
    let edge = s.falloff.clamp(0.0, 1.0).max(crate::pose::EPSILON);
    let eps = crate::pose::EPSILON;

    let mut p = Params {
        tr0: [t[0][0], t[0][1], t[0][2], 0.0],
        tr1: [t[1][0], t[1][1], t[1][2], 0.0],
        origin_texel: [
            origin.x,
            origin.y,
            1.0 / object.dimensions.x,
            1.0 / object.dimensions.y,
        ],
        mix_falloff: [
            (2.0 - mix).min(1.0),
            mix.min(1.0),
            if s.edge == 0 { eps } else { edge },
            if s.edge == 1 { eps } else { edge },
        ],
        misc: [
            s.samples as f32,
            0.5 / s.map_width.max(1) as f32,
            if s.alpha_hashed { 1.0 } else { 0.0 },
            resolution.x * resolution.y,
        ],
        pivot: [
            object.rig.pivot.origin.x,
            object.rig.pivot.origin.y,
            object.rig.pivot.extent.x,
            object.rig.pivot.extent.y,
        ],
        counts: [object.rig.links.len().min(MAX_LINKS) as f32, 0.0, 0.0, 0.0],
        link_pos: [[0.0; 4]; MAX_LINKS],
        link_cmp: [[0.0; 4]; MAX_LINKS],
        link_rot: [[0.0; 4]; MAX_LINKS],
    };
    for (j, l) in object.rig.links.iter().take(MAX_LINKS).enumerate() {
        p.link_pos[j] = [
            l.position.origin.x,
            l.position.origin.y,
            l.position.extent.x,
            l.position.extent.y,
        ];
        p.link_cmp[j] = [
            l.compensation.origin.x,
            l.compensation.origin.y,
            l.compensation.extent.x,
            l.compensation.extent.y,
        ];
        p.link_rot[j] = [l.rotation.origin, l.rotation.extent, 0.0, 0.0];
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_size_is_float4_aligned() {
        assert_eq!(std::mem::size_of::<Params>() % 16, 0);
        assert_eq!(std::mem::size_of::<Params>(), (7 + 3 * MAX_LINKS) * 16);
    }

    #[test]
    fn shader_is_embedded() {
        // DXBC の先頭 4 バイト
        assert_eq!(&SHADER[0..4], b"DXBC");
    }
}
