# MotionBlur_H

Korarei の MotionBlur_K v2.0.2（MIT）の Rust フォーク。フィルタ効果は 2 つ（ラベル HexScript）。
原作は 2026-10-02 に `oldScript/20261002_MotionBlur_K退役/` へ退避した（本体は git に載せていない。`.gitignore` の `oldScript/*/Plugin/`）。

- **ObjectMotionBlur_H**（原作 ObjectMotionBlur_LK）: オブジェクトの動きからぶらす
- **SceneMotionBlur_H**（原作 SceneMotionBlur_K）: NVIDIA Optical Flow で画面全体をぶらす。Turing 以降の NVIDIA GPU が要る

原作は、グループ制御に掛けると配下のオブジェクトが前フレームの記録を取り違え、シークや編集の直後にありえない方向へぶれた。
直した内容と経緯は `issues/20261001_objectmotionblur_lk_group_shared_cache.md`。

- 設定項目の範囲・初期値・並びは原作と同じ。名前は v0.2.0 から日本語（定着した用語をそのまま使う。ルール au2-conventions「ユーザー向け文言」）

  | 原作 | このプラグイン |
  |---|---|
  | Shutter::Angle / Shutter::Phase | シャッター角度 / シャッター位相 |
  | Shutter::Falloff::Edge（Trailing / Leading / Symmetric）/ Amount | 減衰位置（後端 / 前端 / 両端）/ 減衰量 |
  | Sampling::Viewport / Render::Sample Limit | プレビュー最大サンプル数 / 出力最大サンプル数 |
  | Tint::Source（Image / Layer）/ Tint::Image / Tint::Layer | ティント参照元（画像 / レイヤー）/ グラデーションマップ画像 / グラデーションマップレイヤー |
  | Compositing::Mix / Alpha Mode（Alpha Blending / Alpha Hashed） | ミックス / アルファモード（アルファブレンド / ディザ） |
  | Extrapolation（None / Linear / Quadratic）/ Layer Reference（Absolute / Relative） | 外挿（なし / 線形 / 二次）/ レイヤー参照（絶対 / 相対） |
  | Resize / Diagnostics | リサイズ / 診断ログ |
- SceneMotionBlur_H の項目名: Shutter::Angle → シャッター角度、Falloff → 減衰位置 / 減衰量、Sample Limit → プレビュー / 出力最大サンプル数、
  Compositing::Mix → ミックス、Depth::Layer → 深度マップレイヤー、Preset（Slow / Medium / Fast）→ プリセット（高品質 / 標準 / 高速）、
  Layer Reference → レイヤー参照、View（Processed / Flow / Nearest Propagated Flow / Distinct Propagated Flow）→
  表示（処理結果 / オプティカルフロー / 最近傍伝播フロー / 異なる動きの伝播フロー）
- SceneMotionBlur_H は原作と同じ絵を出す（確認用シーン SMB で 11 フレームすべて画素まで一致）。変えたのは、前のフレームを GPU で持つこと
  （原作は CPU の 8bit に読み出して書き戻していた。約 25% 速い）と、NVIDIA Optical Flow のセッションを効果ごとに持つことだけ
- 本体 2.1.11 以上（aviutl2-rs 0.48。グループ制御を本体に聞く `get_group_control_object` を使う）

## ビルド

```powershell
python AI/tools/au2_build.py MotionBlur_H               # au2 release → 本番（C:\ProgramData\aviutl2）へ配置
python AI/tools/au2_build.py MotionBlur_H --no-deploy   # 配置しない（本番との違いだけ出す）
```

ビルドと同梱物は `aviutl2.toml`（[aviutl2-cli](https://github.com/sevenc-nanashi/aviutl2-cli)）が正本。このフォルダで `au2 release` だけを実行すると `release/` に au2pkg ができる。

テスト → リリースビルド → `Plugin/MotionBlur_H/MotionBlur_H.aux2` へ配置。差し替えは AviUtl2 の再起動が要る。
シェーダー（`shaders/blur.hlsl`）は `build.rs` が Windows SDK の fxc でコンパイルして埋め込む。

## 構成

| ファイル | 中身 |
|---|---|
| `src/lib.rs` | 汎用プラグイン（出力中かの判定、キャッシュの破棄、編集の検知）とフィルタの登録 |
| `src/filter.rs` | 設定項目と処理の本体。前フレームの標準描画・グループ制御は本体からその場で取る |
| `src/cache.rs` | 他の効果が動かした分（obj.ox など）の記録。オブジェクトごとに分け、編集で捨てる |
| `src/pose.rs` | 座標計算（原作の ResolveObject / ComputeMotionMetrics / Retrodict の写し） |
| `src/render.rs` / `shaders/blur.hlsl` | ObjectMotionBlur_H のシェーダーと定数 |
| `src/scene/mod.rs` | SceneMotionBlur_H の設定項目と処理の本体（前後のフレームの持ち方、深度レイヤー、区間の判定） |
| `src/scene/gpu.rs` | SceneMotionBlur_H の描画（原作 render.cpp の移植。自前の D3D11 パイプライン） |
| `src/scene/nvof.rs` | NVIDIA Optical Flow SDK（API 5.0、D3D11）の FFI。構造体のレイアウトは SDK のヘッダーを gcc に通した値と一致を確かめた |
| `shaders/scene/*.hlsl` | 原作 SceneMotionBlur_K のシェーダー（そのまま写した。MIT） |
| `verify_judge.py` | 確認用の節 OMB（`AI/tools/debug_items/omb.py`）の判定を机上で検算する |
