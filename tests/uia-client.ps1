# A UI Automation client reading Slate's hidden test window the way a screen reader does (no input sent anywhere):
# the element tree, the text, the units at the caret, rows one by one, attributes, Find, and the events raised while
# tests/uia.txt moves the caret and types. Start it first, then (with SLATE_DATA_DIR set to an empty folder):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\uia-client.ps1
#   Slate.exe --test tests\uia.txt
# Focus events don't reach it: Windows passes those on only from the window with the keyboard, which the hidden test
# window never is.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -ReferencedAssemblies UIAutomationClient, UIAutomationTypes, WindowsBase @"
using System; using System.Runtime.InteropServices; using System.Collections.Concurrent; using System.Windows.Automation;
public static class W { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string title); }
// (event handlers in C#: UI Automation calls them on its own threads, where a PowerShell script block can't run)
public static class Ev {
  public static ConcurrentQueue<string> Got = new ConcurrentQueue<string>();
  public static void Listen(AutomationElement doc, int pid) {
    Automation.AddAutomationEventHandler(TextPattern.TextSelectionChangedEvent, doc, TreeScope.Element, (s, e) => Got.Enqueue("selection"));
    Automation.AddAutomationEventHandler(TextPattern.TextChangedEvent, doc, TreeScope.Element, (s, e) => Got.Enqueue("text"));
    // (focus only in the test window's process: nothing about other windows is recorded)
    Automation.AddAutomationFocusChangedEventHandler((s, e) => {
      try { var el = (AutomationElement)s; if (el.Current.ProcessId == pid) Got.Enqueue("focus " + el.Current.ControlType.ProgrammaticName + " '" + el.Current.Name + "'"); } catch { }
    });
  }
  public static string Drain() { var o = new System.Collections.Generic.List<string>(); string x; while (Got.TryDequeue(out x)) o.Add(x); return string.Join(", ", o); }
}
"@
$A = [System.Windows.Automation.AutomationElement]
$T = [System.Windows.Automation.Text.TextUnit]
$h = [IntPtr]::Zero
# ([NullString]: PowerShell passes $null to a string parameter as "", a window without a title)
for ($i = 0; $i -lt 150 -and $h -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 100; $h = [W]::FindWindowW("SlateTestWindow", [NullString]::Value) }
if ($h -eq [IntPtr]::Zero) { "no test window"; exit 1 }
Start-Sleep -Milliseconds 1500
$el = $A::FromHandle($h)
"window: '" + $el.Current.Name + "' " + $el.Current.ControlType.ProgrammaticName
$w = [System.Windows.Automation.TreeWalker]::RawViewWalker
$c = $w.GetFirstChild($el)
while ($c -ne $null) {
    "  child: " + $c.Current.ControlType.ProgrammaticName + " '" + $c.Current.Name + "' id=" + $c.Current.AutomationId + " keyboard=" + $c.Current.HasKeyboardFocus + " text=" + [bool]$c.GetCurrentPropertyValue($A::IsTextPatternAvailableProperty)
    $c = $w.GetNextSibling($c)
}
$cond = New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, [System.Windows.Automation.ControlType]::Document)
$doc = $el.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
if ($doc -eq $null) { "no document"; exit 1 }
"parent of document: " + $w.GetParent($doc).Current.ControlType.ProgrammaticName
"runtime id: " + ($doc.GetRuntimeId() -join ',')
"bounds: " + $doc.Current.BoundingRectangle
$tp = $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
"document text: " + ($tp.DocumentRange.GetText(-1) -replace "`r", '\r' -replace "`n", '\n')
$sel = $tp.GetSelection()
"selections: " + $sel.Length
$r = $sel[0].Clone()
foreach ($u in @('Character', 'Word', 'Line', 'Paragraph')) {
    $x = $sel[0].Clone()
    $x.ExpandToEnclosingUnit($T::$u)
    "caret " + $u + ": '" + ($x.GetText(-1) -replace "`r", '\r' -replace "`n", '\n') + "'"
}
$all = $tp.DocumentRange.Clone()
$first = $all.Clone()
$first.MoveEndpointByRange([System.Windows.Automation.Text.TextPatternRangeEndpoint]::End, $all, [System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start)
$first.ExpandToEnclosingUnit($T::Line)
$lines = @()
for ($i = 0; $i -lt 10; $i++) {
    $lines += "'" + ($first.GetText(-1) -replace "`r", '\r' -replace "`n", '\n') + "'"
    if ($first.Move($T::Line, 1) -eq 0) { break }
}
"lines: " + ($lines -join ' ')
$rects = $first.GetBoundingRectangles()
"last line's rectangles: " + ($rects -join ' | ')
"font: " + $all.GetAttributeValue([System.Windows.Automation.TextPattern]::FontNameAttribute) + " " + $all.GetAttributeValue([System.Windows.Automation.TextPattern]::FontSizeAttribute) + " read only: " + $all.GetAttributeValue([System.Windows.Automation.TextPattern]::IsReadOnlyAttribute)
$bold = $all.GetAttributeValue([System.Windows.Automation.TextPattern]::FontWeightAttribute)
"font weight (not kept): " + ($bold -eq [System.Windows.Automation.AutomationElement]::NotSupported)
$found = $all.FindText("LINE", $false, $true)
"found: '" + $found.GetText(-1) + "'"
# events while the script moves the caret, types and switches tabs
[Ev]::Listen($doc, $el.Current.ProcessId)
"listening"
Start-Sleep -Seconds 12
"events: " + [Ev]::Drain()
$x = $tp.GetSelection()[0]
$x.ExpandToEnclosingUnit($T::Line)
"caret line now: '" + ($x.GetText(-1) -replace "`r", '\r' -replace "`n", '\n') + "'"
[System.Windows.Automation.Automation]::RemoveAllEventHandlers()
