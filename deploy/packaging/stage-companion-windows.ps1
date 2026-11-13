param(
    [Parameter(Mandatory)][string]$Bundle,
    [Parameter(Mandatory)][string]$AlarmAgent,
    [Parameter(Mandatory)][string]$VCRuntime,
    [Parameter(Mandatory)][string]$Output
)

$ErrorActionPreference = 'Stop'
$bundleRoot = (Resolve-Path -LiteralPath $Bundle).Path
$runtimeRoot = (Resolve-Path -LiteralPath $VCRuntime).Path
$repository = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$native = Get-Content -Raw -LiteralPath (Join-Path $repository 'models/native-deps.lock.json') | ConvertFrom-Json -AsHashtable
$runtime = $native.vc_runtime['windows-x64']
$outputRoot = [IO.Path]::GetFullPath($Output)
if ($outputRoot.Equals($bundleRoot, [StringComparison]::OrdinalIgnoreCase) -or
    $outputRoot.StartsWith($bundleRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The staging directory must be outside the Flutter release bundle.'
}
if ((Test-Path -LiteralPath $outputRoot) -and (Get-ChildItem -LiteralPath $outputRoot -Force | Select-Object -First 1)) {
    throw 'Use an empty Windows Companion staging directory.'
}
foreach ($relative in @('stillwatch_companion.exe', 'flutter_windows.dll', 'data/app.so', 'data/icudtl.dat')) {
    if (-not (Test-Path -LiteralPath (Join-Path $bundleRoot $relative) -PathType Leaf)) { throw "Flutter release file is missing: $relative" }
}
$files = Get-ChildItem -LiteralPath $bundleRoot -File -Recurse
foreach ($file in $files) {
    if ($file.Name -match '\.(onnx|tflite|safetensors|pt)$|onnxruntime|^stillwatch-core|firebase[.-]ai') {
        throw "Unexpected Core or inference artifact in the Companion bundle: $($file.Name)"
    }
    if ($file.Extension -in @('.pdb', '.lib', '.exp')) { continue }
    $relative = [IO.Path]::GetRelativePath($bundleRoot, $file.FullName)
    $destination = Join-Path $outputRoot $relative
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
    Copy-Item -LiteralPath $file.FullName -Destination $destination
}
Copy-Item -LiteralPath (Resolve-Path -LiteralPath $AlarmAgent).Path -Destination (Join-Path $outputRoot 'stillwatch-alarm-agent.exe')
foreach ($library in $runtime.Keys) {
    $file = Join-Path $runtimeRoot $library
    $signature = Get-AuthenticodeSignature -LiteralPath $file
    if ($signature.Status -ne 'Valid' -or (Get-FileHash -LiteralPath $file).Hash -ine $runtime[$library]) {
        throw "Invalid Microsoft Visual C++ runtime signature: $library"
    }
    Copy-Item -LiteralPath $file -Destination $outputRoot
}
Copy-Item -LiteralPath (Join-Path $repository 'LICENSE') -Destination (Join-Path $outputRoot 'LICENSE')
Copy-Item -LiteralPath (Join-Path $repository 'NOTICE') -Destination (Join-Path $outputRoot 'NOTICE')
Copy-Item -LiteralPath (Join-Path $repository 'models/native-deps.lock.json') -Destination (Join-Path $outputRoot 'native-deps.lock.json')
$inventory = [ordered]@{}
foreach ($file in (Get-ChildItem -LiteralPath $outputRoot -File -Recurse | Sort-Object FullName)) {
    $relative = [IO.Path]::GetRelativePath($outputRoot, $file.FullName).Replace('\', '/')
    $inventory[$relative] = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
}
[ordered]@{schema_version=1; version='0.1.0'; target='windows-x64'; files=$inventory} |
    ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $outputRoot 'manifest.json') -Encoding utf8
