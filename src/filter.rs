//! ObjectMotionBlur_H（フィルタ効果）。原作 ObjectMotionBlur_LK v2.0.2（Korarei、MIT）の Rust 移植と修正。
//!
//! 設定項目の範囲・初期値は原作と同じ。名前は v0.2.0 で日本語にした（ユーザーの指示。言い換えず、定着した用語をそのまま使う）。
//! 原作の名前との対応は README.md の表。
//! 原作から変えたところ:
//! - 前のフレームの標準描画とグループ制御の値は、記録に頼らず本体からその場で取る（`live_at`）。
//!   記録するのは、他の効果が動かした分（obj.ox など）だけ（cache.rs）
//! - 掛かっているグループ制御は本体に聞く（`get_group_control_objects`）。原作は上のレイヤーを数えて探しており、
//!   範囲を 1 レイヤー広く取り、前後のフレームでグループ制御の数が変わると別のグループ制御どうしを組にしていた
//! - 保存する 1・2 フレーム目の値（出だしの外挿用）を、オブジェクトごとの枠に分けた（`Store`）。
//!   原作は個別オブジェクトの番号だけで枠を選んでおり、グループ制御の配下では全員が 1 枠を共有した

use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use aviutl2::filter::{
    BlendStateMode, FilterConfigDataHandle, FilterConfigItemSliceExt, FilterConfigItems,
    FilterPlugin, FilterPluginTable, FilterProcVideo, ImageResource, SamplerMode,
};
use aviutl2::generic::{EditState, EffectHandle};
use aviutl2::AnyResult;

use crate::cache::{self, Key, Lookup, ParamPart, RecordInfo};
use crate::log_once;
use crate::pose::{self, Pose, Transform, Vec2, EPSILON};
use crate::render;

#[derive(aviutl2::filter::FilterConfigSelectItems, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    #[item(name = "後端")]
    Trailing,
    #[item(name = "前端")]
    Leading,
    #[item(name = "両端")]
    Symmetric,
}

#[derive(aviutl2::filter::FilterConfigSelectItems, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TintSource {
    #[item(name = "画像")]
    Image,
    #[item(name = "レイヤー")]
    Layer,
}

#[derive(aviutl2::filter::FilterConfigSelectItems, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaMode {
    #[item(name = "アルファブレンド")]
    Blending,
    #[item(name = "ディザ")]
    Hashed,
}

#[derive(aviutl2::filter::FilterConfigSelectItems, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extrapolation {
    #[item(name = "なし")]
    None,
    #[item(name = "線形")]
    Linear,
    #[item(name = "二次")]
    Quadratic,
}

#[derive(aviutl2::filter::FilterConfigSelectItems, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerReference {
    #[item(name = "絶対")]
    Absolute,
    #[item(name = "相対")]
    Relative,
}

/// 出だしの外挿のために保存する、1・2 フレーム目の「効果が動かした分」。オブジェクトごとに 1 枠。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StoreEntry {
    pub used: u32,
    pub layer: u32,
    pub frame_s: u32,
    pub index: u32,
    /// 0 フレーム目の標準描画・グループ制御・効果の分をまとめたハッシュ。読み戻すときの照合に使う
    pub h0: u64,
    pub base_hash: [u64; 2],
    pub param: [ParamPart; 2],
    pub has: [u32; 2],
}

pub const STORE_SLOTS: usize = 128;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Store {
    pub entries: [StoreEntry; STORE_SLOTS],
}

impl Default for Store {
    fn default() -> Self {
        Self {
            entries: [StoreEntry::default(); STORE_SLOTS],
        }
    }
}

#[aviutl2::filter::filter_config_items]
#[derive(Debug, Clone)]
pub struct Config {
    #[group(name = "シャッター", opened = true)]
    shutter: group! {
        #[track(name = "シャッター角度", range = 0.0..=720.0, step = 0.01, default = 180.0)]
        angle: f64,
        #[track(name = "シャッター位相", range = -360.0..=360.0, step = 0.01, default = -90.0)]
        phase: f64,
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
    #[group(name = "ティント", opened = false)]
    tint: group! {
        #[select(name = "ティント参照元", items = TintSource, default = TintSource::Image)]
        tint_source: TintSource,
        #[hide(tint_source != TintSource::Image)]
        #[file(name = "グラデーションマップ画像", filters = {
            "画像ファイル" => ["bmp", "png", "jpg", "jpeg", "tif", "tiff", "webp"],
        })]
        tint_image: Option<PathBuf>,
        #[hide(tint_source != TintSource::Layer)]
        #[track(name = "グラデーションマップレイヤー", range = -100..=100, step = 1.0, default = 0, zero_display = "---")]
        tint_layer: i32,
    },
    #[group(name = "合成", opened = false)]
    compositing: group! {
        #[track(name = "ミックス", range = 0.0..=100.0, step = 0.01, default = 100.0)]
        mix: f64,
        #[select(name = "アルファモード", items = AlphaMode, default = AlphaMode::Blending)]
        alpha_mode: AlphaMode,
    },
    #[group(name = "その他", opened = false)]
    options: group! {
        #[select(name = "外挿", items = Extrapolation, default = Extrapolation::Quadratic)]
        extrapolation: Extrapolation,
        #[select(name = "レイヤー参照", items = LayerReference, default = LayerReference::Absolute)]
        layer_reference: LayerReference,
        #[check(name = "リサイズ", default = true)]
        resize: bool,
        #[check(name = "診断ログ", default = false)]
        diagnostics: bool,
        #[data(name = "Internal::Store")]
        store: FilterConfigDataHandle<Store>,
    },
}

#[aviutl2::plugin(FilterPlugin)]
pub struct ObjectMotionBlur;

/// 負の拡大率の警告を出した回数（ログを埋めないため最初の数回だけ）
static NEGATIVE_SCALE_WARNED: AtomicU32 = AtomicU32::new(0);

impl FilterPlugin for ObjectMotionBlur {
    type Userdata = ();

    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        Ok(Self)
    }

    fn plugin_info(&self) -> FilterPluginTable {
        FilterPluginTable {
            name: "ObjectMotionBlur_H".to_string(),
            label: Some("HexScript".to_string()),
            information: format!(
                "ObjectMotionBlur_H v{} by HexBrowns (fork of ObjectMotionBlur_LK by Korarei)",
                env!("CARGO_PKG_VERSION")
            ),
            flags: aviutl2::bitflag!(aviutl2::filter::FilterPluginFlags { video: true }),
            config_items: Config::to_config_items(),
        }
    }

    fn proc_video(
        &self,
        config: &[aviutl2::filter::FilterConfigItem],
        video: &mut FilterProcVideo<Self::Userdata>,
    ) -> AnyResult<()> {
        let cfg: Config = config.to_struct();
        // 失敗は毎フレーム起きるので、ログは原因ごとに初回だけ。止め方は Err を返したときと同じにする:
        // 以降のフィルタと出力を止め、途中で変えた param（中心）は本体へ書き戻さない（Err のときは aviutl2-rs が書き戻さない）
        let saved = video.param.clone();
        if let Err(e) = apply(&cfg, video) {
            let message = e.to_string();
            if log_once::first(&format!("omb:proc:{message}")) {
                tracing::error!("ObjectMotionBlur_H: {message}{}", log_once::SUFFIX);
            }
            video.param = saved;
            video.prevent_post_effect();
        }
        Ok(())
    }
}

/// 本体の画像の一辺の上限（`obj.getinfo("image_max")` と同じ。ルール au2-conventions「API 重要事項」）
pub const MAX_IMAGE_SIZE: u32 = 16384;

/// 使い回す 0 埋めのバッファを持ち続ける上限。これより大きく要ったときは、使い終わったら手放す（256 MiB = 8192x8192）
const KEEP_ZERO_BYTES: usize = 256 << 20;

/// 大きさを変えるときに `set_image_data` へ渡す 0 埋めのバッファ（フレームをまたいで使い回す。中身は 0 のまま書き換えない）
static ZEROS: parking_lot::Mutex<Vec<u8>> = parking_lot::Mutex::new(Vec::new());

/// 使い回している 0 埋めのバッファを手放す（「キャッシュを破棄」）。貸し出し中なら何もしない
pub fn release_zeros() {
    if let Some(mut pool) = ZEROS.try_lock() {
        *pool = Vec::new();
    }
}

/// `len` バイトの 0 を `f` に貸す。確保できなければ `None`。
///
/// `vec![0u8; n]` は確保に失敗すると abort して本体ごと落ちる（リサイズで一辺 16384 まで広がると 1 GiB）ので、
/// 失敗を返せる `try_reserve_exact` で確保する。別のスレッドが貸し出し中なら、待たずにその場で確保して捨てる。
fn with_zeros<R>(len: usize, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    match ZEROS.try_lock() {
        Some(mut pool) => lend_zeros(&mut pool, len, KEEP_ZERO_BYTES, f),
        None => lend_zeros(&mut Vec::new(), len, 0, f),
    }
}

/// `pool` を `len` バイト以上の 0 にして、先頭 `len` バイトを `f` に貸す。貸した後に `keep` バイトより大きければ手放す。
fn lend_zeros<R>(pool: &mut Vec<u8>, len: usize, keep: usize, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    if pool.len() < len {
        // 足りない分だけ足す。前からある分も 0 のまま（貸すのは共有参照だけ）
        pool.try_reserve_exact(len - pool.len()).ok()?;
        pool.resize(len, 0);
    }
    let result = f(&pool[..len]);
    if pool.len() > keep {
        *pool = Vec::new();
    }
    Some(result)
}

struct GroupRef {
    effect: EffectHandle,
    start: i64,
    end: i64,
}

/// ある時刻の標準描画とグループ制御の値（効果が動かした分は含まない）。
struct Live {
    groups: Vec<Transform>,
    base: aviutl2::filter::ObjectImageParam,
}

fn warn_negative_scale() {
    if NEGATIVE_SCALE_WARNED.fetch_add(1, Ordering::Relaxed) < 3 {
        tracing::warn!("Negative scaling is not supported");
    }
}

/// 現在のオブジェクトに掛かっているグループ制御を、外側から順に返す。
fn collect_groups(video: &mut FilterProcVideo<()>) -> Vec<GroupRef> {
    let handles = video.get_group_control_objects();
    let rs = video.read_section();
    let mut groups = Vec::with_capacity(handles.len());
    for h in handles.iter().rev() {
        let Ok(effect) = rs.find_effect(*h, "グループ制御", 0) else {
            if log_once::first("omb:group_effect_missing") {
                tracing::warn!("グループ制御のエフェクトが見つからない{}", log_once::SUFFIX);
            }
            continue;
        };
        let Ok(lf) = rs.get_object_layer_frame(*h) else {
            continue;
        };
        groups.push(GroupRef {
            effect,
            start: lf.start as i64,
            end: lf.end as i64,
        });
    }
    groups
}

/// オブジェクト基準のフレーム `local` の標準描画とグループ制御の値を本体から取る。
/// グループ制御の値は、そのグループ制御の範囲に収めたフレームで読む（途中から掛かり始めたグループ制御で跳ばない）。
fn live_at(
    video: &mut FilterProcVideo<()>,
    groups: &[GroupRef],
    local: i64,
    cur_local: i64,
    spf: f64,
) -> AnyResult<Live> {
    let offset = (local - cur_local) as f64 * spf;
    // 対象を明示する。グループ制御に掛けた効果で None（今のオブジェクト）にオフセットを付けて取ると、
    // 配下のオブジェクトでない値が返った（1 周目の実機: ぶれが四角の X 座標ぶん伸びた。issue の 2 周目の節）
    let target = video.get_image_object(video.object.layer, 0.0);
    let base = video.get_output_image_param(target, offset)?;
    let scene_frame = video.object.frame_s as i64 + local;
    let rs = video.read_section();
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let f = scene_frame.clamp(g.start, g.end) as f64;
        let x = rs.get_effect_track_value(g.effect, "X", f).unwrap_or(0.0) as f32;
        let y = rs.get_effect_track_value(g.effect, "Y", f).unwrap_or(0.0) as f32;
        let s = rs.get_effect_track_value(g.effect, "拡大率", f).unwrap_or(100.0) as f32;
        let rz = rs.get_effect_track_value(g.effect, "Z軸回転", f).unwrap_or(0.0) as f32;
        if s < 0.0 {
            warn_negative_scale();
        }
        out.push(Transform {
            position: Vec2::new(x, y),
            scale: Vec2::splat((s * 0.01).max(EPSILON)),
            rotation: rz.to_radians(),
        });
    }
    Ok(Live { groups: out, base })
}

fn param_of(video: &FilterProcVideo<()>) -> ParamPart {
    let p = &video.param;
    ParamPart {
        x: p.x,
        y: p.y,
        cx: p.cx,
        cy: p.cy,
        rz: p.rz,
        sx: p.sx,
        sy: p.sy,
    }
}

fn combine(live: &Live, p: &ParamPart) -> Pose {
    let b = &live.base;
    let scale = Vec2::new(b.sx * p.sx, b.sy * p.sy);
    if scale.x < 0.0 || scale.y < 0.0 {
        warn_negative_scale();
    }
    let mut links = live.groups.clone();
    links.push(Transform {
        position: Vec2::new(b.x + p.x, b.y + p.y),
        scale: scale.max_s(EPSILON),
        rotation: (b.rz + p.rz).to_radians(),
    });
    Pose {
        pivot: Vec2::new(b.cx + p.cx, b.cy + p.cy),
        links,
    }
}

/// FNV-1a。値は 1/64 px（角度は 1/64 度）に丸めてから混ぜ、浮動小数の末尾の揺れで外れないようにする。
struct Hasher(u64);
impl Hasher {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn f(&mut self, v: f32) {
        let q = (v as f64 * 64.0).round() as i64;
        for b in q.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn u(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

fn base_hash(live: &Live) -> u64 {
    let mut h = Hasher::new();
    for g in &live.groups {
        h.f(g.position.x);
        h.f(g.position.y);
        h.f(g.scale.x * 100.0);
        h.f(g.rotation.to_degrees());
    }
    let b = &live.base;
    for v in [b.x, b.y, b.cx, b.cy, b.rz, b.sx * 100.0, b.sy * 100.0] {
        h.f(v);
    }
    h.0
}

fn frame_hash(base: u64, p: &ParamPart) -> u64 {
    let mut h = Hasher::new();
    h.u(base);
    for v in [p.x, p.y, p.cx, p.cy, p.rz, p.sx * 100.0, p.sy * 100.0] {
        h.f(v);
    }
    h.0
}

/// 前のフレームの姿勢をどこから得たか（Diagnostics 用。中身は Debug 表示でだけ読む）
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum PrevSource {
    /// 記録した「効果の分」と、本体から取った標準描画・グループ制御
    Cached,
    /// 記録が無い・古いので、効果の分は今と同じとみなした（標準描画・グループ制御の動きだけでぼかす）
    LiveOnly,
    /// 出だしのフレームで、後のフレームから外挿した
    Extrapolated { cached: u32, stored: u32, assumed: u32 },
    /// 前のフレームが無い（ぼかさない）
    None,
}

fn store_lookup(store: &Store, layer: u32, frame_s: u32, index: u32, h0: u64) -> Option<StoreEntry> {
    store
        .entries
        .iter()
        .find(|e| e.used != 0 && e.layer == layer && e.frame_s == frame_s && e.index == index && e.h0 == h0)
        .copied()
}

static STORE_CLOCK: AtomicU32 = AtomicU32::new(1);

fn store_write(handle: &FilterConfigDataHandle<Store>, layer: u32, frame_s: u32, index: u32, h0: u64, k: usize, base: u64, param: ParamPart) {
    {
        let s = handle.read();
        if let Some(e) = s.entries.iter().find(|e| e.used != 0 && e.layer == layer && e.frame_s == frame_s && e.index == index && e.h0 == h0) {
            if e.has[k] != 0 && e.base_hash[k] == base && e.param[k].approx_eq(&param) {
                return; // 同じ値が入っている
            }
        }
    }
    let mut s = handle.write();
    let clock = STORE_CLOCK.fetch_add(1, Ordering::Relaxed).max(1);
    let slot = s
        .entries
        .iter()
        .position(|e| e.used != 0 && e.layer == layer && e.frame_s == frame_s && e.index == index)
        .or_else(|| s.entries.iter().position(|e| e.used == 0))
        .unwrap_or_else(|| {
            s.entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .map(|(i, _)| i)
                .unwrap_or(0)
        });
    let e = &mut s.entries[slot];
    if !(e.used != 0 && e.layer == layer && e.frame_s == frame_s && e.index == index && e.h0 == h0) {
        *e = StoreEntry {
            layer,
            frame_s,
            index,
            h0,
            ..Default::default()
        };
    }
    e.used = clock;
    e.base_hash[k] = base;
    e.param[k] = param;
    e.has[k] = 1;
}

fn fmt_param(p: &aviutl2::filter::ObjectImageParam) -> String {
    format!("({:.1},{:.1} rz{:.1} s{:.3},{:.3} c{:.1},{:.1})", p.x, p.y, p.rz, p.sx, p.sy, p.cx, p.cy)
}

/// Diagnostics 用の調べ。グループ制御に掛けたとき、何がどの値を返すかを 1 行に出す（v0.1.1 で足した。
/// 1 周目の実機で、グループ制御経由のときだけ前フレームの標準描画がおかしかったため）。
fn probe(video: &mut FilterProcVideo<()>, groups: &[GroupRef], local: i64, spf: f64) {
    let none0 = video.get_output_image_param(None, 0.0).map(|p| fmt_param(&p)).unwrap_or_else(|e| format!("err:{e}"));
    let none1 = video.get_output_image_param(None, -spf).map(|p| fmt_param(&p)).unwrap_or_else(|e| format!("err:{e}"));
    let own = video.object.handle;
    let own0 = video.get_output_image_param(Some(own), 0.0).map(|p| fmt_param(&p)).unwrap_or_else(|e| format!("err:{e}"));
    let own1 = video.get_output_image_param(Some(own), -spf).map(|p| fmt_param(&p)).unwrap_or_else(|e| format!("err:{e}"));
    let img = video.get_image_object(video.object.layer, 0.0);
    let (img0, img1) = match img {
        Some(h) => (
            video.get_output_image_param(Some(h), 0.0).map(|p| fmt_param(&p)).unwrap_or_else(|e| format!("err:{e}")),
            video.get_output_image_param(Some(h), -spf).map(|p| fmt_param(&p)).unwrap_or_else(|e| format!("err:{e}")),
        ),
        None => ("none".to_string(), "none".to_string()),
    };
    let p = video.param.clone();
    let scene_frame = video.object.frame_s as i64 + local;
    let rs = video.read_section();
    let mut gs = String::new();
    for g in groups {
        let xs = rs.get_effect_track_value(g.effect, "X", scene_frame as f64).unwrap_or(f64::NAN);
        let xr = rs.get_effect_track_value(g.effect, "X", (scene_frame - g.start) as f64).unwrap_or(f64::NAN);
        let ys = rs.get_effect_track_value(g.effect, "Y", scene_frame as f64).unwrap_or(f64::NAN);
        gs.push_str(&format!(" [g{}-{} Xscene={xs:.1} Xrel={xr:.1} Yscene={ys:.1}]", g.start, g.end));
    }
    tracing::info!(
        "[OMB_H probe] layer={} elayer={} frame={} local={} id={} own==img:{} none0={} none-1={} own0={} own-1={} img0={} img-1={} param={}{}",
        video.object.layer + 1,
        video.object.effect_layer + 1,
        video.object.origin_frame,
        local,
        video.object.id,
        img == Some(own),
        none0,
        none1,
        own0,
        own1,
        img0,
        img1,
        fmt_param(&p),
        gs
    );
}

fn apply(cfg: &Config, video: &mut FilterProcVideo<()>) -> AnyResult<()> {
    let angle = cfg.angle as f32;
    if angle < EPSILON {
        return Ok(());
    }
    let (w, h) = (video.video_object.width, video.video_object.height);
    if w == 0 || h == 0 {
        return Ok(());
    }
    if let Some(num) = video.video_object.num {
        if video.video_object.index >= num {
            if log_once::first("omb:object_count") {
                tracing::warn!("Unable to determine object count{}", log_once::SUFFIX);
            }
            return Ok(());
        }
    }

    let fr = video.scene.frame_rate;
    let spf = *fr.denom() as f64 / (*fr.numer()).max(1) as f64;
    let total = video.object.frame_total.max(1) as i64;
    let local = (video.object.frame as i64).clamp(0, total - 1);
    let origin = video.object.origin_frame;
    let frame_s = video.object.frame_s;
    let layer = video.object.layer;
    let index = video.video_object.index;

    let groups = collect_groups(video);
    if cfg.diagnostics {
        probe(video, &groups, local, spf);
    }
    let param0 = param_of(video);
    let live0 = match live_at(video, &groups, local, local, spf) {
        Ok(l) => l,
        Err(e) => {
            // キーに層とフレームを混ぜない（フレームが進むたびに「初回」になる）
            if log_once::first(&format!("omb:live:{e}")) {
                tracing::error!(
                    "Failed to get object transform at layer {}, frame {}: {e}{}",
                    layer + 1,
                    origin,
                    log_once::SUFFIX
                );
            }
            return Ok(());
        }
    };
    let h_base0 = base_hash(&live0);
    let key = Key {
        effect_id: video.object.effect_id,
        object_id: video.object.id,
        index,
    };
    let info: RecordInfo = cache::record(key, origin, local as u32, h_base0, param0);
    let curr = combine(&live0, &param0);

    // 1・2 フレーム目を描いたら、出だしの外挿用に保存する（0 フレーム目の記録が新しいときだけ）
    let rel = origin as i64 - frame_s as i64;
    if (1..=2).contains(&rel) {
        if let Some((_, b0, p0, true)) = cache::find(key, frame_s, info) {
            let h0 = frame_hash(b0, &p0);
            store_write(&cfg.store, layer, frame_s, index, h0, (rel - 1) as usize, h_base0, param0);
        }
    }

    let (prev, source) = if origin == frame_s {
        let n: i64 = match cfg.extrapolation {
            Extrapolation::None => 0,
            Extrapolation::Linear => 2,
            Extrapolation::Quadratic => 3,
        };
        if n == 0 || local + n - 1 >= total {
            (curr.clone(), PrevSource::None)
        } else {
            let h0 = frame_hash(h_base0, &param0);
            let stored = store_lookup(&cfg.store.read(), layer, frame_s, index, h0);
            let mut poses = vec![curr.clone()];
            let (mut c_cached, mut c_stored, mut c_assumed) = (0u32, 0u32, 0u32);
            for k in 1..n {
                let live_k = live_at(video, &groups, local + k, local, spf)?;
                let hk = base_hash(&live_k);
                let from_cache = cache::usable(
                    cache::find(key, origin + k as u32, info),
                    Lookup { frame: origin + k as u32, base_hash: hk },
                );
                let from_store = stored.and_then(|e| {
                    let i = (k - 1) as usize;
                    (e.has[i] != 0 && e.base_hash[i] == hk).then_some(e.param[i])
                });
                let p = if let Some(p) = from_cache {
                    c_cached += 1;
                    p
                } else if let Some(p) = from_store {
                    c_stored += 1;
                    p
                } else {
                    c_assumed += 1;
                    param0
                };
                poses.push(combine(&live_k, &p));
            }
            (
                pose::retrodict(&poses),
                PrevSource::Extrapolated {
                    cached: c_cached,
                    stored: c_stored,
                    assumed: c_assumed,
                },
            )
        }
    } else {
        let found = cache::find(key, origin.saturating_sub(1), info);
        let mut result = None;
        if let Some((prev_local, _, _, _)) = found {
            let live_p = live_at(video, &groups, prev_local as i64, local, spf)?;
            let lookup = Lookup {
                frame: origin - 1,
                base_hash: base_hash(&live_p),
            };
            if let Some(p) = cache::usable(found, lookup) {
                result = Some((combine(&live_p, &p), PrevSource::Cached));
            }
        }
        match result {
            Some(r) => r,
            None if local >= 1 => {
                let live_p = live_at(video, &groups, local - 1, local, spf)?;
                (combine(&live_p, &param0), PrevSource::LiveOnly)
            }
            None => (curr.clone(), PrevSource::None),
        }
    };

    if curr.links.len() > render::MAX_LINKS && log_once::first("omb:nest_too_deep") {
        tracing::warn!(
            "グループ制御の入れ子が深すぎる（{} 段）。外側の {} 段だけで計算する{}",
            curr.links.len(),
            render::MAX_LINKS,
            log_once::SUFFIX
        );
    }

    let dims = Vec2::new(w as f32, h as f32);
    let object = pose::resolve(dims, &curr, &prev, angle, cfg.phase as f32);

    let exporting = matches!(crate::EDIT_HANDLE.get_edit_state(), Ok(EditState::Save));
    let limit = if exporting { cfg.render_limit } else { cfg.viewport_limit }.clamp(2, 4096);
    let metrics = pose::metrics(&object, (limit / 8).max(2));
    let required = (metrics.length.ceil() + 1.0).clamp(2.0, 65536.0) as u32;
    let samples = limit.clamp(2, required.max(2));

    let (origin_px, resolution) = if cfg.resize {
        let size = Vec2::new(
            (metrics.max.x - metrics.min.x).ceil(),
            (metrics.max.y - metrics.min.y).ceil(),
        );
        let max = MAX_IMAGE_SIZE as f32;
        if size.x > max || size.y > max {
            if log_once::first("omb:too_large") {
                tracing::warn!(
                    "Image size {}x{} exceeds maximum limit of {MAX_IMAGE_SIZE}x{MAX_IMAGE_SIZE}; clipped{}",
                    size.x,
                    size.y,
                    log_once::SUFFIX
                );
            }
            let r = Vec2::new(size.x.min(max), size.y.min(max));
            (metrics.min + (size - r) * 0.5, r)
        } else {
            (metrics.min, size)
        }
    } else {
        (Vec2::ZERO, dims)
    };
    let max = MAX_IMAGE_SIZE as f32;
    // clamp は NaN を NaN のまま返す（u32 にすると 0）ので max → min の順にする
    let (rw, rh) = (resolution.x.max(1.0).min(max) as u32, resolution.y.max(1.0).min(max) as u32);

    let image = ImageResource::Resource("omb_h_image".to_string());
    let map = ImageResource::Resource("omb_h_map".to_string());
    video.copy_image_resource(&ImageResource::Object, &image)?;
    if rw != w || rh != h {
        // 本体の set_image_data は null で「中身なしの大きさ変更」になるが、aviutl2-rs は null を渡せないので 0 で埋める。
        // 確保できなければ、元の画像のまま（ぼかさずに）返す。中心（param）もまだ動かしていない
        let len = rw as usize * rh as usize * 4;
        if with_zeros(len, |blank| video.set_image_data(blank, rw, rh)).is_none() {
            if log_once::first("omb:alloc_failed") {
                tracing::warn!(
                    "{rw}x{rh} の画像のメモリ（{} MiB）を確保できないので、ぼかさずに返します{}",
                    len >> 20,
                    log_once::SUFFIX
                );
            }
            return Ok(());
        }
    }
    if cfg.resize {
        video.param.cx -= origin_px.x + (resolution.x - dims.x) * 0.5;
        video.param.cy -= origin_px.y + (resolution.y - dims.y) * 0.5;
    }

    let clear = [0u8; 4];
    match cfg.tint_source {
        TintSource::Image => match cfg.tint_image.as_ref().filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => {
                if video
                    .copy_image_resource(&ImageResource::ImageFile(path.clone()), &map)
                    .is_err()
                {
                    // 原因は画像ごと（別の画像を選び直したら、その画像の失敗はまた出す）
                    if log_once::first(&format!("omb:tint_image:{}", path.display())) {
                        tracing::error!("Failed to copy image '{}'{}", path.display(), log_once::SUFFIX);
                    }
                    return Ok(());
                }
            }
            None => video.create_image_resource(&map, &clear, 1, 1)?,
        },
        TintSource::Layer => {
            let mut map_layer = cfg.tint_layer as i64;
            if cfg.layer_reference == LayerReference::Relative {
                map_layer += video.object.effect_layer as i64 + 1;
            }
            map_layer -= 1;
            if map_layer < 0 || map_layer == layer as i64 {
                video.create_image_resource(&map, &clear, 1, 1)?;
            } else if video.get_image_object(map_layer as u32, 0.0).is_none() {
                // キーはレイヤーだけ（フレームを混ぜると毎フレーム「初回」になる）
                if log_once::first(&format!("omb:tint_layer:{map_layer}")) {
                    tracing::error!("No object exists at layer {}, frame {}{}", map_layer + 1, origin, log_once::SUFFIX);
                }
                return Ok(());
            } else {
                video.copy_image_resource(
                    &ImageResource::Layer {
                        layer: map_layer as u32,
                        apply_additional_effects: true,
                    },
                    &map,
                )?;
            }
        }
    }
    let (map_w, _) = video.get_image_resource_size(&map).unwrap_or((1, 1));

    let params = render::build(
        &object,
        origin_px,
        Vec2::new(rw as f32, rh as f32),
        &render::Shading {
            mix: cfg.mix as f32 * 0.01,
            falloff: cfg.falloff as f32 * 0.01,
            edge: match cfg.edge {
                Edge::Trailing => 0,
                Edge::Leading => 1,
                Edge::Symmetric => 2,
            },
            samples,
            map_width: map_w,
            alpha_hashed: cfg.alpha_mode == AlphaMode::Hashed,
        },
    );

    // 本体が持っている状態を借りるだけなので、Release しない（au2-rs-plugin「本体が描いた画面を受け取る」）
    let blend = video.get_blend_state(BlendStateMode::Copy).map(ManuallyDrop::new);
    let sampler = video.get_sampler_state(SamplerMode::Clip).map(ManuallyDrop::new);
    let (Some(blend), Some(sampler)) = (blend, sampler) else {
        if log_once::first("omb:blend_sampler") {
            tracing::error!("Failed to get blend / sampler state{}", log_once::SUFFIX);
        }
        return Ok(());
    };
    video.exec_pixelshader_data(
        render::SHADER,
        &ImageResource::Object,
        &[image, map],
        params,
        &blend,
        &sampler,
    )?;

    if cfg.diagnostics {
        tracing::info!(
            "ObjectMotionBlur_H layer={} index={} frame={} local={} groups={} prev={:?} matched={} invalidated={} consecutive={} required={} samples={} size={}x{}",
            layer + 1,
            index,
            origin,
            local,
            groups.len(),
            source,
            info.matched,
            info.invalidated,
            info.consecutive,
            required,
            samples,
            rw,
            rh
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lend_zeros_reuses_and_stays_zero() {
        let mut pool = Vec::new();
        let n = lend_zeros(&mut pool, 16, 1024, |b| {
            assert!(b.iter().all(|&x| x == 0));
            b.len()
        });
        assert_eq!(n, Some(16));
        let cap = pool.capacity();
        // 小さい要求は確保し直さずに先頭を貸す
        assert_eq!(lend_zeros(&mut pool, 8, 1024, |b| b.len()), Some(8));
        assert_eq!(pool.capacity(), cap);
        // 足りなければ足す。前からある分も 0
        assert_eq!(lend_zeros(&mut pool, 64, 1024, |b| b.iter().all(|&x| x == 0)), Some(true));
        assert_eq!(pool.len(), 64);
    }

    #[test]
    fn lend_zeros_releases_over_keep() {
        let mut pool = Vec::new();
        assert_eq!(lend_zeros(&mut pool, 4096, 1024, |b| b.len()), Some(4096));
        assert_eq!(pool.capacity(), 0, "上限を超えた分は使い終わったら手放す");
    }

    #[test]
    fn lend_zeros_fails_without_abort() {
        // vec! なら abort する大きさ。確保できないことを None で返す
        let mut pool = Vec::new();
        let called = lend_zeros(&mut pool, isize::MAX as usize + 1, KEEP_ZERO_BYTES, |_| ());
        assert!(called.is_none());
        assert_eq!(pool.capacity(), 0);
    }

    #[test]
    fn max_image_bytes_fit() {
        // 一辺の上限どうしでも 1 GiB で、usize（と aviutl2-rs の u32 の検査）に収まる
        let len = MAX_IMAGE_SIZE as usize * MAX_IMAGE_SIZE as usize * 4;
        assert_eq!(len, 1 << 30);
        assert!(MAX_IMAGE_SIZE.checked_mul(MAX_IMAGE_SIZE).and_then(|v| v.checked_mul(4)).is_some());
    }
}
