param(
    [Parameter(Mandatory = $true)][string]$FixturePath,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)

# Runs only the repository's headless reference generator, never a desktop app.
# No existing output is removed or overwritten; only this process tree is owned.
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$gfsProjectDirectory = Join-Path $PSScriptRoot "wpf_reference"
$gfsFixturePath = [IO.Path]::GetFullPath($FixturePath)
$gfsOutputDirectory = [IO.Path]::GetFullPath($OutputDirectory).TrimEnd('\', '/')
$gfsLogsDirectory = $gfsOutputDirectory + ".logs"
$gfsAssembly = Join-Path $gfsProjectDirectory "bin/Release/net9.0-windows/WpfReference.dll"
foreach ($gfsPath in @($gfsFixturePath, $gfsOutputDirectory, $gfsAssembly)) {
    if ($gfsPath.Contains('"')) { throw "Reference paths cannot contain quotes." }
}
if (-not (Test-Path -LiteralPath $gfsFixturePath -PathType Leaf)) { throw "Fixture definition is missing." }
if (-not (Test-Path -LiteralPath $gfsAssembly -PathType Leaf)) { throw "Build WpfReference in Release mode first." }
if ((Test-Path -LiteralPath $gfsOutputDirectory) -or (Test-Path -LiteralPath $gfsLogsDirectory)) {
    throw "The reference output and log directories must both be new. Nothing was deleted."
}
$null = New-Item -ItemType Directory -Path $gfsLogsDirectory

Push-Location $gfsProjectDirectory
try {
    # Honor this directory's global.json, not another SDK from the runner image.
    $env:GFS_DOTNET_SDK_VERSION = (& dotnet --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $env:GFS_DOTNET_SDK_VERSION -notmatch '^9\.0\.') {
        throw "The reference must use a .NET 9 SDK."
    }
    $gfsDotnet = (Get-Command dotnet -CommandType Application).Source
    $gfsArguments = @($gfsAssembly, $gfsFixturePath, $gfsOutputDirectory) | ForEach-Object { '"' + $_ + '"' }
    $gfsStdout = Join-Path $gfsLogsDirectory "stdout.log"
    $gfsStderr = Join-Path $gfsLogsDirectory "stderr.log"
    $gfsProcess = Start-Process -FilePath $gfsDotnet -ArgumentList $gfsArguments -WorkingDirectory $gfsProjectDirectory `
        -RedirectStandardOutput $gfsStdout -RedirectStandardError $gfsStderr -PassThru -NoNewWindow
    $gfsDeadline = [Diagnostics.Stopwatch]::StartNew()
    $gfsFailure = $null
    try {
        Write-Output ("REFERENCE_STARTED pid={0} sdk={1}" -f $gfsProcess.Id, $env:GFS_DOTNET_SDK_VERSION)
        while (-not $gfsProcess.WaitForExit(200)) {
            $gfsProcess.Refresh()
            if ($gfsDeadline.Elapsed.TotalSeconds -ge 120) { $gfsFailure = "120-second reference deadline exceeded."; break }
            if (-not $gfsProcess.HasExited -and $gfsProcess.WorkingSet64 -gt 512MB) {
                $gfsFailure = "512 MiB reference working-set limit exceeded."; break
            }
            $gfsLogBytes = 0L
            foreach ($gfsLog in @($gfsStdout, $gfsStderr)) {
                if (Test-Path -LiteralPath $gfsLog -PathType Leaf) { $gfsLogBytes += (Get-Item -LiteralPath $gfsLog).Length }
            }
            if ($gfsLogBytes -gt 2MB) { $gfsFailure = "2 MiB reference log limit exceeded."; break }
        }
        if ($gfsFailure) {
            if (-not $gfsProcess.HasExited) { $gfsProcess.Kill($true) }
            $null = $gfsProcess.WaitForExit(5000)
            throw $gfsFailure
        }
        $gfsProcess.WaitForExit()
        $gfsCode = $gfsProcess.ExitCode
        Write-Output ("REFERENCE_EXIT pid={0} status={1} seconds={2:F3}" -f $gfsProcess.Id, $gfsCode, $gfsDeadline.Elapsed.TotalSeconds)
        if ($gfsCode -ne 0) { throw "WPF generator failed with exit code $gfsCode." }
        if (-not (Test-Path -LiteralPath (Join-Path $gfsOutputDirectory "index.json") -PathType Leaf)) {
            throw "Reference generator exited without an index."
        }
    }
    finally {
        if (-not $gfsProcess.HasExited) {
            $gfsProcess.Kill($true)
            $null = $gfsProcess.WaitForExit(5000)
        }
        $gfsProcess.Dispose()
        foreach ($gfsLog in @($gfsStdout, $gfsStderr)) {
            if (Test-Path -LiteralPath $gfsLog -PathType Leaf) {
                Write-Output ("REFERENCE_LOG {0}" -f $gfsLog)
                Get-Content -LiteralPath $gfsLog -Tail 100
            }
        }
    }
}
finally { Pop-Location }
