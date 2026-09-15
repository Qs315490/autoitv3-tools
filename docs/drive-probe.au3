; 官方解释器探针：DriveGetDrive 在"没有这种盘"时到底返回什么。
;
; 用法（装了 AutoIt3 的 Windows 上，不依赖任何 #include）：
;     AutoIt3.exe docs\drive-probe.au3
; 或直接双击。把消息框里三行贴回来。
;
; 问的是失败时的形状：自动帮助页只说"失败时 @error=1"，没说返回值。
; 我们需要知道它是"长度 1、[0]=0 的数组"还是"标量 0"，因为脚本会写 $d[0]。
; "BOGUS" 是文档里"参数不是合法类型"的那条失败路径，任何机器上都走得到。

Func Show($tag, $a)
Local $type = VarGetType($a)
Local $ub = -1
Local $first = String($a)
If IsArray($a) Then
$ub = UBound($a, 1)
$first = $a[0]
EndIf
Return $tag & ": type=" & $type & " ub=" & $ub & " [0]=" & $first & @CRLF
EndFunc

Local $bogus = DriveGetDrive("BOGUS")
Local $bogus_err = @error
Local $all = DriveGetDrive("ALL")
Local $all_err = @error
Local $text = Show("BOGUS", $bogus) & Show("ALL", $all)
$text = $text & "err: bogus=" & $bogus_err & " all=" & $all_err & @CRLF
MsgBox(0, "probe", $text)
