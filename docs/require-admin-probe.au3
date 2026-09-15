; #RequireAdmin 探针（仅 Windows）。
;
; 官方解释器：  AutoIt3.exe docs\require-admin-probe.au3
; 本工具：      au3 run  docs\require-admin-probe.au3
; 对照（不提权）：au3 run  docs\require-admin-probe.au3 --no-elevate
;
; 前两次应当弹一次 UAC，并且 IsAdmin() 与 config\SYSTEM 的读数都是"有权限"；
; 对照那次不弹窗、IsAdmin()=0、系统配置单元读不开。三行的形状一致就说明
; "#RequireAdmin 之后以管理员身份跑"与官方一致。
#RequireAdmin

ConsoleWrite("IsAdmin()=" & IsAdmin() & @CRLF)
ConsoleWrite("@UserName=" & @UserName & @ComputerName & @CRLF)

; IsAdmin() 是脚本视角的自述，这里再做一次只有管理员能做到的事作为旁证：
; 读 System32\config\SYSTEM（默认 ACL 只给 Administrators 与 SYSTEM，
; 普通用户会拿到拒绝访问）。只读、不改动任何东西。
Local $fh = FileOpen(@WindowsDir & "\System32\config\SYSTEM", 0)
Local $state = "denied"
If $fh <> -1 Then
    $state = "ok"
    FileClose($fh)
EndIf
ConsoleWrite("read System32\config\SYSTEM=" & $state & " @error=" & @error & @CRLF)
