param([string]$Renderer = "native", [int]$Runs = 20, [string]$OutDir)
# Runs flick --bench and grabs crops of the screen centre while it runs.
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
New-Item -ItemType Directory -Force $OutDir | Out-Null
$exe = Join-Path $PSScriptRoot "..\target\release\flick.exe"
$json = Join-Path $OutDir "bench-$Renderer.json"
Remove-Item $json -ErrorAction SilentlyContinue
$p = Start-Process $exe -ArgumentList "--bench", $Renderer, "--runs", $Runs, "--out", "`"$json`"" -PassThru
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$size = 520
$x = [int]($b.Width / 2 - $size / 2); $y = [int]($b.Height / 2 - $size / 2)
Start-Sleep -Milliseconds 1500
for ($i = 0; $i -lt 16; $i++) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
    $g.Dispose()
    $bmp.Save((Join-Path $OutDir ("$Renderer-{0:D2}.png" -f $i)))
    $bmp.Dispose()
    Start-Sleep -Milliseconds 70
}
$p.WaitForExit(120000) | Out-Null
"exit=$($p.ExitCode) screen=$($b.Width)x$($b.Height)"
if (Test-Path $json) { Get-Content $json -Raw } else { "no results file" }
