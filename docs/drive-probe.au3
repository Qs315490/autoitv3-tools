; 官方解释器探针：DriveGetDrive 失败时到底返回什么形状。
;
; 用法（装了 AutoIt3 的 Windows 上，不依赖任何 #include）：
;     AutoIt3.exe docs\drive-probe.au3
; 或直接双击。把消息框里几行贴回来。
;
; 帮助页只说"失败时 @error=1"（参数不是合法类型，或本机没有该类型的盘），
; 没写返回值。已经量到 `"BOGUS"`（非法类型）返回的是**空字符串**；
; 这里再量"合法类型但没有这种盘"，看是不是同样返回空字符串。

Func Show($tag, $a)
Local $type = VarGetType($a)
If IsArray($a) Then
Return $tag & ": type=" & $type & " ub=" & UBound($a, 1) & " [0]=" & $a[0] & @CRLF
EndIf
Return $tag & ": type=" & $type & " value=" & $a & @CRLF
EndFunc

Local $bogus = DriveGetDrive("BOGUS")
Local $bogus_err = @error
Local $ram = DriveGetDrive("RAMDISK")
Local $ram_err = @error
Local $cd = DriveGetDrive("CDROM")
Local $cd_err = @error
Local $all = DriveGetDrive("ALL")
Local $all_err = @error
Local $text = Show("BOGUS", $bogus) & Show("RAMDISK", $ram) & Show("CDROM", $cd) & Show("ALL", $all)
$text = $text & "err: bogus=" & $bogus_err & " ram=" & $ram_err & " cd=" & $cd_err & " all=" & $all_err & @CRLF
MsgBox(0, "probe", $text)
