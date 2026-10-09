# Runs the viewer in --showcase mode against a Mac agent (real or fake) and collects what a user
# would see: every app window (Mac chrome included), the launcher and the whole Windows desktop
# with all apps open, as PNG files in $Out, plus showcase.txt.
param(
  [Parameter(Mandatory)] [string] $Relay,
  [string] $Session = "",
  # the user's way in: the ID and password the Mac prints (instead of -Session + RM_SESSION_TOKEN)
  [string] $Id = "",
  [string] $Password = "",
  [string] $Apps = "xcode",
  [string] $Out = "out\showcase",
  [int] $Settle = 12,
  [int] $TimeoutSec = 900,
  [string] $Type = ""
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
New-Item -ItemType Directory -Force -Path $Out | Out-Null
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $Out "ready"), (Join-Path $Out "desktop.done")
$log = Join-Path $Out "viewer.log"
$viewer = Start-Process -PassThru -NoNewWindow -FilePath target\release\remote-mac-viewer.exe `
  -ArgumentList ("--relay $Relay " + $(if ($Id) { "--id $Id --password $Password" } else { "--session $Session" }) + " --showcase `"$Out`" --apps $Apps --settle $Settle --no-shortcuts" + $(if ($Type) { " --type `"$Type`"" } else { "" })) -RedirectStandardError $log

function Save-Desktop([string] $path) {
  try {
    $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host "desktop screenshot: $path ($($b.Width)x$($b.Height))"
  } catch { Write-Host "desktop screenshot failed: $_" }
}

# a picture of one of MacBridge's own surfaces (the viewer asks: shot.req = "name x y w h")
function Save-Region([string] $name, [int] $x, [int] $y, [int] $w, [int] $h) {
  try {
    $m = 32
    $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $l = [Math]::Max($b.Left, $x - $m); $t = [Math]::Max($b.Top, $y - $m)
    $r = [Math]::Min($b.Right, $x + $w + $m); $btm = [Math]::Min($b.Bottom, $y + $h + $m)
    $bmp = New-Object System.Drawing.Bitmap ($r - $l), ($btm - $t)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($l, $t, 0, 0, $bmp.Size)
    $bmp.Save((Join-Path $Out "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host "screenshot of $name ($($r - $l)x$($btm - $t))"
  } catch { Write-Host "screenshot of $name failed: $_" }
}

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$shot = $false
while (-not $viewer.HasExited -and (Get-Date) -lt $deadline) {
  $req = Join-Path $Out "shot.req"
  if (Test-Path $req) {
    $p = (Get-Content $req -Raw).Trim().Split(" ")
    Remove-Item -Force $req
    Start-Sleep -Milliseconds 400
    Save-Region $p[0] ([int]$p[1]) ([int]$p[2]) ([int]$p[3]) ([int]$p[4])
    New-Item -ItemType File -Force -Path (Join-Path $Out "$($p[0]).done") | Out-Null
  }
  if (-not $shot -and (Test-Path (Join-Path $Out "ready"))) {
    Start-Sleep -Seconds 2
    Save-Desktop (Join-Path $Out "desktop.png")
    New-Item -ItemType File -Force -Path (Join-Path $Out "desktop.done") | Out-Null
    $shot = $true
  }
  Start-Sleep -Milliseconds 500
}
if (-not $viewer.HasExited) { Write-Host "showcase timed out"; Save-Desktop (Join-Path $Out "desktop-timeout.png"); $viewer.Kill(); $code = 124 } else { $code = $viewer.ExitCode }

Get-ChildItem -Path $Out -Filter *.bmp | ForEach-Object {
  $img = [System.Drawing.Image]::FromFile($_.FullName)
  $img.Save([IO.Path]::ChangeExtension($_.FullName, ".png"), [System.Drawing.Imaging.ImageFormat]::Png)
  $img.Dispose(); Remove-Item $_.FullName
}
Write-Host "=== viewer log"; Get-Content $log -ErrorAction SilentlyContinue
Write-Host "=== showcase.txt"; Get-Content (Join-Path $Out "showcase.txt") -ErrorAction SilentlyContinue
Write-Host "screenshots:"; Get-ChildItem -Path $Out -Filter *.png | ForEach-Object { Write-Host "  $($_.Name) $($_.Length) bytes" }
# the pictures in the log too (JPEG, base64 between markers), where they can be looked at
# without downloading the artifact
if ($env:RM_UI_GALLERY) {
  $jpeg = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq "image/jpeg" }
  $q = New-Object System.Drawing.Imaging.EncoderParameters 1
  $q.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), 90L
  Get-ChildItem -Path $Out -Filter *.png | Where-Object { $_.Name -match '^(1\d-|desktop\.png|00-launcher|01-)' } | ForEach-Object {
    $img = [System.Drawing.Image]::FromFile($_.FullName)
    $k = [Math]::Min(1.0, 1400.0 / $img.Width)
    $bmp = New-Object System.Drawing.Bitmap ([int]($img.Width * $k)), ([int]($img.Height * $k))
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.DrawImage($img, 0, 0, $bmp.Width, $bmp.Height)
    $ms = New-Object IO.MemoryStream
    $bmp.Save($ms, $jpeg, $q)
    $s = [Convert]::ToBase64String($ms.ToArray())
    Write-Host "GALLERY-BEGIN $($_.BaseName)"
    for ($i = 0; $i -lt $s.Length; $i += 4000) { Write-Host $s.Substring($i, [Math]::Min(4000, $s.Length - $i)) }
    Write-Host "GALLERY-END"
    $g.Dispose(); $bmp.Dispose(); $img.Dispose(); $ms.Dispose()
  }
}
Write-Host "showcase exit code: $code"
exit $code
