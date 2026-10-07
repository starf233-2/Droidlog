# Builds the flashable Droidlog Boot Log module zip.
#
# It refuses to build unless every script passes these static checks. A shell script that
# fails one of them would be discovered only at boot, which is far too late:
#
#   1. LF line endings only   (a CR before the newline becomes part of the command)
#   2. no UTF-8 BOM           (Android's sh reads it as part of a command name)
#   3. ASCII only             (nothing in these scripts needs more)
#   4. no forbidden commands  (mount, setprop, sepolicy, dd to a block device, ...)
#   5. every write target is inside the module's own directory
#
# Checks 4 and 5 run on the code with comment lines removed, so prose that *describes* what the
# module does not do ("no mounts") cannot trip them — and cannot hide a real command either.
#
# Only the files listed in $ModuleFiles are packaged, so a stray file in the module directory
# can never end up being flashed.

$ErrorActionPreference = 'Stop'

$ModuleDir = 'D:\new pjkt\droidlog\contrib\ksu-bootlog'
$OutDir = 'D:\new pjkt'
$Version = 'v1.0.0'
$OutZip = Join-Path $OutDir "Droidlog-BootLog-$Version.zip"

$ModuleFiles = @(
  'module.prop',
  'post-fs-data.sh',
  'service.sh',
  'boot-completed.sh',
  'customize.sh',
  'uninstall.sh',
  'ctl.sh',
  'action.sh'
)

Add-Type -AssemblyName System.IO.Compression.FileSystem

# Commands that must never appear in code. Matched as whole words, against code only.
# A quoted string counts too: a dangerous word in a message is rephrased rather than the
# check being loosened, because "it is only a string" is how it ends up being a command.
$ForbiddenCommands = @(
  'mount', 'umount', 'resetprop', 'setprop', 'sepolicy', 'magiskpolicy',
  'mkfs', 'mke2fs', 'tune2fs', 'dd', 'su', 'insmod', 'reboot'
)
# Paths that must never be written to.
$ForbiddenPaths = @(
  'system/', 'vendor/', 'product/', 'odm/', '/dev/block', 'boot.img', 'init.rc', '/proc/sys'
)
# A write target must start with one of these, or be a device/null sink.
$AllowedTargetPrefix = @('$OUT', '$CONF', '$PIDF', '$DIR', '$MODDIR', '/data/adb/droidlog', '/dev/null')

function Get-CodeLines([string]$path) {
  $lines = [System.IO.File]::ReadAllLines($path)
  $code = @()
  foreach ($line in $lines) {
    if ($line -match '^\s*#') { continue }
    $code += $line
  }
  return $code
}

$failed = 0

Write-Host "=== 1-3: byte checks ==="
foreach ($name in $ModuleFiles) {
  $path = Join-Path $ModuleDir $name
  if (-not (Test-Path -LiteralPath $path)) {
    Write-Host ("  MISSING  {0}" -f $name); $failed++; continue
  }
  $bytes = [System.IO.File]::ReadAllBytes($path)
  $cr = 0; $nonAscii = 0
  foreach ($b in $bytes) { if ($b -eq 13) { $cr++ } elseif ($b -gt 127) { $nonAscii++ } }
  $bom = ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)
  $ok = ($cr -eq 0) -and (-not $bom) -and ($nonAscii -eq 0)
  if (-not $ok) { $failed++ }
  Write-Host ("  {0,-5} {1,-20} CR={2} BOM={3} nonASCII={4} bytes={5}" -f `
    $(if ($ok) { 'ok' } else { 'FAIL' }), $name, $cr, $bom, $nonAscii, $bytes.Length)
}

Write-Host "=== 4: forbidden commands (code only, whole words) ==="
$hits = 0
foreach ($name in $ModuleFiles) {
  $path = Join-Path $ModuleDir $name
  if (-not (Test-Path -LiteralPath $path)) { continue }
  $n = 0
  foreach ($line in Get-CodeLines $path) {
    $n++
    foreach ($cmd in $ForbiddenCommands) {
      if ($line -match ('(?<![\w./-])' + [regex]::Escape($cmd) + '(?![\w.-])')) {
        Write-Host ("  HIT  {0}:{1}  command '{2}'  ->  {3}" -f $name, $n, $cmd, $line.Trim())
        $hits++
      }
    }
    foreach ($p in $ForbiddenPaths) {
      if ($line.Contains($p)) {
        Write-Host ("  HIT  {0}:{1}  path '{2}'  ->  {3}" -f $name, $n, $p, $line.Trim())
        $hits++
      }
    }
  }
}
if ($hits -gt 0) { $failed += $hits; Write-Host ("  {0} hit(s)" -f $hits) } else { Write-Host "  clean" }

Write-Host "=== 5: every write target stays inside the module's own directory ==="
$violations = 0
foreach ($name in $ModuleFiles) {
  $path = Join-Path $ModuleDir $name
  if (-not (Test-Path -LiteralPath $path)) { continue }
  $n = 0
  foreach ($line in Get-CodeLines $path) {
    $n++
    # Redirections that create or append. The negative lookbehind keeps `->` and `>=` out: a `>`
    # inside a message ("raw -> effective") is not a redirection, and an earlier version of this
    # check reported every one of them as a violation.
    #
    # Quoted text is deliberately NOT stripped here. Doing that replaced the target of `> "$OUT/x"`
    # with the quote character and produced twenty false violations, which is a worse failure than
    # the one it was meant to fix: an unusable check gets disabled, and then it catches nothing.
    foreach ($m in [regex]::Matches($line, '(?<![-=<])>>?\s*("?)([^\s;&|)<>]+)')) {
      $target = $m.Groups[2].Value
      if ($target.StartsWith('&')) { continue }   # 2>&1: fd duplication, not a path
      $okTarget = $false
      foreach ($prefix in $AllowedTargetPrefix) {
        if ($target.StartsWith($prefix)) { $okTarget = $true; break }
      }
      if (-not $okTarget) {
        Write-Host ("  VIOLATION  {0}:{1}  writes to '{2}'  ->  {3}" -f $name, $n, $target, $line.Trim())
        $violations++
      }
    }
    # Destructive path arguments. `cp`/`mv`/`ln` are here because a copy *to* a system path is
    # just as damaging as an rm, and the first version of this check only looked at mkdir/rm.
    if ($line -match '^\s*(mkdir|rm|touch|find|chmod|cp|mv|ln|cat)\b') {
      foreach ($m in [regex]::Matches($line, '"([^"]+)"')) {
        $arg = $m.Groups[1].Value
        $okArg = $false
        foreach ($prefix in $AllowedTargetPrefix) {
          if ($arg.StartsWith($prefix)) { $okArg = $true; break }
        }
        if (-not $okArg -and $arg -notmatch '^/proc/\$PID' -and $arg -notmatch '^\$') {
          Write-Host ("  VIOLATION  {0}:{1}  path argument '{2}'  ->  {3}" -f $name, $n, $arg, $line.Trim())
          $violations++
        }
      }
      # Unquoted absolute paths count too: `rm -rf /system` has no quotes, and an untested
      # checker would wave it through.
      foreach ($tok in ($line -split '\s+')) {
        $t = $tok.Trim('"', "'", ';')
        if (-not $t.StartsWith('/')) { continue }
        if ($t.StartsWith('/data/adb/droidlog') -or $t -eq '/dev/null' -or $t.StartsWith('/proc/$PID')) { continue }
        Write-Host ("  VIOLATION  {0}:{1}  unquoted path '{2}'  ->  {3}" -f $name, $n, $t, $line.Trim())
        $violations++
      }
    }
  }
}
if ($violations -gt 0) { $failed += $violations; Write-Host ("  {0} violation(s)" -f $violations) } else { Write-Host "  clean" }

if ($failed -gt 0) {
  Write-Host ("=== REFUSING TO BUILD: {0} check(s) failed ===" -f $failed)
  exit 1
}

if (Test-Path -LiteralPath $OutZip) { Remove-Item -LiteralPath $OutZip -Force }
$zip = [System.IO.Compression.ZipFile]::Open($OutZip, 'Create')
try {
  foreach ($name in $ModuleFiles) {
    $path = Join-Path $ModuleDir $name
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $path, $name, 'Optimal')
  }
} finally {
  $zip.Dispose()
}

Write-Host "=== built ==="
Write-Host ("  {0}" -f $OutZip)
Write-Host ("  {0:N2} MB  SHA256 {1}" -f ((Get-Item -LiteralPath $OutZip).Length / 1MB), (Get-FileHash -LiteralPath $OutZip -Algorithm SHA256).Hash)
Write-Host ("  entries: {0} (all at the zip root, as KernelSU expects)" -f $ModuleFiles.Count)
