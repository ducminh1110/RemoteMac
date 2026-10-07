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

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$shot = $false
while (-not $viewer.HasExited -and (Get-Date) -lt $deadline) {
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
Write-Host "showcase exit code: $code"
exit $code
