param(
    [Parameter(Mandatory)][string]$Payload,
    [Parameter(Mandatory)][string]$Output,
    [Parameter(Mandatory)][string]$Wix,
    [switch]$Companion
)

$ErrorActionPreference = 'Stop'
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
$swXml.Save($swGenerated)
$swBuild = @('build', '-arch', 'x64', '-b', $PSScriptRoot, '-d', "Payload=$swPayload")
if (-not $Companion) { $swBuild += @('-ext', 'WixToolset.Util.wixext/5.0.2') }
& $swWix @swBuild (Join-Path $PSScriptRoot $swTemplate) $swGenerated -out $swMsi
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($swBundle) {
    & $swWix build -arch x64 -ext WixToolset.BootstrapperApplications.wixext/5.0.2 `
        -d "CompanionMsi=$swMsi" (Join-Path $PSScriptRoot 'windows-companion-bundle.wxs') -out $swOutput
}
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Get-FileHash -LiteralPath $swOutput -Algorithm SHA256
