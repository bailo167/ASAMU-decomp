# The half of tools/dev/win_host.sh that runs on the Windows machine.
#
# win_host.sh sends this text over SSH (base64 on standard input; see ps_run
# there) after one line that sets $root, and followed by one command such as
# `WhGate 'NAME' @()`. It is written for Windows PowerShell 5.1, the version
# every Windows installation has, and is never run as a script file.
#
# Rules this file keeps:
#   - every folder it creates, writes to or deletes comes from WhPath, which
#     returns only <root>\work\<NAME>, <root>\target-<NAME> or <root>\logs\<NAME>;
#   - it never deletes or walks through a link (junction or symbolic link);
#   - a failure of its own is one plain line on standard error and exit code 4
#     (2 for a bad name or root).

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$env:Path = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:Path
$env:CARGO_TERM_COLOR = 'never'

# A folder that is in a work tree from the start of a sync until the sync is
# verified; the files arrive in it (see "sync" below).
$WhMarker = '.win_host_incomplete'

function WhSay([string]$line) {
    [Console]::Out.WriteLine($line)
    [Console]::Out.Flush()
}

function WhFail([string]$message, [int]$code = 4) {
    [Console]::Error.WriteLine('win_host: ' + $message)
    exit $code
}

# Text that win_host.sh passed as base64 (UTF-8), so that no character of it is
# ever read as PowerShell syntax.
function WhText([string]$base64) {
    return [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($base64))
}

# The only source of paths that are written to or deleted. $kind is work,
# target or logs. The name and the root were checked by win_host.sh already;
# they are checked again here because this is the side that deletes.
function WhPath([string]$kind, [string]$name) {
    if ($root -cnotmatch '^[A-Za-z]:(\\[A-Za-z0-9._-]+)+$' -or [IO.Path]::GetFullPath($root) -ne $root) {
        WhFail ('bad root folder: ' + $root) 2
    }
    $reserved = '^(con|prn|aux|nul|com[0-9]|lpt[0-9])(\.|$)'
    if ($name -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$' -or $name.Contains('..') -or $name.EndsWith('.') -or $name -match $reserved) {
        WhFail ('bad work tree name: ' + $name) 2
    }
    if ($kind -eq 'work') { $path = $root + '\work\' + $name }
    elseif ($kind -eq 'target') { $path = $root + '\target-' + $name }
    elseif ($kind -eq 'logs') { $path = $root + '\logs\' + $name }
    else { WhFail ('internal error: unknown kind of folder ' + $kind) }
    if ([IO.Path]::GetFullPath($path) -ne $path) { WhFail ('bad work tree name: ' + $name) 2 }
    return $path
}

function WhIsLink([string]$path) {
    $item = Get-Item -LiteralPath $path -Force
    return [bool]($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
}

# Bytes and number of files below a folder. Links are not followed; the \\?\
# form reads paths longer than 260 characters, which build output can have.
function WhTreeSize([string]$path) {
    $bytes = [int64]0
    $files = [int64]0
    if (Test-Path -LiteralPath $path -PathType Container) {
        $stack = New-Object System.Collections.Stack
        $stack.Push('\\?\' + $path)
        while ($stack.Count -gt 0) {
            $dir = New-Object System.IO.DirectoryInfo([string]$stack.Pop())
            try {
                foreach ($entry in $dir.EnumerateFileSystemInfos()) {
                    if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { continue }
                    if ($entry.Attributes -band [IO.FileAttributes]::Directory) { $stack.Push($entry.FullName) }
                    else { $bytes += $entry.Length; $files++ }
                }
            } catch {
                # A folder that vanished or cannot be read while a build runs counts as empty.
            }
        }
    }
    return @($bytes, $files)
}

function WhSize([int64]$bytes) {
    $inv = [Globalization.CultureInfo]::InvariantCulture
    if ($bytes -ge 1GB) { return ($bytes / 1GB).ToString('0.00', $inv) + ' GB' }
    if ($bytes -ge 1MB) { return ($bytes / 1MB).ToString('0.0', $inv) + ' MB' }
    if ($bytes -ge 1KB) { return ($bytes / 1KB).ToString('0.0', $inv) + ' KB' }
    return ([string]$bytes + ' bytes')
}

# Why a folder from WhPath must not be removed ('' when it may be, or does not exist).
function WhNotRemovable([string]$path) {
    $parent = Split-Path -Parent $path
    if ((Test-Path -LiteralPath $parent) -and (WhIsLink $parent)) {
        return ($parent + ' is a link (junction or symbolic link): refusing to work through it')
    }
    if (-not (Test-Path -LiteralPath $path)) { return '' }
    if (-not (Test-Path -LiteralPath $path -PathType Container)) {
        return ($path + ' exists and is not a folder: look at it and remove it by hand')
    }
    if (WhIsLink $path) {
        return ($path + ' is a link (junction or symbolic link): refusing to delete through it')
    }
    return ''
}

# Remove a folder from WhPath and everything in it. Returns '' or why it could
# not. `rd /s /q` removes read-only files and removes a link inside the tree
# without touching what it points to; the .NET call is a second try for paths
# too long for rd.
function WhRemoveTree([string]$path) {
    $why = WhNotRemovable $path
    if ($why) { return $why }
    if (-not (Test-Path -LiteralPath $path)) { return '' }
    $out = & cmd.exe /d /c ('rd /s /q ' + $path + ' < NUL 2>&1')
    if (Test-Path -LiteralPath $path) {
        try { [IO.Directory]::Delete('\\?\' + $path, $true) } catch { }
    }
    if (Test-Path -LiteralPath $path) {
        $first = [string](@($out) | Select-Object -First 1)
        return ('could not remove all of ' + $path + ' (is a program still using it?) ' + $first).TrimEnd()
    }
    return ''
}

# Enter the work tree NAME and point cargo at its own target folder.
function WhEnter([string]$name) {
    $work = WhPath 'work' $name
    if (-not (Test-Path -LiteralPath $work -PathType Container)) {
        WhFail ('no work tree ' + $name + ' on this machine: run `sync ' + $name + '` first')
    }
    if (Test-Path -LiteralPath (Join-Path $work $WhMarker)) {
        WhFail ('work tree ' + $name + ' is incomplete (a sync was interrupted or failed): run `sync ' + $name + '` again')
    }
    Set-Location -LiteralPath $work
    $env:CARGO_TARGET_DIR = WhPath 'target' $name
    return $work
}

# One argument as a program started by Windows will read it back (the rules of
# the Microsoft C runtime: backslashes are literal except before a quote).
function WhArgument([string]$text) {
    if ($text.Length -gt 0 -and $text -notmatch '[\s"]') { return $text }
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('"')
    $slashes = 0
    foreach ($ch in $text.ToCharArray()) {
        if ($ch -eq [char]92) { $slashes++; continue }
        if ($ch -eq [char]34) { [void]$sb.Append([char]92, 2 * $slashes + 1) }
        else { [void]$sb.Append([char]92, $slashes) }
        [void]$sb.Append($ch)
        $slashes = 0
    }
    [void]$sb.Append([char]92, 2 * $slashes)
    [void]$sb.Append('"')
    return $sb.ToString()
}

# Start a program with exactly these arguments, wait, and exit with its code.
# PowerShell 5.1's own way of passing arguments to programs loses empty
# arguments and breaks ones that contain quotes, so the command line is built
# here. Output and errors of the program go straight to the SSH connection.
# Exit code 255 is kept for "the machine could not be reached": a code outside
# 0..254 is reported as 254, with the real one on standard error.
function WhRun([string]$program, [string[]]$arguments) {
    $found = @(Get-Command -Name $program -CommandType Application -ErrorAction SilentlyContinue)
    if ($found.Count -eq 0) { WhFail ('program not found on this machine: ' + $program) 127 }
    $path = $found[0].Path
    if ($path -match '\.(cmd|bat)$') {
        WhFail ($program + ' is a batch file, which reads its arguments by other rules: use `ps` to run it') 2
    }
    $parts = New-Object System.Collections.ArrayList
    foreach ($argument in $arguments) { [void]$parts.Add((WhArgument $argument)) }
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $path
    $start.Arguments = ($parts.ToArray() -join ' ')
    $start.UseShellExecute = $false
    $start.WorkingDirectory = (Get-Location).ProviderPath
    # The program gets an empty standard input of its own: the one of this
    # session may never report its end (see BOOTSTRAP in win_host.sh).
    $start.RedirectStandardInput = $true
    $process = [System.Diagnostics.Process]::Start($start)
    $process.StandardInput.Close()
    $process.WaitForExit()
    $code = $process.ExitCode
    if ($code -lt 0 -or $code -gt 254) {
        [Console]::Error.WriteLine('win_host: ' + $program + ' ended with exit code ' + $code + ' (reported as 254)')
        $code = 254
    }
    exit $code
}

# --- sync -------------------------------------------------------------------
#
# The files do not arrive under their own names: Windows' tar cannot create a
# name with characters outside the machine's legacy code page (measured: it
# skips Japanese and emoji names with "empty or unreadable filename"). So
# win_host.sh sends its list of names as <work>\.win_host_incomplete\manifest
# and file number i of the list as <work>\.win_host_incomplete\<i> (six
# digits), and WhSyncVerify moves each to its place. That folder doubles as
# the mark of an unfinished sync.

# Empty the work tree NAME (creating it if needed) and mark it incomplete.
function WhSyncPrepare([string]$name) {
    $work = WhPath 'work' $name
    $staging = Join-Path $work $WhMarker
    $why = WhNotRemovable $work
    if ($why) { WhFail $why }
    if (Test-Path -LiteralPath $work) {
        # From here until WhSyncVerify agrees, cargo/exec/gate refuse this tree.
        try { New-Item -ItemType Directory -Force -Path $staging | Out-Null } catch { }
        $why = WhRemoveTree $work
        if ($why) {
            try { New-Item -ItemType Directory -Force -Path $staging | Out-Null } catch { }
            WhFail $why
        }
    }
    New-Item -ItemType Directory -Force -Path $staging | Out-Null
}

# Put the received files in their places and compare the result with what
# win_host.sh sent. The list of names arrives with the files, as
# <work>\.win_host_incomplete\manifest (UTF-8, one name per line with /
# separators, in the order of the file numbers); win_host.sh tells its
# SHA-256, the number of files and the number of bytes. Only when names and
# bytes agree is the tree usable.
function WhSyncVerify([string]$name, [int]$count, [int64]$bytes, [string]$digest) {
    $work = WhPath 'work' $name
    $staging = Join-Path $work $WhMarker
    $again = '. Run `sync ' + $name + '` again'
    $listFile = '\\?\' + $staging + '\manifest'
    if (-not [IO.File]::Exists($listFile)) {
        WhFail ('the list of files did not arrive in work tree ' + $name + $again)
    }
    $listBytes = [IO.File]::ReadAllBytes($listFile)
    $sha = [Security.Cryptography.SHA256]::Create()
    $got = ([BitConverter]::ToString($sha.ComputeHash($listBytes))).Replace('-', '')
    if ($got -ne $digest) { WhFail ('the list of files arrived damaged in work tree ' + $name + $again) }
    # Not a path this side would write to: empty, ".", ".." or dot/space-ended
    # parts, or characters a Windows file name cannot hold.
    $unsafe = '(^|/)(/|$)|[. ](/|$)|[\\:*?"<>|\x00-\x1f]'
    $expected = New-Object 'System.Collections.Generic.HashSet[string]' -ArgumentList ([StringComparer]::Ordinal)
    $index = 0
    foreach ($relative in [Text.Encoding]::UTF8.GetString($listBytes).Split([char]10)) {
        if ($relative.Length -eq 0) { continue }
        $index++
        if ($relative -cmatch $unsafe -or $relative.StartsWith($WhMarker, [StringComparison]::OrdinalIgnoreCase)) {
            WhFail ('refusing the file name ' + $relative + $again)
        }
        $source = '\\?\' + $staging + '\' + $index.ToString('000000')
        $target = '\\?\' + $work + '\' + $relative.Replace('/', '\')
        try {
            [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target))
            [IO.File]::Move($source, $target)
        } catch {
            WhFail ('file ' + $index + ' (' + $relative + ') did not arrive or could not be put in place: ' + $_.Exception.Message + $again)
        }
        [void]$expected.Add($relative)
    }
    if ($index -ne $count -or $expected.Count -ne $count) {
        WhFail ('the list of files in work tree ' + $name + ' has ' + $index + ' names, not ' + $count + $again)
    }
    [IO.File]::Delete($listFile)
    $prefix = '\\?\' + $work + '\'
    $extra = New-Object System.Collections.ArrayList
    $found = 0
    $foundBytes = [int64]0
    $stack = New-Object System.Collections.Stack
    $stack.Push('\\?\' + $work)
    while ($stack.Count -gt 0) {
        $dir = New-Object System.IO.DirectoryInfo([string]$stack.Pop())
        foreach ($entry in $dir.EnumerateFileSystemInfos()) {
            $isLink = [bool]($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)
            $isDir = [bool]($entry.Attributes -band [IO.FileAttributes]::Directory)
            $relative = $entry.FullName.Substring($prefix.Length).Replace('\', '/')
            if ($isDir -and -not $isLink) { $stack.Push($entry.FullName); continue }
            if (-not $isDir -and -not $isLink -and $expected.Remove($relative)) { $found++; $foundBytes += $entry.Length }
            else { [void]$extra.Add($relative) }
        }
    }
    if ($expected.Count -gt 0 -or $extra.Count -gt 0 -or $foundBytes -ne $bytes) {
        $missing = @($expected | Select-Object -First 5) -join ', '
        $surplus = @($extra | Select-Object -First 5) -join ', '
        WhFail ('work tree ' + $name + ' does not match what was sent: ' + $expected.Count + ' missing [' + $missing + '], ' +
            $extra.Count + ' unexpected [' + $surplus + '], ' + $foundBytes + ' bytes instead of ' + $bytes + $again)
    }
    # Empty by now (a file left in it was counted as unexpected above).
    [IO.Directory]::Delete('\\?\' + $staging, $false)
    $logs = WhPath 'logs' $name
    New-Item -ItemType Directory -Force -Path $logs | Out-Null
    $now = [DateTime]::UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'", [Globalization.CultureInfo]::InvariantCulture)
    Set-Content -LiteralPath (Join-Path $logs 'sync.txt') -Encoding ASCII -Value ('last sync ' + $now + ', ' + $found + ' files, ' + $foundBytes + ' bytes')
    WhSay ('win_host: synced ' + $found + ' files (' + $foundBytes + ' bytes) to ' + $work)
}

# --- gate -------------------------------------------------------------------

# The project's gate in the work tree NAME: one summary line per step on
# standard output, each step's whole output in <root>\logs\NAME\<step>.log.
# $only (step names) limits the run. Exit code 1 if any step failed.
function WhGate([string]$name, [string[]]$only) {
    $work = WhEnter $name
    $logs = WhPath 'logs' $name
    New-Item -ItemType Directory -Force -Path $logs | Out-Null
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $steps = @(
        @('fmt', 'fmt --all --check'),
        @('clippy', 'clippy --workspace --all-targets --locked -- -D warnings'),
        @('test', 'test --workspace --locked'),
        @('build', 'build --workspace --locked')
    )
    $report = New-Object System.Collections.ArrayList
    $failed = New-Object System.Collections.ArrayList
    $stamp = Join-Path $logs 'sync.txt'
    if (Test-Path -LiteralPath $stamp) {
        $line = 'gate ' + $name + ': ' + $work + ', ' + [string](Get-Content -LiteralPath $stamp -TotalCount 1)
        WhSay $line
        [void]$report.Add($line)
    }
    $ran = 0
    $whole = [Diagnostics.Stopwatch]::StartNew()
    foreach ($step in $steps) {
        $id = [string]$step[0]
        if (@($only).Count -gt 0 -and @($only) -notcontains $id) { continue }
        $ran++
        $log = Join-Path $logs ($id + '.log')
        $watch = [Diagnostics.Stopwatch]::StartNew()
        # cmd.exe does the redirection, so the log is cargo's own bytes in order.
        & cmd.exe /d /c ('cargo ' + $step[1] + ' < NUL > ' + $log + ' 2>&1')
        $code = $LASTEXITCODE
        $seconds = $watch.Elapsed.TotalSeconds.ToString('0.0', $inv)
        if ($code -eq 0) { $result = 'ok' } else { $result = 'FAILED'; [void]$failed.Add($id) }
        $line = 'gate ' + $name + ': ' + $id.PadRight(7) + $result.PadRight(7) + ($seconds + ' s').PadLeft(9) + '  cargo ' + $step[1] + '  [' + $log + ']'
        if ($code -ne 0) { $line += '  (exit ' + $code + ')' }
        WhSay $line
        [void]$report.Add($line)
        if ($code -ne 0) {
            [Console]::Error.WriteLine('--- ' + $id + ' failed; the last lines of ' + $log + ':')
            foreach ($text in @(Get-Content -LiteralPath $log -Tail 30 -Encoding UTF8)) { [Console]::Error.WriteLine($text) }
            [Console]::Error.WriteLine('---')
        }
    }
    $seconds = $whole.Elapsed.TotalSeconds.ToString('0.0', $inv)
    if ($failed.Count -eq 0) { $line = 'gate ' + $name + ': PASSED, ' + $ran + ' steps in ' + $seconds + ' s' }
    else { $line = 'gate ' + $name + ': FAILED (' + ($failed.ToArray() -join ', ') + '), ' + $failed.Count + ' of ' + $ran + ' steps, in ' + $seconds + ' s' }
    WhSay $line
    [void]$report.Add($line)
    Set-Content -LiteralPath (Join-Path $logs 'gate.txt') -Encoding ASCII -Value $report.ToArray()
    if ($failed.Count -gt 0) { exit 1 }
    exit 0
}

# --- clean, disk, info -------------------------------------------------------

# Remove the work tree NAME and its target folder, nothing else (the logs stay).
function WhClean([string]$name) {
    $freed = [int64]0
    $problems = 0
    foreach ($path in @((WhPath 'work' $name), (WhPath 'target' $name))) {
        if (-not (Test-Path -LiteralPath $path)) {
            WhSay ('win_host: ' + $path + ' does not exist')
            continue
        }
        $why = WhNotRemovable $path
        if ($why) { [Console]::Error.WriteLine('win_host: ' + $why); $problems++; continue }
        $before = WhTreeSize $path
        $why = WhRemoveTree $path
        if ($why) {
            $after = WhTreeSize $path
            $freed += ($before[0] - $after[0])
            [Console]::Error.WriteLine('win_host: ' + $why)
            $problems++
            continue
        }
        $freed += $before[0]
        WhSay ('win_host: removed ' + $path + ' (' + (WhSize $before[0]) + ', ' + $before[1] + ' files)')
    }
    WhSay ('win_host: freed ' + (WhSize $freed))
    if ($problems -gt 0) { exit 4 }
}

function WhFreeSpace {
    $drive = New-Object System.IO.DriveInfo($root.Substring(0, 2))
    return ($drive.Name.TrimEnd('\') + ' ' + (WhSize $drive.AvailableFreeSpace) + ' free of ' + (WhSize $drive.TotalSize))
}

# Free space, and the size of every work tree and target folder below the root.
function WhDisk {
    WhSay ('disk ' + (WhFreeSpace))
    $rows = New-Object System.Collections.ArrayList
    $workRoot = Join-Path $root 'work'
    if (Test-Path -LiteralPath $workRoot -PathType Container) {
        foreach ($dir in @(Get-ChildItem -LiteralPath $workRoot -Directory -Force | Sort-Object Name)) {
            [void]$rows.Add(@(('work\' + $dir.Name), $dir.FullName))
        }
    }
    if (Test-Path -LiteralPath $root -PathType Container) {
        foreach ($dir in @(Get-ChildItem -LiteralPath $root -Directory -Force | Sort-Object Name)) {
            if ($dir.Name -eq 'target' -or $dir.Name -like 'target-*') { [void]$rows.Add(@($dir.Name, $dir.FullName)) }
        }
    }
    $total = [int64]0
    foreach ($row in $rows) {
        $size = WhTreeSize $row[1]
        $total += $size[0]
        WhSay (([string]$row[0]).PadRight(30) + (WhSize $size[0]).PadLeft(12) + ([string]$size[1]).PadLeft(9) + ' files')
    }
    WhSay ('total'.PadRight(30) + (WhSize $total).PadLeft(12))
}

# Toolchain, free space and the work trees that exist.
function WhInfo {
    foreach ($tool in @('rustc', 'cargo')) {
        $found = @(Get-Command -Name $tool -CommandType Application -ErrorAction SilentlyContinue)
        if ($found.Count -eq 0) { WhSay ($tool + ': not found') }
        else { WhSay ([string](@(& $found[0].Path --version) | Select-Object -First 1)) }
    }
    WhSay ('disk ' + (WhFreeSpace))
    $workRoot = Join-Path $root 'work'
    if (Test-Path -LiteralPath $workRoot -PathType Container) {
        foreach ($dir in @(Get-ChildItem -LiteralPath $workRoot -Directory -Force | Sort-Object Name)) {
            WhSay ('work tree ' + $dir.Name)
        }
    }
}
