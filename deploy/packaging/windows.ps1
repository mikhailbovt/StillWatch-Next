param(
    [Parameter(Mandatory)][string]$Payload,
    [Parameter(Mandatory)][string]$Output,
    [Parameter(Mandatory)][string]$Wix,
    [switch]$Companion,
    [string]$SignTool,
    [ValidatePattern('^[A-Fa-f0-9]{40}$')][string]$SigningThumbprint,
    [uri]$TimestampUrl,
    [switch]$MachineSigningStore
)

$ErrorActionPreference = 'Stop'
$swSign = @()
if ($SignTool -or $SigningThumbprint -or $TimestampUrl -or $MachineSigningStore) {
    if (!$SignTool -or !$SigningThumbprint -or !$TimestampUrl -or !$TimestampUrl.IsAbsoluteUri -or
        $TimestampUrl.Scheme -notin @('http', 'https') -or $TimestampUrl.UserInfo) {
        throw 'Signing requires SignTool, a certificate thumbprint and an RFC 3161 timestamp URL.'
    }
    $swSignTool = (Resolve-Path -LiteralPath $SignTool).Path
    $swStore = if ($MachineSigningStore) { 'LocalMachine' } else { 'CurrentUser' }
    $swSigner = Get-Item -LiteralPath "Cert:\$swStore\My\$SigningThumbprint" -ErrorAction SilentlyContinue
    $swNow = [DateTime]::Now
    if (!$swSigner -or !$swSigner.HasPrivateKey -or $swNow -lt $swSigner.NotBefore -or $swNow -ge $swSigner.NotAfter) {
        throw "No valid signing identity with a private key in $swStore\My for the requested thumbprint."
    }
    $swSign = @('sign', '/s', 'My', '/sha1', $SigningThumbprint, '/fd', 'SHA256', '/tr', $TimestampUrl.AbsoluteUri, '/td', 'SHA256')
    if ($MachineSigningStore) { $swSign += '/sm' }
}

function Sign-Installer([string]$Path) {
    & $swSignTool @swSign $Path
    if ($LASTEXITCODE -ne 0) { throw 'Installer signing or timestamping failed.' }
    & $swSignTool verify /pa /all /tw $Path
    if ($LASTEXITCODE -ne 0) { throw 'Installer signature or timestamp verification failed.' }
}

$swPayload = (Resolve-Path -LiteralPath $Payload).Path
$swOutput = [IO.Path]::GetFullPath($Output)
$swExtension = [IO.Path]::GetExtension($swOutput)
if ($swExtension -ine '.msi' -and (!$Companion -or $swExtension -ine '.exe')) {
    throw 'Select .msi for Core or .msi/.exe for Companion.'
}
$swBundle = $swExtension -ieq '.exe'
$swMsi = [IO.Path]::ChangeExtension($swOutput, '.msi')
$swParent = [IO.Path]::GetDirectoryName($swOutput)
[IO.Directory]::CreateDirectory($swParent) | Out-Null
$swWix = (Resolve-Path -LiteralPath $Wix).Path
$swEntry = if ($Companion) { 'stillwatch_companion.exe' } else { 'bin/stillwatch-core.exe' }
$swTemplate = if ($Companion) { 'windows-companion.wxs' } else { 'windows.wxs' }
$swManifest = Get-Content -Raw -LiteralPath (Join-Path $swPayload 'manifest.json') | ConvertFrom-Json -AsHashtable
if ($swManifest.target -ne 'windows-x64' -or -not $swManifest.files.Contains($swEntry)) { throw 'Invalid Windows payload' }
$swVersion = $swManifest.version
if ($swVersion -isnot [string] -or $swVersion -cnotmatch '\A(?:0|[1-9][0-9]{0,2})\.(?:0|[1-9][0-9]{0,2})\.(?:0|[1-9][0-9]{0,4})\z') {
    throw 'The payload must declare a three-part MSI product version.'
}
$swNumbers = $swVersion.Split('.')
if ([int]$swNumbers[0] -gt 255 -or [int]$swNumbers[1] -gt 255 -or [int]$swNumbers[2] -gt 65535) {
    throw 'MSI product version components exceed 255.255.65535.'
}
$swXml = [Xml.XmlDocument]::new()
$swNs = 'http://wixtoolset.org/schemas/v4/wxs'
$swRoot = $swXml.AppendChild($swXml.CreateElement('Wix', $swNs))
$swFragment = $swRoot.AppendChild($swXml.CreateElement('Fragment', $swNs))
$swGroup = $swFragment.AppendChild($swXml.CreateElement('ComponentGroup', $swNs))
$swGroup.SetAttribute('Id', 'NativePayload')
$swDirs = @{ '' = 'InstallFolder'; 'bin' = 'Binaries' }
$swDirFragments = @{}
$swPaths = @($swManifest.files.Keys) + 'manifest.json'
foreach ($swPath in $swPaths) {
    if ($swPath -cnotmatch '^[A-Za-z0-9._/-]+$' -or $swPath -match '(^|/)\.\.(/|$)') { throw 'Invalid payload path' }
    $swAbsolute = (Resolve-Path -LiteralPath (Join-Path $swPayload $swPath)).Path
    if ($swManifest.files.Contains($swPath) -and (Get-FileHash -LiteralPath $swAbsolute).Hash -ine $swManifest.files[$swPath]) {
        throw "Payload digest mismatch for $swPath"
    }
    if ($swPath -eq $swEntry) { continue }
    $swSegments = $swPath.Split('/')
    $swRelative = ''
    foreach ($swPart in ($swSegments | Select-Object -SkipLast 1)) {
        $swBase = $swRelative
        $swRelative = if ($swRelative) { "$swRelative/$swPart" } else { $swPart }
        if ($swDirs.ContainsKey($swRelative)) { continue }
        $swKey = 'dir_' + ($swRelative -replace '[^A-Za-z0-9]', '_')
        $swDirs[$swRelative] = $swKey
        $swParentRef = $swDirFragments[$swBase]
        if ($null -eq $swParentRef) {
            $swParentRef = $swFragment.AppendChild($swXml.CreateElement('DirectoryRef', $swNs))
            $swParentRef.SetAttribute('Id', $swDirs[$swBase])
            $swDirFragments[$swBase] = $swParentRef
        }
        $swChild = $swParentRef.AppendChild($swXml.CreateElement('Directory', $swNs))
        $swChild.SetAttribute('Name', $swPart)
        $swChild.SetAttribute('Id', $swKey)
        $swDirFragments[$swRelative] = $swChild
    }
    $swComponent = $swGroup.AppendChild($swXml.CreateElement('Component', $swNs))
    $swComponent.SetAttribute('Directory', $swDirs[$swRelative])
    $swFile = $swComponent.AppendChild($swXml.CreateElement('File', $swNs))
    $swFile.SetAttribute('Source', $swAbsolute)
    $swFile.SetAttribute('KeyPath', 'yes')
}
$swGenerated = [IO.Path]::ChangeExtension($swOutput, '.payload.wxs')
$swBuild = @('build', '-arch', 'x64', '-b', $PSScriptRoot, '-d', "Payload=$swPayload", '-d', "ProductVersion=$swVersion")
if (-not $Companion) { $swBuild += @('-ext', 'WixToolset.Util.wixext/5.0.2') }
$swStaging = $null
$swBuildMsi = $swMsi
$swBuildBundle = $swOutput
try {
    if ($swSign) {
        $swBuildParent = (Resolve-Path -LiteralPath $swParent).Path
        $swStaging = Join-Path $swBuildParent ('.stillwatch-signing-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $swStaging | Out-Null
        $swBuildMsi = Join-Path $swStaging 'installer.msi'
        $swBuildBundle = Join-Path $swStaging 'unsigned-bundle.exe'
        $swGenerated = Join-Path $swStaging 'payload.wxs'
    }
    $swXml.Save($swGenerated)
    & $swWix @swBuild (Join-Path $PSScriptRoot $swTemplate) $swGenerated -out $swBuildMsi
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    if ($swSign) { Sign-Installer $swBuildMsi }
    if ($swBundle) {
        & $swWix build -arch x64 -ext WixToolset.BootstrapperApplications.wixext/5.0.2 `
            -d "CompanionMsi=$swBuildMsi" -d "ProductVersion=$swVersion" `
            (Join-Path $PSScriptRoot 'windows-companion-bundle.wxs') -out $swBuildBundle
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        if ($swSign) {
            $swEngine = Join-Path $swStaging 'burn-engine.exe'
            $swSignedBundle = Join-Path $swStaging 'signed-bundle.exe'
            & $swWix burn detach $swBuildBundle -engine $swEngine
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            Sign-Installer $swEngine
            & $swWix burn reattach $swBuildBundle -engine $swEngine -out $swSignedBundle
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            Sign-Installer $swSignedBundle
            [IO.File]::Move($swSignedBundle, $swOutput, $true)
        }
    }
    if ($swSign -and !$swBundle) { [IO.File]::Move($swBuildMsi, $swMsi, $true) }
} finally {
    if ($swStaging -and (Test-Path -LiteralPath $swStaging)) {
        $swResolved = (Resolve-Path -LiteralPath $swStaging).Path
        $swDirectory = Get-Item -LiteralPath $swResolved -Force
        if ($swResolved -eq $swStaging -and $swDirectory.Parent.FullName -eq $swBuildParent -and
            -not ($swDirectory.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            Remove-Item -LiteralPath $swResolved -Recurse -Force
        }
    }
}
Get-FileHash -LiteralPath $swOutput -Algorithm SHA256
