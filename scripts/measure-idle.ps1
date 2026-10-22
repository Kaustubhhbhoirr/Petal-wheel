param([string]$Renderer, [switch]$OpenOnce, [int]$SettleSec = 15, [int]$SampleSec = 30)
# Starts flick, lets it settle, then measures CPU and memory of its whole process tree
# (flick.exe plus any msedgewebview2.exe children).
$exe = Join-Path $PSScriptRoot "..\target\release\flick.exe"
$args = @("--renderer", $Renderer)
if ($OpenOnce) { $args += "--open-once" }
$root = Start-Process $exe -ArgumentList $args -PassThru

function Tree([int]$id) {
    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId
    $ids = [System.Collections.Generic.List[int]]::new(); $ids.Add($id)
    for ($i = 0; $i -lt $ids.Count; $i++) {
        $all | Where-Object { $_.ParentProcessId -eq $ids[$i] } | ForEach-Object { $ids.Add([int]$_.ProcessId) }
    }
    $ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }
}

Start-Sleep -Seconds $SettleSec
$before = Tree $root.Id
$cpu0 = ($before | Measure-Object CPU -Sum).Sum
Start-Sleep -Seconds $SampleSec
$after = Tree $root.Id
$cpu1 = ($after | Measure-Object CPU -Sum).Sum
$cores = [Environment]::ProcessorCount
[pscustomobject]@{
    renderer      = $Renderer + $(if ($OpenOnce) { " (after one open)" } else { "" })
    processes     = $after.Count
    private_mb    = [math]::Round(($after | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB, 1)
    working_set_mb = [math]::Round(($after | Measure-Object WorkingSet64 -Sum).Sum / 1MB, 1)
    flick_only_ws_mb = [math]::Round(($after | Where-Object Id -eq $root.Id).WorkingSet64 / 1MB, 1)
    cpu_seconds   = [math]::Round($cpu1 - $cpu0, 3)
    cpu_percent   = [math]::Round(100 * ($cpu1 - $cpu0) / ($SampleSec * $cores), 3)
}
$after | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
