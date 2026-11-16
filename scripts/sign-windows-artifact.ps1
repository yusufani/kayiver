# Signs a future release using an existing publisher certificate. Never installs
# certificates, changes trust, executes the candidate, or modifies the input.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$OutputPath,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{40}$')][string]$Thumbprint,
    [Parameter(Mandatory = $true)][string]$ReportPath
)
$ErrorActionPreference = 'Stop'
$candidate = (Resolve-Path -LiteralPath $Path).ProviderPath
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
$report = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ReportPath)
if ($candidate -eq $output -or $candidate -eq $report -or $output -eq $report) {
    throw 'Input, signed output and report must be separate files.'
}
if ((Test-Path -LiteralPath $output) -or (Test-Path -LiteralPath $report)) {
    throw 'Output or report already exists; refusing to overwrite evidence.'
}
$hash = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant()
$blocked = Get-Content -Raw (Join-Path $PSScriptRoot 'windows-blocked-artifacts.json') | ConvertFrom-Json
if ($blocked.PSObject.Properties.Name -contains $hash) {
    throw 'Unresolved incident sample: obtain Microsoft review before signing or redeploying these bytes.'
}
$certificate = Get-Item -LiteralPath "Cert:\CurrentUser\My\$Thumbprint"
if (-not $certificate.HasPrivateKey -or $certificate.NotAfter -le (Get-Date) -or
    $certificate.NotBefore -gt (Get-Date)) { throw 'A current publisher certificate with its private key is required.' }
$codeSigning = @($certificate.Extensions | Where-Object { $_ -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension] } |
    ForEach-Object { $_.EnhancedKeyUsages } | Where-Object { $_.Value -eq '1.3.6.1.5.5.7.3.3' })
if ($codeSigning.Count -eq 0) { throw 'Certificate is not valid for code signing.' }
Copy-Item -LiteralPath $candidate -Destination $output
# No private-key export or passwords in arguments. A token/provider may show
# its own authorization prompt. A self-signed certificate will fail validation
# on a normal target and is not a substitute for a trusted public publisher.
$signature = Set-AuthenticodeSignature -LiteralPath $output -Certificate $certificate `
    -HashAlgorithm SHA256 -TimestampServer 'http://timestamp.digicert.com'
if ($signature.Status -ne 'Valid') { throw "Signing failed validation: $($signature.Status). Output is not approved for distribution." }
& (Join-Path $PSScriptRoot 'verify-windows-artifact.ps1') -Path $output -ReportPath $report -RequireSignature
if ((Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
    throw 'Original input changed during signing.'
}
Write-Output 'Signed output passed exact-file verification. Runtime acceptance still requires isolated testing before installation.'
