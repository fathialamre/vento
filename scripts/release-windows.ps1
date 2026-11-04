# Builds the Windows installer published on GitHub:
# target\release-setup\vento-<version>-windows-setup.exe, one setup for x64
# and ARM64 PCs (it installs the build that fits), and its line for the
# release's SHA256SUMS.txt in target\release-setup\SHA256SUMS.txt. Needs:
#   - the Rust targets: rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc
#   - Inno Setup 6 (winget install JRSoftware.InnoSetup)
#   - cargo-about (cargo install cargo-about --locked --features cli; on an
#     ARM64 PC add --target x86_64-pc-windows-msvc, as ring needs clang there)
#
# Signing, with Azure Trusted Signing, when both are set:
#   VENTO_SIGN_DLIB      Azure.CodeSigning.Dlib.dll (the x64 one, from the
#                        Microsoft.Trusted.Signing.Client NuGet package)
#   VENTO_SIGN_METADATA  metadata.json: {"Endpoint": ..., "CodeSigningAccountName": ...,
#                        "CertificateProfileName": ...}
# and signed in with `az login`. Without them the setup is unsigned.
#
# Each program's exit code is checked; Windows PowerShell 5.1 would take
# what cargo prints on stderr for errors under ErrorActionPreference Stop.
$PSDefaultParameterValues["*:ErrorAction"] = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path crates\vento\Cargo.toml -Pattern '^version = "(.*)"$' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$out = "target\release-setup"
$stage = "$out\vento-$version"
$targets = @{ "x64" = "x86_64-pc-windows-msvc"; "arm64" = "aarch64-pc-windows-msvc" }

$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "release-windows.ps1: Inno Setup 6 not found" }

$signing = $env:VENTO_SIGN_DLIB -and $env:VENTO_SIGN_METADATA
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName | Select-Object -Last 1 -ExpandProperty FullName
if ($signing -and -not $signtool) { throw "release-windows.ps1: signtool.exe not found (Windows SDK)" }

# Signs `files` with Azure Trusted Signing, timestamped, if it is set up.
function Sign([string[]]$files) {
    if (-not $signing) { return }
    & $signtool sign /v /fd SHA256 /tr http://timestamp.acs.microsoft.com /td SHA256 `
        /dlib $env:VENTO_SIGN_DLIB /dmdf $env:VENTO_SIGN_METADATA @files
    if ($LASTEXITCODE -ne 0) { throw "release-windows.ps1: signing failed" }
}

if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

foreach ($arch in $targets.Keys) {
    $triple = $targets[$arch]
    cargo build -p vento --release --locked --target $triple
    if ($LASTEXITCODE -ne 0) { throw "release-windows.ps1: build for $triple failed" }
    New-Item -ItemType Directory -Force "$stage\$arch" | Out-Null
    Copy-Item "target\$triple\release\vento.exe" "$stage\$arch\vento.exe"
}
Sign @("$stage\x64\vento.exe", "$stage\arm64\vento.exe")

Copy-Item LICENSE "$stage\LICENSE"
Copy-Item crates\vento\ui\icons\LICENSE-tabler "$stage\LICENSE-tabler"
$notices = "$stage\THIRD-PARTY-NOTICES.txt"
cargo about generate --fail --target $targets["x64"] --target $targets["arm64"] -o $notices about.hbs
if ($LASTEXITCODE -ne 0) { throw "release-windows.ps1: cargo about failed" }

# cargo-about lists Rust crates only. libwebp-sys builds Google's libwebp C
# library into the app, so that library's own licence goes in as well.
$webp = (cargo metadata --format-version 1 --locked | ConvertFrom-Json).packages |
    Where-Object { $_.name -eq "libwebp-sys" } | Select-Object -First 1
$copying = Join-Path (Split-Path -Parent $webp.manifest_path) "vendor\COPYING"
if (-not (Test-Path $copying)) { throw "release-windows.ps1: libwebp's COPYING not found" }
Add-Content -Path $notices -Encoding UTF8 -Value @(
    "",
    "--------------------------------------------------------------------------------",
    "libwebp licence (BSD-3-Clause)",
    "",
    "Used by:",
    "  - libwebp, the C library built into libwebp-sys $($webp.version)",
    ""
)
Get-Content $copying | Add-Content -Path $notices -Encoding UTF8

& $iscc /Q "/DAppVersion=$version" "/DStage=$root\$stage" "/DOutputDir=$root\$out" scripts\vento-setup.iss
if ($LASTEXITCODE -ne 0) { throw "release-windows.ps1: Inno Setup failed" }
$setup = "$out\vento-$version-windows-setup.exe"
Sign @($setup)
if (-not $signing) { Write-Warning "unsigned: set VENTO_SIGN_DLIB and VENTO_SIGN_METADATA to sign" }

# The in-app updater checks the downloaded setup against its line in the
# release's SHA256SUMS.txt. Taken after signing, which changes the file;
# written with a bare LF, as shasum writes the Mac's line.
$sums = "$out\SHA256SUMS.txt"
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$root\$sums", "$hash  vento-$version-windows-setup.exe`n")
$setup
$sums
