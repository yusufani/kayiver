# The receiver launcher owns this helper. Scope Windows toast delivery to
# the lifetime of its visible presentation window in the interactive session.
param([int]$OwnerPid, [switch]$Recover)
$ErrorActionPreference='Stop'
$quiet='Microsoft.QuietHoursProfile.AlarmsOnly'
$runOnce='HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce'
$entry='KayiverRestoreDisplayNotifications'
$journal=Join-Path $PSScriptRoot 'quiet-hours-restore.json'
$mutex=[Threading.Mutex]::new($false,'Local\KayiverDisplayNotificationSession')
$locked=$false

function Restore-Value($keyPath,$valueName,$receipt) {
    if(Test-Path -LiteralPath $receipt){
        $saved=Get-Content -LiteralPath $receipt -Raw|ConvertFrom-Json
        if((Test-Path -LiteralPath $keyPath) -and (Get-Item -LiteralPath $keyPath).GetValue($valueName) -eq 0){
            if($saved.present){
                New-ItemProperty -LiteralPath $keyPath -Name $valueName -PropertyType DWord -Value $saved.value -Force|Out-Null
            }else{
                Remove-ItemProperty -LiteralPath $keyPath -Name $valueName
            }
        }
        Remove-Item -LiteralPath $receipt
    }
}

function Restore-Preference {
    if(Test-Path -LiteralPath $journal){
        $previous=Get-Content -LiteralPath $journal -Raw|ConvertFrom-Json
        if([KayiverQuietHours]::Read() -eq $quiet){
            [KayiverQuietHours]::Select($previous.profile)
        }
        Remove-Item -LiteralPath $journal
    }
    Restore-Value 'HKCU:\Software\Microsoft\Windows\CurrentVersion\PushNotifications' 'ToastEnabled' (Join-Path $PSScriptRoot 'windows-toast-restore.json')
    # Upgrade an interrupted session from the sender-specific preview.
    Restore-Value 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\com.nvidia.nvapp' 'Enabled' (Join-Path $PSScriptRoot 'nvidia-notification-restore.json')
    if(Test-Path -LiteralPath $runOnce){
        Remove-ItemProperty -LiteralPath $runOnce -Name $entry -ErrorAction SilentlyContinue
    }
}

function Suspend-Preference {
    $previous=[KayiverQuietHours]::Read()
    if(!$previous -or $previous -eq $quiet){return}
    # Arm recovery before changing Windows. A normal exit restores immediately.
    # If the computer powers off mid-session, restore at the next user login.
    $temp=$journal+'.tmp'
    @{profile=$previous}|ConvertTo-Json|Set-Content -LiteralPath $temp -Encoding UTF8
    [IO.File]::Move($temp,$journal)
    if(!(Test-Path -LiteralPath $runOnce)){New-Item -Path $runOnce -Force|Out-Null}
    $recoverCommand='"'+(Join-Path $PSHOME 'powershell.exe')+'" -NoProfile -NonInteractive -WindowStyle Hidden -File "'+$PSCommandPath+'" -Recover'
    New-ItemProperty -LiteralPath $runOnce -Name $entry -PropertyType String -Value $recoverCommand -Force|Out-Null
    [KayiverQuietHours]::Select($quiet)
}

try{
    try{$locked=$mutex.WaitOne(3000)}catch [Threading.AbandonedMutexException]{$locked=$true}
    if(!$locked){return}
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class KayiverQuietHours {
    [ComImport, Guid("6bff4732-81ec-4ffb-ae67-b6c1bc29631f"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IQuietHoursSettings {
        void GetProfile([MarshalAs(UnmanagedType.LPWStr)] out string profile);
        void SetProfile([MarshalAs(UnmanagedType.LPWStr)] string profile);
    }
    private static IQuietHoursSettings Open() {
        return (IQuietHoursSettings)Activator.CreateInstance(Type.GetTypeFromCLSID(
            new Guid("f53321fa-34f8-4b7f-b9a3-361877cb94cf"),true));
    }
    public static string Read() {
        var settings=Open();
        try {string profile; settings.GetProfile(out profile); return profile;}
        finally {Marshal.ReleaseComObject(settings);}
    }
    public static void Select(string profile) {
        var settings=Open();
        try {settings.SetProfile(profile);}
        finally {Marshal.ReleaseComObject(settings);}
    }
}
'@
    Restore-Preference
    if($Recover){return}
    $owner=Get-Process -Id $OwnerPid -ErrorAction Stop
    $null=$owner.Handle
    $receiver=Join-Path $PSScriptRoot 'kayiver-display-receiver.exe'
    Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes,WindowsBase
    $uiElement=[System.Windows.Automation.AutomationElement]
    $toastRoot=[System.Windows.Automation.PropertyCondition]::new($uiElement::AutomationIdProperty,'ToastCenterScrollViewer')
    $dismiss=[System.Windows.Automation.PropertyCondition]::new($uiElement::AutomationIdProperty,'DismissButton')
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class KayiverDisplayWindow {
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    static extern IntPtr FindWindowW(string name, string title);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int left,top,right,bottom; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern IntPtr FindWindowEx(IntPtr parent,IntPtr after,string cls,string title);
    public static System.Collections.Generic.List<IntPtr> ToastCandidates() {
        var result=new System.Collections.Generic.List<IntPtr>();
        Rect panel;
        var viewer=FindWindowW("KayiverExtendedDisplay",null);
        if(!IsWindowVisible(viewer) || !GetWindowRect(viewer,out panel))return result;
        var hwnd=IntPtr.Zero;
        while((hwnd=FindWindowEx(IntPtr.Zero,hwnd,"Windows.UI.Core.CoreWindow",null))!=IntPtr.Zero){
            Rect r;
            if(!IsWindowVisible(hwnd) || !GetWindowRect(hwnd,out r) || r.left>=panel.right ||
               r.top>=panel.bottom || r.right<=panel.left || r.bottom<=panel.top)continue;
            uint pid;GetWindowThreadProcessId(hwnd,out pid);
            try {
                using(var p=System.Diagnostics.Process.GetProcessById((int)pid)){
                    if(p.ProcessName=="ShellExperienceHost" || p.ProcessName=="ShellHost")result.Add(hwnd);
                }
            } catch(ArgumentException) {}
        }
        return result;
    }
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
        if($activeId){
            # A banner that predates Quiet Hours can remain above the viewer.
            # Select only the shell toast chrome on this panel. Localized titles,
            # message contents and sender-specific action buttons are irrelevant.
            foreach($handle in [KayiverDisplayWindow]::ToastCandidates()){
                try{
                    $window=$uiElement::FromHandle($handle)
                    $center=$window.FindFirst([System.Windows.Automation.TreeScope]::Children,$toastRoot)
                    if(!$center){continue}
                    $buttons=$center.FindAll([System.Windows.Automation.TreeScope]::Descendants,$dismiss)
                    foreach($button in $buttons){
                        if(!$button.Current.IsOffscreen -and $button.Current.IsEnabled){
                            $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                        }
                    }
                }catch [System.Windows.Automation.ElementNotAvailableException]{
                    # The toast expired while being inspected.
                }catch [System.InvalidOperationException]{
                    # The shell can replace the controls during an animation.
                }catch [System.Runtime.InteropServices.COMException]{
                    # The shell can restart while the receiver stays open.
                }
            }
        }
        if($owner.WaitForExit(400)){break}
    }
}finally{
    if($locked){try{Restore-Preference}finally{$mutex.ReleaseMutex()}}
    $mutex.Dispose()
}
