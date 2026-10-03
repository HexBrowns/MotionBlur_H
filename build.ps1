# build.ps1 — MotionBlur_H ビルド & デプロイ
#
# 使い方: .\build.ps1
#         .\build.ps1 -NoDeploy   （配置せずビルドのみ）
#         .\build.ps1 -SkipTest   （テストを飛ばす）
#
# シェーダー（shaders/blur.hlsl）は build.rs が Windows SDK の fxc でコンパイルして埋め込む。

param(
    [switch]$NoDeploy,
    [switch]$SkipTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$SrcDir      = $PSScriptRoot
$AviUtl2Root = "C:\ProgramData\aviutl2"
$PluginDir   = "$AviUtl2Root\Plugin\MotionBlur_H"

Write-Host "`n=== MotionBlur_H ビルド (Rust) ===" -ForegroundColor Yellow

Push-Location $SrcDir
try {
    if (-not $SkipTest) {
        Write-Host "`n--- テスト ---" -ForegroundColor Cyan
        cargo test
        if ($LASTEXITCODE -ne 0) {
            throw "cargo test failed (exit $LASTEXITCODE)"
        }
    }

    Write-Host "`n--- リリースビルド ---" -ForegroundColor Cyan
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed (exit $LASTEXITCODE)"
    }
} finally {
    Pop-Location
}

$DllPath = Join-Path $SrcDir "target\release\motion_blur_h.dll"
if (-not (Test-Path -LiteralPath $DllPath)) {
    throw "ビルド成果物が見つかりません: $DllPath"
}

$Aux2Out = Join-Path $SrcDir "MotionBlur_H.aux2"
Copy-Item -LiteralPath $DllPath -Destination $Aux2Out -Force
$size = (Get-Item -LiteralPath $Aux2Out).Length
Write-Host ("  完了: {0} ({1:N0} bytes)" -f $Aux2Out, $size) -ForegroundColor Green

# 外部 DLL 依存の確認（VC ランタイム以外がぶら下がっていないか）
$objdump = "C:\msys64\ucrt64\bin\objdump.exe"
if (Test-Path -LiteralPath $objdump) {
    Write-Host "`n--- DLL 依存 ---" -ForegroundColor Cyan
    $dump = & $objdump -p $Aux2Out
    $dump | Select-String "DLL Name" | ForEach-Object { "  $_" }
    # 本体が呼ぶ入口が書き出されているか（register_generic_plugin! を忘れると 0 件になり、起動時に
    # 「Failed to register common plugin. GetProcAddress() failed.」が出る）
    if (-not ($dump | Select-String "GetCommonPluginTable")) {
        throw "GetCommonPluginTable が書き出されていません（register_generic_plugin! を確かめる）"
    }
    Write-Host "  書き出し: GetCommonPluginTable あり" -ForegroundColor DarkGreen
} else {
    Write-Host "`n  (objdump が無いので DLL 依存の確認は省略)" -ForegroundColor DarkGray
}

if ($NoDeploy) {
    Write-Host "`n-NoDeploy が指定されたので配置しません。" -ForegroundColor DarkGray
    return
}

Write-Host "`n=== デプロイ ===" -ForegroundColor Yellow

if (-not (Test-Path -LiteralPath $PluginDir)) {
    New-Item -ItemType Directory -Path $PluginDir -Force | Out-Null
}
Copy-Item -LiteralPath $Aux2Out -Destination "$PluginDir\MotionBlur_H.aux2" -Force
Write-Host "  配置: $PluginDir\MotionBlur_H.aux2" -ForegroundColor DarkGreen

Write-Host "`n※ プラグインの差し替えは AviUtl2 の再起動が必要です。" -ForegroundColor Yellow
