; 官方解释器探针：@error / @extended 到底在什么情况下能"走出"一个函数。
;
; 用法（装了 AutoIt3 的 Windows 上，不依赖任何 #include）：
;     AutoIt3.exe docs\error-scope-probe.au3
; 或直接双击。把消息框里的 A..G 七行原样贴回来。
;
; 背景：官方 3.3 的规则不是"进函数清 0、返回时原样带出来"，而是"只有本函数自己
; 调过 SetError/SetExtended，返回值才能带出去；被调函数留下的 @error 在本函数内部
; 看得见，函数一返回就没了"。这里逐条量：
;   A 只调 SetError、之后不再调用别的函数   → 应该能带出去
;   B `Return SetError(...)`                → 应该能带出去（官方 UDF 的惯用写法）
;   C SetError 之后又调用了内建函数(Sleep)  → 内建函数会把值冲掉
;   D 只发生内建函数失败(FileOpen 打不开)   → 这一条最关键：能不能带出去？
;   E 代理函数 `Return 另一个函数()`        → 本函数没 SetError，应该带不出去
;   F 调用之后自己再 SetError               → 应该能带出去
;   G 只调 SetExtended                      → @extended 带出去、@error 为 0

Func OnlySetError()
	SetError(11, 7, 0)
EndFunc

Func ReturnSetError()
	Return SetError(12, 8, 0)
EndFunc

Func SetThenCall()
	SetError(13, 9, 0)
	Sleep(1)
EndFunc

Func BuiltinFail()
	Local $h = FileOpen("Z:\no\such\dir\x.txt")
	Return 0
EndFunc

Func ProxyReturn()
	Return ReturnSetError()
EndFunc

Func SetAfterCall()
	ReturnSetError()
	SetError(14, 10, 0)
EndFunc

Func OnlySetExtended()
	SetExtended(21, 0)
EndFunc

Local $r = ""
OnlySetError()
$r = $r & "A=" & @error & "/" & @extended & @CRLF
ReturnSetError()
$r = $r & "B=" & @error & "/" & @extended & @CRLF
SetThenCall()
$r = $r & "C=" & @error & "/" & @extended & @CRLF
BuiltinFail()
$r = $r & "D=" & @error & @CRLF
ProxyReturn()
$r = $r & "E=" & @error & "/" & @extended & @CRLF
SetAfterCall()
$r = $r & "F=" & @error & "/" & @extended & @CRLF
OnlySetExtended()
$r = $r & "G=" & @error & "/" & @extended & @CRLF
MsgBox(0, "probe", $r)
