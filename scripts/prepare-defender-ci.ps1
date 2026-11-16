# Strengthen protection on an ephemeral hosted scan runner only. Never deploy
# this script to the user's desktop or use it to allow a detected sample.
[CmdletBinding()]
param([string]$ReportPath = 'defender-ci-preparation.json')
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:KAYIVER_EPHEMERAL_SCAN_HOST -ne 'github-hosted') {
    throw 'Defender preparation is restricted to an ephemeral GitHub-hosted runner.'
}
$report = [ordered]@{schema=1; checked_at_utc=[DateTime]::UtcNow.ToString('o'); signature_updates=@()}
$failure = $null
try {
    $service=Get-Service WinDefend
    if ($service.Status -ne 'Running') { Start-Service WinDefend }
    $status = Get-MpComputerStatus
    $preferences = Get-MpPreference
    $report.before = [ordered]@{
        realtime=$status.RealTimeProtectionEnabled
        behavior_monitor=$status.BehaviorMonitorEnabled
        signatures=$status.AntivirusSignatureVersion
        signatures_updated_utc=$(if ($status.AntivirusSignatureLastUpdated) {$status.AntivirusSignatureLastUpdated.ToUniversalTime().ToString('o')}else{$null})
        excluded_paths=@($preferences.ExclusionPath | Where-Object {$_}).Count
        excluded_extensions=@($preferences.ExclusionExtension | Where-Object {$_}).Count
        excluded_processes=@($preferences.ExclusionProcess | Where-Object {$_}).Count
    }
    # Hosted images may exclude C:\ and D:\ for build performance. A scan
    # runner must not inherit those exclusions. Removing them strengthens
    # protection; the independent verifier checks the resulting file policy.
    $paths=@($preferences.ExclusionPath | Where-Object {$_})
    $extensions=@($preferences.ExclusionExtension | Where-Object {$_})
    $processes=@($preferences.ExclusionProcess | Where-Object {$_})
    if ($paths.Count) { Remove-MpPreference -ExclusionPath $paths }
    if ($extensions.Count) { Remove-MpPreference -ExclusionExtension $extensions }
    if ($processes.Count) { Remove-MpPreference -ExclusionProcess $processes }
    if (-not $status.RealTimeProtectionEnabled) { Set-MpPreference -DisableRealtimeMonitoring $false }
    if (-not $status.BehaviorMonitorEnabled) { Set-MpPreference -DisableBehaviorMonitoring $false }
    # Use Microsoft's supported direct source first, avoiding a disabled
    # Windows Update service in the image. No cloud/sample-consent setting
    # is changed. The verifier still requires definitions newer than 24h.
    foreach ($source in @('MMPC','MicrosoftUpdateServer')) {
        try {
            Update-MpSignature -UpdateSource $source -ErrorAction Stop
            $report.signature_updates += [ordered]@{source=$source;success=$true}
            break
        } catch {
            $report.signature_updates += [ordered]@{source=$source;success=$false;error=$_.Exception.Message}
            Write-Warning "Signature update through $source failed; the exact-file verifier will still enforce freshness."
        }
    }
    $status=Get-MpComputerStatus
    $report.after=[ordered]@{
        realtime=$status.RealTimeProtectionEnabled
        behavior_monitor=$status.BehaviorMonitorEnabled
        signatures=$status.AntivirusSignatureVersion
        signatures_updated_utc=$(if ($status.AntivirusSignatureLastUpdated) {$status.AntivirusSignatureLastUpdated.ToUniversalTime().ToString('o')}else{$null})
    }
} catch {
    $failure=$_.Exception.Message
    $report.error=$failure
} finally {
    $report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReportPath -Encoding UTF8
}
if ($failure) { throw $failure }
