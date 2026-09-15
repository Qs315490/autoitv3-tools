; 诊断脚本：用**我们的 au3** 跑，看盘枚举和 Windows 目录判断在你这台机器上是什么样。
;
;     .\au3 run .\docs\win-drive-check.au3
;
; 它做的正是 EDv9 那段代码做的事：DriveGetDrive("FIXED,REMOVABLE") 列盘，
; 然后对每个盘拼 "\Windows"，看这个目录在不在、是不是目录、有没有
; System32\config\SYSTEM 和 SOFTWARE。输出可以直接定位卡在哪一步。

Local $a = DriveGetDrive("FIXED,REMOVABLE")
Local $err = @error
ConsoleWrite("DriveGetDrive(FIXED,REMOVABLE) type=" & VarGetType($a) & " err=" & $err & @CRLF)
ConsoleWrite("@WindowsDir=" & @WindowsDir & "  @SystemDrive=" & @SystemDrive & @CRLF)
If Not IsArray($a) Then
ConsoleWrite("  (不是数组，值=" & $a & ") —— 原生 DriveGetDrive 还是旧的？" & @CRLF)
Exit
EndIf
For $i = 1 To $a[0]
Local $root = $a[$i]
Local $win = StringTrimRight($root, 1) & "\Windows"
Local $hives = FileExists($win & "\System32\config\SYSTEM") And FileExists($win & "\System32\config\SOFTWARE")
Local $is_dir = StringInStr(FileGetAttrib($win), "D") ? 1 : 0
Local $same = ($win = @WindowsDir) ? 1 : 0
ConsoleWrite("  root=" & $root & " type=" & DriveGetType($root) & " status=" & DriveStatus($root) & _
" win=" & $win & " exists=" & FileExists($win) & " isdir=" & $is_dir & " hives=" & $hives & " ==@WindowsDir=" & $same & @CRLF)
Next
