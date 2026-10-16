# legionctl-host.ps1 (todo-151 f): restart or shut down this VM, or ask the Linux host (legionctl) to restart or shut
# down the WHOLE machine. Copy it into Windows (for example through a windows.vm.shares folder) and run it as
#   powershell -ExecutionPolicy Bypass -File legionctl-host.ps1 [-Action <name>]
# A host action needs two approvals on the Linux desktop: legionctl's question, then the host's own (polkit) dialog.
param(
    [ValidateSet('', 'vm-restart', 'vm-shutdown', 'shutdown', 'restart', 'restart-windows')]
    [string]$Action = ''
)

# .NET Framework does not open \\.\ device paths: the port is opened with CreateFile
Add-Type -Namespace Legionctl -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern Microsoft.Win32.SafeHandles.SafeFileHandle CreateFile(
    string name, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
'@
# GENERIC_READ | GENERIC_WRITE, OPEN_EXISTING
$handle = [Legionctl.Native]::CreateFile('\\.\Global\org.legionctl.host.0', 0xC0000000, 0, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
if ($handle.IsInvalid) {
    Write-Host 'This Windows does not run in the legionctl VM (no host channel, or the virtio-serial driver is missing).'
    Write-Host "Windows' own Restart and Shut down act on the whole machine."
    exit 1
}

$actions = [ordered]@{
    'vm-restart'      = 'Restart this VM only (Linux keeps running)'
    'vm-shutdown'     = 'Shut down this VM only (Linux keeps running)'
    'shutdown'        = 'WHOLE MACHINE: shut down (Linux and this VM)'
    'restart'         = 'WHOLE MACHINE: restart into Linux'
    'restart-windows' = 'WHOLE MACHINE: restart into Windows on the machine itself (bare metal)'
}
if ($Action -eq '') {
    Write-Host 'This Windows runs in a VM on Linux (legionctl).'
    $keys = @($actions.Keys)
    for ($i = 0; $i -lt $keys.Count; $i++) { Write-Host ('{0}  {1}' -f ($i + 1), $actions[$keys[$i]]) }
    $choice = Read-Host 'Number (Enter: nothing)'
    if ($choice -notmatch '^[1-5]$') { exit 0 }
    $Action = $keys[[int]$choice - 1]
}

switch ($Action) {
    'vm-restart' { $handle.Close(); Restart-Computer; exit 0 }
    'vm-shutdown' { $handle.Close(); Stop-Computer; exit 0 }
}

$stream = New-Object System.IO.FileStream($handle, [System.IO.FileAccess]::ReadWrite)
$writer = New-Object System.IO.StreamWriter($stream)
$writer.NewLine = "`n"
$writer.AutoFlush = $true
$reader = New-Object System.IO.StreamReader($stream)
Write-Host ('Asked the Linux host: {0}. Approve it on the Linux desktop.' -f $actions[$Action])
$writer.WriteLine($Action)
$reply = $reader.ReadLine()
Write-Host $reply
$stream.Close()
if ($reply -like 'ok:*') { exit 0 } else { exit 2 }
