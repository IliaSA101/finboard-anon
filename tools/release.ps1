# Собирает выпуск: dist\finboard-anon-<версия>-windows-x64.zip (внутри папка finboard-anon-<версия>, exe с версией в имени) и dist\SHA256SUMS.txt.
# Запуск из любого места: pwsh tools\release.ps1 [-SkipTests]
# Перед выпуском также: полный прогон мутаций (python -u tools/mutate.py) и открытие примеров в Excel (tools\excel_open.ps1).
param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
Set-Location $root

$ver = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value

# Пакет собирается по белому списку. Если программу запускали на dist\finboard-anon\Пример, рядом
# остались *_anon*.xlsx, *_профиль*.json и КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ*.csv — им не место в архиве.
$pkgFiles = @('ИНСТРУКЦИЯ.txt', 'Пример\demo_dashboard.xlsx', 'Пример\demo_profile.json')
# Следы сборок прежнего вида в исходной папке (в git не входят): не копируются, но и не мешают.
$staleBuild = @('finboard-anon.exe', 'finboard-anon-cli.exe', 'LICENSE.txt', 'THIRD_PARTY_NOTICES.txt')

# Все файлы папки (и скрытые) — путями от неё, через «\».
function Get-RelativeFiles([string]$dir) {
  $full = (Resolve-Path -LiteralPath $dir).Path.TrimEnd('\')
  @(Get-ChildItem -LiteralPath $full -Recurse -File -Force | ForEach-Object { $_.FullName.Substring($full.Length + 1) } | Sort-Object)
}

# Ошибка со списком всех файлов папки не из списка.
function Assert-OnlyFiles([string]$dir, [string[]]$allowed) {
  $extra = @(Get-RelativeFiles $dir | Where-Object { $allowed -notcontains $_ })
  if ($extra.Count) {
    throw "В $dir лишние файлы (могут содержать персональные данные) — уберите их и запустите снова:`n  $($extra -join "`n  ")"
  }
}

$src = Join-Path $root 'dist\finboard-anon'
Assert-OnlyFiles $src ($pkgFiles + $staleBuild)
# Ресурсы exe (иконка и версия) — под текущую версию из Cargo.toml.
python tools/icon_to_coff.py
if ($LASTEXITCODE) { throw 'не собран assets\icon.o' }

if (-not $SkipTests) {
  cargo test
  if ($LASTEXITCODE) { throw 'тесты не прошли' }
}

# Пути машины (папка проекта, кэш крейтов, тулчейн) не должны попасть в exe: rustc вшивает их в сообщения паник.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $HOME '.rustup' }
$remap = @{ $root = 'finboard-anon'; $cargoHome = 'cargo'; $rustupHome = 'rustup' }
$env:CARGO_ENCODED_RUSTFLAGS = ($remap.GetEnumerator() | ForEach-Object { "--remap-path-prefix=$($_.Key)=$($_.Value)" }) -join [char]0x1f
# Отдельная папка сборки: другие флаги не должны пересобирать обычный target\release.
cargo build --release --target-dir target\publish
if ($LASTEXITCODE) { throw 'сборка не прошла' }
Remove-Item Env:CARGO_ENCODED_RUSTFLAGS

$exes = 'finboard-anon.exe', 'finboard-anon-cli.exe' | ForEach-Object { Join-Path $root "target\publish\release\$_" }
$forbidden = @($root, $cargoHome, $rustupHome, $HOME, $env:USERNAME) | Where-Object { $_ } | Sort-Object -Unique
foreach ($exe in $exes) {
  $text = [System.Text.Encoding]::Latin1.GetString([System.IO.File]::ReadAllBytes($exe))
  foreach ($f in $forbidden) {
    # Пути встречаются и как есть, и с «/», и в именах символов, где «\ : /» заменены на «_».
    foreach ($v in @($f, $f.Replace('\', '/'), ($f -replace '[\\/:]', '_'))) {
      if ($text.IndexOf($v, [StringComparison]::OrdinalIgnoreCase) -ge 0) { throw "в $exe остался путь машины: $v" }
    }
  }
}

python tools/third_party.py
if ($LASTEXITCODE) { throw 'не собран THIRD_PARTY_NOTICES.txt' }

# Версия — в свойствах exe (ресурс из assets\icon.o), в именах exe, папки и архива: сразу видно, что за выпуск.
foreach ($exe in $exes) {
  $v = (Get-Item $exe).VersionInfo
  if ($v.ProductVersion -ne $ver -or $v.FileVersion -ne $ver) { throw "в свойствах $exe версия $($v.ProductVersion), а не $ver" }
}
$name = "finboard-anon-$ver"
$pkg = Join-Path $root "dist\$name"
Remove-Item $pkg -Recurse -Force -ErrorAction SilentlyContinue
# Инструкция и пример — по списку из dist\finboard-anon (они в git), exe — с версией в имени.
Assert-OnlyFiles $src ($pkgFiles + $staleBuild)
New-Item -ItemType Directory $pkg | Out-Null
foreach ($f in $pkgFiles) {
  $to = Join-Path $pkg $f
  New-Item -ItemType Directory -Force (Split-Path $to) | Out-Null
  Copy-Item -LiteralPath (Join-Path $src $f) $to
}
Copy-Item $exes[0] (Join-Path $pkg "finboard-anon-$ver.exe")
Copy-Item $exes[1] (Join-Path $pkg "finboard-anon-cli-$ver.exe")
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $pkg 'LICENSE.txt') -Force
Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.txt') $pkg -Force
# В пакете — ровно эти файлы, не больше и не меньше.
$expected = @($pkgFiles + @("finboard-anon-$ver.exe", "finboard-anon-cli-$ver.exe", 'LICENSE.txt', 'THIRD_PARTY_NOTICES.txt'))
Assert-OnlyFiles $pkg $expected
$missing = @($expected | Where-Object { -not (Test-Path -LiteralPath (Join-Path $pkg $_)) })
if ($missing.Count) { throw "В пакете $pkg нет файлов: $($missing -join ', ')" }

$zip = Join-Path $root "dist\$name-windows-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $pkg -DestinationPath $zip

$sums = Join-Path $root 'dist\SHA256SUMS.txt'
@($zip) + (Get-ChildItem $pkg -Filter *.exe).FullName | ForEach-Object {
  "$((Get-FileHash $_ -Algorithm SHA256).Hash.ToLower())  $(Split-Path $_ -Leaf)"
} | Set-Content $sums -Encoding utf8NoBOM
Get-Content $sums
"Готово: $zip"
