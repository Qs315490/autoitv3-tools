; 官方解释器探针：`$数组[下标]()`——"函数名当值放进数组、再按下标调用"。
;
; 用法（装了 AutoIt3 的 Windows 上，不依赖任何 #include）：
;     AutoIt3.exe docs\varcall-probe.au3
; 或直接双击。把消息框里的内容（或者报的语法/运行错误）原样贴回来。
;
; 混淆脚本里满屏都是「裸函数名放进数组、按下标调用」，而那张表是这样建起来的
; （一份保护器生成的脚本里的一行）：
;     Local $g[] = [14, FileFindNextFile, IniRead, DllClose, ...]   ; 裸函数名，没有 $ 也没有引号
; 要确认三件事：
;   1. 官方 3.3.16 里"裸函数名当值"和 `$数组[下标]()` 是不是合法语法；
;   2. 是的话，被调函数用 SetError() 设的 @error 是否照样传给调用者（直接调用也一样）；
;   3. 顺带量一下直接调用作对照。

Func Inner()
	Return SetError(-2, 0, 0)
EndFunc

Func OuterDirect()
	Inner()
EndFunc

Func OuterIndirect($tbl)
	$tbl[0]()
EndFunc

Global $tbl[1]
$tbl[0] = Inner

Local $text = "version=" & @AutoItVersion & @CRLF
OuterDirect()
$text &= "direct=" & @error & @CRLF
OuterIndirect($tbl)
$text &= "indirect=" & @error & @CRLF
MsgBox(0, "probe", $text)
