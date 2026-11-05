# Cameleer vs Apache Camel benchmark suite, Windows PowerShell entry point.
#
#   .\scripts\bench.ps1               # tools, build, run both sides, write docs\BENCHMARKS.md
#   .\scripts\bench.ps1 run           # any scripts/bench.sh command: tools | build | run | report | all
#   $env:BENCH_QUICK = "1"; .\scripts\bench.ps1   # short smoke run
#
# The suite itself is scripts/bench.sh; this wrapper runs it with Git for Windows' bash
# (installed with Git, which the repository needs anyway). Needs cargo on PATH.
param([string]$Command = "all")
$ErrorActionPreference = "Stop"

$candidates = @(
    "$env:ProgramFiles\Git\bin\bash.exe",
    "${env:ProgramFiles(x86)}\Git\bin\bash.exe",
    "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe"
)
$git = Get-Command git -ErrorAction SilentlyContinue
if ($git) {
    # ...\Git\cmd\git.exe -> ...\Git\bin\bash.exe
    $candidates += Join-Path (Split-Path (Split-Path $git.Source)) "bin\bash.exe"
}
$bash = $candidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $bash) {
    throw "Git Bash not found. Install Git for Windows (https://git-scm.com/download/win) or run scripts/bench.sh from WSL2."
}

$script = Join-Path $PSScriptRoot "bench.sh"
& $bash $script $Command
exit $LASTEXITCODE
