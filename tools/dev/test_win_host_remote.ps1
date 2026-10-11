# Test of the PowerShell half of tools/dev/win_host.sh (win_host_remote.ps1).
# It needs a Windows machine, and runs with Windows PowerShell 5.1.
#
# Through win_host.sh, which loads the shared functions before the script:
#     tools/dev/win_host.sh ps < tools/dev/test_win_host_remote.ps1
# or on the Windows machine itself (CI does this):
#     powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\test_win_host_remote.ps1
#
# What it checks is the side that deletes: that WhPath gives out nothing but
# <root>\work\NAME, <root>\target-NAME and <root>\logs\NAME whatever name or
# root it is handed; that `clean` and `sync` remove those folders and nothing
# beside them, never through a link and never the folder a link points to;
# that a sync whose list of names tries to leave the work tree is refused; and
# that an argument reaches a program as it was given.
#
# It works in one scratch folder and removes it at the end:
# $env:WIN_HOST_TEST_DIR when that is set, <root>\scratch\win_host_remote_test
# when run through win_host.sh, <temporary folder>\win_host_remote_test
# otherwise. The folder must be a plain path on a drive (letters, digits,
# . _ -), and either not exist or be left over from an earlier run of this test.

if (-not (Get-Command WhPath -ErrorAction SilentlyContinue)) {
    . (Join-Path $PSScriptRoot 'win_host_remote.ps1')
}
$ErrorActionPreference = 'Stop'

# Counted in one object: a plain variable set in a function would be that function's own.
$TCount = @{ Passed = 0; Failed = 0 }
function TOk([string]$what) {
    $TCount.Passed++
    WhSay ('ok    ' + $what)
}
function TBad([string]$what, [string]$why) {
    $TCount.Failed++
    WhSay ('FAIL  ' + $what)
    if ($why) { WhSay ('        ' + $why) }
}
function TIs([string]$what, [string]$actual, [string]$expected) {
    if ($actual -ceq $expected) { TOk $what } else { TBad $what ('got:      ' + $actual + [Environment]::NewLine + '        expected: ' + $expected) }
}
# The body must end in WhFail with this exit code.
function TRefused([string]$what, [int]$code, [scriptblock]$body) {
    try {
        $result = & $body
        TBad $what ('was not refused; result: ' + [string]$result)
    } catch {
        $text = $_.Exception.Message
        if ($text.StartsWith('WhFail ' + $code + ':')) { TOk $what } else { TBad $what $text }
    }
}
# Everything below a folder as sorted lines: F name size, D name, L name (a link; not entered).
function TTree([string]$top) {
    $rows = New-Object System.Collections.ArrayList
    $stack = New-Object System.Collections.Stack
    $prefix = '\\?\' + $top
    $stack.Push($prefix)
    while ($stack.Count -gt 0) {
        $dir = New-Object System.IO.DirectoryInfo([string]$stack.Pop())
        foreach ($entry in $dir.EnumerateFileSystemInfos()) {
            $relative = $entry.FullName.Substring($prefix.Length)
            if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { [void]$rows.Add('L ' + $relative) }
            elseif ($entry.Attributes -band [IO.FileAttributes]::Directory) { [void]$rows.Add('D ' + $relative); $stack.Push($entry.FullName) }
            else { [void]$rows.Add('F ' + $relative + ' ' + $entry.Length) }
        }
    }
    $sorted = $rows.ToArray()
    [Array]::Sort($sorted, [StringComparer]::Ordinal)
    return ($sorted -join "`n")
}
# The lines of a tree that are not below one of the given folders (relative, starting with \).
function TWithout([string]$tree, [string[]]$folders) {
    $kept = New-Object System.Collections.ArrayList
    foreach ($line in $tree.Split([char]10)) {
        $name = $line.Substring(2)
        $below = $false
        foreach ($folder in $folders) {
            if ($name -ceq $folder -or $name.StartsWith($folder + '\') -or $name.StartsWith($folder + ' ')) { $below = $true }
        }
        if (-not $below) { [void]$kept.Add($line) }
    }
    return ($kept.ToArray() -join "`n")
}
function TFile([string]$path, [string]$text = 'x') {
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName('\\?\' + $path))
    [IO.File]::WriteAllText('\\?\' + $path, $text)
}
function TJunction([string]$link, [string]$target) {
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($link))
    [void][IO.Directory]::CreateDirectory($target)
    New-Item -ItemType Junction -Path $link -Value $target | Out-Null
}

# In this test a refusal is an error that can be caught, not the end of the session.
function WhFail([string]$message, [int]$code = 4) {
    throw ('WhFail ' + $code + ': ' + $message)
}

# --- the scratch folder ---------------------------------------------------------

$realRoot = $root
if ($env:WIN_HOST_TEST_DIR) { $scratch = $env:WIN_HOST_TEST_DIR }
elseif ($realRoot) { $scratch = $realRoot + '\scratch\win_host_remote_test' }
else { $scratch = [IO.Path]::GetTempPath().TrimEnd('\') + '\win_host_remote_test' }
$mark = $scratch + '\.win_host_remote_test'
if ($scratch -cnotmatch '\A[A-Za-z]:(\\[A-Za-z0-9._-]+){2,}\z' -or [IO.Path]::GetFullPath($scratch) -cne $scratch) {
    [Console]::Error.WriteLine('test_win_host_remote: the scratch folder ' + $scratch + ' is not a plain folder at least two levels below a drive (letters, digits, . _ -): set WIN_HOST_TEST_DIR')
    exit 2
}
if ((Test-Path -LiteralPath $scratch) -and -not (Test-Path -LiteralPath $mark) -and @(Get-ChildItem -LiteralPath $scratch -Force).Count -gt 0) {
    [Console]::Error.WriteLine('test_win_host_remote: ' + $scratch + ' exists and is not from an earlier run of this test: refusing to use it')
    exit 2
}
function TRemoveScratch {
    if (-not (Test-Path -LiteralPath $scratch)) { return }
    # `rd /s /q` removes a link without touching what it points to; the .NET call is for long paths.
    & cmd.exe /d /c ('rd /s /q ' + $scratch + ' < NUL 2>&1') | Out-Null
    if (Test-Path -LiteralPath $scratch) { [IO.Directory]::Delete('\\?\' + $scratch, $true) }
}
TRemoveScratch
[void][IO.Directory]::CreateDirectory($scratch)
$markText = 'scratch folder of tools/dev/test_win_host_remote.ps1'
[IO.File]::WriteAllText($mark, $markText)
WhSay ('test_win_host_remote: working in ' + $scratch)

# --- 1. WhPath: names and roots ---------------------------------------------------

$root = $scratch + '\r1'
$long64 = 'a' * 64
foreach ($name in @('h1', 'H1', 'a', '0', 'a.b', 'a-b', 'a_b', 'wf_fddd8ce3-3cf-1', 'console', 'com10', 'nul2', $long64)) {
    TIs ('WhPath: name ' + $name) ((WhPath 'work' $name) + '|' + (WhPath 'target' $name) + '|' + (WhPath 'logs' $name)) ($root + '\work\' + $name + '|' + $root + '\target-' + $name + '|' + $root + '\logs\' + $name)
}
$badNames = @('', '.', '..', '...', 'a..b', 'a/b', 'a\b', '../x', '..\x', 'h1\..\..\x', 'h1/../../x', '\x', '/x', 'C:', 'C:x', 'C:\x', 'h1:stream',
    '-x', '--help', '-', 'a b', ' h1', 'h1 ', 'a;b', 'a&b', 'a|b', 'a>b', 'a<b', 'a^b', 'a,b', 'a=b', 'a(b)', 'a!b', 'a*', 'a?', 'a~1', 'a+b', 'a@b', 'a#b',
    '$(id)', '`id`', '$x', "a'b", "'", 'a"b', 'a%b', '%SystemRoot%', 'h1.', '.hidden', '_x', 'con', 'NUL', 'aux.txt', 'com1', 'LPT9', 'Con.h1', 'prn.a.b',
    ('n' + [char]0xE4 + 'me'), ([string][char]0xFF0E + [char]0xFF0E), "a`nb", "h1`n", "h1`r", "h1`r`n", "`nh1", "h1`tx", "h1`0", ('a' + $long64))
foreach ($name in $badNames) {
    foreach ($kind in @('work', 'target', 'logs')) {
        TRefused ('WhPath refuses the name ' + ($name | ConvertTo-Json) + ' (' + $kind + ')') 2 { WhPath $kind $name }
    }
}
TRefused 'WhPath refuses an unknown kind of folder' 4 { WhPath 'scratch' 'h1' }
TRefused 'WhPath refuses an empty kind of folder' 4 { WhPath '' 'h1' }
$badRoots = @('', 'D:\', 'C:\', 'c:\', 'D:\\', 'D:', 'C:', 'D:.', 'D:..', 'D:asamu', 'asamu', '\', '\asamu', '/', '.', '..', '\\server\share\x', '\\?\D:\asamu', '\\.\D:\asamu',
    'D:\asamu\', 'D:\asamu\..', 'D:\asamu\..\x', 'D:\..', 'D:\.', 'D:\.\asamu', 'D:\asamu.', 'D:\asamu.\x', 'D:\a b', 'D:\asamu ', ' D:\asamu', 'D:\a"b', "D:\a'b",
    'D:\a&b', 'D:\a;b', 'D:\a%b', 'D:\a^b', 'D:\a$b', 'D:\a,b', 'D:\a(b)', 'D:/asamu', 'D:\asamu/x', 'DD:\asamu', '1:\asamu', 'D:\asamu\\x', 'D:\asamu:x', 'D::\asamu',
    'D:\nul', 'D:\asamu\COM1', 'D:\aux.d\x', 'D:\con\asamu', "D:\asamu`n", "D:\asamu`nx", "D:\asamu`r", ('D:\' + [char]0xE4))
foreach ($bad in $badRoots) {
    $root = $bad
    foreach ($kind in @('work', 'target', 'logs')) {
        TRefused ('WhPath refuses the root ' + ($bad | ConvertTo-Json) + ' (' + $kind + ')') 2 { WhPath $kind 'h1' }
    }
    TRefused ('clean refuses the root ' + ($bad | ConvertTo-Json)) 2 { WhClean 'h1' }
    TRefused ('sync refuses the root ' + ($bad | ConvertTo-Json)) 2 { WhSyncPrepare 'h1' }
}
$root = $null
TRefused 'WhPath refuses a root that is not set' 2 { WhPath 'work' 'h1' }
$root = $scratch + '\r1'
foreach ($name in @('..', 'a\b', 'h1\..\..\x', "h1`n", 'nul', '', 'a b', "a'b")) {
    TRefused ('clean refuses the name ' + ($name | ConvertTo-Json)) 2 { WhClean $name }
    TRefused ('sync refuses the name ' + ($name | ConvertTo-Json)) 2 { WhSyncPrepare $name }
    TRefused ('entering a work tree refuses the name ' + ($name | ConvertTo-Json)) 2 { WhEnter $name }
}
TIs 'the refused names and roots left nothing behind' (TTree $scratch) ('F \.win_host_remote_test ' + $markText.Length)

# --- 2. clean removes its two folders and nothing else -----------------------------

$root = $scratch + '\r1'
$deep = ('\' + ('d' * 60)) * 5
TFile ($root + '\work\a\file.txt') 'in the work tree'
TFile ($root + '\work\a\sub\x y.txt')
TFile ($root + '\work\a\read-only.txt')
[IO.File]::SetAttributes($root + '\work\a\read-only.txt', [IO.FileAttributes]::ReadOnly)
TFile ($root + '\work\a' + $deep + '\long-path.txt')
TFile ($root + '\target-a\debug\big.bin') ('0' * 5000)
TFile ($root + '\target-a' + $deep + '\long-path.bin')
TFile ($root + '\logs\a\gate.txt')
TFile ($root + '\precious\keep.txt') 'must survive'
TJunction ($root + '\work\a\link-in-work') ($root + '\precious')
TJunction ($root + '\target-a\link-in-target') ($root + '\precious')
foreach ($other in @('\keep.txt', '\work\ab\keep.txt', '\work\a.b\keep.txt', '\work\a-\keep.txt', '\work\A2\keep.txt', '\work\keep.txt', '\target\keep.txt',
        '\target-ab\keep.txt', '\target-a.b\keep.txt', '\target-\keep.txt', '\targets-a\keep.txt', '\logs\ab\keep.txt', '\scratch\a\keep.txt', '\a\keep.txt', '\work\b\work\a\keep.txt')) {
    TFile ($root + $other)
}
$before = TTree $root
$expected = TWithout $before @('\work\a', '\target-a')
WhClean 'a'
TIs 'clean: the work tree and the target folder are gone, everything beside them is as before' (TTree $root) $expected
TIs 'clean: the folder a link in the tree pointed to still has its file' ([IO.File]::ReadAllText($root + '\precious\keep.txt')) 'must survive'
WhClean 'a'
TIs 'clean again: nothing to remove, nothing changed' (TTree $root) $expected
WhClean 'never-there'
TIs 'clean of a name that has no folders: nothing changed' (TTree $root) $expected

# A file held open by a program: the folder stays, says so, and goes on the next call.
TFile ($root + '\work\h\held.txt')
TFile ($root + '\work\h\free.txt')
TFile ($root + '\target-h\x.bin')
$held = [IO.File]::Open($root + '\work\h\held.txt', [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
try {
    TRefused 'clean: a file in use is reported (exit code 4)' 4 { WhClean 'h' }
    TIs 'clean: with a file in use, the target folder is removed all the same' ([string](Test-Path -LiteralPath ($root + '\target-h'))) 'False'
    TIs 'clean: the file in use is still there' ([string](Test-Path -LiteralPath ($root + '\work\h\held.txt'))) 'True'
    TRefused 'sync: a work tree with a file in use is refused' 4 { WhSyncPrepare 'h' }
    TIs 'sync: the tree it could not empty is marked incomplete' ([string](Test-Path -LiteralPath ($root + '\work\h\.win_host_incomplete'))) 'True'
    TRefused 'a work tree marked incomplete cannot be entered' 4 { WhEnter 'h' }
} finally {
    $held.Dispose()
}
WhClean 'h'
TIs 'clean: once the file is free the work tree goes' (TTree $root) $expected

# --- 3. links are never followed or deleted through ---------------------------------

# The work tree itself is a link.
TFile ($scratch + '\elsewhere\tree\keep.txt') 'must survive'
TJunction ($root + '\work\j') ($scratch + '\elsewhere\tree')
TFile ($root + '\target-j\x.bin')
$before = TTree $scratch
TRefused 'clean: a work tree that is a link is refused' 4 { WhClean 'j' }
TIs 'clean: the link, and the folder it points to, are untouched (the target folder is removed)' (TTree $scratch) (TWithout $before @('\r1\target-j'))
TRefused 'sync: a work tree that is a link is refused' 4 { WhSyncPrepare 'j' }
TIs 'sync: the link, and the folder it points to, are untouched' (TTree $scratch) (TWithout $before @('\r1\target-j'))
[IO.Directory]::Delete($root + '\work\j', $false)

# A name that is a file, not a folder.
TFile ($root + '\work\f')
TFile ($root + '\target-f')
$before = TTree $scratch
TRefused 'clean: a work tree name that is a file is refused' 4 { WhClean 'f' }
TRefused 'sync: a work tree name that is a file is refused' 4 { WhSyncPrepare 'f' }
TIs 'clean and sync: the files are untouched' (TTree $scratch) $before

# <root>\work is a link.
$root = $scratch + '\r2'
TFile ($scratch + '\elsewhere\works\b\keep.txt') 'must survive'
TJunction ($root + '\work') ($scratch + '\elsewhere\works')
$before = TTree $scratch
TRefused 'clean: refuses to work through <root>\work when that is a link' 4 { WhClean 'b' }
TRefused 'sync: refuses to work through <root>\work when that is a link' 4 { WhSyncPrepare 'b' }
TIs 'clean and sync through a linked <root>\work: nothing changed' (TTree $scratch) $before
TIs 'the folder behind <root>\work still has its file' ([IO.File]::ReadAllText($scratch + '\elsewhere\works\b\keep.txt')) 'must survive'

# The root itself is a link.
$root = $scratch + '\r3'
TFile ($scratch + '\elsewhere\root\work\c\keep.txt') 'must survive'
TFile ($scratch + '\elsewhere\root\target-c\keep.txt') 'must survive'
TJunction $root ($scratch + '\elsewhere\root')
$before = TTree $scratch
TRefused 'clean: refuses a root that is a link' 4 { WhClean 'c' }
TRefused 'sync: refuses a root that is a link' 4 { WhSyncPrepare 'c' }
TIs 'clean and sync through a linked root: nothing changed' (TTree $scratch) $before
TIs 'the folders behind a linked root still have their files' ([IO.File]::ReadAllText($scratch + '\elsewhere\root\work\c\keep.txt') + '/' + [IO.File]::ReadAllText($scratch + '\elsewhere\root\target-c\keep.txt')) 'must survive/must survive'

# --- 4. sync: the received files get their names, and only inside the work tree ------

$root = $scratch + '\r4'
# What win_host.sh's tar stream leaves in the work tree: the list of names and the numbered files.
function TReceive([string]$name, [string[]]$names) {
    WhSyncPrepare $name
    $staging = $root + '\work\' + $name + '\.win_host_incomplete'
    $list = [Text.Encoding]::UTF8.GetBytes((($names | ForEach-Object { $_ + "`n" }) -join ''))
    [IO.File]::WriteAllBytes($staging + '\manifest', $list)
    $bytes = 0
    for ($i = 0; $i -lt $names.Count; $i++) {
        $content = [Text.Encoding]::UTF8.GetBytes('content of file ' + ($i + 1))
        [IO.File]::WriteAllBytes($staging + '\' + ($i + 1).ToString('000000'), $content)
        $bytes += $content.Length
    }
    $sha = [Security.Cryptography.SHA256]::Create()
    $digest = ([BitConverter]::ToString($sha.ComputeHash($list))).Replace('-', '').ToLowerInvariant()
    return @($names.Count, $bytes, $digest)
}
$japanese = [string][char]0x65E5 + [char]0x672C + [char]0x8A9E
$good = @('plain.txt', 'dir/sub/deep.txt', 'with space.txt', "it's.txt", '-rf', '--help', '& calc.exe &.txt', '$(calc).txt', "'; Remove-Item -Recurse x; '.txt",
    '`whoami`.txt', 'a^b.txt', ';x.txt', '%SystemRoot%/%CD%.txt', '@(1,2)/$env_PATH.txt', ($japanese + '/' + $japanese + '.txt'), ('emoji-' + [char]::ConvertFromUtf32(0x1F980) + '.txt'),
    'CASE.txt', 'Case/lower.txt', '.hidden/.gitignore', ('long/' + ('n' * 120) + '/' + ('m' * 60) + '.txt'))
TFile ($root + '\work\s\left-over\from-an-earlier-sync.txt')
TFile ($root + '\target-s\kept.bin')
$sent = TReceive 's' $good
WhSyncVerify 's' $sent[0] $sent[1] $sent[2]
$tree = TTree ($root + '\work\s')
$want = New-Object System.Collections.ArrayList
$folders = New-Object 'System.Collections.Generic.HashSet[string]' -ArgumentList ([StringComparer]::Ordinal)
for ($i = 0; $i -lt $good.Count; $i++) {
    $windows = '\' + $good[$i].Replace('/', '\')
    [void]$want.Add('F ' + $windows + ' ' + ('content of file ' + ($i + 1)).Length)
    $at = $windows
    while ($at.LastIndexOf('\') -gt 0) { $at = $at.Substring(0, $at.LastIndexOf('\')); [void]$folders.Add('D ' + $at) }
}
$want.AddRange(@($folders))
$wanted = $want.ToArray()
[Array]::Sort($wanted, [StringComparer]::Ordinal)
TIs 'sync: the work tree holds exactly the files of the list, under their names' $tree ($wanted -join "`n")
TIs 'sync: a file has the content that was sent under its number' ([IO.File]::ReadAllText($root + '\work\s\' + $japanese + '\' + $japanese + '.txt')) 'content of file 15'
TIs 'sync: the target folder is not touched' (TTree ($root + '\target-s')) 'F \kept.bin 1'
TIs 'sync: says when it was done' ([string]((Get-Content -LiteralPath ($root + '\logs\s\sync.txt') -TotalCount 1) -match '\Alast sync \d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ, 20 files, \d+ bytes\z')) 'True'
$here = (Get-Location).ProviderPath
$savedTarget = $env:CARGO_TARGET_DIR
try {
    TIs 'a verified work tree can be entered' (WhEnter 's') ($root + '\work\s')
    TIs 'entering sets the target folder of that name' $env:CARGO_TARGET_DIR ($root + '\target-s')
} finally {
    Set-Location -LiteralPath $here
    $env:CARGO_TARGET_DIR = $savedTarget
}

# Lists that try to put a file outside the work tree, or under a name Windows reads differently.
$outside = TTree $scratch
$escapes = @('../escape.txt', '..\escape.txt', 'a/../../escape.txt', '..', 'a/..', '/escape.txt', '\escape.txt', 'C:/escape.txt', 'C:escape.txt', 'a//b.txt', 'a/./b.txt', './b.txt',
    'ends-with-dot.', 'ends-with-space ', 'folder./x.txt', 'folder /x.txt', 'stream.txt:hidden', 'a?b', 'a*b', 'a"b', 'a<b', 'a>b', 'a|b', "a`tb", "a`rb",
    '.win_host_incomplete/000001', '.WIN_HOST_INCOMPLETE/x', '.win_host_incomplete')
foreach ($escape in $escapes) {
    $sent = TReceive 'e' @('fine.txt', $escape)
    TRefused ('sync refuses the file name ' + ($escape | ConvertTo-Json)) 4 { WhSyncVerify 'e' $sent[0] $sent[1] $sent[2] }
    TIs ('sync: nothing was written outside the work tree for ' + ($escape | ConvertTo-Json)) (TWithout (TTree $scratch) @('\r4\work\e')) $outside
    TRefused ('sync: the tree stays unusable after the refused name ' + ($escape | ConvertTo-Json)) 4 { WhEnter 'e' }
}
$outside = TWithout (TTree $scratch) @('\r4\work\e', '\r4\logs\e')
$sent = TReceive 'e' @('one.txt', 'two.txt')
TRefused 'sync: a wrong number of files is refused' 4 { WhSyncVerify 'e' 3 $sent[1] $sent[2] }
$sent = TReceive 'e' @('one.txt', 'two.txt')
TRefused 'sync: a wrong number of bytes is refused' 4 { WhSyncVerify 'e' $sent[0] ($sent[1] + 1) $sent[2] }
$sent = TReceive 'e' @('one.txt', 'two.txt')
TRefused 'sync: a list that is not the one announced is refused' 4 { WhSyncVerify 'e' $sent[0] $sent[1] ('0' * 64) }
$sent = TReceive 'e' @('one.txt', 'two.txt')
[IO.File]::Delete($root + '\work\e\.win_host_incomplete\000002')
TRefused 'sync: a file that did not arrive is refused' 4 { WhSyncVerify 'e' $sent[0] $sent[1] $sent[2] }
$sent = TReceive 'e' @('one.txt', 'two.txt')
TFile ($root + '\work\e\.win_host_incomplete\000003')
TRefused 'sync: a file that is not in the list is refused' 4 { WhSyncVerify 'e' $sent[0] $sent[1] $sent[2] }
$sent = TReceive 'e' @('one.txt', 'ONE.txt')
TRefused 'sync: two names that differ only in case are refused' 4 { WhSyncVerify 'e' $sent[0] $sent[1] $sent[2] }
TRefused 'sync: after every refusal the tree is unusable' 4 { WhEnter 'e' }
$sent = TReceive 'e' @('one.txt', 'two.txt')
WhSyncVerify 'e' $sent[0] $sent[1] $sent[2]
TIs 'sync: a good sync after the refused ones leaves exactly its files' (TTree ($root + '\work\e')) ("F \one.txt 17`nF \two.txt 17")
TIs 'sync: all of that changed nothing outside the work tree and its log folder' (TWithout (TTree $scratch) @('\r4\work\e', '\r4\logs\e')) $outside
TRefused 'entering a work tree that does not exist is refused' 4 { WhEnter 'nowhere' }

# --- 5. arguments reach a program as they were given ---------------------------------

$echo = $scratch + '\echo-arguments.ps1'
[IO.File]::WriteAllText($echo, '[Console]::Out.Write(($args | ForEach-Object { [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]$_)) }) -join ",")')
$awkward = @('plain', 'a b', 'a b\', 'ends\', 'ends\\', 'sp q"uo te', '"', '\"', 'a\\"b', '""', "it's", '^&|<>', '%OS%', '!x!', '$env:PATH', '`id`', ';', '@args', '#x', 'a,b', '(x)', '=', '-x', '/x',
    '\\server\share\', 'C:\Program Files\x y\', ('caf' + [char]0xE9), $japanese, "tab`there", '  two  spaces  ', '*', '?', '~', '{a}', '[b]')
$line = New-Object System.Collections.ArrayList
foreach ($argument in $awkward) { [void]$line.Add((WhArgument $argument)) }
$start = New-Object System.Diagnostics.ProcessStartInfo
$start.FileName = (Get-Command -Name powershell.exe -CommandType Application | Select-Object -First 1).Path
$start.Arguments = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File ' + $echo + ' ' + ($line.ToArray() -join ' ')
$start.UseShellExecute = $false
$start.RedirectStandardOutput = $true
$start.RedirectStandardInput = $true
$process = [System.Diagnostics.Process]::Start($start)
$process.StandardInput.Close()
$output = $process.StandardOutput.ReadToEnd()
$process.WaitForExit()
$back = @($output.Trim().Split(',') | ForEach-Object { [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($_)) })
TIs 'arguments: as many arrive as were given' ([string]$back.Count) ([string]$awkward.Count)
for ($i = 0; $i -lt $awkward.Count; $i++) {
    TIs ('arguments: ' + ($awkward[$i] | ConvertTo-Json) + ' arrives unchanged') ([string]$back[$i]) $awkward[$i]
}
TIs 'arguments: an empty one is kept' (WhArgument '') '""'

# --- the end --------------------------------------------------------------------------

$root = $realRoot
TRemoveScratch
if (Test-Path -LiteralPath $scratch) { TBad 'the scratch folder is removed at the end' $scratch } else { TOk 'the scratch folder is removed at the end' }
WhSay ''
WhSay ('test_win_host_remote: ' + $TCount.Passed + ' passed, ' + $TCount.Failed + ' failed')
if ($TCount.Failed -gt 0) { exit 1 }
exit 0
