; 文件/目录族出错时的 @error / @extended，用来和官方解释器逐行对齐。
;
;   官方：    AutoIt3_x64.exe docs\file-error-probe.au3
;             （AutoIt3.exe 是 32 位，System32 会被重定向到 SysWOW64；这个探针只用
;              @TempDir，不碰 System32，但 x64 那份的输出与我们的可比性最好）
;   本工具：  au3 run docs\file-error-probe.au3 --faithful
;             （探针要建两个临时文件，默认的确定性 profile 会拒绝写入）
;
; 两边的行数和标签应当完全对应，逐行比 @error/@extended；哪一行不一样就是哪儿需要改。
; 帮助页没有写 @error 的函数（FileOpen/FileClose/FileWrite/FileDelete/…）在这里正好
; 能看出来官方到底是"不设置"还是"设成 1"。

Func P($label, $value, $err, $ext)
    ConsoleWrite($label & " => [" & $value & "] @error=" & $err & " @extended=" & $ext & @CRLF)
EndFunc

Local $dir = @TempDir & "\au3-file-probe"
DirCreate($dir)
Local $missing = $dir & "\no-such-file.txt"
Local $empty = $dir & "\empty.txt"
Local $text = $dir & "\text.txt"
FileDelete($empty)
FileDelete($text)
FileWrite($empty, "")
FileWrite($text, "one" & @CRLF & "two" & @CRLF)

Local $v

$v = FileExists($missing)
P("FileExists(missing)", $v, @error, @extended)

$v = FileGetSize($missing)
P("FileGetSize(missing)", $v, @error, @extended)

$v = FileGetSize($dir)
P("FileGetSize(dir)", $v, @error, @extended)

$v = FileGetSize($text)
P("FileGetSize(text)", $v, @error, @extended)

$v = FileGetTime($missing)
P("FileGetTime(missing)", $v, @error, @extended)

$v = FileGetAttrib($missing)
P("FileGetAttrib(missing)", $v, @error, @extended)

$v = FileGetLongName($missing)
P("FileGetLongName(missing)", $v, @error, @extended)

$v = FileGetShortName($missing)
P("FileGetShortName(missing)", $v, @error, @extended)

$v = FileGetLongName(@TempDir)
P("FileGetLongName(dir)", $v, @error, @extended)

$v = FileGetVersion($missing)
P("FileGetVersion(missing)", $v, @error, @extended)

$v = FileGetEncoding($missing)
P("FileGetEncoding(missing)", $v, @error, @extended)

$v = FileOpen($missing, 0)
P("FileOpen(missing)", $v, @error, @extended)

$v = FileClose(9999)
P("FileClose(9999)", $v, @error, @extended)

$v = FileFlush(9999)
P("FileFlush(9999)", $v, @error, @extended)

$v = FileGetPos(9999)
P("FileGetPos(9999)", $v, @error, @extended)

$v = FileSetPos(9999, 0)
P("FileSetPos(9999)", $v, @error, @extended)

$v = FileRead(9999)
P("FileRead(9999)", $v, @error, @extended)

$v = FileReadLine(9999)
P("FileReadLine(9999)", $v, @error, @extended)

$v = FileWrite(9999, "x")
P("FileWrite(9999)", $v, @error, @extended)

Local $a = FileReadToArray($empty)
P("FileReadToArray(empty)[0]", $a[0], @error, @extended)

$a = FileReadToArray($missing)
P("FileReadToArray(missing)[0]", $a[0], @error, @extended)

$v = DirGetSize($missing)
P("DirGetSize(missing)", $v, @error, @extended)

$a = DirGetSize($missing, 1)
P("DirGetSize(missing,1)[0]", $a[0], @error, @extended)

$v = DirGetSize($dir)
P("DirGetSize(dir)", $v, @error, @extended)

$v = FileFindFirstFile($dir & "\nothing-*.txt")
P("FileFindFirstFile(empty match)", $v, @error, @extended)

$v = FileFindFirstFile($missing & "\*.txt")
P("FileFindFirstFile(missing dir)", $v, @error, @extended)

$v = FileFindFirstFile($missing)
P("FileFindFirstFile(missing path)", $v, @error, @extended)
