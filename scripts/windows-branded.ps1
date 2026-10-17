# A provider's Taro (Taro Enterprise) as its customers see it: the official viewer, unmodified,
# renamed Acme.exe, with a provider\ folder next to it (provider.json and icon.png), against the
# fake agent. Its relay (one on this PC that requires the provider's key) and that key come from
# provider.json alone: the viewer is started with no --relay and no RM_RELAY_KEY, so connecting
# at all proves both were taken from there. Screenshots of the launcher, Taro's own surfaces
# (RM_UI_GALLERY) and the connect window go to out\branded-client. Also: a provider folder with a
# broken provider.json stops the viewer (exit 4) instead of running as the standard Taro.
# Run after windows-smoke.ps1 (it builds the release binaries).
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --release -p rm-relay -p rm-fakeagent -p rm-viewer
if ($LASTEXITCODE -ne 0) { exit 3 }
Add-Type -AssemblyName System.Drawing
$failed = 0
$out = "out\branded-client"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $out, "out\Acme", "out\Broken"
New-Item -ItemType Directory -Force -Path $out, "out\Acme\provider", "out\Broken\provider" | Out-Null
$port = 47950
$key = "acme-ci-key-0123456789abcdef"

# the package, as the provider's wizard builds it
Copy-Item target\release\remote-mac-viewer.exe out\Acme\Acme.exe
$provider = [ordered]@{
  schema = "taro.provider/v1"; provider_id = "acme-cloud"
  product = [ordered]@{ name = "Acme Connect"; short_name = "Acme"; vendor = "Acme Cloud Ltd"; support_url = "https://acme.example/help" }
  branding = [ordered]@{ primary_color = "#0F766E"; theme = "system"; icon = "icon.png" }
  relay = [ordered]@{ host = "127.0.0.1"; port = $port; key = $key }
  updates = [ordered]@{ clients = "notify" }
}
# (UTF-8 without a byte-order mark)
[IO.File]::WriteAllText((Join-Path $PWD "out\Acme\provider\provider.json"), ($provider | ConvertTo-Json -Depth 5))
# its icon: a teal rounded square with an A
$bmp = New-Object System.Drawing.Bitmap 256, 256
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$r = 56
$path.AddArc(8, 8, $r, $r, 180, 90); $path.AddArc(248 - $r, 8, $r, $r, 270, 90)
$path.AddArc(248 - $r, 248 - $r, $r, $r, 0, 90); $path.AddArc(8, 248 - $r, $r, $r, 90, 90); $path.CloseFigure()
$g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml("#0F766E"))), $path)
$font = New-Object System.Drawing.Font "Segoe UI", 120, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
$fmt = New-Object System.Drawing.StringFormat
$fmt.Alignment = [System.Drawing.StringAlignment]::Center; $fmt.LineAlignment = [System.Drawing.StringAlignment]::Center
$g.DrawString("A", $font, [System.Drawing.Brushes]::White, (New-Object System.Drawing.RectangleF 0, 0, 256, 256), $fmt)
$bmp.Save((Join-Path $PWD "out\Acme\provider\icon.png"), [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Copy-Item out\Acme\provider\* $out\ -Include provider.json, icon.png

# a broken package: the viewer stops (exit 4), never runs as the standard Taro
Copy-Item target\release\remote-mac-viewer.exe out\Broken\Broken.exe
[IO.File]::WriteAllText((Join-Path $PWD "out\Broken\provider\provider.json"), "{ not json")
$b = Start-Process -PassThru -NoNewWindow -Wait -FilePath out\Broken\Broken.exe -ArgumentList "--smoke" -RedirectStandardError "$out\broken.log"
Write-Host "broken provider.json: exit $($b.ExitCode)"; Get-Content "$out\broken.log"
if ($b.ExitCode -ne 4) { Write-Host "a broken provider.json did not stop the viewer"; $failed++ }

# the provider's relay: on this PC, requiring the provider's key; the fake agent has the key
$env:RM_RELAY_KEY = $key
$env:RM_SESSION_TOKEN = "branded-" + [guid]::NewGuid().ToString("N")
$relay = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-relay.exe -ArgumentList "127.0.0.1:$port" -RedirectStandardError "$out\relay.log"
Start-Sleep -Seconds 1
$agent = Start-Process -PassThru -NoNewWindow -FilePath target\release\rm-fakeagent.exe -ArgumentList "--relay 127.0.0.1:$port --session branded" -RedirectStandardError "$out\fakeagent.log"
Start-Sleep -Seconds 1
# the viewer: no key from the environment, no --relay (both from provider.json)
Remove-Item Env:RM_RELAY_KEY
Remove-Item Env:RM_RELAY -ErrorAction SilentlyContinue
$env:RM_UI_GALLERY = "1"
& ./scripts/windows-showcase.ps1 -Exe "out\Acme\Acme.exe" -Session branded -Apps "testapp,notes" -Out "$out\showcase" -Settle 2 -TimeoutSec 150
if ($LASTEXITCODE -ne 0) { Write-Host "the branded showcase failed"; $failed++ }
Remove-Item Env:RM_UI_GALLERY
foreach ($p in @($agent, $relay)) { if (-not $p.HasExited) { $p.Kill() } }
$vlog = Get-Content "$out\showcase\viewer.log" -ErrorAction SilentlyContinue
if (-not ($vlog | Select-String -SimpleMatch "Acme Connect (Taro")) { Write-Host "the viewer did not start as Acme Connect"; $failed++ }
if (-not ($vlog | Select-String -SimpleMatch "relay 127.0.0.1:$port")) { Write-Host "the viewer did not take the provider's relay"; $failed++ }
if ($vlog | Select-String -SimpleMatch $key) { Write-Host "the provider's key was written to the log"; $failed++ }
if (-not (Test-Path "$out\showcase\00-launcher.png")) { Write-Host "no launcher screenshot"; $failed++ }

# the connect window as it first shows (no ID given)
try {
  Add-Type -Namespace WB -Name U -MemberDefinition @"
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
public struct RECT { public int Left, Top, Right, Bottom; }
"@
  $cv = Start-Process -PassThru -NoNewWindow -FilePath out\Acme\Acme.exe -RedirectStandardError "$out\viewer-connect.log"
  $h = [IntPtr]::Zero
  for ($i = 0; $i -lt 120 -and $h -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 250; $h = [WB.U]::FindWindow("RmConnect", $null) }
  if ($h -ne [IntPtr]::Zero) {
    [WB.U]::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Seconds 3
    $r = New-Object WB.U+RECT
    [WB.U]::GetWindowRect($h, [ref]$r) | Out-Null
    $m = 40
    $bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left + 2 * $m), ($r.Bottom - $r.Top + 2 * $m)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left - $m, $r.Top - $m, 0, 0, $bmp.Size)
    $bmp.Save((Join-Path $PWD "$out\connect-window.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host "connect window: $out\connect-window.png"
  } else {
    Write-Host "the connect window did not show (viewer exited: $($cv.HasExited))"; $failed++
    Get-Content "$out\viewer-connect.log" -ErrorAction SilentlyContinue | Select-Object -First 40
  }
  if (-not $cv.HasExited) { $cv.Kill() }
} catch { Write-Host "connect window picture failed: $_"; $failed++ }
Write-Host "branded client screenshots:"; Get-ChildItem -Recurse -Path $out -Filter *.png | ForEach-Object { Write-Host "  $($_.FullName) $($_.Length) bytes" }
exit $failed
