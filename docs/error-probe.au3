; 官方解释器探针：被调函数用 SetError() 设的值，会不会传给调用者。
;
; 用法（装了 AutoIt3 的 Windows 上，不依赖任何 #include）：
;     AutoIt3.exe docs\error-probe.au3
; 或直接双击。把消息框里的三行原样贴回来。
;
; 问的是 @error 的传播规则：官方帮助页说"进入用户函数时 @error 置 0"，
; 源码里 `Parser_UserFunctionCall` 也是进函数清 0、返回时**不**恢复调用者的值
; （只有 adlib/hotkey/GUI 事件回调才恢复）。这里量一下直接调用和 Call() 两种写法。

Func Inner()
	Return SetError(-2, 0, 0)
EndFunc

Func OuterDirect()
	Inner()
EndFunc

Func OuterCall()
	Call("Inner")
EndFunc

Local $text = "version=" & @AutoItVersion & @CRLF
OuterDirect()
$text &= "direct=" & @error & @CRLF
OuterCall()
$text &= "call=" & @error & @CRLF
MsgBox(0, "probe", $text)
