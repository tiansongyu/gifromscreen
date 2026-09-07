param([Parameter(Mandatory = $true)][string]$OutputDirectory)

# Independent manual probe. No process other than this owned child tree is stopped.
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw "This probe requires Windows PowerShell 7 and actual WPF." }
$gfsProbeSource = [IO.Path]::GetFullPath($PSScriptRoot)
$gfsProbeOutput = [IO.Path]::GetFullPath($OutputDirectory).TrimEnd('\', '/')
$gfsProbeLogs = $gfsProbeOutput + ".logs"
$gfsProbeAssembly = Join-Path $gfsProbeSource "bin/Release/net9.0-windows/CinemagraphProbe.dll"
foreach ($gfsProbePath in @($gfsProbeSource, $gfsProbeOutput, $gfsProbeAssembly)) {
    if ($gfsProbePath.Contains('"') -or $gfsProbePath.StartsWith('\\')) { throw "Probe paths must be local and cannot contain quotes." }
}
if (-not (Test-Path -LiteralPath $gfsProbeAssembly -PathType Leaf)) { throw "Build CinemagraphProbe in Release first." }
if ((Test-Path -LiteralPath $gfsProbeOutput) -or (Test-Path -LiteralPath $gfsProbeLogs)) {
    throw "Output and logs must both be new. Nothing was removed."
}
if (-not (Test-Path -LiteralPath ([IO.Path]::GetDirectoryName($gfsProbeOutput)) -PathType Container)) {
    throw "The output parent must already exist."
}

function Assert-ProbeFilesBounded {
    param([string]$Root, [long]$Maximum, [int]$MaxFiles)
    $gfsMeasuredBytes = 0L
    $gfsMeasuredFiles = 0
    if (Test-Path -LiteralPath $Root -PathType Container) {
        foreach ($gfsMeasuredFile in Get-ChildItem -LiteralPath $Root -Recurse -File) {
            $gfsMeasuredBytes += $gfsMeasuredFile.Length
            $gfsMeasuredFiles++
            if ($gfsMeasuredBytes -gt $Maximum -or $gfsMeasuredFiles -gt $MaxFiles) {
                throw "Probe file budget exceeded at $Root."
            }
        }
    }
}

$null = New-Item -ItemType Directory -Path $gfsProbeLogs
Push-Location $gfsProbeSource
try {
    $env:GFS_DOTNET_SDK_VERSION = (& dotnet --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $env:GFS_DOTNET_SDK_VERSION -notmatch '^9\.0\.') { throw "A .NET 9 SDK is required." }
    if ($env:GITHUB_SHA -notmatch '^[a-fA-F0-9]{40}$') { throw "Set GITHUB_SHA to the exact source commit before a manual local run." }
    $gfsProbeDotnet = (Get-Command dotnet -CommandType Application).Source
    $gfsProbeArguments = @($gfsProbeAssembly, $gfsProbeSource, $gfsProbeOutput) | ForEach-Object { '"' + $_ + '"' }
    $gfsProbeStdout = Join-Path $gfsProbeLogs "stdout.log"
    $gfsProbeStderr = Join-Path $gfsProbeLogs "stderr.log"
    $gfsProbeProcess = Start-Process -FilePath $gfsProbeDotnet -ArgumentList $gfsProbeArguments -WorkingDirectory $gfsProbeSource `
        -RedirectStandardOutput $gfsProbeStdout -RedirectStandardError $gfsProbeStderr -PassThru -NoNewWindow
    $gfsProbeClock = [Diagnostics.Stopwatch]::StartNew()
    try {
        Write-Output ("CINEMAGRAPH_PROBE_STARTED pid={0} sdk={1}" -f $gfsProbeProcess.Id, $env:GFS_DOTNET_SDK_VERSION)
        while (-not $gfsProbeProcess.WaitForExit(200)) {
            $gfsProbeProcess.Refresh()
            if ($gfsProbeClock.Elapsed.TotalSeconds -ge 120) { throw "120-second probe deadline exceeded." }
            if (-not $gfsProbeProcess.HasExited -and $gfsProbeProcess.WorkingSet64 -gt 512MB) { throw "512 MiB probe working-set limit exceeded." }
            Assert-ProbeFilesBounded -Root $gfsProbeOutput -Maximum 16MB -MaxFiles 4096
            Assert-ProbeFilesBounded -Root $gfsProbeLogs -Maximum 2MB -MaxFiles 4
        }
        $gfsProbeProcess.WaitForExit()
        if ($gfsProbeClock.Elapsed.TotalSeconds -ge 120) { throw "120-second probe deadline exceeded." }
        Assert-ProbeFilesBounded -Root $gfsProbeOutput -Maximum 16MB -MaxFiles 4096
        Assert-ProbeFilesBounded -Root $gfsProbeLogs -Maximum 2MB -MaxFiles 4
        if ($gfsProbeProcess.ExitCode -ne 0) { throw "Probe failed with exit code $($gfsProbeProcess.ExitCode)." }
        if (-not (Test-Path -LiteralPath (Join-Path $gfsProbeOutput "index.json") -PathType Leaf)) { throw "Probe returned without a completed index." }
        Write-Output ("CINEMAGRAPH_PROBE_COMPLETE seconds={0:F3}" -f $gfsProbeClock.Elapsed.TotalSeconds)
    }
    finally {
        if (-not $gfsProbeProcess.HasExited) {
            $gfsProbeProcess.Kill($true)
            $null = $gfsProbeProcess.WaitForExit(5000)
        }
        $gfsProbeProcess.Dispose()
        foreach ($gfsProbeLog in @($gfsProbeStdout, $gfsProbeStderr)) {
            if (Test-Path -LiteralPath $gfsProbeLog -PathType Leaf) {
                Write-Output ("CINEMAGRAPH_PROBE_LOG {0}" -f $gfsProbeLog)
                Get-Content -LiteralPath $gfsProbeLog -Tail 40
            }
        }
    }
}
finally { Pop-Location }
