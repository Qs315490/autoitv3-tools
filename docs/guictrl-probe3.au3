; 官方解释器探针之三：把探针二没问完的补齐（探针二在 $GUI_EXPAND 处挂了）。
;
; 用法：AutoIt3.exe docs\guictrl-probe3.au3 > probe3.txt
;
; 重点是 ListViewItem 的"写哪一格"规则，和 TabItem/TreeViewItem 的读法。

Global Const $GUI_CHECKED = 0x01
Global Const $GUI_INDETERMINATE = 0x02
Global Const $GUI_UNCHECKED = 0x04
Global Const $GUI_SHOW = 0x10
Global Const $GUI_HIDE = 0x20
Global Const $GUI_ENABLE = 0x40
Global Const $GUI_DISABLE = 0x80
Global Const $GUI_FOCUS = 0x100
Global Const $GUI_DEFBUTTON = 0x200
Global Const $GUI_EXPAND = 0x400

; ---------------------------------------------------------------------------
; 1. ListViewItem 的写入规则：三列 "a|b|c" 为底，逐个试
; ---------------------------------------------------------------------------
Local $win = GUICreate("probe3", 400, 300)
Local $lv = GUICtrlCreateListView("c1|c2|c3", 0, 0, 300, 120)
Local $item = GUICtrlCreateListViewItem("a|b|c", $lv)
Func Reset()
    GUICtrlSetData($item, "a|b|c")
EndFunc
Reset()
GUICtrlSetData($item, "x")
ConsoleWrite("cell.one_field=" & GUICtrlRead($item) & @CRLF)
Reset()
GUICtrlSetData($item, "x|y")
ConsoleWrite("cell.two_fields=" & GUICtrlRead($item) & @CRLF)
Reset()
GUICtrlSetData($item, "x|")
ConsoleWrite("cell.x_and_sep=" & GUICtrlRead($item) & @CRLF)
Reset()
GUICtrlSetData($item, "|y")
ConsoleWrite("cell.sep_and_y=" & GUICtrlRead($item) & @CRLF)
Reset()
GUICtrlSetData($item, "|||")
ConsoleWrite("cell.three_seps=" & GUICtrlRead($item) & @CRLF)
Reset()
GUICtrlSetData($item, "x||z")
ConsoleWrite("cell.x_sep_sep_z=" & GUICtrlRead($item) & @CRLF)
Reset()
GUICtrlSetData($item, "")
ConsoleWrite("cell.empty=" & GUICtrlRead($item) & @CRLF)
Reset()
Local $ret = GUICtrlSetData($item, "||9")
ConsoleWrite("cell.ret=" & $ret & " err=" & @error & @CRLF)
ConsoleWrite("cell.after_||9=" & GUICtrlRead($item) & @CRLF)

; 对 ListView 本身写数据：返回值/错误码
Local $lvret = GUICtrlSetData($lv, "x|y|z")
ConsoleWrite("lv.setdata_ret=" & $lvret & " err=" & @error & @CRLF)
ConsoleWrite("item.after_lv_setdata=" & GUICtrlRead($item) & @CRLF)

; 单列 ListView 的 GetSubItemCount 与 item 读法
Local $one = GUICtrlCreateListView("only", 0, 140, 200, 60)
Local $oneitem = GUICtrlCreateListViewItem("solo", $one)
ConsoleWrite("onecol.item_read=" & GUICtrlRead($oneitem) & @CRLF)
ConsoleWrite("onecol.subitem_count=" & ControlListView($win, "", $one, "GetSubItemCount") & @CRLF)
GUICtrlSetData($oneitem, "solo2")
ConsoleWrite("onecol.after_setdata=" & GUICtrlRead($oneitem) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; 2. TreeViewItem：$GUI_EXPAND 之后的状态字（探针二挂在这里）
; ---------------------------------------------------------------------------
$win = GUICreate("probe3", 400, 300)
Local $tv = GUICtrlCreateTreeView(0, 0, 150, 150)
Local $root = GUICtrlCreateTreeViewItem("root", $tv)
Local $child = GUICtrlCreateTreeViewItem("child", $root)
ConsoleWrite("tvitem.fresh_state=" & GUICtrlGetState($root) & @CRLF)
GUICtrlSetState($child, $GUI_EXPAND)
ConsoleWrite("tvitem.after_expand=" & GUICtrlRead($child) & @CRLF)
GUICtrlSetState($child, $GUI_FOCUS)
ConsoleWrite("tvitem.after_focus=" & GUICtrlRead($child) & @CRLF)
GUICtrlSetState($child, $GUI_DEFBUTTON)
ConsoleWrite("tvitem.after_bold=" & GUICtrlRead($child) & @CRLF)
GUICtrlSetState($child, 0)
ConsoleWrite("tvitem.after_zero=" & GUICtrlRead($child) & @CRLF)
ConsoleWrite("tvitem.after_zero_adv=" & GUICtrlRead($child, 1) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; 3. TabItem：默认读法、改名、状态字
; ---------------------------------------------------------------------------
$win = GUICreate("probe3", 400, 300)
Local $tab = GUICtrlCreateTab(0, 0, 200, 150)
Local $page1 = GUICtrlCreateTabItem("one")
Local $label1 = GUICtrlCreateLabel("first", 10, 30)
Local $page2 = GUICtrlCreateTabItem("two")
Local $label2 = GUICtrlCreateLabel("second", 10, 30)
GUICtrlCreateTabItem("")
ConsoleWrite("tabitem.read=" & GUICtrlRead($page1) & @CRLF)
ConsoleWrite("tabitem.read_adv=" & GUICtrlRead($page1, 1) & @CRLF)
ConsoleWrite("tabitem.state=" & GUICtrlGetState($page1) & @CRLF)
GUICtrlSetData($page1, "renamed")
ConsoleWrite("tabitem.after_setdata=" & GUICtrlRead($page1) & @CRLF)
GUICtrlSetState($page1, $GUI_SHOW)
ConsoleWrite("tab.read_after_show1=" & GUICtrlRead($tab) & @CRLF)
GUICtrlSetState($page2, $GUI_SHOW)
ConsoleWrite("tab.read_after_show2=" & GUICtrlRead($tab) & @CRLF)
ConsoleWrite("label1.state=" & GUICtrlGetState($label1) & @CRLF)
ConsoleWrite("label1.visible=" & GUICtrlGetState($label1) & @CRLF)
GUIDelete($win)
