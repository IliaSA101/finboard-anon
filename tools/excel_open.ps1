param([string]$Root)
# Открывает каждый результат в Excel: Open падает на файлах, которым нужно «восстановление».
$files = Get-ChildItem $Root -Recurse -Filter *.xlsx | Where-Object { $_.Name -match '(_anon|^пакет_|^очередь_|^a1|^b1|^b2|^b_)' }
$xl = New-Object -ComObject Excel.Application
$xl.DisplayAlerts = $false
$xl.Visible = $false
$ok = 0; $bad = @()
foreach ($f in $files) {
  try {
    $wb = $xl.Workbooks.Open($f.FullName, 0, $true)
    $wb.Close($false)
    $ok++
  } catch {
    $bad += "$($f.FullName): $($_.Exception.Message)"
  }
}
$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
"opened: $ok, failed: $($bad.Count)"
$bad
