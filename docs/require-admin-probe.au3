; #RequireAdmin 探针（仅 Windows）。
;
; 三份输出对照着看，判据是**官方的同一份脚本**：
;
;   官方解释器：    AutoIt3.exe docs\require-admin-probe.au3    → 弹 UAC
;   本工具：        au3 run  docs\require-admin-probe.au3        → 弹 UAC
;   本工具（不提权）：au3 run  docs\require-admin-probe.au3 --no-elevate
;
; 前两次的 IsAdmin() 都应当是 1（而且是同一个 API 的答案，不是我们自说自话），
; 对照那次是 0。@WindowsDir 三行应当完全一样——它是提权后仍然要准确的路径宏。
;
; 最后三行看的是 System32\config 这个目录：默认 ACL 里普通用户连列目录/读属性
; 都没有，管理员有。所以 FileExists/FileGetSize 在前两次应当成功、对照那次应当
; 失败；FileOpen 这一行**不是**判据——那两个配置单元被内核持有，管理员也打不开，
; 三行都可能一样。哪一行对不上，把三份输出一起贴出来。
#RequireAdmin

; 结果攒起来用一个 MsgBox 弹出来：AutoIt3.exe 是 GUI 子系统程序，从控制台启动时
; 它**没有**控制台，ConsoleWrite 看不到（只有 SciTE 那样自己建管道的宿主收得到）；
; 提权副本更是连父进程的管道都继承不到，而对话框与这些都无关。
Global $Report = ""

Func Say($line)
    Global $Report = $Report & $line & @CRLF
    ConsoleWrite($line & @CRLF)
EndFunc

Local $dir = @WindowsDir
Local $system = $dir & "\System32\config\SYSTEM"

Say("IsAdmin()=" & IsAdmin())
Say("@WindowsDir=" & $dir)
Say("@UserName=" & @UserName & " @ComputerName=" & @ComputerName)

Local $exists = FileExists($system)
Local $exists_err = @error
Say("FileExists(" & $system & ")=" & $exists & " @error=" & $exists_err)

Local $size = FileGetSize($system)
Local $size_err = @error
Say("FileGetSize=" & $size & " @error=" & $size_err)

Local $fh = FileOpen($system, 0)
Local $open_err = @error
If $fh <> -1 Then
    FileClose($fh)
EndIf
Say("FileOpen=" & $fh & " @error=" & $open_err)

MsgBox(64, "#RequireAdmin probe", $Report)
