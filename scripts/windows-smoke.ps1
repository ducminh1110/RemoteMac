# Windows viewer smoke test: relay + scripted fake agent + the real Win32 viewer in --smoke mode,
# once per renderer (Direct3D 11 default, GDI fallback). The viewer drives its own window procedures
# with synthetic input and verifies end to end: frames decoded and painted, typing/mouse/keys reach the
# "remote" app, resize, taskbar identity, clipboard both ways, close.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --release -p rm-relay -p rm-fakeagent -p rm-viewer
if ($LASTEXITCODE -ne 0) { exit 3 }
New-Item -ItemType Directory -Force -Path out | Out-Null
$failed = 0
# the file the smoke "picks" in the Windows Open dialog (700 KiB: several upload chunks)
$pick = Join-Path $PWD "out\smoke-pick.bin"
[IO.File]::WriteAllBytes($pick, (New-Object byte[] 716800))
$env:RM_SMOKE_PICK_FILE = $pick
$port = 47901
foreach ($renderer in @("d3d11", "gdi")) {
  $port++
  $env:RM_SESSION_TOKEN = "smoke-" + [guid]::NewGuid().ToString("N")
  $env:RM_SHORTCUT_DIR = Join-Path $PWD "out\shortcuts-$renderer"
  $relay = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-relay.exe -ArgumentList "127.0.0.1:$port" -RedirectStandardError "out\relay-$renderer.log"
  Start-Sleep -Seconds 1
  $agent = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-fakeagent.exe -ArgumentList "--relay 127.0.0.1:$port --session smoke-$renderer" -RedirectStandardError "out\fakeagent-$renderer.log"
  Start-Sleep -Seconds 1
  $viewer = Start-Process -PassThru -NoNewWindow -FilePath target\release\remote-mac-viewer.exe -ArgumentList "--relay 127.0.0.1:$port --session smoke-$renderer --app testapp --renderer $renderer --smoke" -RedirectStandardError "out\viewer-$renderer.log"
  if (-not $viewer.WaitForExit(90000)) { Write-Host "viewer timed out"; $viewer.Kill(); $code = 124 } else { $code = $viewer.ExitCode }
  foreach ($p in @($agent, $relay)) { if (-not $p.HasExited) { $p.Kill() } }
  Write-Host "=== viewer log ($renderer)"; Get-Content "out\viewer-$renderer.log"
  Write-Host "viewer exit code ($renderer): $code"
  if ($code -ne 0) { $failed++ }
  $left = @(Get-ChildItem -Path $env:RM_SHORTCUT_DIR -Filter *.lnk -ErrorAction SilentlyContinue)
  if ($left.Count -ne 0) { Write-Host "shortcuts left after the viewer exited: $($left.Name)"; $failed++ }
}

# Showcase against the fake agent: screenshots of its apps in Mac-style windows (artifact).
$port++
$env:RM_SESSION_TOKEN = "showcase-" + [guid]::NewGuid().ToString("N")
$relay = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-relay.exe -ArgumentList "127.0.0.1:$port" -RedirectStandardError "out\relay-showcase.log"
Start-Sleep -Seconds 1
$agent = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-fakeagent.exe -ArgumentList "--relay 127.0.0.1:$port --session showcase" -RedirectStandardError "out\fakeagent-showcase.log"
Start-Sleep -Seconds 1
$env:RM_STATS = "1"   # the stats overlay in these screenshots
$env:RM_UI_GALLERY = "1"   # and MacBridge's own surfaces (loading window, glass menu, search, banner)
& ./scripts/windows-showcase.ps1 -Relay "127.0.0.1:$port" -Session showcase -Apps "testapp,notes" -Out "out\showcase-fake" -Settle 2 -TimeoutSec 120
if ($LASTEXITCODE -ne 0) { $failed++ }
Remove-Item Env:RM_STATS
Remove-Item Env:RM_UI_GALLERY
# the video went over UDP (FEC) and the stats line names the decoder and pacing
$vlog = Get-Content "out\showcase-fake\viewer.log" -ErrorAction SilentlyContinue
$vlog | Select-String -Pattern "video decoder|frame pacing|stream:" | Select-Object -First 6 | ForEach-Object { Write-Host $_.Line }
if (-not ($vlog | Select-String -Pattern "Network UDP\+FEC")) { Write-Host "video did not use UDP in the showcase"; $failed++ }
foreach ($p in @($agent, $relay)) { if (-not $p.HasExited) { $p.Kill() } }

# the connect window as it first shows (no ID given): a picture of it in the log
try {
  Add-Type -AssemblyName System.Drawing
  Add-Type -Namespace W -Name U -MemberDefinition @"
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
public struct RECT { public int Left, Top, Right, Bottom; }
"@
  $cv = Start-Process -PassThru -NoNewWindow -FilePath target\release\remote-mac-viewer.exe -RedirectStandardError "out\viewer-connect.log"
  $h = [IntPtr]::Zero
  for ($i = 0; $i -lt 120 -and $h -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 250; $h = [W.U]::FindWindow("RmConnect", $null) }
  if ($h -ne [IntPtr]::Zero) {
    [W.U]::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Seconds 2
    $r = New-Object W.U+RECT
    [W.U]::GetWindowRect($h, [ref]$r) | Out-Null
    $bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
    $ms = New-Object IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $b64 = [Convert]::ToBase64String($ms.ToArray())
    Write-Host "GALLERY-BEGIN 16-connect"
    for ($i = 0; $i -lt $b64.Length; $i += 4000) { Write-Host $b64.Substring($i, [Math]::Min(4000, $b64.Length - $i)) }
    Write-Host "GALLERY-END"
    $g.Dispose(); $bmp.Dispose(); $ms.Dispose()
  } else {
    Write-Host "the connect window did not show (viewer exited: $($cv.HasExited))"
    Get-Content "out\viewer-connect.log" -ErrorAction SilentlyContinue | Select-Object -First 40
  }
  if (-not $cv.HasExited) { $cv.Kill() }
} catch { Write-Host "connect window picture failed: $_" }
exit $failed
