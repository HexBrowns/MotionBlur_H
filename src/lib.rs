//! MotionBlur_H — ObjectMotionBlur_LK（Korarei、MIT）の Rust フォーク。
//!
//! 汎用プラグインの中にフィルタ効果 ObjectMotionBlur_H を 1 本ぶら下げる形（aviutl2-rs の metronome-plugin と同じ）。
//! 汎用プラグイン側は次の 3 つのためにある。
//! - 出力中かどうか（Sample Limit を Viewport / Render で切り替える）を `EDIT_HANDLE` で聞く
//! - 「キャッシュを破棄」で記録を全部捨てる
//! - 編集されたら世代を進め、古い記録を使わないようにする（出力・再生中は進めない）
//!
//! フィルタ効果は 2 つ: ObjectMotionBlur_H（filter.rs）と SceneMotionBlur_H（scene/。NVIDIA Optical Flow を使う）。

pub mod cache;
pub mod filter;
pub mod log_once;
pub mod pose;
pub mod render;
pub mod scene;

use aviutl2::generic::{EditState, GlobalEditHandle};
use aviutl2::AnyResult;

pub static EDIT_HANDLE: GlobalEditHandle = GlobalEditHandle::new();

#[aviutl2::plugin(GenericPlugin)]
pub struct MotionBlurH {
    object: aviutl2::generic::SubPlugin<filter::ObjectMotionBlur>,
    scene: aviutl2::generic::SubPlugin<scene::SceneMotionBlur>,
}

impl aviutl2::generic::GenericPlugin for MotionBlurH {
    fn new(info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        aviutl2::tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .event_format(aviutl2::logger::AviUtl2Formatter)
            .with_writer(aviutl2::logger::AviUtl2LogWriter)
            .init();
        Ok(Self {
            object: aviutl2::generic::SubPlugin::new_filter_plugin(&info)?,
            scene: aviutl2::generic::SubPlugin::new_filter_plugin(&info)?,
        })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "MotionBlur_H".to_string(),
            information: format!(
                "MotionBlur_H v{} by HexBrowns (fork of MotionBlur_K by Korarei)",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        registry.register_filter_plugin(&self.object);
        registry.register_filter_plugin(&self.scene);
        EDIT_HANDLE.init(registry.create_edit_handle());
    }

    fn on_clear_cache(&mut self, _edit_section: &aviutl2::generic::EditSection) {
        cache::clear_all();
        scene::gpu::reset();
        filter::release_zeros();
        // 初回だけにしていた失敗のログを、もう一度出せるようにする
        log_once::reset();
    }

    fn event_update_object_info(&mut self) {
        // 出力・再生中に来ても編集ではないので、記録は捨てない
        if matches!(EDIT_HANDLE.get_edit_state(), Ok(EditState::Edit)) {
            cache::bump_generation();
        }
    }
}

// 本体が呼ぶ関数（GetCommonPluginTable / RegisterPlugin など）を書き出す。これが無いと読み込みで
// 「Failed to register common plugin. GetProcAddress() failed.」になる（2026-10-01 に踏んだ）
aviutl2::register_generic_plugin!(MotionBlurH);
