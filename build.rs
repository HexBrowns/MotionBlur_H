//! シェーダーを fxc でコンパイルし、OUT_DIR/*.cso に置く（src から include_bytes! で埋め込む）。
//!
//! - shaders/blur.hlsl → blur.cso（ObjectMotionBlur_H。本体の exec_pixelshader_data に渡す）
//! - shaders/scene/*.hlsl → scene_<名前>.cso（SceneMotionBlur_H。自前の D3D11 パイプラインで使う）。
//!   原作 SceneMotionBlur_K の src/scene/intern/shaders をそのまま写したもので、フラグも原作の CompileShaders.cmake と同じ
//!
//! fxc は Windows SDK のものを新しい版から探す（AI/tools/check_hlsl_compile.py と同じ探し方）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn find_fxc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FXC") {
        return Some(PathBuf::from(p));
    }
    let root = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("x64").join("fxc.exe").is_file())
        .collect();
    versions.sort();
    versions.pop().map(|p| p.join("x64").join("fxc.exe"))
}

fn compile(fxc: &Path, src: &Path, profile: &str, out: &Path) {
    println!("cargo:rerun-if-changed={}", src.display());
    let status = Command::new(fxc)
        .args(["/nologo", "/T", profile, "/E", "main", "/O3", "/WX", "/Qstrip_reflect", "/Qstrip_debug", "/Fo"])
        .arg(out)
        .arg(src)
        .status()
        .expect("fxc を起動できない");
    assert!(status.success(), "fxc がシェーダーのコンパイルに失敗した: {}", src.display());
}

fn main() {
    println!("cargo:rerun-if-env-changed=FXC");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let fxc = find_fxc().expect("fxc.exe が見つからない（Windows SDK を入れるか、環境変数 FXC で場所を指定する）");

    compile(&fxc, Path::new("shaders/blur.hlsl"), "ps_5_0", &out_dir.join("blur.cso"));

    let scene = [
        ("fullscreen", "vs_5_0"),
        ("convert", "ps_5_0"),
        ("decode", "ps_5_0"),
        ("debug", "ps_5_0"),
        ("premultiply", "ps_5_0"),
        ("push", "ps_5_0"),
        ("pull", "ps_5_0"),
        ("resolve", "ps_5_0"),
        ("propagate", "ps_5_0"),
        ("blur", "ps_5_0"),
        ("smooth", "cs_5_0"),
    ];
    for (name, profile) in scene {
        let src = PathBuf::from("shaders/scene").join(format!("{name}.hlsl"));
        compile(&fxc, &src, profile, &out_dir.join(format!("scene_{name}.cso")));
    }
}
