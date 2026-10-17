# TaroEnterpriseSetup.exe as a Windows app, on a real Windows runner: it opens its own window
# (WebView2, no browser), the window shows the wizard, a screenshot is taken, and closing the
# window ends the program.
#   pwsh enterprise/scripts/windows-setup-app.ps1 PATH\TO\TaroEnterpriseSetup.exe OUT-DIR
param([string]$Exe, [string]$Out)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
Add-Type -AssemblyName System.Drawing
Add-Type -Namespace TS -Name U -MemberDefinition @"
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
public struct RECT { public int Left, Top, Right, Bottom; }
"@
$failed = 0
$p = Start-Process -PassThru -FilePath $Exe -ArgumentList @("--output", (Join-Path $Out "packages"))
$h = [IntPtr]::Zero
for ($i = 0; $i -lt 120 -and $h -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 250; $h = [TS.U]::FindWindow([NullString]::Value, "Taro Enterprise Setup") }
if ($h -eq [IntPtr]::Zero) {
  Write-Host "no 'Taro Enterprise Setup' window (exited: $($p.HasExited))"; $failed++
} else {
  # the page loads from the local server inside the window
  Start-Sleep -Seconds 8
  [TS.U]::SetForegroundWindow($h) | Out-Null
  Start-Sleep -Seconds 1
  $r = New-Object TS.U+RECT
  [TS.U]::GetWindowRect($h, [ref]$r) | Out-Null
  $w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
  Write-Host "window: ${w}x${hh}"
  $bmp = New-Object System.Drawing.Bitmap $w, $hh
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
  $bmp.Save((Join-Path $Out "setup-app-window.png"), [System.Drawing.Imaging.ImageFormat]::Png)
  # not a blank window: the console's dark top bar and its light page are both there
  $colors = @{}
  for ($y = 40; $y -lt $hh; $y += 23) { for ($x = 0; $x -lt $w; $x += 29) { $c = $bmp.GetPixel($x, $y); $colors["$($c.R / 32 -as [int]),$($c.G / 32 -as [int]),$($c.B / 32 -as [int])"] = 1 } }
  $g.Dispose(); $bmp.Dispose()
  Write-Host "distinct colours: $($colors.Count)"
  if ($colors.Count -lt 6) { Write-Host "the window looks empty"; $failed++ }
  # closing the window ends the program
  [TS.U]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
  if (-not $p.WaitForExit(15000)) { Write-Host "still running 15 s after its window was closed"; $failed++; $p.Kill() }
  else { Write-Host "closed: exit code $($p.ExitCode)" }
}
if (-not $p.HasExited) { $p.Kill() }
exit $failed
