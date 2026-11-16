# Never launches the candidate or changes Defender protection/exclusions.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$ReportPath,
    [switch]$RequireSignature,
    [switch]$UpdateSignatures
)
$ErrorActionPreference = 'Stop'
$reportFullPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ReportPath)
if ($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path) -eq $reportFullPath) {
    throw 'ReportPath must differ from the candidate path.'
}
$report = [ordered]@{
    schema = 1
    checked_at_utc = [DateTime]::UtcNow.ToString('o')
    passed = $false
    signature_required = [bool]$RequireSignature
}
$failure = $null
$verificationStarted = [DateTime]::UtcNow
try {
    $candidate = (Resolve-Path -LiteralPath $Path).ProviderPath
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { throw 'Candidate is not a file.' }
    $hash = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant()
    $report.sha256 = $hash
    $report.file_name = [IO.Path]::GetFileName($candidate)
    $blocked = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-blocked-artifacts.json') -Raw | ConvertFrom-Json
    if ($blocked.PSObject.Properties.Name -contains $hash) {
        throw "This exact artifact has an unresolved Defender detection: $($blocked.$hash). Submit it for review; do not redeploy it."
    }
    if ($UpdateSignatures) { Update-MpSignature -ErrorAction Stop }
    $status = Get-MpComputerStatus -ErrorAction Stop
    $report.defender = [ordered]@{
        engine = $status.AMEngineVersion
        signatures = $status.AntivirusSignatureVersion
        signatures_updated_utc = $status.AntivirusSignatureLastUpdated.ToUniversalTime().ToString('o')
        antivirus_enabled = $status.AntivirusEnabled
        realtime_enabled = $status.RealTimeProtectionEnabled
        behavior_monitor_enabled = $status.BehaviorMonitorEnabled
    }
    if (-not $status.AntivirusEnabled -or -not $status.RealTimeProtectionEnabled -or -not $status.BehaviorMonitorEnabled) {
        throw 'Defender antivirus, real-time protection and behavior monitoring must be enabled.'
    }
    if ($status.AntivirusSignatureLastUpdated.ToUniversalTime() -lt [DateTime]::UtcNow.AddHours(-24)) {
        throw 'Defender signatures are older than 24 hours. Update signatures and try again.'
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $candidate
    $report.authenticode = [ordered]@{
        status = [string]$signature.Status
        publisher = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { $null }
        thumbprint = if ($signature.SignerCertificate) { $signature.SignerCertificate.Thumbprint } else { $null }
    }
    if ($signature.Status -ne 'Valid' -and $signature.Status -ne 'NotSigned') {
        throw "Invalid Authenticode signature: $($signature.Status)."
    }
    # Use the current Defender platform, rather than an older Windows inbox copy.
    $platformRoot = Join-Path $env:ProgramData 'Microsoft\Windows Defender\Platform'
    $scanner = Get-ChildItem -LiteralPath $platformRoot -Filter MpCmdRun.exe -Recurse |
        Sort-Object { $_.VersionInfo.FileVersionRaw } -Descending | Select-Object -First 1
    if (-not $scanner) { throw 'Current Defender scanner was not found.' }
    $scanOutput = & $scanner.FullName -Scan -ScanType 3 -File $candidate 2>&1
    $scanExit = $LASTEXITCODE
    $report.scan_exit_code = $scanExit
    $report.scan_output = ($scanOutput | Out-String).Trim()
    if ($scanExit -ne 0) { throw "Defender scan failed or detected a threat (exit $scanExit)." }
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { throw 'Candidate disappeared during scanning.' }
    if ((Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
        throw 'Candidate changed during verification.'
    }
    # MpCmdRun may also return 0 when a detection was successfully remediated.
    # Check current detections as well as file survival. Old, remediated path
    # history must not permanently prevent installing a reviewed replacement.
    # The exact unresolved incident hashes remain blocked independently above.
    $activeThreatIds = @(Get-MpThreat -ErrorAction Stop | Where-Object IsActive | Select-Object -ExpandProperty ThreatID)
    $detections = @(Get-MpThreatDetection -ErrorAction Stop | Where-Object {
        $samePath = @($_.Resources | Where-Object { $_.IndexOf($candidate, [StringComparison]::OrdinalIgnoreCase) -ge 0 }).Count -gt 0
        $recent = ($_.InitialDetectionTime -and $_.InitialDetectionTime.ToUniversalTime() -ge $verificationStarted.AddSeconds(-5)) -or
            ($_.LastThreatStatusChangeTime -and $_.LastThreatStatusChangeTime.ToUniversalTime() -ge $verificationStarted.AddSeconds(-5))
        $samePath -and ($recent -or $activeThreatIds -contains $_.ThreatID)
    })
    $report.matching_current_detections = $detections.Count
    if ($detections.Count -gt 0) {
        throw 'Defender has an active or current verification-period detection for this candidate. Review it before deployment.'
    }
    if ($RequireSignature -and $signature.Status -ne 'Valid') {
        throw 'Public Windows releases require a trusted Authenticode signature. Self-signing is insufficient.'
    }
    $report.passed = $true
} catch {
    $failure = $_.Exception.Message
    $report.error = $failure
} finally {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($reportFullPath)) | Out-Null
    $report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $reportFullPath -Encoding UTF8
}
if ($failure) { throw $failure }
Write-Output "Verified SHA-256 $hash. This scan is point-in-time evidence, not a guarantee against future detections."
