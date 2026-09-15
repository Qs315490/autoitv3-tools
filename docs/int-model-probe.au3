; 整数模型探针：字面量怎么定型、运算溢出时是回绕还是升位。
;
;   官方：    AutoIt3_x64.exe docs\int-model-probe.au3
;   本工具：  au3 run docs\int-model-probe.au3
;
; 动机：EDv9 的 `YBTAOQLMPVSD` 里有 `Local $ZCOJSVR[$JABWOGY + 4294967295]`，
; 按"减一"用（0xFFFFFFFF = -1）。官方跑得动，说明它对这个字面量/这次加法的处理
; 和我们不同：我们按 64 位算，于是申请 42 亿个元素。这份输出用来钉准规则。
Local $s = ""
$s = $s & "VarGetType(4294967295)=" & VarGetType(4294967295) & @CRLF
$s = $s & "4294967295=" & 4294967295 & @CRLF
$s = $s & "5 + 4294967295=" & (5 + 4294967295) & @CRLF
$s = $s & "VarGetType(5 + 4294967295)=" & VarGetType(5 + 4294967295) & @CRLF
$s = $s & "VarGetType(2147483648)=" & VarGetType(2147483648) & @CRLF
$s = $s & "2147483647 + 1=" & (2147483647 + 1) & @CRLF
$s = $s & "VarGetType(2147483647 + 1)=" & VarGetType(2147483647 + 1) & @CRLF
$s = $s & "0xFFFFFFFF=" & 0xFFFFFFFF & @CRLF
$s = $s & "VarGetType(0xFFFFFFFF)=" & VarGetType(0xFFFFFFFF) & @CRLF
$s = $s & "0x100000000=" & 0x100000000 & @CRLF
$s = $s & "2 ^ 32=" & (2 ^ 32) & @CRLF
$s = $s & "VarGetType(2 ^ 32)=" & VarGetType(2 ^ 32) & @CRLF
$s = $s & "1000000 * 1000000=" & (1000000 * 1000000) & @CRLF
$s = $s & "VarGetType(1000000 * 1000000)=" & VarGetType(1000000 * 1000000) & @CRLF
$s = $s & "VarGetType(4294967296)=" & VarGetType(4294967296) & @CRLF
$s = $s & "4294967296=" & 4294967296 & @CRLF
$s = $s & "Int(4294967295)=" & Int(4294967295) & @CRLF
$s = $s & "5 - 1=" & (5 - 1) & @CRLF
$s = $s & "-1=" & (-1) & @CRLF
MsgBox(64, "int model", $s)
