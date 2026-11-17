# Installs only the receive-only companion for the signed-in user. Does not
# replace the KVM, add a login trigger, or change Defender/firewall settings.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Path,
    [Parameter(Mandatory=$true)][string]$Peer,
    [Parameter(Mandatory=$true)][string]$MonitorId
)
$ErrorActionPreference='Stop'
$identity=[Security.Principal.WindowsIdentity]::GetCurrent().Name
if ($identity -ne (Get-CimInstance Win32_ComputerSystem).UserName) {
    throw 'Install as the user signed in to the Windows desktop.'
}
$root=Join-Path $env:LOCALAPPDATA 'kayiver/display-receiver'
$exe=Join-Path $root 'kayiver-display-receiver.exe'
$taskName='kayiver-display-receiver'
if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    throw 'Receiver is already installed; stop and inspect the existing receiver before updating.'
}
if (Test-Path -LiteralPath $exe) { throw 'Existing receiver must be inspected before replacement.' }
New-Item -ItemType Directory -Force $root|Out-Null
# Scan original bytes before copying, then independently verify the installed
# path. Refuse unresolved incident samples through the ordinary verifier.
& (Join-Path $PSScriptRoot 'verify-windows-artifact.ps1') -Path $Path -ReportPath (Join-Path $root 'candidate-verification.json')
Copy-Item -LiteralPath $Path -Destination $exe
& (Join-Path $PSScriptRoot 'verify-windows-artifact.ps1') -Path $exe -ReportPath (Join-Path $root 'verification.json')
@{peer=$Peer;monitor_id=$MonitorId}|ConvertTo-Json|Set-Content (Join-Path $root 'selection.json') -Encoding UTF8
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'display-notification-session.ps1') -Destination $root
$launcher=Join-Path $root 'start.ps1'
@'
$ErrorActionPreference='Stop'
$exe=Join-Path $PSScriptRoot 'kayiver-display-receiver.exe'
$v=Get-Content -Raw (Join-Path $PSScriptRoot 'verification.json')|ConvertFrom-Json
if(-not $v.passed -or (Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant() -ne $v.sha256){throw 'Receiver bytes no longer match verification'}
$s=Get-Content -Raw (Join-Path $PSScriptRoot 'selection.json')|ConvertFrom-Json
$guard=Join-Path $PSScriptRoot 'display-notification-session.ps1'
Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList ('-NoProfile -NonInteractive -WindowStyle Hidden -File "'+$guard+'" -OwnerPid '+$PID) -WindowStyle Hidden
& $exe --peer $s.peer --monitor-id $s.monitor_id > (Join-Path $PSScriptRoot 'status.log') 2> (Join-Path $PSScriptRoot 'errors.log')
exit $LASTEXITCODE
'@|Set-Content -LiteralPath $launcher -Encoding UTF8
$action=New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -WindowStyle Hidden -File "'+$launcher+'"') -WorkingDirectory $root
$principal=New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
$settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Description 'Kayiver extra display: explicitly launched receiver, no automatic startup or input sharing'|Out-Null
$menu=Join-Path ([Environment]::GetFolderPath('Programs')) 'Kayiver Extra Display.lnk'
$shell=New-Object -ComObject WScript.Shell
$shortcut=$shell.CreateShortcut($menu)
$shortcut.TargetPath=Join-Path $env:WINDIR 'System32/schtasks.exe'
$shortcut.Arguments='/Run /TN kayiver-display-receiver'
$shortcut.Description='Accept the selected Mac on this monitor. Start Extra display on the Mac next. Local Escape stops the viewer.'
$shortcut.WindowStyle=7
$shortcut.Save()
Write-Output 'Receiver installed with a Start Menu shortcut. No automatic startup was enabled.'
