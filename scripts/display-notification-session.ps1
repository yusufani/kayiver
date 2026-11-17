# The receiver launcher owns this helper. Scope the NVIDIA App preference to
# the lifetime of its visible presentation window in the interactive session.
param([int]$OwnerPid, [switch]$Recover)
$ErrorActionPreference='Stop'
$sender='HKCU:\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\com.nvidia.nvapp'
$runOnce='HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce'
$entry='KayiverRestoreDisplayNotifications'
$journal=Join-Path $PSScriptRoot 'nvidia-notification-restore.json'
$mutex=[Threading.Mutex]::new($false,'Local\KayiverDisplayNotificationSession')
$locked=$false

function Restore-Preference {
    if(Test-Path -LiteralPath $journal){
        $saved=Get-Content -LiteralPath $journal -Raw|ConvertFrom-Json
        if((Test-Path -LiteralPath $sender) -and (Get-Item -LiteralPath $sender).GetValue('Enabled') -eq 0){
            if($saved.present){
                New-ItemProperty -LiteralPath $sender -Name Enabled -PropertyType DWord -Value $saved.value -Force|Out-Null
            }else{
                Remove-ItemProperty -LiteralPath $sender -Name Enabled
            }
        }
        Remove-Item -LiteralPath $journal
    }
    if(Test-Path -LiteralPath $runOnce){
        Remove-ItemProperty -LiteralPath $runOnce -Name $entry -ErrorAction SilentlyContinue
    }
}

function Suspend-Preference {
    if(!(Test-Path -LiteralPath $sender)){return}
    $key=Get-Item -LiteralPath $sender
    $previous=$key.GetValue('Enabled')
    if($null -ne $previous -and ($key.GetValueKind('Enabled') -ne 'DWord' -or $previous -eq 0)){return}
    # Arm recovery before changing Windows. A normal exit restores immediately.
    # If the computer powers off mid-session, restore at the next user login.
    $temp=$journal+'.tmp'
    @{present=($null -ne $previous);value=$previous}|ConvertTo-Json|Set-Content -LiteralPath $temp -Encoding UTF8
    [IO.File]::Move($temp,$journal)
    if(!(Test-Path -LiteralPath $runOnce)){New-Item -Path $runOnce -Force|Out-Null}
    $recoverCommand='"'+(Join-Path $PSHOME 'powershell.exe')+'" -NoProfile -NonInteractive -WindowStyle Hidden -File "'+$PSCommandPath+'" -Recover'
    New-ItemProperty -LiteralPath $runOnce -Name $entry -PropertyType String -Value $recoverCommand -Force|Out-Null
    New-ItemProperty -LiteralPath $sender -Name Enabled -PropertyType DWord -Value 0 -Force|Out-Null
}

try{
    try{$locked=$mutex.WaitOne(3000)}catch [Threading.AbandonedMutexException]{$locked=$true}
    if(!$locked){return}
    Restore-Preference
    if($Recover){return}
    $owner=Get-Process -Id $OwnerPid -ErrorAction Stop
    $null=$owner.Handle
    $receiver=Join-Path $PSScriptRoot 'kayiver-display-receiver.exe'
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class KayiverDisplayWindow {
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    static extern IntPtr FindWindowW(string name, string title);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    public static uint VisibleProcess() {
        IntPtr hwnd=FindWindowW("KayiverExtendedDisplay",null);
        uint pid=0;
        if(hwnd!=IntPtr.Zero && IsWindowVisible(hwnd)) GetWindowThreadProcessId(hwnd,out pid);
        return pid;
    }
}
'@
    $activeId=0
    while(!$owner.HasExited){
        $visibleId=[KayiverDisplayWindow]::VisibleProcess()
        if($visibleId -ne $activeId){
            if($activeId){Restore-Preference}
            $activeId=0
            if($visibleId){
                $process=Get-Process -Id $visibleId -ErrorAction SilentlyContinue
                if($process -and $process.Path -eq $receiver){
                    Suspend-Preference
                    $activeId=$visibleId
                }
            }
        }
        if($owner.WaitForExit(400)){break}
    }
}finally{
    if($locked){try{Restore-Preference}finally{$mutex.ReleaseMutex()}}
    $mutex.Dispose()
}
