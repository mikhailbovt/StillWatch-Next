param([string]$Directory)

$ErrorActionPreference = 'Stop'
$swRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
if (!$Directory) { $Directory = Join-Path $swRoot '.local/native' }
$Directory = [System.IO.Path]::GetFullPath($Directory)
New-Item -ItemType Directory -Path $Directory -Force | Out-Null
$swLock = Get-Content -LiteralPath (Join-Path $swRoot 'models/native-deps.lock.json') -Raw | ConvertFrom-Json -AsHashtable

function Install-VerifiedArchive($Spec, $ExpectedPath) {
    if (Test-Path -LiteralPath $ExpectedPath) { return }
    $swName = [System.IO.Path]::GetFileName(([uri]$Spec.url).AbsolutePath)
    $swArchive = Join-Path $Directory $swName
    if (!(Test-Path -LiteralPath $swArchive)) {
        curl.exe -fsSL --retry 3 -o $swArchive $Spec.url
        if ($LASTEXITCODE) { throw "Unable to fetch $swName" }
    }
    if ((Get-FileHash -LiteralPath $swArchive).Hash -ne $Spec.archive_sha256) {
        throw "Archive digest mismatch for $swName"
    }
    tar.exe -xf $swArchive -C $Directory
    if ($LASTEXITCODE -or !(Test-Path -LiteralPath $ExpectedPath)) { throw "Unable to extract $swName" }
}

$swFfmpeg = $swLock.ffmpeg['windows-x64']
$swFfmpegDirectory = Join-Path $Directory $swFfmpeg.directory
Install-VerifiedArchive $swFfmpeg (Join-Path $swFfmpegDirectory 'bin/avcodec-62.dll')
foreach ($swDll in $swFfmpeg.dll_sha256.GetEnumerator()) {
    $swPath = Join-Path $swFfmpegDirectory ('bin/' + $swDll.Key)
    if ((Get-FileHash -LiteralPath $swPath).Hash -ne $swDll.Value) { throw "Native library digest mismatch for $($swDll.Key)" }
}
$swOrt = $swLock.ort['windows-x64']
$swOrtPath = Join-Path $Directory $swOrt.library
Install-VerifiedArchive $swOrt $swOrtPath
if ((Get-FileHash -LiteralPath $swOrtPath).Hash -ne $swOrt.library_sha256) { throw 'ONNX Runtime library digest mismatch' }

$env:FFMPEG_DIR = $swFfmpegDirectory
$env:Path = (Join-Path $swFfmpegDirectory 'bin') + [System.IO.Path]::PathSeparator + $env:Path
if (!$env:LIBCLANG_PATH) {
    $swClang = Join-Path $env:ProgramFiles 'LLVM/bin'
    if (!(Test-Path -LiteralPath (Join-Path $swClang 'libclang.dll'))) { throw 'Set LIBCLANG_PATH to the installed libclang directory' }
    $env:LIBCLANG_PATH = $swClang
}
if ($env:GITHUB_ENV) {
    "FFMPEG_DIR=$env:FFMPEG_DIR" | Out-File -LiteralPath $env:GITHUB_ENV -Append -Encoding utf8NoBOM
    "LIBCLANG_PATH=$env:LIBCLANG_PATH" | Out-File -LiteralPath $env:GITHUB_ENV -Append -Encoding utf8NoBOM
    (Join-Path $swFfmpegDirectory 'bin') | Out-File -LiteralPath $env:GITHUB_PATH -Append -Encoding utf8NoBOM
}
Write-Output $swOrtPath
