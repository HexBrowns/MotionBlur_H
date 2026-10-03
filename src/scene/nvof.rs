//! NVIDIA Optical Flow SDK（API 5.0、D3D11）の FFI と、原作が使う範囲の薄い包み。
//!
//! 型と値は SDK の nvOpticalFlowCommon.h / nvOpticalFlowD3D11.h（MIT。原作 SceneMotionBlur_K の
//! src/extern/nvidia_sdk/optical_flow に同梱）をそのまま写した。レイアウトは x64 の `#[repr(C)]` で一致する。
//! バッファの作り方は同じ場所の NvOFD3D11.cpp / NvOF.cpp（原作が手を入れた版。8bit のコストと双方向）に合わせた。

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BIND_UNORDERED_ACCESS, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16_SINT, DXGI_FORMAT_R8_UINT, DXGI_SAMPLE_DESC,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

pub type NV_OF_STATUS = i32;
pub const NV_OF_SUCCESS: NV_OF_STATUS = 0;
pub const NV_OF_ERR_OF_NOT_AVAILABLE: NV_OF_STATUS = 1;
pub const NV_OF_ERR_UNSUPPORTED_DEVICE: NV_OF_STATUS = 2;
pub const NV_OF_ERR_DEVICE_DOES_NOT_EXIST: NV_OF_STATUS = 3;
pub const NV_OF_ERR_INVALID_PTR: NV_OF_STATUS = 4;
pub const NV_OF_ERR_INVALID_PARAM: NV_OF_STATUS = 5;
pub const NV_OF_ERR_INVALID_CALL: NV_OF_STATUS = 6;
pub const NV_OF_ERR_INVALID_VERSION: NV_OF_STATUS = 7;
pub const NV_OF_ERR_OUT_OF_MEMORY: NV_OF_STATUS = 8;
pub const NV_OF_ERR_NOT_INITIALIZED: NV_OF_STATUS = 9;
pub const NV_OF_ERR_UNSUPPORTED_FEATURE: NV_OF_STATUS = 10;
pub const NV_OF_ERR_GENERIC: NV_OF_STATUS = 11;

pub const NV_OF_FALSE: u32 = 0;
pub const NV_OF_TRUE: u32 = 1;

pub const NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES: u32 = 0;

pub const NV_OF_PERF_LEVEL_SLOW: u32 = 5;
pub const NV_OF_PERF_LEVEL_MEDIUM: u32 = 10;
pub const NV_OF_PERF_LEVEL_FAST: u32 = 20;

pub const NV_OF_OUTPUT_VECTOR_GRID_SIZE_MAX: u32 = 5;
pub const NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED: u32 = 0;

pub const NV_OF_MODE_OPTICALFLOW: u32 = 1;

pub const NV_OF_BUFFER_USAGE_INPUT: u32 = 1;
pub const NV_OF_BUFFER_USAGE_OUTPUT: u32 = 2;
pub const NV_OF_BUFFER_USAGE_COST: u32 = 4;

pub const NV_OF_BUFFER_FORMAT_ABGR8: u32 = 3;
pub const NV_OF_BUFFER_FORMAT_SHORT2: u32 = 5;
pub const NV_OF_BUFFER_FORMAT_UINT8: u32 = 7;

pub const NV_OF_PRED_DIRECTION_BOTH: u32 = 2;

/// (5 << 4) | 0
pub const NV_OF_API_VERSION: u32 = 0x50;

pub type NvOFHandle = *mut c_void;
pub type NvOFGPUBufferHandle = *mut c_void;
pub type NvOFPrivDataHandle = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NV_OF_INIT_PARAMS {
    pub width: u32,
    pub height: u32,
    pub outGridSize: u32,
    pub hintGridSize: u32,
    pub mode: u32,
    pub perfLevel: u32,
    pub enableExternalHints: u32,
    pub enableOutputCost: u32,
    pub hPrivData: usize,
    pub disparityRange: u32,
    pub enableRoi: u32,
    pub predDirection: u32,
    pub enableGlobalFlow: u32,
    pub inputBufferFormat: u32,
}

#[repr(C)]
pub struct NV_OF_EXECUTE_INPUT_PARAMS {
    pub inputFrame: NvOFGPUBufferHandle,
    pub referenceFrame: NvOFGPUBufferHandle,
    pub externalHints: NvOFGPUBufferHandle,
    pub disableTemporalHints: u32,
    pub padding: u32,
    pub hPrivData: NvOFPrivDataHandle,
    pub padding2: u32,
    pub numRois: u32,
    pub roiData: *mut c_void,
}

#[repr(C)]
pub struct NV_OF_EXECUTE_OUTPUT_PARAMS {
    pub outputBuffer: NvOFGPUBufferHandle,
    pub outputCostBuffer: NvOFGPUBufferHandle,
    pub hPrivData: NvOFPrivDataHandle,
    pub bwdOutputBuffer: NvOFGPUBufferHandle,
    pub bwdOutputCostBuffer: NvOFGPUBufferHandle,
    pub globalFlowBuffer: NvOFGPUBufferHandle,
}

type PfnCreate = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut NvOFHandle) -> NV_OF_STATUS;
type PfnInit = unsafe extern "system" fn(NvOFHandle, *const NV_OF_INIT_PARAMS) -> NV_OF_STATUS;
type PfnFormatCount = unsafe extern "system" fn(NvOFHandle, u32, u32, *mut u32) -> NV_OF_STATUS;
type PfnFormat = unsafe extern "system" fn(NvOFHandle, u32, u32, *mut DXGI_FORMAT) -> NV_OF_STATUS;
type PfnRegister = unsafe extern "system" fn(NvOFHandle, *mut c_void, *mut NvOFGPUBufferHandle) -> NV_OF_STATUS;
type PfnUnregister = unsafe extern "system" fn(NvOFGPUBufferHandle) -> NV_OF_STATUS;
type PfnRun = unsafe extern "system" fn(
    NvOFHandle,
    *const NV_OF_EXECUTE_INPUT_PARAMS,
    *mut NV_OF_EXECUTE_OUTPUT_PARAMS,
) -> NV_OF_STATUS;
type PfnDestroy = unsafe extern "system" fn(NvOFHandle) -> NV_OF_STATUS;
type PfnLastError = unsafe extern "system" fn(NvOFHandle, *mut u8, *mut u32) -> NV_OF_STATUS;
type PfnCaps = unsafe extern "system" fn(NvOFHandle, u32, *mut u32, *mut u32) -> NV_OF_STATUS;

/// NV_OF_D3D11_API_FUNCTION_LIST（並びは SDK と同じ。名前だけ Rust 側で付けている）
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FunctionList {
    pub create_d3d11: Option<PfnCreate>,
    pub init: Option<PfnInit>,
    pub surface_format_count: Option<PfnFormatCount>,
    pub surface_format: Option<PfnFormat>,
    pub register_resource: Option<PfnRegister>,
    pub unregister_resource: Option<PfnUnregister>,
    pub run: Option<PfnRun>,
    pub destroy: Option<PfnDestroy>,
    pub last_error: Option<PfnLastError>,
    pub caps: Option<PfnCaps>,
}

type PfnCreateInstance = unsafe extern "system" fn(u32, *mut FunctionList) -> NV_OF_STATUS;

#[derive(Debug)]
pub struct NvofError {
    pub status: NV_OF_STATUS,
    pub context: &'static str,
}

impl std::fmt::Display for NvofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.status {
            NV_OF_ERR_OF_NOT_AVAILABLE => "NVIDIA Optical Flow is not available",
            NV_OF_ERR_UNSUPPORTED_DEVICE => "unsupported device",
            NV_OF_ERR_DEVICE_DOES_NOT_EXIST => "device does not exist",
            NV_OF_ERR_INVALID_PTR => "invalid pointer",
            NV_OF_ERR_INVALID_PARAM => "invalid parameter",
            NV_OF_ERR_INVALID_CALL => "NVIDIA Optical Flow API was called in an invalid order",
            NV_OF_ERR_INVALID_VERSION => "NVIDIA Optical Flow API version is incompatible",
            NV_OF_ERR_OUT_OF_MEMORY => "out of memory",
            NV_OF_ERR_NOT_INITIALIZED => "NVIDIA Optical Flow is not initialized",
            NV_OF_ERR_UNSUPPORTED_FEATURE => "unsupported feature",
            _ => "NVIDIA Optical Flow reported an internal error",
        };
        write!(f, "{} ({}, status {})", what, self.context, self.status)
    }
}

impl std::error::Error for NvofError {}

fn check(status: NV_OF_STATUS, context: &'static str) -> Result<(), NvofError> {
    if status == NV_OF_SUCCESS {
        Ok(())
    } else {
        Err(NvofError { status, context })
    }
}

fn unavailable(context: &'static str) -> NvofError {
    NvofError {
        status: NV_OF_ERR_OF_NOT_AVAILABLE,
        context,
    }
}

/// nvofapi64.dll から取った関数表。プロセスで 1 回だけ読み込み、DLL は解放しない（ドライバの DLL なので残っていてよい）。
static API: OnceLock<Option<FunctionList>> = OnceLock::new();

fn api() -> Result<FunctionList, NvofError> {
    let list = API.get_or_init(|| unsafe {
        let module: HMODULE = LoadLibraryW(windows::core::w!("nvofapi64.dll")).ok()?;
        let proc = GetProcAddress(module, PCSTR(c"NvOFAPICreateInstanceD3D11".as_ptr().cast()))?;
        let create: PfnCreateInstance = std::mem::transmute(proc);
        let mut list: FunctionList = std::mem::zeroed();
        if create(NV_OF_API_VERSION, &mut list) != NV_OF_SUCCESS {
            return None;
        }
        Some(list)
    });
    list.ok_or_else(|| unavailable("load nvofapi64.dll"))
}

/// NVOF に登録した D3D11 テクスチャ。
pub struct Buffer {
    pub handle: NvOFGPUBufferHandle,
    pub texture: ID3D11Texture2D,
    unregister: PfnUnregister,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            (self.unregister)(self.handle);
        }
    }
}

/// NVOF のセッション 1 つ（原作 NvOFD3D11 + NvOF::Init）。
pub struct Session {
    handle: NvOFHandle,
    list: FunctionList,
    pub width: u32,
    pub height: u32,
    pub grid_size: u32,
}

unsafe impl Send for Session {}
unsafe impl Send for Buffer {}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(destroy) = self.list.destroy {
            unsafe {
                destroy(self.handle);
            }
        }
    }
}

impl Session {
    /// 入力は ABGR8、モードはオプティカルフロー、出力は双方向・コストあり。
    /// グリッドは 1 が使えれば 1、無ければ 1 より大きい中で最小のもの（原作の CheckGridSize / GetNextMinGridSize）。
    pub fn new(device: &ID3D11Device, context: &ID3D11DeviceContext, width: u32, height: u32, perf_level: u32) -> Result<Self, NvofError> {
        let list = api()?;
        let create = list.create_d3d11.ok_or_else(|| unavailable("nvCreateOpticalFlowD3D11"))?;
        let mut handle: NvOFHandle = std::ptr::null_mut();
        check(unsafe { create(device.as_raw(), context.as_raw(), &mut handle) }, "nvCreateOpticalFlowD3D11")?;
        if handle.is_null() {
            return Err(unavailable("nvCreateOpticalFlowD3D11"));
        }
        let mut session = Session {
            handle,
            list,
            width,
            height,
            grid_size: 1,
        };

        session.check_formats()?;

        let sizes = session.grid_sizes()?;
        let grid = if sizes.contains(&1) {
            1
        } else {
            sizes
                .iter()
                .copied()
                .filter(|&s| s > 1 && s < NV_OF_OUTPUT_VECTOR_GRID_SIZE_MAX)
                .min()
                .ok_or(NvofError {
                    status: NV_OF_ERR_UNSUPPORTED_FEATURE,
                    context: "No supported NVIDIA Optical Flow output grid size was found",
                })?
        };
        session.grid_size = grid;

        let params = NV_OF_INIT_PARAMS {
            width,
            height,
            outGridSize: grid,
            hintGridSize: NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED,
            mode: NV_OF_MODE_OPTICALFLOW,
            perfLevel: perf_level,
            enableExternalHints: NV_OF_FALSE,
            enableOutputCost: NV_OF_TRUE,
            enableRoi: NV_OF_FALSE,
            predDirection: NV_OF_PRED_DIRECTION_BOTH,
            enableGlobalFlow: NV_OF_FALSE,
            inputBufferFormat: NV_OF_BUFFER_FORMAT_ABGR8,
            ..Default::default()
        };
        let f = session.list.init.ok_or_else(|| unavailable("nvOFInit"))?;
        check(unsafe { f(session.handle, &params) }, "nvOFInit")?;
        Ok(session)
    }

    fn formats(&self, usage: u32) -> Result<Vec<DXGI_FORMAT>, NvofError> {
        let count_f = self.list.surface_format_count.ok_or_else(|| unavailable("nvOFGetSurfaceFormatCountD3D11"))?;
        let get_f = self.list.surface_format.ok_or_else(|| unavailable("nvOFGetSurfaceFormatD3D11"))?;
        let mut count = 0u32;
        check(unsafe { count_f(self.handle, usage, NV_OF_MODE_OPTICALFLOW, &mut count) }, "nvOFGetSurfaceFormatCountD3D11")?;
        let mut v = vec![DXGI_FORMAT(0); count as usize];
        if count > 0 {
            check(unsafe { get_f(self.handle, usage, NV_OF_MODE_OPTICALFLOW, v.as_mut_ptr()) }, "nvOFGetSurfaceFormatD3D11")?;
        }
        Ok(v)
    }

    /// 入力 ABGR8（B8G8R8A8_UNORM）と出力 SHORT2（R16G16_SINT）が使えるか（原作 NvOFD3D11 のコンストラクタ）。
    fn check_formats(&self) -> Result<(), NvofError> {
        let input = self.formats(NV_OF_BUFFER_USAGE_INPUT)?;
        let output = self.formats(NV_OF_BUFFER_USAGE_OUTPUT)?;
        if input.contains(&DXGI_FORMAT_B8G8R8A8_UNORM) && output.contains(&DXGI_FORMAT_R16G16_SINT) {
            Ok(())
        } else {
            Err(NvofError {
                status: NV_OF_ERR_INVALID_PARAM,
                context: "Invalid buffer format",
            })
        }
    }

    fn grid_sizes(&self) -> Result<Vec<u32>, NvofError> {
        let caps = self.list.caps.ok_or_else(|| unavailable("nvOFGetCaps"))?;
        let mut size = 0u32;
        check(unsafe { caps(self.handle, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES, std::ptr::null_mut(), &mut size) }, "nvOFGetCaps")?;
        let mut v = vec![0u32; size as usize];
        if size > 0 {
            check(unsafe { caps(self.handle, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES, v.as_mut_ptr(), &mut size) }, "nvOFGetCaps")?;
        }
        Ok(v)
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.width.div_ceil(self.grid_size), self.height.div_ceil(self.grid_size))
    }

    /// 原作 NvOFBufferD3D11 の 2 つ目のコンストラクタ（テクスチャを作って登録する）。
    pub fn create_buffer(&self, device: &ID3D11Device, usage: u32) -> Result<Buffer, Box<dyn std::error::Error>> {
        let (w, h, format) = match usage {
            NV_OF_BUFFER_USAGE_INPUT => (self.width, self.height, DXGI_FORMAT_B8G8R8A8_UNORM),
            NV_OF_BUFFER_USAGE_OUTPUT => {
                let (w, h) = self.output_size();
                (w, h, DXGI_FORMAT_R16G16_SINT)
            }
            NV_OF_BUFFER_USAGE_COST => {
                let (w, h) = self.output_size();
                (w, h, DXGI_FORMAT_R8_UINT)
            }
            _ => return Err(Box::new(unavailable("unsupported buffer usage"))),
        };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        let texture = texture.ok_or("CreateTexture2D returned null")?;
        let register = self.list.register_resource.ok_or_else(|| unavailable("nvOFRegisterResourceD3D11"))?;
        let unregister = self.list.unregister_resource.ok_or_else(|| unavailable("nvOFUnregisterResourceD3D11"))?;
        let mut handle: NvOFGPUBufferHandle = std::ptr::null_mut();
        check(unsafe { register(self.handle, texture.as_raw(), &mut handle) }, "nvOFRegisterResourceD3D11")?;
        Ok(Buffer {
            handle,
            texture,
            unregister,
        })
    }

    /// 双方向・コストつきで 1 回流す（原作 NvOF::Execute）。input → reference のフローが forward。
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        input: &Buffer,
        reference: &Buffer,
        forward: &Buffer,
        forward_cost: &Buffer,
        backward: &Buffer,
        backward_cost: &Buffer,
        disable_temporal_hints: bool,
    ) -> Result<(), NvofError> {
        let run_fn = self.list.run.ok_or_else(|| unavailable("nvOFExecute"))?;
        let inp = NV_OF_EXECUTE_INPUT_PARAMS {
            inputFrame: input.handle,
            referenceFrame: reference.handle,
            externalHints: std::ptr::null_mut(),
            disableTemporalHints: if disable_temporal_hints { NV_OF_TRUE } else { NV_OF_FALSE },
            padding: 0,
            hPrivData: std::ptr::null_mut(),
            padding2: 0,
            numRois: 0,
            roiData: std::ptr::null_mut(),
        };
        let mut out = NV_OF_EXECUTE_OUTPUT_PARAMS {
            outputBuffer: forward.handle,
            outputCostBuffer: forward_cost.handle,
            hPrivData: std::ptr::null_mut(),
            bwdOutputBuffer: backward.handle,
            bwdOutputCostBuffer: backward_cost.handle,
            globalFlowBuffer: std::ptr::null_mut(),
        };
        check(unsafe { run_fn(self.handle, &inp, &mut out) }, "nvOFExecute")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_sdk_x64() {
        // nvOpticalFlowCommon.h の構造体を x64 で並べた大きさ
        assert_eq!(std::mem::size_of::<NV_OF_INIT_PARAMS>(), 64);
        assert_eq!(std::mem::offset_of!(NV_OF_INIT_PARAMS, hPrivData), 32);
        assert_eq!(std::mem::offset_of!(NV_OF_INIT_PARAMS, inputBufferFormat), 56); // 32 + ポインタ 8 + u32 x 4
        assert_eq!(std::mem::size_of::<NV_OF_EXECUTE_INPUT_PARAMS>(), 56);
        assert_eq!(std::mem::offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, hPrivData), 32);
        assert_eq!(std::mem::offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, roiData), 48);
        assert_eq!(std::mem::size_of::<NV_OF_EXECUTE_OUTPUT_PARAMS>(), 48);
        assert_eq!(std::mem::size_of::<FunctionList>(), 80);
    }
}
