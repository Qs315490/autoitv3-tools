; 文件/目录族出错（以及 FileWrite 成功时的返回值）在官方与本工具下逐行对照。
;
;   官方：    AutoIt3_x64.exe docs\file-error-probe.au3
;             AutoIt3.exe 是 32 位，System32 会被重定向到 SysWOW64；这份探针只用
;             @TempDir，但 x64 那份与我们的可比性最好。
;   本工具：  au3 run docs\file-error-probe.au3 --faithful
;             （探针要建几个临时文件，默认的确定性 profile 会拒绝写入）
;
; 两边的标签应当一一对应，逐行比 value/@error/@extended。帮助页没有写 @error 的函数
; （FileOpen/FileClose/FileWrite/FileDelete/…）在这里正好能看出官方是"根本不设置"还是
; "设成 1"。末尾几条是坏句柄/只读句柄——万一官方在那种情况下直接报错终止，前面的
; 行也已经打完了。

; 结果攒起来用一个 MsgBox 弹出来：AutoIt3.exe 是 GUI 子系统程序，从控制台启动时
; 它没有控制台，ConsoleWrite 看不到（只有 SciTE 那样自己建管道的宿主收得到）。
; ConsoleWrite 仍然保留，方便在 SciTE 里直接抄文本。
Global $Report = ""

; 任何形状都转成字符串再拼——官方在某几行返回的是标量而不是数组，直接取下标的话
; 整个脚本会以 "Subscript used on non-accessible variable" 终止，一行都拿不到。
Func Show($v)
    If IsArray($v) Then
        Local $s = "array[" & UBound($v) & "]"
        Local $i
        For $i = 0 To UBound($v) - 1
            $s = $s & "|" & Show($v[$i])
        Next
        Return $s
    EndIf
    If IsString($v) Then
        Return "[" & $v & "]"
    EndIf
    Return "[" & String($v) & "]"
EndFunc

Func P($label, $value, $err, $ext)
    Local $line = $label & " => " & Show($value) & " @error=" & $err & " @extended=" & $ext
    ; 普通赋值就改脚本级那个 Global；`Global $Report = $Report & ...` 会被官方
    ; 解释器拒绝："Can not initialize a variable with itself"。
    $Report = $Report & $line & @CRLF
    ConsoleWrite($line & @CRLF)
EndFunc

Local $dir = @TempDir & "\au3-file-probe"
DirCreate($dir)
Local $missing = $dir & "\no-such-file.txt"
Local $empty = $dir & "\empty.txt"
Local $text = $dir & "\text.txt"
Local $named = $dir & "\named.txt"
FileDelete($empty)
FileDelete($text)
FileDelete($named)

Local $v

; ---- 建两个夹具文件：顺便看 FileWrite 用文件名时的返回值 ----
$v = FileWrite($empty, "")
P("FileWrite(empty-file, empty)", $v, @error, @extended)

$v = FileWrite($text, "one" & @CRLF & "two" & @CRLF)
P("FileWrite(text-file, 10 bytes)", $v, @error, @extended)

$v = FileWriteLine($named, "line")
P("FileWriteLine(new-file, line)", $v, @error, @extended)

$v = FileWriteLine($named, "already" & @CRLF)
P("FileWriteLine(ends in CRLF)", $v, @error, @extended)

$v = FileGetSize($named)
P("FileGetSize(named)", $v, @error, @extended)

; ---- 读查询 ----
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

; ---- 目录与查找 ----
$v = DirGetSize($missing)
P("DirGetSize(missing)", $v, @error, @extended)

Local $a = DirGetSize($missing, 1)
P("DirGetSize(missing,1)", $a, @error, @extended)

$v = DirGetSize($dir)
P("DirGetSize(dir)", $v, @error, @extended)

$v = FileFindFirstFile($dir & "\nothing-*.txt")
P("FileFindFirstFile(no match)", $v, @error, @extended)

$v = FileFindFirstFile($missing & "\*.txt")
P("FileFindFirstFile(missing dir)", $v, @error, @extended)

$v = FileFindFirstFile($missing)
P("FileFindFirstFile(missing path)", $v, @error, @extended)

; ---- 数组 ----
$a = FileReadToArray($empty)
P("FileReadToArray(empty)", $a, @error, @extended)

$a = FileReadToArray($missing)
P("FileReadToArray(missing)", $a, @error, @extended)

; ---- FileSetPos 的三种 origin（帮助页是三参数）----
; @error 必须在 FileSetPos 之后立刻取走：中间任何一次函数调用（包括下面那个
; FileGetPos）都会把它重置。
Local $hp = FileOpen($text, 0)
Local $e = 0
Local $x = 0

$v = FileSetPos($hp, 0, 2)
$e = @error
$x = @extended
Local $at_end = FileGetPos($hp)
P("FileSetPos(end,0)", $v & "/" & $at_end, $e, $x)

$v = FileSetPos($hp, -2, 1)
$e = @error
$x = @extended
Local $at_back = FileGetPos($hp)
P("FileSetPos(cur,-2)", $v & "/" & $at_back, $e, $x)

$v = FileSetPos($hp, 2, 0)
$e = @error
$x = @extended
Local $at_start = FileGetPos($hp)
P("FileSetPos(begin,2)", $v & "/" & $at_start, $e, $x)

FileClose($hp)

; ---- 定位之后再写：非追加模式写在当前位置，追加模式忽略位置写末尾 ----
Local $sp = $dir & "\seek.txt"
FileDelete($sp)
Local $sh = FileOpen($sp, 2)
FileWrite($sh, "abcdef")
Local $moved = FileSetPos($sh, 2, 0)
Local $wrote = FileWrite($sh, "X")
$e = @error
$x = @extended
Local $at = FileGetPos($sh)
FileClose($sh)
Local $ah = FileOpen($sp, 1)
Local $append_start = FileGetPos($ah)
FileSetPos($ah, 0, 0)
FileWrite($ah, "Z")
FileClose($ah)
Local $rh = FileOpen($sp, 0)
Local $content = FileRead($rh)
FileClose($rh)
; 期望：位置 2 写 X 后是 abXdef、位置 3；$FO_APPEND 打开的句柄**起点在末尾**
; （start=6）但把位置挪到 0 之后照样写在 0 —— 于是 Z 覆盖首字节，文件成 ZbXdef。
P("seek(2)+write(X)+append(Z) pos=" & $at & " start=" & $append_start, $content, $e, $x)

; ---- 坏句柄 / 只读句柄（放最后）----
$v = FileClose(9999)
P("FileClose(9999)", $v, @error, @extended)

$v = FileFlush(9999)
P("FileFlush(9999)", $v, @error, @extended)

$v = FileGetPos(9999)
P("FileGetPos(9999)", $v, @error, @extended)

$v = FileSetPos(9999, 0, 0)
P("FileSetPos(9999)", $v, @error, @extended)

$v = FileRead(9999)
P("FileRead(9999)", $v, @error, @extended)

$v = FileReadLine(9999)
P("FileReadLine(9999)", $v, @error, @extended)

$v = FileWrite(9999, "x")
P("FileWrite(9999)", $v, @error, @extended)

Local $h = FileOpen($text, 0)
$v = FileWrite($h, "x")
P("FileWrite(read-only handle)", $v, @error, @extended)
FileClose($h)

MsgBox(64, "file-error-probe", $Report)
