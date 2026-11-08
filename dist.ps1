# Builds the Windows distribution: dist\asset-browser-<version>-windows-x64.zip
# Run from anywhere: powershell -ExecutionPolicy Bypass -File dist.ps1

$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
$previousFlags = $env:RUSTFLAGS
try {
    $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' -List).Matches[0].Groups[1].Value
    $name = "asset-browser-$version-windows-x64"

    # Static CRT, so the exe starts on a PC without the Visual C++ redistributable.
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --locked --profile dist
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

    $stage = Join-Path dist $name
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage | Out-Null
    Copy-Item target\dist\asset-browser.exe, README.md $stage

    $zip = "dist\$name.zip"
    Compress-Archive -Path $stage -DestinationPath $zip -Force
    Write-Host "Built $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
}
finally {
    $env:RUSTFLAGS = $previousFlags
    Pop-Location
}
