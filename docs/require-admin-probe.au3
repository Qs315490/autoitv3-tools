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

Local $dir = @WindowsDir
Local $system = $dir & "\System32\config\SYSTEM"

ConsoleWrite("IsAdmin()=" & IsAdmin() & @CRLF)
ConsoleWrite("@WindowsDir=" & $dir & @CRLF)
ConsoleWrite("@UserName=" & @UserName & " @ComputerName=" & @ComputerName & @CRLF)

Local $exists = FileExists($system)
Local $exists_err = @error
ConsoleWrite("FileExists(" & $system & ")=" & $exists & " @error=" & $exists_err & @CRLF)

Local $size = FileGetSize($system)
Local $size_err = @error
ConsoleWrite("FileGetSize=" & $size & " @error=" & $size_err & @CRLF)

Local $fh = FileOpen($system, 0)
Local $open_err = @error
If $fh <> -1 Then
    FileClose($fh)
EndIf
ConsoleWrite("FileOpen=" & $fh & " @error=" & $open_err & @CRLF)
