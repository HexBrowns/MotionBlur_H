//! SceneMotionBlur_H（フィルタ効果 / フィルタオブジェクト）。原作 SceneMotionBlur_K v2.0.2（Korarei、MIT）の Rust 移植。
//!
//! 連続するフレームのオプティカルフロー（NVIDIA Optical Flow）から、画面全体にモーションブラーを掛ける。
//! 処理の手順とシェーダーは原作のまま（gpu.rs / shaders/scene/）。設定項目の範囲・初期値・並びも原作と同じで、
//! 名前は日本語にした（ルール au2-conventions「ユーザー向け文言」）。
//!
//! 原作から変えたところ:
//! - 前のフレームの画像は、CPU の 8bit に読み出して次のフレームで書き戻す代わりに、GPU のテクスチャへコピーして持つ
//! - NVOF のセッションは (幅, 高さ, プリセット) ごとの共有ではなく、エフェクトのインスタンスごとに持つ

pub mod gpu;
pub mod nvof;

use aviutl2::filter::{
    FilterConfigItemSliceExt, FilterConfigItems, FilterPlugin, FilterPluginTable, FilterProcVideo, ImageResource,
};
use aviutl2::generic::EditState;
use aviutl2::AnyResult;

use crate::filter::{Edge, LayerReference};
use gpu::{Borrowed, FrameTexture, SessionKey};

#[derive(aviutl2::filter::FilterConfigSelectItems, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    #[item(name = "高品質")]
    Slow,
    #[item(name = "標準")]
    Medium,
    #[item(name = "高速")]
    Fast,
}

#[derive(aviutl2::filter::FilterConfigSelectItems, Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    #[item(name = "処理結果")]
    Processed,
    #[item(name = "オプティカルフロー")]
    Flow,
    #[item(name = "最近傍伝播フロー")]
    Nearest,
    #[item(name = "異なる動きの伝播フロー")]
    Distinct,
}

#[aviutl2::filter::filter_config_items]
#[derive(Debug, Clone)]
pub struct SceneConfig {
    #[group(name = "シャッター", opened = true)]
    shutter: group! {
        #[track(name = "シャッター角度", range = 0.0..=720.0, step = 0.01, default = 180.0)]
        angle: f64,
        #[separator(name = "減衰")]
        #[select(name = "減衰位置", items = Edge, default = Edge::Symmetric)]
        edge: Edge,
        #[track(name = "減衰量", range = 0.0..=100.0, step = 0.01, default = 2.0)]
        falloff: f64,
    },
    #[group(name = "サンプリング", opened = false)]
    sampling: group! {
        #[separator(name = "プレビュー")]
        #[track(name = "プレビュー最大サンプル数", range = 2..=4096, step = 1.0, default = 128)]
        viewport_limit: u32,
        #[separator(name = "出力")]
        #[track(name = "出力最大サンプル数", range = 2..=4096, step = 1.0, default = 512)]
        render_limit: u32,
    },
    #[group(name = "合成", opened = false)]
    compositing: group! {
        #[track(name = "ミックス", range = 0.0..=100.0, step = 0.01, default = 100.0)]
        mix: f64,
    },
    #[group(name = "深度", opened = false)]
    depth: group! {
        #[track(name = "深度マップレイヤー", range = -100..=100, step = 1.0, default = 0, zero_display = "---")]
        depth_layer: i32,
    },
    #[group(name = "その他", opened = false)]
    options: group! {
        #[select(name = "プリセット", items = Preset, default = Preset::Slow)]
        preset: Preset,
        #[select(name = "レイヤー参照", items = LayerReference, default = LayerReference::Absolute)]
        layer_reference: LayerReference,
        #[select(name = "表示", items = View, default = View::Processed)]
        view: View,
    },
}

/// 前後のフレーム 1 枚分
#[derive(Default)]
struct Slot {
    tex: Option<FrameTexture>,
    /// オブジェクト基準のフレーム
    frame: Option<i64>,
    section: i64,
}

/// エフェクトのインスタンスごとの状態（原作 Instance）
pub struct Instance {
    key: Option<SessionKey>,
    session: Option<gpu::Session>,
    prev: Slot,
    curr: Slot,
}

// GPU のオブジェクトを持つが、触るのは描画中（ユーザーデータの書き込みロックの中）だけ
unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

impl aviutl2::filter::FilterUserdata for Instance {
    fn new(_effect_id: i64) -> Self {
        Instance {
            key: None,
            session: None,
            prev: Slot::default(),
            curr: Slot::default(),
        }
    }
}

#[aviutl2::plugin(FilterPlugin)]
pub struct SceneMotionBlur;

impl FilterPlugin for SceneMotionBlur {
    type Userdata = Instance;

    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        Ok(Self)
    }

    fn plugin_info(&self) -> FilterPluginTable {
        FilterPluginTable {
            name: "SceneMotionBlur_H".to_string(),
            label: Some("HexScript".to_string()),
            information: format!(
                "SceneMotionBlur_H v{} by HexBrowns (fork of SceneMotionBlur_K by Korarei)",
                env!("CARGO_PKG_VERSION")
            ),
            flags: aviutl2::bitflag!(aviutl2::filter::FilterPluginFlags { video: true, filter: true }),
            config_items: SceneConfig::to_config_items(),
        }
    }

    fn proc_video(
        &self,
        config: &[aviutl2::filter::FilterConfigItem],
        video: &mut FilterProcVideo<Self::Userdata>,
    ) -> AnyResult<()> {
        let cfg: SceneConfig = config.to_struct();
        if let Err(e) = apply(&cfg, video) {
            tracing::error!("SceneMotionBlur_H: {e}");
        }
        Ok(())
    }
}

const EPSILON: f32 = 1.0e-5;
const DEPTH: &str = "smb_h_depth";

fn apply(cfg: &SceneConfig, video: &mut FilterProcVideo<Instance>) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = (video.video_object.width, video.video_object.height);
    if w == 0 || h == 0 {
        return Ok(());
    }
    if video.video_object.num != Some(1) {
        return Err("This effect only supports a single object".into());
    }
    let angle = cfg.angle as f32;
    if angle <= EPSILON {
        return Ok(());
    }

    let object_frame = video.object.frame as i64;
    let frame = video.object.frame_s as i64 + object_frame;
    let effect_layer = video.object.effect_layer;

    // 区間（中間点で分けた区切り）。区間が変わったら前のフレームを使わない（場面転換の扱い。原作 README）
    let section = {
        let rs = video.read_section();
        let object = rs
            .find_object_after(effect_layer as usize, frame.max(0) as usize)?
            .ok_or_else(|| format!("No object exists at layer {}, frame {}", effect_layer + 1, frame))?;
        let n = rs.get_object_section_num(object)?;
        let mut section = 0i64;
        while (section as usize) < n && rs.get_object_section_frame(object, section as usize)? as i64 <= frame {
            section += 1;
        }
        section
    };

    // 深度マップ（無ければ 1x1 の白）
    {
        let mut layer = cfg.depth_layer as i64;
        if cfg.layer_reference == LayerReference::Relative {
            layer += effect_layer as i64 + 1;
        }
        layer -= 1;
        let res = ImageResource::Resource(DEPTH.to_string());
        if layer < 0 || layer == video.object.layer as i64 {
            video.create_image_resource(&res, &[255u8, 255, 255, 255], 1, 1)?;
        } else if video.get_image_object(layer as u32, 0.0).is_none() {
            return Err(format!("No object exists at layer {}, frame {}", layer + 1, frame).into());
        } else {
            video.copy_image_resource(
                &ImageResource::Layer {
                    layer: layer as u32,
                    apply_additional_effects: true,
                },
                &res,
            )?;
        }
    }

    let dst = video.get_image_texture2d().map(std::mem::ManuallyDrop::new).ok_or("Failed to get 'ID3D11Texture2D' pointers")?;
    let depth_ptr = video.get_image_resource_texture2d(&ImageResource::Resource(DEPTH.to_string()))?;
    let depth = unsafe { Borrowed::from_raw(depth_ptr) }.ok_or("Failed to get 'ID3D11Texture2D' pointers")?;

    let key = SessionKey {
        w,
        h,
        preset: match cfg.preset {
            Preset::Slow => 0,
            Preset::Medium => 1,
            Preset::Fast => 2,
        },
    };

    let exporting = matches!(crate::EDIT_HANDLE.get_edit_state(), Ok(EditState::Save));
    let sample_limit = if exporting { cfg.render_limit } else { cfg.viewport_limit } as i32;
    let origin_is_start = video.object.origin_frame == video.object.frame_s;

    let mut guard = video.userdata.write();
    let inst = &mut *guard;

    if inst.key != Some(key) {
        inst.session = None;
        inst.prev = Slot::default();
        inst.curr = Slot::default();
        inst.key = Some(key);
    }

    if inst.curr.frame.is_some() && inst.curr.frame != Some(object_frame) {
        std::mem::swap(&mut inst.prev, &mut inst.curr);
    }

    if inst.curr.tex.is_none() {
        inst.curr.tex = Some(FrameTexture::like(&dst)?);
    }
    inst.curr.tex.as_ref().unwrap().copy_from(&dst)?;
    inst.curr.frame = Some(object_frame);
    inst.curr.section = section;

    if origin_is_start || (inst.prev.frame.is_some() && inst.prev.section != inst.curr.section) {
        inst.prev.frame = None;
    }

    let mut scale = 0.0f32;
    let mut should_use_temporal_hints = false;
    let use_prev = match (inst.prev.frame, inst.prev.tex.as_ref()) {
        (Some(pf), Some(_)) => {
            let df = object_frame - pf;
            if df == 1 {
                scale = 1.0;
                should_use_temporal_hints = true;
            } else if df != 0 {
                scale = 1.0 / df as f32;
            }
            true
        }
        _ => false,
    };

    let amount = (angle / 360.0).max(0.0);
    let params = gpu::Params {
        key,
        should_use_temporal_hints,
        scale: scale * amount,
        falloff_edge: match cfg.edge {
            Edge::Trailing => 0,
            Edge::Leading => 1,
            Edge::Symmetric => 2,
        },
        falloff_amount: (cfg.falloff as f32 * 0.01).clamp(0.0, 1.0),
        sample_limit,
        mix: (cfg.mix as f32 * 0.01).clamp(0.0, 1.0),
        view_mode: match cfg.view {
            View::Processed => 0,
            View::Flow => 1,
            View::Nearest => 2,
            View::Distinct => 3,
        },
    };

    let curr = inst.curr.tex.as_ref().unwrap();
    let target = if use_prev { inst.prev.tex.as_ref().unwrap() } else { curr };
    gpu::render(&mut inst.session, &dst, curr, target, &depth.0, &params)
}
