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
exit $failed
