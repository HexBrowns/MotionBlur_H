//! 座標変換の計算。原作（ObjectMotionBlur_LK v2.0.2 の src/object/intern/object.cpp）の
//! `ResolveObject` / `ComputeMotionMetrics` / `Retrodict` を写したもの。
//!
//! 原作との違いは入力の作り方だけで、式は変えていない。
//! - 原作は前後のフレームのグループ制御を「近い順の段数」で組にしていた。ここでは呼び出し側が
//!   同じグループ制御どうしを組にした `Pose` を渡す（`links` の長さは前後で同じ）

use std::ops::{Add, Mul, Neg, Sub};

pub const EPSILON: f32 = 1.0e-5;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn splat(v: f32) -> Self {
        Self { x: v, y: v }
    }
    pub fn recip(self) -> Self {
        Self::new(1.0 / self.x, 1.0 / self.y)
    }
    pub fn max_s(self, v: f32) -> Self {
        Self::new(self.x.max(v), self.y.max(v))
    }
    pub fn mul_e(self, o: Vec2) -> Self {
        Self::new(self.x * o.x, self.y * o.y)
    }
    pub fn norm(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
    pub fn ln(self) -> Self {
        Self::new(self.x.ln(), self.y.ln())
    }
    pub fn exp(self) -> Self {
        Self::new(self.x.exp(), self.y.exp())
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}
impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}
impl Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, s: f32) -> Vec2 {
        Vec2::new(self.x * s, self.y * s)
    }
}
impl Neg for Vec2 {
    type Output = Vec2;
    fn neg(self) -> Vec2 {
        Vec2::new(-self.x, -self.y)
    }
}

pub fn lerp(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    a + (b - a) * t
}

/// 1 段ぶんの座標変換（移動 → 回転 → 拡大の順に掛かる）。回転はラジアン。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub position: Vec2,
    pub scale: Vec2,
    pub rotation: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: Vec2::ZERO,
            scale: Vec2::splat(1.0),
            rotation: 0.0,
        }
    }
}

/// ある時刻の姿勢。`links` は外側のグループ制御から順に並び、最後がオブジェクト自身。
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    pub pivot: Vec2,
    pub links: Vec<Transform>,
}

/// 2x3 のアフィン変換。`m[r] = [a, b, t]` で、点 p は `(a*p.x + b*p.y + t)` に写る。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [[f32; 3]; 2],
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    };

    pub fn translation(t: Vec2) -> Self {
        Affine {
            m: [[1.0, 0.0, t.x], [0.0, 1.0, t.y]],
        }
    }
    pub fn rotation(a: f32) -> Self {
        let (s, c) = a.sin_cos();
        Affine {
            m: [[c, -s, 0.0], [s, c, 0.0]],
        }
    }
    pub fn scaling(s: Vec2) -> Self {
        Affine {
            m: [[s.x, 0.0, 0.0], [0.0, s.y, 0.0]],
        }
    }
    /// self * o（o を先に掛ける）
    pub fn then(&self, o: &Affine) -> Affine {
        let a = &self.m;
        let b = &o.m;
        let mut r = [[0.0f32; 3]; 2];
        for i in 0..2 {
            r[i][0] = a[i][0] * b[0][0] + a[i][1] * b[1][0];
            r[i][1] = a[i][0] * b[0][1] + a[i][1] * b[1][1];
            r[i][2] = a[i][0] * b[0][2] + a[i][1] * b[1][2] + a[i][2];
        }
        Affine { m: r }
    }
    pub fn apply(&self, p: Vec2) -> Vec2 {
        Vec2::new(
            self.m[0][0] * p.x + self.m[0][1] * p.y + self.m[0][2],
            self.m[1][0] * p.x + self.m[1][1] * p.y + self.m[1][2],
        )
    }
    pub fn apply_linear(&self, p: Vec2) -> Vec2 {
        Vec2::new(
            self.m[0][0] * p.x + self.m[0][1] * p.y,
            self.m[1][0] * p.x + self.m[1][1] * p.y,
        )
    }
    pub fn inverse(&self) -> Affine {
        let [[a, b, tx], [c, d, ty]] = self.m;
        let det = a * d - b * c;
        let inv = if det.abs() < f32::MIN_POSITIVE { 0.0 } else { 1.0 / det };
        let (ia, ib, ic, id) = (d * inv, -b * inv, -c * inv, a * inv);
        Affine {
            m: [
                [ia, ib, -(ia * tx + ib * ty)],
                [ic, id, -(ic * tx + id * ty)],
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Segment<T> {
    pub origin: T,
    pub extent: T,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Link {
    pub position: Segment<Vec2>,
    pub compensation: Segment<Vec2>,
    pub rotation: Segment<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rig {
    pub pivot: Segment<Vec2>,
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub dimensions: Vec2,
    pub transform: Affine,
    pub rig: Rig,
}

/// 原作の `ResolveObject` の後半。`curr` が今のフレーム、`prev` が 1 つ前（外挿した値のこともある）。
pub fn resolve(dimensions: Vec2, curr: &Pose, prev: &Pose, angle_deg: f32, phase_deg: f32) -> Object {
    assert_eq!(curr.links.len(), prev.links.len(), "前後の段数が違う");
    let amount = (angle_deg / 360.0).max(0.0);
    let phase = phase_deg / angle_deg;

    let center = dimensions * 0.5;
    let pivot_st = curr.pivot + center;
    let pivot_ed = lerp(pivot_st, prev.pivot + center, amount);

    let mut transform = Affine::IDENTITY;
    let mut links = Vec::with_capacity(curr.links.len());

    for (st, ed) in curr.links.iter().zip(prev.links.iter()) {
        transform = transform
            .then(&Affine::translation(st.position))
            .then(&Affine::rotation(st.rotation))
            .then(&Affine::scaling(st.scale));

        let cmp_st = st.scale.recip();
        let cmp_ed = lerp(cmp_st, ed.scale.recip(), amount);
        let cmp_shift = (cmp_ed - cmp_st) * phase;
        let cmp_origin = (cmp_st + cmp_shift).max_s(EPSILON);

        let ed_position = lerp(st.position, ed.position, amount);
        let ed_rotation = st.rotation + (ed.rotation - st.rotation) * amount;

        links.push(Link {
            position: Segment {
                origin: st.position + (ed_position - st.position) * phase,
                extent: ed_position - st.position,
            },
            compensation: Segment {
                origin: cmp_origin,
                extent: (cmp_ed + cmp_shift).max_s(EPSILON) - cmp_origin,
            },
            rotation: Segment {
                origin: st.rotation + (ed_rotation - st.rotation) * phase,
                extent: ed_rotation - st.rotation,
            },
        });
    }

    transform = transform.then(&Affine::translation(-pivot_st));

    Object {
        dimensions,
        transform,
        rig: Rig {
            pivot: Segment {
                origin: pivot_st + (pivot_ed - pivot_st) * phase,
                extent: pivot_ed - pivot_st,
            },
            links,
        },
    }
}

/// 軌跡の外接矩形（`min`, `max`）と、四隅がたどる道のりの最大値。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub min: Vec2,
    pub max: Vec2,
    pub length: f32,
}

/// 原作の `ComputeMotionMetrics`。回転だけ半ステップずれた位置で評価するのも原作どおり。
pub fn metrics(object: &Object, samples: u32) -> Metrics {
    let samples = samples.max(1);
    let step = 1.0 / samples as f32;
    let base_to_world = object.transform.inverse();
    let d = object.dimensions;
    let corners = [Vec2::ZERO, Vec2::new(d.x, 0.0), Vec2::new(0.0, d.y), d];

    let mut min = Vec2::ZERO;
    let mut max = d;
    let mut prev = [Vec2::ZERO; 4];
    let mut paths = [0.0f32; 4];
    let mut len = 0.0f32;

    for i in 0..=samples {
        let t = step * i as f32;
        let mut smp_to_base = base_to_world;

        for link in &object.rig.links {
            let pos = link.position.origin + link.position.extent * t;
            let scale = (link.compensation.origin + link.compensation.extent * t).recip();
            let angle = link.rotation.origin + link.rotation.extent * step * (i as f32 + 0.5);
            let (s, c) = angle.sin_cos();
            let lin = Affine {
                m: [[c * scale.x, -s * scale.y, 0.0], [s * scale.x, c * scale.y, 0.0]],
            };
            // translation += linear * pos; linear *= lin（lin は平行移動を持たない）
            let tr = smp_to_base.apply_linear(pos);
            smp_to_base.m[0][2] += tr.x;
            smp_to_base.m[1][2] += tr.y;
            smp_to_base = smp_to_base.then(&lin);
        }

        let pivot = object.rig.pivot.origin + object.rig.pivot.extent * t;
        let origin = smp_to_base.apply(-pivot);

        let mut curr = [Vec2::ZERO; 4];
        for (j, corner) in corners.iter().enumerate() {
            curr[j] = origin + smp_to_base.apply_linear(*corner);
            min = Vec2::new(min.x.min(curr[j].x), min.y.min(curr[j].y));
            max = Vec2::new(max.x.max(curr[j].x), max.y.max(curr[j].y));
            if i > 0 {
                paths[j] += (curr[j] - prev[j]).norm();
                len = len.max(paths[j]);
            }
        }
        prev = curr;
    }

    Metrics {
        min,
        max,
        length: if len.is_finite() { len } else { f32::MAX },
    }
}

/// 原作の `Retrodict` の重み。`values[0]` が 0 フレーム目、続けて 1, 2 … フレーム目。
fn retrodict_weights(n: usize) -> Vec<f32> {
    let mut w = vec![0.0f32; n];
    w[0] = 1.0;
    let mut c = -((n - 1) as f32);
    for i in 1..n {
        w[0] += 1.0 / i as f32;
        w[i] = c / i as f32;
        c = -c * (n - 1 - i) as f32 / (i + 1) as f32;
    }
    w
}

fn clamp_toward(v: f32, lim: f32) -> f32 {
    v.clamp(lim.min(0.0), lim.max(0.0))
}

fn retro_scalar(values: &[f32], w: &[f32]) -> f32 {
    let est: f32 = values.iter().zip(w).map(|(v, w)| v * w).sum();
    let lim = 3.0 * (values[1] - values[0]);
    values[0] - clamp_toward(values[0] - est, lim)
}

fn retro_vec(values: &[Vec2], w: &[f32]) -> Vec2 {
    let xs: Vec<f32> = values.iter().map(|v| v.x).collect();
    let ys: Vec<f32> = values.iter().map(|v| v.y).collect();
    Vec2::new(retro_scalar(&xs, w), retro_scalar(&ys, w))
}

/// 0, 1, 2 … フレーム目の姿勢から、−1 フレーム目を見積もる（原作の `Extrapolate` の中身）。
/// 拡大率は対数の上で、変化量は 1 フレーム目との差の 3 倍までに抑える。
pub fn retrodict(poses: &[Pose]) -> Pose {
    assert!(poses.len() >= 2);
    let w = retrodict_weights(poses.len());
    let depth = poses[0].links.len();
    let pivots: Vec<Vec2> = poses.iter().map(|p| p.pivot).collect();
    let mut links = Vec::with_capacity(depth);
    for k in 0..depth {
        let pos: Vec<Vec2> = poses.iter().map(|p| p.links[k].position).collect();
        let lsc: Vec<Vec2> = poses.iter().map(|p| p.links[k].scale.ln()).collect();
        let rot: Vec<f32> = poses.iter().map(|p| p.links[k].rotation).collect();
        links.push(Transform {
            position: retro_vec(&pos, &w),
            scale: retro_vec(&lsc, &w).exp().max_s(EPSILON),
            rotation: retro_scalar(&rot, &w),
        });
    }
    Pose {
        pivot: retro_vec(&pivots, &w),
        links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj_pose(x: f32, y: f32) -> Pose {
        Pose {
            pivot: Vec2::ZERO,
            links: vec![Transform {
                position: Vec2::new(x, y),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn weights_match_original() {
        // 原作の式を手で展開した値。2 点は 2v0 - v1、3 点は 2.5v0 - 2v1 + 0.5v2
        assert_eq!(retrodict_weights(2), vec![2.0, -1.0]);
        assert_eq!(retrodict_weights(3), vec![2.5, -2.0, 0.5]);
    }

    #[test]
    fn retrodict_linear_motion() {
        // 0→1→2 フレームで x が 10 ずつ減る（右から入ってくる）なら、−1 フレームは +10
        let p = retrodict(&[obj_pose(100.0, 0.0), obj_pose(90.0, 0.0), obj_pose(80.0, 0.0)]);
        assert!((p.links[0].position.x - 110.0).abs() < 1e-4, "{:?}", p);
    }

    #[test]
    fn no_motion_gives_identity_rig() {
        let pose = obj_pose(50.0, 20.0);
        let o = resolve(Vec2::new(100.0, 40.0), &pose, &pose, 360.0, -180.0);
        assert_eq!(o.rig.links[0].position.extent, Vec2::ZERO);
        let m = metrics(&o, 16);
        assert!(m.length.abs() < 1e-3, "{:?}", m);
        assert!((m.min - Vec2::ZERO).norm() < 1e-3 && (m.max - Vec2::new(100.0, 40.0)).norm() < 1e-3);
    }

    #[test]
    fn translation_extends_box_by_motion() {
        // 1 フレームで右へ 60 動いた。Angle 360 / Phase -180 なら前後 30 ずつ伸びる
        let curr = obj_pose(60.0, 0.0);
        let prev = obj_pose(0.0, 0.0);
        let o = resolve(Vec2::new(100.0, 40.0), &curr, &prev, 360.0, -180.0);
        let m = metrics(&o, 64);
        assert!((m.length - 60.0).abs() < 0.5, "{:?}", m);
        assert!((m.min.x + 30.0).abs() < 0.5 && (m.max.x - 130.0).abs() < 0.5, "{:?}", m);
    }

    #[test]
    fn affine_inverse_roundtrip() {
        let a = Affine::translation(Vec2::new(3.0, -2.0))
            .then(&Affine::rotation(0.7))
            .then(&Affine::scaling(Vec2::new(2.0, 0.5)));
        let p = Vec2::new(5.0, 7.0);
        let q = a.inverse().apply(a.apply(p));
        assert!((q - p).norm() < 1e-4);
    }
}
