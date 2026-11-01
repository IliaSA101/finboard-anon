//! Файл-ключ: CSV «Тип;ID;Реальное значение;Фейковое значение».
//! Содержит реальные персональные данные — хранится только у владельца.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::fake::KeyEntry;
use crate::profile::{parse_kind_key, Kind};

const HEADER: [&str; 4] = ["Тип", "ID", "Реальное значение", "Фейковое значение"];

/// Разделители, с которыми ключ читается: свой «;» и те, что ставят при пересохранении Excel с английскими
/// настройками, LibreOffice и Google Таблицы. Какой из них — решает только заголовок: он узнаётся ровно
/// при одном разделителе, так что угадывания по данным нет.
const DELIMITERS: [u8; 3] = [b';', b',', b'\t'];

pub fn write(path: &Path, entries: &[KeyEntry]) -> Result<()> {
    let mut data = Vec::new();
    data.extend_from_slice("\u{feff}".as_bytes()); // BOM — чтобы Excel открыл UTF-8 с кириллицей
    {
        let mut w = csv::WriterBuilder::new().delimiter(b';').from_writer(&mut data);
        w.write_record(HEADER)?;
        for e in entries {
            let id = e.id.map(|i| i.to_string()).unwrap_or_default();
            w.write_record([e.kind_key.as_str(), id.as_str(), e.real.as_str(), e.fake.as_str()])?;
        }
        w.flush()?;
    }
    // Через временный файл: при сбое записи прежний ключ (с фейками других файлов) не теряется.
    // Временный файл — тоже с настоящими значениями: при сбое он удаляется, а не остаётся рядом.
    // Перед переименованием — сброс на диск: при отключении питания сразу после записи на месте ключа
    // мог остаться файл нулевой длины, а результаты, им покрытые, уже лежат рядом.
    let tmp = tmp_path(path);
    let done = write_synced(&tmp, &data)
        .with_context(|| format!("не записывается ключ {}", tmp.display()))
        .and_then(|_| std::fs::rename(&tmp, path).with_context(|| format!("не записывается ключ {}", path.display())));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Временный файл ключа: «<ключ>.csv.tmp» рядом с ним. В нём настоящие значения.
pub fn tmp_path(key: &Path) -> PathBuf {
    key.with_extension("csv.tmp")
}

/// Временный файл ключа, оставшийся от запуска, который не дошёл до конца (процесс сняли, выключили
/// компьютер): в нём настоящие значения, и лежит он рядом с ключом, как обычный файл. Удаляется в начале
/// следующего запуска — под замком ключа, так что чужой идущий запуск его не пишет. Возвращает путь удалённого.
pub fn remove_leftover_tmp(key: &Path) -> Option<PathBuf> {
    let tmp = tmp_path(key);
    (tmp.is_file() && std::fs::remove_file(&tmp).is_ok()).then_some(tmp)
}

/// Запись файла со сбросом на диск (не только в кэш системы).
fn write_synced(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(data)?;
    f.sync_all()
}

/// Отпечаток файла ключа — время изменения и размер; `None` — файла нет. Снимается до чтения ключа
/// и сверяется перед записью: изменился — значит его записал кто-то ещё, и наша запись затёрла бы чужие строки.
pub type Stamp = Option<(Option<SystemTime>, u64)>;

pub fn stamp(path: &Path) -> Stamp {
    std::fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()))
}

/// Замок давнее этого — брошенный, даже если номер процесса снова кем-то занят.
const LOCK_STALE: Duration = Duration::from_secs(12 * 3600);

/// Замок ключа: файл «<ключ>.lock» рядом с ключом, пока идёт запуск, который этот ключ пишет.
/// Два запуска на одном ключе (два окна, окно и командная строка) прочитали бы один и тот же ключ,
/// выдали бы новым людям одинаковые ID, а последний записанный ключ потерял бы строки другого —
/// уже отправленные файлы стало бы нечем расшифровать. Замок снимается при выходе из области —
/// и при ошибке, и при сбое (панике).
pub struct KeyLock {
    path: PathBuf,
    /// Что записано в замок: снимается только он сам.
    text: String,
}

impl Drop for KeyLock {
    fn drop(&mut self) {
        // Свой замок могли счесть брошенным и заменить своим (запуск шёл дольше 12 часов) — чужой не снимаем.
        remove_if_same(&self.path, &self.text);
    }
}

/// Удаляет замок, только если в нём всё ещё `expected`. Просто «прочитать, сравнить, удалить» мало: между
/// чтением и удалением другой запуск мог убрать этот замок и поставить свой — удалён был бы его свежий замок,
/// и на ключе работали бы два запуска. Поэтому замок сначала забирается переименованием (это мгновенно и
/// целиком), сравнивается уже под новым именем и только тогда удаляется; чужой замок возвращается на место.
fn remove_if_same(path: &Path, expected: &str) -> bool {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{}-{}.снят", std::process::id(), nanos()));
    let taken = PathBuf::from(s);
    if std::fs::rename(path, &taken).is_err() {
        return false;
    }
    if read_text(&taken) == expected {
        let _ = std::fs::remove_file(&taken);
        return true;
    }
    // Чужой — на место. Жёсткая ссылка не затирает замок, если за это время поставили ещё один
    // (переименование в Windows затёрло бы); где ссылок нет (флешка, сетевая папка) — переименование, если место свободно.
    if std::fs::hard_link(&taken, path).is_err() && !path.exists() {
        let _ = std::fs::rename(&taken, path);
    }
    let _ = std::fs::remove_file(&taken);
    false
}

fn read_text(path: &Path) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

pub fn lock_path(key: &Path) -> PathBuf {
    let mut s = key.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// Кто держит замок: номер процесса, компьютер, начало (секунды от 1970 г.) и время запуска самого процесса
/// (нет в замках версий до 0.5.0 и там, где его не узнать).
struct Holder {
    pid: u32,
    host: String,
    started: u64,
    process: Option<String>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn host_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default()
}

fn parse_holder(text: &str) -> Option<Holder> {
    let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix('=')).map(str::trim);
    Some(Holder {
        pid: field("pid")?.parse().ok()?,
        host: field("компьютер")?.to_string(),
        started: field("начат")?.parse().ok()?,
        process: field("процесс_запущен").filter(|s| !s.is_empty()).map(String::from),
    })
}

/// Замок брошен: запуск, который его поставил, уже не идёт (процесс завершился аварийно, компьютер выключили).
/// Процесс проверяется только на этом же компьютере (ключ может лежать в общей папке); замок старше
/// `LOCK_STALE` брошен в любом случае. Непонятное содержимое — брошен, если файлу больше минуты
/// (меньше — его, возможно, ещё дописывают).
///
/// Номер процесса после перезагрузки достаётся другим программам — и нашей тоже. Поэтому живой процесс
/// с тем же номером — ещё не владелец: сверяется и время его запуска, записанное в замке.
fn is_stale(text: &str, modified: Option<SystemTime>) -> bool {
    match parse_holder(text) {
        Some(h) => {
            let age = now_secs().saturating_sub(h.started);
            if age > LOCK_STALE.as_secs() {
                return true;
            }
            if h.host != host_name() {
                return false;
            }
            // Время запуска узнать не удалось (процесс другого пользователя) — считается, что тот же.
            let other_start = |now: Option<String>| h.process.is_some() && now.is_some() && now != h.process;
            if h.pid == std::process::id() {
                // Наш номер: либо замок этого же процесса (второй прогон в том же окне), либо номер остался от
                // процесса до перезагрузки — тогда время запуска другое.
                return other_start(process_start(h.pid));
            }
            !process_alive(h.pid) || other_start(process_start(h.pid))
        }
        None => modified.and_then(|m| m.elapsed().ok()).is_some_and(|a| a > Duration::from_secs(60)),
    }
}

/// Время запуска процесса — как строка, только для сравнения на равенство (`None` — не узнать).
#[cfg(windows)]
fn process_start(pid: u32) -> Option<String> {
    type Handle = *mut std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn GetProcessTimes(process: Handle, creation: *mut u64, exit: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    // SAFETY: обычные вызовы Windows с проверкой результата; FILETIME — 8 байт, как u64; описатель закрывается.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let (mut creation, mut exit, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
        let ok = GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user) != 0;
        CloseHandle(h);
        (ok && creation != 0).then(|| creation.to_string())
    }
}

/// Linux: номер загрузки системы и время запуска процесса от загрузки (поле 22 в /proc/<pid>/stat).
#[cfg(not(windows))]
fn process_start(pid: u32) -> Option<String> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Имя процесса в скобках может содержать пробелы — поля считаются после последней «)».
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    Some(format!("{}/{start}", boot.trim()))
}

/// Идёт ли процесс с этим номером (только Windows и Linux; где проверить нельзя — считается, что идёт).
#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    type Handle = *mut std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    // SAFETY: обычные вызовы Windows с проверкой результата; описатель закрывается.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            // Нет такого процесса — «неверный параметр»; нет доступа (процесс другого пользователя) — процесс есть.
            return std::io::Error::last_os_error().raw_os_error() != Some(ERROR_INVALID_PARAMETER);
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        !ok || code == STILL_ACTIVE
    }
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    let proc = Path::new("/proc");
    !proc.is_dir() || proc.join(pid.to_string()).exists()
}

/// Ставит замок на ключ. Занят живым запуском — ошибка «Ключ сейчас занят…»; брошенный замок убирается.
pub fn lock(key: &Path) -> Result<KeyLock> {
    let path = lock_path(key);
    let lock_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for _ in 0..3 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                // «метка» — чтобы два замка одного процесса (два прогона подряд в одном окне) не совпадали текстом.
                let text = format!(
                    "Идёт обезличивание с этим ключом. Файл удаляется сам по окончании.\r\npid={}\r\nкомпьютер={}\r\nначат={}\r\nпроцесс_запущен={}\r\nметка={}\r\n",
                    std::process::id(),
                    host_name(),
                    now_secs(),
                    process_start(std::process::id()).unwrap_or_default(),
                    nanos()
                );
                if let Err(e) = f.write_all(text.as_bytes()).and_then(|_| f.sync_all()) {
                    // Файл только что создан этим запуском — он наш, даже недописанный.
                    drop(f);
                    let _ = std::fs::remove_file(&path);
                    return Err(anyhow!(e)).with_context(|| format!("не записывается замок ключа {}", path.display()));
                }
                return Ok(KeyLock { path: path.clone(), text });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let text = read_text(&path);
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                if is_stale(&text, modified) {
                    // Убираем, только если это всё тот же брошенный замок: его мог уже заменить свежий замок другого запуска.
                    remove_if_same(&path, &text);
                    continue;
                }
                let who = match parse_holder(&text) {
                    Some(h) => {
                        let mins = now_secs().saturating_sub(h.started) / 60;
                        let place = if h.host.is_empty() || h.host == host_name() { String::new() } else { format!(" на компьютере {}", h.host) };
                        format!(" (процесс {}{place}, начат {mins} мин назад)", h.pid)
                    }
                    None => String::new(),
                };
                bail!(
                    "Ключ сейчас занят другим запуском обезличивания{who}: {}.\n\
                     Два запуска на одном ключе выдали бы новым людям одинаковые ID, а последний затёр бы записи другого.\n\
                     Ничего не записано. Дождитесь окончания того запуска (другое окно программы или командная строка) и запустите снова.\n\
                     Если другого запуска точно нет (программа закрылась аварийно), удалите файл «{lock_name}» рядом с ключом.",
                    key.display()
                );
            }
            Err(e) => {
                return Err(anyhow!(e)).with_context(|| format!("не создаётся замок ключа {} — ничего не записано", path.display()));
            }
        }
    }
    bail!("Ключ сейчас занят другим запуском обезличивания: {}. Ничего не записано — запустите снова.", key.display())
}

/// Свободное имя для прежнего ключа: «<имя>_прежний.csv», «<имя>_прежний_2.csv»…
pub fn backup_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("ключ");
    (1..)
        .map(|n| path.with_file_name(if n == 1 { format!("{stem}_прежний.csv") } else { format!("{stem}_прежний_{n}.csv") }))
        .find(|p| !p.exists())
        .unwrap()
}

/// Копия ключа перед его перезаписью: «<имя>_копия.csv», одна, обновляется каждым запуском.
/// Не «_прежний»: те не затираются — в них чужие ключи, отложенные при прогоне без загрузки.
pub fn copy_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("ключ");
    path.with_file_name(format!("{stem}_копия.csv"))
}

/// Кладёт рядом копию ключа, который сейчас будет перезаписан; возвращает её путь.
pub fn keep_copy(path: &Path) -> Result<PathBuf> {
    let copy = copy_path(path);
    std::fs::copy(path, &copy).with_context(|| {
        format!("не сохраняется копия ключа {} — ключ не перезаписан, файлы не записаны", copy.display())
    })?;
    Ok(copy)
}

pub fn read(path: &Path) -> Result<Vec<KeyEntry>> {
    let bytes = std::fs::read(path).with_context(|| format!("не читается ключ {}", path.display()))?;
    let body = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    // Ключ, прочитанный не целиком, хуже отсутствующего: при записи в него же пропавшие соответствия
    // заменились бы новыми фейками навсегда. Поэтому любая непонятная строка — ошибка, а не пропуск.
    let damaged = |why: String| {
        anyhow!(
            "Ключ {} похоже повреждён или пересохранён в другом формате: {why}.\n\
             Ключ НЕ изменён, обезличивание не выполнено.\n\
             Как исправить: верните копию ключа (рядом лежит «{}», если ключ уже перезаписывался) \
             или сохраните его как CSV в кодировке UTF-8 с разделителем «;» и первой строкой \
             «Тип;ID;Реальное значение;Фейковое значение».",
            path.display(),
            copy_path(path).file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        )
    };
    let text = std::str::from_utf8(body).map_err(|_| damaged("файл не в кодировке UTF-8".into()))?;
    let delim = DELIMITERS
        .into_iter()
        .find(|&d| header_matches(text, d))
        .ok_or_else(|| damaged("первая строка — не «Тип;ID;Реальное значение;Фейковое значение»".into()))?;
    let mut r = csv::ReaderBuilder::new().delimiter(delim).flexible(true).from_reader(text.as_bytes());
    let mut out = Vec::new();
    // Строки с номерами, испорченными Excel: номер строки и показываемое значение (настоящее — скрыто).
    let mut spoiled: Vec<(usize, String)> = Vec::new();
    // Номер строки — как в Excel: заголовок первый, ячейка с переносом строки остаётся одной строкой.
    for (i, rec) in r.records().enumerate() {
        let line = i + 2;
        let rec = rec.map_err(|e| damaged(format!("строка {line}: CSV не разбирается ({e})")))?;
        if rec.iter().all(|f| f.trim().is_empty()) {
            continue; // пустая строка, например «;;;» после правки в Excel
        }
        if rec.len() < 4 || rec.iter().skip(4).any(|f| !f.trim().is_empty()) {
            return Err(damaged(format!("строка {line}: полей {} вместо 4", rec.len())));
        }
        let e = KeyEntry { kind_key: rec[0].to_string(), id: rec[1].trim().parse().ok(), real: rec[2].to_string(), fake: rec[3].to_string() };
        if let Some(shown) = spoiled_by_excel(&e) {
            spoiled.push((line, shown));
        }
        out.push(e);
    }
    if let Some((line, shown)) = spoiled.first() {
        let more = match spoiled.len() - 1 {
            0 => String::new(),
            n => format!(", и ещё строк с такими числами: {n}"),
        };
        bail!(
            "Ключ {} похоже пересохранён в Excel: числа в нём испорчены (строка {line}: «{shown}»{more}).\n\
             Excel при сохранении превращает длинные номера (ИНН, счета, карты) в вид «7,70708E+11» и срезает \
             ведущие нули — по такому ключу прежние фейки не восстановить, а испорченные строки остались бы в нём навсегда.\n\
             Ключ НЕ изменён, обезличивание не выполнено.\n\
             Как исправить: верните ключ из «{}» (копия, которую программа кладёт рядом перед каждой перезаписью ключа) \
             или из своей резервной копии. Смотреть ключ в Excel можно, но не сохраняйте его там (при закрытии — «Не сохранять»).",
            path.display(),
            copy_path(path).file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        );
    }
    Ok(out)
}

/// Типы, у которых фейк — номер из стольких же цифр, сколько в настоящем значении.
const NUMBER_KINDS: [Kind; 5] = [Kind::Inn, Kind::Snils, Kind::Passport, Kind::Account, Kind::Phone];

/// Запись ключа, испорченная Excel (ключ открыли и сохранили в Excel — он превращает чисто цифровые ячейки
/// в числа): что показать в сообщении или `None`, если запись цела. Настоящее значение показывается скрытым.
///
/// Признаки:
/// - число в экспоненциальной записи «7,70708E+11» / «7.70708E+11» — так Excel показывает (и сохраняет) номера
///   длиннее 11 цифр. В xlsx и в ключе программы таких значений нет: xlsx хранит числа с точкой и всеми цифрами;
/// - номер (ИНН, СНИЛС, паспорт, счёт, телефон), у которого фейк короче настоящего: фейк программы — из стольких же
///   цифр, а Excel срезает его ведущие нули («00000000000000000012» → «12»);
/// - настоящий номер короче фейка: срезаны ведущие нули настоящего. Исключения — короткий код до 4 цифр
///   («Дог1» → «Дог900001») и ИНН в 9 или 11 цифр (ведущий ноль срезан ещё в исходной книге, фейк — полный).
fn spoiled_by_excel(e: &KeyEntry) -> Option<String> {
    if excel_exponent(&e.fake) {
        return Some(e.fake.trim().to_string());
    }
    if excel_exponent(&e.real) {
        return Some(hide_digits(e.real.trim()));
    }
    let kind = parse_kind_key(&e.kind_key)?.0;
    if !NUMBER_KINDS.contains(&kind) {
        return None;
    }
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).count();
    let (r, f) = (digits(&e.real), digits(&e.fake));
    // Фейк короче настоящего не бывает: значит, Excel срезал у фейка ведущие нули. Обратное («настоящий короче
    // фейка») порчей не считается: программа сама удлиняет фейк, когда номеров той же длины не хватило.
    if f < r {
        return Some(e.fake.trim().to_string());
    }
    None
}

/// «7,70708E+11», «4.07028E+19», «1E+15» — номер от 12 цифр, как его показывает и сохраняет Excel (порядок
/// от 11). Мантисса с запятой — всегда Excel; с точкой — если в ней не больше шести цифр (столько показывает
/// Excel; xlsx хранит число со всеми значащими цифрами).
fn excel_exponent(s: &str) -> bool {
    let s = s.trim();
    let Some((mant, exp)) = s.split_once("E+") else { return false };
    if exp.len() != 2 || exp.parse::<u32>().map_or(true, |p| p < 11) {
        return false;
    }
    let (int, frac, comma) = match mant.split_once([',', '.']) {
        Some((i, f)) => (i, f, mant.contains(',')),
        None => (mant, "", false),
    };
    let all_digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !all_digits(int) || !all_digits(frac) {
        return false;
    }
    comma || int.len() + frac.len() <= 6
}

/// Цифры настоящего значения — «#»: «7,70708E+11» → «#,#####E+11» (видно, что число испорчено, но не какое).
fn hide_digits(s: &str) -> String {
    let (mant, exp) = s.split_at(s.find('E').unwrap_or(s.len()));
    mant.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>() + exp
}

/// Первая строка — заголовок ключа при этом разделителе (лишние пустые колонки допустимы).
fn header_matches(text: &str, delim: u8) -> bool {
    let mut r = csv::ReaderBuilder::new().delimiter(delim).flexible(true).has_headers(false).from_reader(text.as_bytes());
    match r.records().next() {
        Some(Ok(h)) => {
            h.len() >= 4
                && h.iter().zip(HEADER).all(|(f, want)| f.trim() == want)
                && h.iter().skip(4).all(|f| f.trim().is_empty())
        }
        _ => false,
    }
}
