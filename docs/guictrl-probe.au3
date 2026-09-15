; 官方解释器探针：把下面这些函数的真实行为打出来，用来核准仿真层的语义。
;
; 用法（在装了 AutoIt3 的 Windows 上）：
;     AutoIt3.exe docs\guictrl-probe.au3 > probe.txt
; 或双击生成的 exe。脚本自己 GUICreate 后立刻 GUIDelete，窗口只会闪一下。
; 输出每行一个 `名字=值`，把整个 probe.txt 贴回来即可。
;
; 只问文档没写清、或者两种读法都说得通的地方：ListView 的两种加行方式、
; 读回来的是索引还是控件号、页签是第几页被自动选中、-1 是不是"最后创建的控件"。

; 常量写成字面量：两份解释器都认，且不必依赖 AutoIt 安装目录里的 GUIConstantsEx.au3。
Global Const $GUI_CHECKED = 0x01
Global Const $GUI_INDETERMINATE = 0x02
Global Const $GUI_UNCHECKED = 0x04
Global Const $GUI_SHOW = 0x10
Global Const $GUI_HIDE = 0x20
Global Const $GUI_FOCUS = 0x100
Global Const $GUI_DEFBUTTON = 0x200
Global Const $GUI_EXPAND = 0x400
Global Const $GUI_ONTOP = 0x800

ConsoleWrite("-- version=" & @AutoItVersion & @CRLF)

; ---------------------------------------------------------------------------
; ListView：GUICtrlSetData 与 GUICtrlCreateListViewItem 各做了什么
; ---------------------------------------------------------------------------
Local $win = GUICreate("probe", 400, 300)
Local $lv = GUICtrlCreateListView("c1|c2|c3", 0, 0, 300, 100)
GUICtrlSetData($lv, "data1|data2")
GUICtrlSetData($lv, "data3|data4")
ConsoleWrite("lv.setdata_count=" & GUICtrlSendMsg($lv, 0x1004, 0, 0) & @CRLF)
ConsoleWrite("lv.read_after_setdata=" & GUICtrlRead($lv) & @CRLF)

Local $item1 = GUICtrlCreateListViewItem("i1a|i1b|i1c", $lv)
Local $item2 = GUICtrlCreateListViewItem("i2a|i2b|i2c", $lv)
ConsoleWrite("item1=" & $item1 & " item2=" & $item2 & @CRLF)
ConsoleWrite("lv.count_after_items=" & GUICtrlSendMsg($lv, 0x1004, 0, 0) & @CRLF)
ConsoleWrite("item1.read=" & GUICtrlRead($item1) & @CRLF)
ConsoleWrite("item1.read_adv=" & GUICtrlRead($item1, 1) & @CRLF)
ConsoleWrite("lv.read_unselected=" & GUICtrlRead($lv) & @CRLF)
ConsoleWrite("lv.read_adv_unselected=" & GUICtrlRead($lv, 1) & @CRLF)

; 选中第一行：官方内置的 ControlListView 有几个命令
Local $sel = ControlListView($win, "", $lv, "Select", 0)
ConsoleWrite("clv.select_ret=" & $sel & " err=" & @error & @CRLF)
ConsoleWrite("lv.read_selected=" & GUICtrlRead($lv) & @CRLF)
ConsoleWrite("lv.read_adv_selected=" & GUICtrlRead($lv, 1) & @CRLF)
ConsoleWrite("clv.getselected=" & ControlListView($win, "", $lv, "GetSelected") & @CRLF)
ConsoleWrite("clv.getitemcount=" & ControlListView($win, "", $lv, "GetItemCount") & @CRLF)
ConsoleWrite("clv.gettext_0_0=" & ControlListView($win, "", $lv, "GetText", 0, 0) & @CRLF)
ConsoleWrite("clv.gettext_0_2=" & ControlListView($win, "", $lv, "GetText", 0, 2) & @CRLF)

; GUICtrlSetData 更新一行：只写第二列时另外两列会怎样？
Local $two = GUICtrlCreateListViewItem("a|b|c", $lv)
GUICtrlSetData($two, "||9")
ConsoleWrite("item_row_after_||9=" & GUICtrlRead($two) & @CRLF)
GUICtrlSetData($two, "x")
ConsoleWrite("item_row_after_x=" & GUICtrlRead($two) & @CRLF)

; GUICtrlSetState($item, $GUI_FOCUS) 会不会选中那一行？
GUICtrlSetState($item2, $GUI_FOCUS)
ConsoleWrite("lv.read_after_item_focus=" & GUICtrlRead($lv) & @CRLF)
ConsoleWrite("item2.state=" & GUICtrlRead($item2) & @CRLF)

GUIDelete($win)

; ---------------------------------------------------------------------------
; TreeView：父子关系、读回来的是控件号还是文本、bold/选中
; ---------------------------------------------------------------------------
$win = GUICreate("probe", 400, 300)
Local $tv = GUICtrlCreateTreeView(0, 0, 200, 200)
Local $root = GUICtrlCreateTreeViewItem("root", $tv)
Local $child = GUICtrlCreateTreeViewItem("child", $root)
Local $sibling = GUICtrlCreateTreeViewItem("sibling", $tv)
ConsoleWrite("tv.ids=" & $root & "," & $child & "," & $sibling & @CRLF)
ConsoleWrite("tv.item_count=" & GUICtrlSendMsg($tv, 0x1105, 0, 0) & @CRLF)
GUICtrlSetState($child, $GUI_FOCUS)
ConsoleWrite("tv.read_after_focus=" & GUICtrlRead($tv) & @CRLF)
ConsoleWrite("child.state=" & GUICtrlRead($child) & @CRLF)
ConsoleWrite("child.read=" & GUICtrlRead($child) & @CRLF)
ConsoleWrite("child.read_adv=" & GUICtrlRead($child, 1) & @CRLF)
GUICtrlSetState($child, $GUI_DEFBUTTON)
ConsoleWrite("child.state_after_bold=" & GUICtrlRead($child) & @CRLF)
GUICtrlSetState($child, 0)
ConsoleWrite("child.state_after_zero=" & GUICtrlRead($child) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; Tab：默认选第几页、页上的控件是不是被隐藏、$GUI_SHOW 的效果
; ---------------------------------------------------------------------------
$win = GUICreate("probe", 400, 300)
Local $tab = GUICtrlCreateTab(0, 0, 300, 200)
Local $page1 = GUICtrlCreateTabItem("one")
Local $label1 = GUICtrlCreateLabel("first", 10, 30)
Local $page2 = GUICtrlCreateTabItem("two")
Local $label2 = GUICtrlCreateLabel("second", 10, 30)
GUICtrlCreateTabItem("")
ConsoleWrite("tab.ids=" & $tab & "," & $page1 & "," & $page2 & @CRLF)
ConsoleWrite("tab.read=" & GUICtrlRead($tab) & @CRLF)
ConsoleWrite("tab.read_adv=" & GUICtrlRead($tab, 1) & @CRLF)
ConsoleWrite("label1.state=" & GUICtrlGetState($label1) & @CRLF)
ConsoleWrite("label2.state=" & GUICtrlGetState($label2) & @CRLF)
GUICtrlSetState($page2, $GUI_SHOW)
ConsoleWrite("tab.read_after_show2=" & GUICtrlRead($tab) & @CRLF)
ConsoleWrite("label1.state_after_show2=" & GUICtrlGetState($label1) & @CRLF)
ConsoleWrite("label2.state_after_show2=" & GUICtrlGetState($label2) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; -1 = 最后创建的控件；复选框三态；Combo 是追加还是替换
; ---------------------------------------------------------------------------
$win = GUICreate("probe", 400, 300)
Local $label = GUICtrlCreateLabel("first", 0, 0)
GUICtrlSetData(-1, "second")
ConsoleWrite("-1.read=" & GUICtrlRead($label) & @CRLF)
Local $check = GUICtrlCreateCheckbox("c", 0, 20)
ConsoleWrite("check.unchecked=" & GUICtrlRead($check) & @CRLF)
GUICtrlSetState($check, $GUI_CHECKED)
ConsoleWrite("check.checked=" & GUICtrlRead($check) & @CRLF)
GUICtrlSetState($check, $GUI_INDETERMINATE)
ConsoleWrite("check.indeterminate=" & GUICtrlRead($check) & @CRLF)
ConsoleWrite("check.read_adv=" & GUICtrlRead($check, 1) & @CRLF)
Local $combo = GUICtrlCreateCombo("", 0, 40)
GUICtrlSetData($combo, "a|b")
GUICtrlSetData($combo, "c")
ConsoleWrite("combo.count=" & GUICtrlSendMsg($combo, 0x0146, 0, 0) & @CRLF)
GUICtrlSetData($combo, "|reset")
ConsoleWrite("combo.count_after_reset=" & GUICtrlSendMsg($combo, 0x0146, 0, 0) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; 窗口尺寸：GUICreate 的宽高是客户区还是整窗
; ---------------------------------------------------------------------------
$win = GUICreate("probe-size", 500, 300, 10, 20)
GUISetState(@SW_SHOW, $win)
Sleep(300)
Local $pos = WinGetPos($win)
Local $client = WinGetClientSize($win)
ConsoleWrite("win.pos=" & $pos[0] & "," & $pos[1] & "," & $pos[2] & "," & $pos[3] & @CRLF)
ConsoleWrite("win.client=" & $client[0] & "," & $client[1] & @CRLF)
GUIDelete($win)
