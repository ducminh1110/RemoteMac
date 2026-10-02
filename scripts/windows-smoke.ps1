# Windows viewer smoke test: relay + scripted fake agent + the real Win32 viewer in --smoke mode.
# The viewer drives its own window procedures with synthetic input and verifies, end to end,
# that frames are decoded and painted, typing/mouse/keys reach the "remote" app, resize and close work.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --release -p rm-relay -p rm-fakeagent -p rm-viewer
if ($LASTEXITCODE -ne 0) { exit 3 }
New-Item -ItemType Directory -Force -Path out | Out-Null
$env:RM_SESSION_TOKEN = "smoke-" + [guid]::NewGuid().ToString("N")
$port = 47901
$relay = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-relay.exe -ArgumentList "127.0.0.1:$port" -RedirectStandardError out\relay.log
Start-Sleep -Seconds 1
$agent = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-fakeagent.exe -ArgumentList "--relay 127.0.0.1:$port --session smoke-1" -RedirectStandardError out\fakeagent.log
Start-Sleep -Seconds 1
$viewer = Start-Process -PassThru -NoNewWindow -FilePath target\release\remote-mac-viewer.exe -ArgumentList "--relay 127.0.0.1:$port --session smoke-1 --app testapp --smoke" -RedirectStandardError out\viewer.log
if (-not $viewer.WaitForExit(90000)) { Write-Host "viewer timed out"; $viewer.Kill(); $code = 124 } else { $code = $viewer.ExitCode }
foreach ($p in @($agent, $relay)) { if (-not $p.HasExited) { $p.Kill() } }
Write-Host "=== viewer log"; Get-Content out\viewer.log
Write-Host "=== fake agent log"; Get-Content out\fakeagent.log -ErrorAction SilentlyContinue
Write-Host "viewer exit code: $code"
exit $code
