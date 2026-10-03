//! SceneMotionBlur_H の描画。原作 SceneMotionBlur_K v2.0.2 の src/scene/intern/render.cpp を写したもの。
//!
//! 本体のテクスチャ（object）と同じデバイスで、自前のパイプライン（フルスクリーン三角形 + ピクセル / コンピュートシェーダー）を回す。
//! 手順は原作どおり:
//!   1. 前後のフレームを ABGR8 に変換して NVIDIA Optical Flow に渡す（双方向・コストつき。原作どおり 2 回流す）
//!   2. Regularize: デコード → 深度で重み付け → ピラミッドの押し上げ・引き下げ → 平滑化（2 つの代表フロー）
//!   3. Propagate: Jump Flooding で 2 層のフローを広げる
//!   4. Blur: フローに沿ってぶらす（表示を「オプティカルフロー」などにすると途中のフローを色で出す）
//!
//! 原作との違い: パイプラインとセッションの持ち方だけ。原作は (幅, 高さ, プリセット) ごとにセッションを共有し参照数で消していた。
//! ここではエフェクトのインスタンスごとに持つ（scene/mod.rs の `Instance`）。パイプラインはデバイスごとに 1 つで、
//! 「キャッシュを破棄」で世代を進めて作り直す。

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_SAMPLE_DESC};

use super::nvof;

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

const EPSILON: f32 = 1.0e-5;

mod cso {
    pub static FULLSCREEN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_fullscreen.cso"));
    pub static CONVERT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_convert.cso"));
    pub static DECODE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_decode.cso"));
    pub static DEBUG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_debug.cso"));
    pub static PREMULTIPLY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_premultiply.cso"));
    pub static PUSH: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_push.cso"));
    pub static PULL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_pull.cso"));
    pub static RESOLVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_resolve.cso"));
    pub static PROPAGATE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_propagate.cso"));
    pub static BLUR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_blur.cso"));
    pub static SMOOTH: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene_smooth.cso"));
}

// ---------------------------------------------------------------- 定数（原作 render.cpp の Param）

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct DecodeParam {
    resolution: [u32; 2],
    grid_size: u32,
    scale: f32,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Res2Param {
    resolution: [u32; 2],
    padding: [u32; 2],
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct PushParam {
    resolution: [u32; 4],
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct PropagateParam {
    resolution: [u32; 2],
    stride: i32,
    padding: f32,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct BlurParam {
    texel: [f32; 2],
    mix: [f32; 2],
    falloff: [f32; 2],
    sample_limit: i32,
    padding: f32,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct DebugParam {
    scale: f32,
    padding: [f32; 3],
}

const CB_SIZE: usize = 32; // 上の構造体で最大のもの（BlurParam）

// ---------------------------------------------------------------- パイプライン

struct Pipeline {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dss: ID3D11DepthStencilState,
    vs: ID3D11VertexShader,
    convert: ID3D11PixelShader,
    decode: ID3D11PixelShader,
    debug: ID3D11PixelShader,
    premultiply: ID3D11PixelShader,
    push: ID3D11PixelShader,
    pull: ID3D11PixelShader,
    resolve: ID3D11PixelShader,
    propagate: ID3D11PixelShader,
    blur: ID3D11PixelShader,
    smooth: ID3D11ComputeShader,
    smp: ID3D11SamplerState,
    cb: ID3D11Buffer,
    generation: u64,
}

unsafe impl Send for Pipeline {}

static PIPELINE: Mutex<Option<Pipeline>> = Mutex::new(None);
/// 「キャッシュを破棄」で進む。インスタンスのセッションは世代が違えば作り直す
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub fn reset() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    *PIPELINE.lock() = None;
}

fn out<T>(o: Option<T>, what: &'static str) -> AnyResult<T> {
    o.ok_or_else(|| format!("{what} returned null").into())
}

impl Pipeline {
    fn new(device: ID3D11Device) -> AnyResult<Self> {
        unsafe {
            let ctx = device.GetImmediateContext()?;

            let mut dss = None;
            device.CreateDepthStencilState(
                &D3D11_DEPTH_STENCIL_DESC {
                    DepthEnable: false.into(),
                    DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D11_COMPARISON_ALWAYS,
                    StencilEnable: false.into(),
                    StencilReadMask: D3D11_DEFAULT_STENCIL_READ_MASK as u8,
                    StencilWriteMask: D3D11_DEFAULT_STENCIL_WRITE_MASK as u8,
                    ..Default::default()
                },
                Some(&mut dss),
            )?;

            let ps = |bytes: &[u8]| -> AnyResult<ID3D11PixelShader> {
                let mut s = None;
                device.CreatePixelShader(bytes, None, Some(&mut s))?;
                out(s, "CreatePixelShader")
            };

            let mut vs = None;
            device.CreateVertexShader(cso::FULLSCREEN, None, Some(&mut vs))?;
            let mut smooth = None;
            device.CreateComputeShader(cso::SMOOTH, None, Some(&mut smooth))?;

            let mut smp = None;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MipLODBias: 0.0,
                    MaxAnisotropy: 1,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    BorderColor: [0.0; 4],
                    MinLOD: 0.0,
                    MaxLOD: D3D11_FLOAT32_MAX,
                },
                Some(&mut smp),
            )?;

            let mut cb = None;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: CB_SIZE as u32,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    MiscFlags: 0,
                    StructureByteStride: 0,
                },
                None,
                Some(&mut cb),
            )?;

            Ok(Pipeline {
                convert: ps(cso::CONVERT)?,
                decode: ps(cso::DECODE)?,
                debug: ps(cso::DEBUG)?,
                premultiply: ps(cso::PREMULTIPLY)?,
                push: ps(cso::PUSH)?,
                pull: ps(cso::PULL)?,
                resolve: ps(cso::RESOLVE)?,
                propagate: ps(cso::PROPAGATE)?,
                blur: ps(cso::BLUR)?,
                dss: out(dss, "CreateDepthStencilState")?,
                vs: out(vs, "CreateVertexShader")?,
                smooth: out(smooth, "CreateComputeShader")?,
                smp: out(smp, "CreateSamplerState")?,
                cb: out(cb, "CreateBuffer")?,
                device,
                ctx,
                generation: GENERATION.load(Ordering::Relaxed),
            })
        }
    }

    fn upload<T: Copy>(&self, value: &T) -> AnyResult<()> {
        assert!(std::mem::size_of::<T>() <= CB_SIZE);
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            std::ptr::copy_nonoverlapping(value as *const T as *const u8, mapped.pData as *mut u8, std::mem::size_of::<T>());
            self.ctx.Unmap(&self.cb, 0);
        }
        Ok(())
    }

    fn viewport(w: u32, h: u32) -> D3D11_VIEWPORT {
        D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: w as f32,
            Height: h as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }
    }

    /// フルスクリーン三角形を 1 回描く。`srvs` は t0〜、`rtvs` は出力（複数可）。
    fn draw(&self, ps: &ID3D11PixelShader, srvs: &[Option<ID3D11ShaderResourceView>], rtvs: &[Option<ID3D11RenderTargetView>], vp: &D3D11_VIEWPORT) {
        unsafe {
            self.ctx.PSSetShader(ps, None);
            if !srvs.is_empty() {
                self.ctx.PSSetShaderResources(0, Some(srvs));
            }
            self.ctx.OMSetRenderTargets(Some(rtvs), None);
            self.ctx.RSSetViewports(Some(&[*vp]));
            self.ctx.Draw(3, 0);
            self.ctx.OMSetRenderTargets(None, None);
            if !srvs.is_empty() {
                let nulls: Vec<Option<ID3D11ShaderResourceView>> = vec![None; srvs.len()];
                self.ctx.PSSetShaderResources(0, Some(&nulls));
            }
        }
    }

    /// 原作 Render の冒頭（状態の設定）
    fn begin(&self) {
        unsafe {
            self.ctx.OMSetBlendState(None, None, 0xffff_ffff);
            self.ctx.OMSetDepthStencilState(&self.dss, 0);
            self.ctx.RSSetState(None);
            self.ctx.GSSetShader(None, None);
            self.ctx.IASetInputLayout(None);
            self.ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.VSSetShader(&self.vs, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            self.ctx.PSSetSamplers(0, Some(&[Some(self.smp.clone())]));
            self.ctx.CSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            self.ctx.CSSetSamplers(0, Some(&[Some(self.smp.clone())]));
        }
    }
}

// ---------------------------------------------------------------- セッション（インスタンスごと）

struct Resource {
    _tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: ID3D11RenderTargetView,
    uav: ID3D11UnorderedAccessView,
}

impl Resource {
    fn new(device: &ID3D11Device, w: u32, h: u32) -> AnyResult<Self> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex = None;
            device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = out(tex, "CreateTexture2D")?;
            let (mut srv, mut rtv, mut uav) = (None, None, None);
            device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
            device.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
            device.CreateUnorderedAccessView(&tex, None, Some(&mut uav))?;
            Ok(Resource {
                srv: out(srv, "CreateShaderResourceView")?,
                rtv: out(rtv, "CreateRenderTargetView")?,
                uav: out(uav, "CreateUnorderedAccessView")?,
                _tex: tex,
            })
        }
    }
}

struct Level {
    w: u32,
    h: u32,
    pushed: Resource,
    pulled: Option<Resource>,
}

pub struct Session {
    of: nvof::Session,
    inputs: [(nvof::Buffer, ID3D11RenderTargetView); 2],
    outputs: [(nvof::Buffer, ID3D11ShaderResourceView); 2],
    costs: [(nvof::Buffer, ID3D11ShaderResourceView); 2],
    resources: [Resource; 4],
    levels: Vec<Level>,
    vp: D3D11_VIEWPORT,
    pub key: SessionKey,
    generation: u64,
}

unsafe impl Send for Session {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionKey {
    pub w: u32,
    pub h: u32,
    pub preset: u32,
}

impl Session {
    /// 原作 CreateSession
    fn new(p: &Pipeline, key: SessionKey) -> AnyResult<Self> {
        let perf = match key.preset {
            0 => nvof::NV_OF_PERF_LEVEL_SLOW,
            1 => nvof::NV_OF_PERF_LEVEL_MEDIUM,
            _ => nvof::NV_OF_PERF_LEVEL_FAST,
        };
        let (w, h) = (key.w, key.h);
        let of = nvof::Session::new(&p.device, &p.ctx, w, h, perf)?;

        let input = |_: usize| -> AnyResult<(nvof::Buffer, ID3D11RenderTargetView)> {
            let b = of.create_buffer(&p.device, nvof::NV_OF_BUFFER_USAGE_INPUT)?;
            let mut rtv = None;
            unsafe { p.device.CreateRenderTargetView(&b.texture, None, Some(&mut rtv))? };
            Ok((b, out(rtv, "CreateRenderTargetView")?))
        };
        let output = |usage: u32| -> AnyResult<(nvof::Buffer, ID3D11ShaderResourceView)> {
            let b = of.create_buffer(&p.device, usage)?;
            let mut srv = None;
            unsafe { p.device.CreateShaderResourceView(&b.texture, None, Some(&mut srv))? };
            Ok((b, out(srv, "CreateShaderResourceView")?))
        };

        let inputs = [input(0)?, input(1)?];
        let outputs = [output(nvof::NV_OF_BUFFER_USAGE_OUTPUT)?, output(nvof::NV_OF_BUFFER_USAGE_OUTPUT)?];
        let costs = [output(nvof::NV_OF_BUFFER_USAGE_COST)?, output(nvof::NV_OF_BUFFER_USAGE_COST)?];
        let resources = [
            Resource::new(&p.device, w, h)?,
            Resource::new(&p.device, w, h)?,
            Resource::new(&p.device, w, h)?,
            Resource::new(&p.device, w, h)?,
        ];

        // ピラミッド（1x1 まで半分ずつ。最初と最後の段は pulled を持たない）
        let mut levels = Vec::new();
        let (mut lw, mut lh) = (w, h);
        loop {
            let coarsest = lw == 1 && lh == 1;
            let pulled = if !levels.is_empty() && !coarsest {
                Some(Resource::new(&p.device, lw, lh)?)
            } else {
                None
            };
            levels.push(Level {
                w: lw,
                h: lh,
                pushed: Resource::new(&p.device, lw, lh)?,
                pulled,
            });
            if coarsest {
                break;
            }
            lw = lw.div_ceil(2).max(1);
            lh = lh.div_ceil(2).max(1);
        }

        Ok(Session {
            of,
            inputs,
            outputs,
            costs,
            resources,
            levels,
            vp: Pipeline::viewport(w, h),
            key,
            generation: p.generation,
        })
    }
}

// ---------------------------------------------------------------- 1 回分の描画

pub struct Params {
    pub key: SessionKey,
    pub should_use_temporal_hints: bool,
    pub scale: f32,
    pub falloff_edge: u32,
    pub falloff_amount: f32,
    pub sample_limit: i32,
    pub mix: f32,
    pub view_mode: u32,
}

/// 本体から借りたテクスチャ。手放すときに Release しない（au2-rs-plugin「本体が描いた画面を受け取る」）
pub struct Borrowed(pub ManuallyDrop<ID3D11Texture2D>);

impl Borrowed {
    /// # Safety
    /// `ptr` は本体が持つ ID3D11Texture2D で、フィルタ処理の間は有効であること
    pub unsafe fn from_raw(ptr: *mut std::ffi::c_void) -> Option<Self> {
        (!ptr.is_null()).then(|| Borrowed(ManuallyDrop::new(unsafe { windows::core::Interface::from_raw(ptr) })))
    }
}

/// 前後のフレームを持つためのテクスチャ（本体の object と同じ形式・大きさ）
pub struct FrameTexture {
    pub tex: ID3D11Texture2D,
    pub srv: ID3D11ShaderResourceView,
}

unsafe impl Send for FrameTexture {}

impl FrameTexture {
    pub fn like(src: &ID3D11Texture2D) -> AnyResult<Self> {
        unsafe {
            let device = src.GetDevice()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            src.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_DEFAULT;
            desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
            desc.CPUAccessFlags = 0;
            desc.MiscFlags = 0;
            desc.MipLevels = 1;
            desc.ArraySize = 1;
            let mut tex = None;
            device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = out(tex, "CreateTexture2D")?;
            let mut srv = None;
            device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
            Ok(FrameTexture {
                srv: out(srv, "CreateShaderResourceView")?,
                tex,
            })
        }
    }

    pub fn copy_from(&self, src: &ID3D11Texture2D) -> AnyResult<()> {
        unsafe {
            let ctx = src.GetDevice()?.GetImmediateContext()?;
            ctx.CopyResource(&self.tex, src);
        }
        Ok(())
    }
}

/// 原作 Render + Context::Draw。`session` はインスタンスが持つ（無い・合わない・世代が古いときは作り直す）。
pub fn render(
    session: &mut Option<Session>,
    dst: &ID3D11Texture2D,
    reference: &FrameTexture,
    target: &FrameTexture,
    depth: &ID3D11Texture2D,
    params: &Params,
) -> AnyResult<()> {
    let mut guard = PIPELINE.lock();
    let device = unsafe { dst.GetDevice()? };
    let generation = GENERATION.load(Ordering::Relaxed);
    if guard.as_ref().is_none_or(|p| p.device != device || p.generation != generation) {
        *guard = Some(Pipeline::new(device)?);
    }
    let p = guard.as_ref().unwrap();

    let stale = session
        .as_ref()
        .is_none_or(|s| s.key != params.key || s.generation != p.generation);
    if stale {
        *session = None; // 先に古いセッションを消す（NVOF のセッション数の上限を食わないように）
        *session = Some(Session::new(p, params.key)?);
    }
    let s = session.as_mut().unwrap();

    let result = draw(p, s, dst, reference, target, depth, params);
    if result.is_err() {
        *session = None;
    }
    result
}

fn draw(
    p: &Pipeline,
    s: &Session,
    dst: &ID3D11Texture2D,
    reference: &FrameTexture,
    target: &FrameTexture,
    depth: &ID3D11Texture2D,
    params: &Params,
) -> AnyResult<()> {
    let (w, h) = (params.key.w, params.key.h);
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        dst.GetDesc(&mut desc);
        if desc.Width != w || desc.Height != h {
            return Err("destination size does not match".into());
        }
    }

    p.begin();

    let mut depth_srv = None;
    unsafe { p.device.CreateShaderResourceView(depth, None, Some(&mut depth_srv))? };
    let depth_srv = out(depth_srv, "CreateShaderResourceView")?;

    // 1. ABGR8 へ変換して NVOF に流す（原作も 2 回流している。「2 回で精度が上がる」というコメントつき）
    p.draw(&p.convert, &[Some(reference.srv.clone())], &[Some(s.inputs[0].1.clone())], &s.vp);
    p.draw(&p.convert, &[Some(target.srv.clone())], &[Some(s.inputs[1].1.clone())], &s.vp);
    for _ in 0..2 {
        s.of.run(
            &s.inputs[0].0,
            &s.inputs[1].0,
            &s.outputs[0].0,
            &s.costs[0].0,
            &s.outputs[1].0,
            &s.costs[1].0,
            !params.should_use_temporal_hints,
        )?;
    }
    // NVOF が状態を変えるので、パイプラインの状態を設定し直す
    p.begin();

    let mut rtv = None;
    unsafe { p.device.CreateRenderTargetView(dst, None, Some(&mut rtv))? };
    let rtv = out(rtv, "CreateRenderTargetView")?;

    regularize(p, s, &depth_srv, params)?;

    if params.view_mode == 1 {
        return debug(p, &rtv, &s.resources[0].srv, params.scale, &s.vp);
    }

    let pos = propagate(p, s, &depth_srv, params)?;

    match params.view_mode {
        2 => return debug(p, &rtv, &s.resources[pos].srv, params.scale, &s.vp),
        3 => return debug(p, &rtv, &s.resources[pos + 2].srv, params.scale, &s.vp),
        _ => {}
    }

    let edge = params.falloff_amount.max(EPSILON);
    p.upload(&BlurParam {
        texel: [1.0 / w as f32, 1.0 / h as f32],
        mix: [1.0 - params.mix, params.mix],
        falloff: [
            if params.falloff_edge == 0 { EPSILON } else { edge },
            if params.falloff_edge == 1 { EPSILON } else { edge },
        ],
        sample_limit: params.sample_limit,
        padding: 0.0,
    })?;
    p.draw(
        &p.blur,
        &[
            Some(reference.srv.clone()),
            Some(s.resources[pos].srv.clone()),
            Some(s.resources[pos + 2].srv.clone()),
        ],
        &[Some(rtv)],
        &s.vp,
    );
    Ok(())
}

fn debug(p: &Pipeline, rtv: &ID3D11RenderTargetView, flow: &ID3D11ShaderResourceView, scale: f32, vp: &D3D11_VIEWPORT) -> AnyResult<()> {
    p.upload(&DebugParam { scale, padding: [0.0; 3] })?;
    p.draw(&p.debug, &[Some(flow.clone())], &[Some(rtv.clone())], vp);
    Ok(())
}

/// 原作 Regularize。resources[0] と [2] に 2 つの代表フローの初期値を書く。
fn regularize(p: &Pipeline, s: &Session, depth: &ID3D11ShaderResourceView, params: &Params) -> AnyResult<()> {
    let (w, h) = (params.key.w, params.key.h);

    p.upload(&DecodeParam {
        resolution: [w, h],
        grid_size: s.of.grid_size,
        scale: params.scale,
    })?;
    p.draw(
        &p.decode,
        &[
            Some(s.outputs[0].1.clone()),
            Some(s.outputs[1].1.clone()),
            Some(s.costs[0].1.clone()),
            Some(s.costs[1].1.clone()),
        ],
        &[Some(s.resources[0].rtv.clone())],
        &s.vp,
    );

    let base = &s.levels[0];
    p.upload(&Res2Param { resolution: [w, h], padding: [0; 2] })?;
    // 原作はここで RSSetViewports をしていない（直前のデコードと同じ全面のビューポート）。draw は毎回設定するので同じ結果になる
    p.draw(
        &p.premultiply,
        &[Some(s.resources[0].srv.clone()), Some(depth.clone())],
        &[Some(base.pushed.rtv.clone())],
        &s.vp,
    );

    for i in 1..s.levels.len() {
        let (src, dst) = (&s.levels[i - 1], &s.levels[i]);
        p.upload(&PushParam {
            resolution: [dst.w, dst.h, src.w, src.h],
        })?;
        p.draw(
            &p.push,
            &[Some(src.pushed.srv.clone()), Some(depth.clone())],
            &[Some(dst.pushed.rtv.clone())],
            &Pipeline::viewport(dst.w, dst.h),
        );
    }

    let mut coarse = s.levels.last().unwrap().pushed.srv.clone();
    let mut i = s.levels.len() - 1;
    while i > 1 {
        i -= 1;
        let level = &s.levels[i];
        let pulled = level.pulled.as_ref().ok_or("pyramid level without pulled buffer")?;
        p.upload(&Res2Param {
            resolution: [level.w, level.h],
            padding: [0; 2],
        })?;
        p.draw(
            &p.pull,
            &[Some(level.pushed.srv.clone()), Some(coarse.clone()), Some(depth.clone())],
            &[Some(pulled.rtv.clone())],
            &Pipeline::viewport(level.w, level.h),
        );
        coarse = pulled.srv.clone();
    }
    if s.levels.len() <= 1 {
        coarse = base.pushed.srv.clone();
    }

    p.upload(&Res2Param { resolution: [w, h], padding: [0; 2] })?;
    p.draw(
        &p.resolve,
        &[Some(base.pushed.srv.clone()), Some(coarse), Some(depth.clone())],
        &[Some(s.resources[1].rtv.clone())],
        &s.vp,
    );

    // 平滑化（コンピュートシェーダー。resources[1] と深度 → resources[0] と [2]）
    p.upload(&Res2Param { resolution: [w, h], padding: [0; 2] })?;
    unsafe {
        p.ctx.CSSetShader(&p.smooth, None);
        p.ctx.CSSetShaderResources(0, Some(&[Some(s.resources[1].srv.clone()), Some(depth.clone())]));
        let uavs = [Some(s.resources[0].uav.clone()), Some(s.resources[2].uav.clone())];
        p.ctx.CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
        p.ctx.Dispatch(w.div_ceil(16), h.div_ceil(16), 1);
        let null_uavs: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
        p.ctx.CSSetUnorderedAccessViews(0, 2, Some(null_uavs.as_ptr()), None);
        p.ctx.CSSetShaderResources(0, Some(&[None, None]));
    }
    Ok(())
}

/// 原作 Propagate（Jump Flooding）。戻り値 `curr` について resources[curr] が第 1 フロー、[curr + 2] が第 2 フロー。
fn propagate(p: &Pipeline, s: &Session, depth: &ID3D11ShaderResourceView, params: &Params) -> AnyResult<usize> {
    let (w, h) = (params.key.w, params.key.h);
    let mut curr = 0usize;
    let mut step = bit_floor(w.max(h));
    while step > 0 {
        let next = curr ^ 1;
        p.upload(&PropagateParam {
            resolution: [w, h],
            stride: step as i32,
            padding: 0.0,
        })?;
        p.draw(
            &p.propagate,
            &[
                Some(s.resources[curr].srv.clone()),
                Some(s.resources[curr + 2].srv.clone()),
                Some(depth.clone()),
            ],
            &[Some(s.resources[next].rtv.clone()), Some(s.resources[next + 2].rtv.clone())],
            &s.vp,
        );
        curr = next;
        step >>= 1;
    }
    Ok(curr)
}

/// std::bit_floor（v 以下で最大の 2 のべき。0 なら 0）
pub fn bit_floor(v: u32) -> u32 {
    if v == 0 {
        0
    } else {
        1 << (31 - v.leading_zeros())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_sizes_match_original() {
        // 原作 Param の各構造体は alignas(16)。最大は Blur の 32 バイト
        assert_eq!(std::mem::size_of::<DecodeParam>(), 16);
        assert_eq!(std::mem::size_of::<Res2Param>(), 16);
        assert_eq!(std::mem::size_of::<PushParam>(), 16);
        assert_eq!(std::mem::size_of::<PropagateParam>(), 16);
        assert_eq!(std::mem::size_of::<BlurParam>(), 32);
        assert_eq!(std::mem::size_of::<DebugParam>(), 16);
    }

    #[test]
    fn bit_floor_matches_std() {
        assert_eq!(bit_floor(0), 0);
        assert_eq!(bit_floor(1), 1);
        assert_eq!(bit_floor(1920), 1024);
        assert_eq!(bit_floor(2048), 2048);
    }

    #[test]
    fn shaders_are_embedded() {
        for b in [cso::FULLSCREEN, cso::CONVERT, cso::DECODE, cso::SMOOTH, cso::PROPAGATE, cso::BLUR] {
            assert_eq!(&b[0..4], b"DXBC");
        }
    }
}
