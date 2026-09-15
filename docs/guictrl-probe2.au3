; 官方解释器探针之二：第一轮剩下的、文档没写死的地方。
;
; 用法：AutoIt3.exe docs\guictrl-probe2.au3 > probe2.txt
; 输出仍然是每行一个 `名字=值`，把 probe2.txt 整段贴回来即可。
;
; 只问第一轮答案对不上或者没覆盖的：状态字怎么累积、ListViewItem 的文本/单元格
; 更新规则、$LVS_EX_CHECKBOXES 与三态复选框、TabItem/TreeViewItem 的默认读法。

Global Const $GUI_CHECKED = 0x01
Global Const $GUI_INDETERMINATE = 0x02
Global Const $GUI_UNCHECKED = 0x04
Global Const $GUI_SHOW = 0x10
Global Const $GUI_HIDE = 0x20
Global Const $GUI_ENABLE = 0x40
Global Const $GUI_DISABLE = 0x80

; ---------------------------------------------------------------------------
; 1. 状态字：新建后是多少，隐藏/显示/禁用怎么改
; ---------------------------------------------------------------------------
Local $win = GUICreate("probe2", 400, 300)
Local $label = GUICtrlCreateLabel("l", 0, 0)
ConsoleWrite("state.fresh=" & GUICtrlGetState($label) & @CRLF)
GUICtrlSetState($label, $GUI_HIDE)
ConsoleWrite("state.after_hide=" & GUICtrlGetState($label) & @CRLF)
GUICtrlSetState($label, $GUI_SHOW)
ConsoleWrite("state.after_show=" & GUICtrlGetState($label) & @CRLF)
GUICtrlSetState($label, $GUI_DISABLE)
ConsoleWrite("state.after_disable=" & GUICtrlGetState($label) & @CRLF)
GUICtrlSetState($label, $GUI_ENABLE)
ConsoleWrite("state.after_enable=" & GUICtrlGetState($label) & @CRLF)
GUICtrlSetState($label, 0)
ConsoleWrite("state.after_zero=" & GUICtrlGetState($label) & @CRLF)
ConsoleWrite("state.visible_after_zero=" & GUICtrlGetState($label) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; 2. ListViewItem：单列文本、""/"|" 的擦除、少给几列、给 ListView 写数据
; ---------------------------------------------------------------------------
$win = GUICreate("probe2", 400, 300)
Local $lv = GUICtrlCreateListView("c1|c2|c3", 0, 0, 300, 120)
Local $solo = GUICtrlCreateListViewItem("solo", $lv)
ConsoleWrite("lv.solo_read=" & GUICtrlRead($solo) & @CRLF)
Local $three = GUICtrlCreateListViewItem("a|b|c", $lv)
GUICtrlSetData($three, "")
ConsoleWrite("item.after_empty=" & GUICtrlRead($three) & @CRLF)
GUICtrlSetData($three, "a|b|c")
GUICtrlSetData($three, "|")
ConsoleWrite("item.after_one_sep=" & GUICtrlRead($three) & @CRLF)
GUICtrlSetData($three, "a|b|c")
GUICtrlSetData($three, "only|two")
ConsoleWrite("item.after_two_cells=" & GUICtrlRead($three) & @CRLF)
ConsoleWrite("lv.subitem_count=" & ControlListView($win, "", $lv, "GetSubItemCount") & @CRLF)
; 对 ListView 本身写数据：会改哪一行？
GUICtrlSetData($lv, "||z")
ConsoleWrite("item1_after_lv_setdata=" & GUICtrlRead($solo) & @CRLF)
ConsoleWrite("item2_after_lv_setdata=" & GUICtrlRead($three) & @CRLF)
; 选中第一行以后再对 ListView 写数据
ControlListView($win, "", $lv, "Select", 0)
GUICtrlSetData($lv, "||q")
ConsoleWrite("item1_after_lv_setdata_selected=" & GUICtrlRead($solo) & @CRLF)
ConsoleWrite("item2_after_lv_setdata_selected=" & GUICtrlRead($three) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; 3. $LVS_EX_CHECKBOXES（扩展样式 0x4）与三态复选框
; ---------------------------------------------------------------------------
$win = GUICreate("probe2", 400, 300)
Local $lv2 = GUICtrlCreateListView("c1", 0, 0, 200, 80, -1, 0x4)
Local $item = GUICtrlCreateListViewItem("row", $lv2)
ConsoleWrite("lvcheck.read=" & GUICtrlRead($item) & @CRLF)
ConsoleWrite("lvcheck.read_adv=" & GUICtrlRead($item, 1) & @CRLF)
GUICtrlSetState($item, $GUI_CHECKED)
ConsoleWrite("lvcheck.read_adv_checked=" & GUICtrlRead($item, 1) & @CRLF)
GUICtrlSetState($item, $GUI_UNCHECKED)
ConsoleWrite("lvcheck.read_adv_unchecked=" & GUICtrlRead($item, 1) & @CRLF)
Local $tri = GUICtrlCreateCheckbox("t", 0, 100, 0, 0, 0x0006)
GUICtrlSetState($tri, $GUI_INDETERMINATE)
ConsoleWrite("tri.indeterminate=" & GUICtrlRead($tri) & @CRLF)
GUICtrlSetState($tri, $GUI_CHECKED)
ConsoleWrite("tri.checked=" & GUICtrlRead($tri) & @CRLF)
GUICtrlSetState($tri, $GUI_UNCHECKED)
ConsoleWrite("tri.unchecked=" & GUICtrlRead($tri) & @CRLF)
Local $plain = GUICtrlCreateCheckbox("p", 0, 130)
GUICtrlSetState($plain, $GUI_INDETERMINATE)
ConsoleWrite("plain.indeterminate=" & GUICtrlRead($plain) & @CRLF)
GUIDelete($win)

; ---------------------------------------------------------------------------
; 4. TreeViewItem 与 TabItem 的默认读法
; ---------------------------------------------------------------------------
$win = GUICreate("probe2", 400, 300)
Local $tv = GUICtrlCreateTreeView(0, 0, 150, 150)
Local $root = GUICtrlCreateTreeViewItem("root", $tv)
Local $child = GUICtrlCreateTreeViewItem("child", $root)
ConsoleWrite("tvitem.fresh_read=" & GUICtrlRead($root) & @CRLF)
ConsoleWrite("tvitem.fresh_read_adv=" & GUICtrlRead($root, 1) & @CRLF)
GUICtrlSetState($child, $GUI_EXPAND)
ConsoleWrite("tvitem.after_expand=" & GUICtrlRead($child) & @CRLF)
Local $tab = GUICtrlCreateTab(200, 0, 150, 150)
Local $page = GUICtrlCreateTabItem("page")
Local $in_page = GUICtrlCreateLabel("x", 210, 30)
ConsoleWrite("tabitem.read=" & GUICtrlRead($page) & @CRLF)
ConsoleWrite("tabitem.read_adv=" & GUICtrlRead($page, 1) & @CRLF)
GUICtrlSetData($page, "renamed")
ConsoleWrite("tabitem.after_setdata=" & GUICtrlRead($page) & @CRLF)
ConsoleWrite("tabitem.state=" & GUICtrlGetState($page) & @CRLF)
ConsoleWrite("tab.read=" & GUICtrlRead($tab) & @CRLF)
GUIDelete($win)
