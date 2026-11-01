//! Чтение структуры xlsx: листы, общие строки, значения ячеек.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use zip::result::ZipError;
use zip::ZipArchive;

pub type Zip = ZipArchive<BufReader<File>>;

pub fn open_zip(path: &Path) -> Result<Zip> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "xls" {
        bail!("Формат .xls не поддерживается — пересохраните файл в Excel как .xlsx");
    }
    let f = File::open(path).with_context(|| format!("не открывается {}", path.display()))?;
    ZipArchive::new(BufReader::new(f)).map_err(|_| {
        anyhow::anyhow!("{} — не xlsx-файл (или файл защищён паролем). Снимите пароль и сохраните как .xlsx", path.display())
    })
}

pub fn read_part(zip: &mut Zip, name: &str) -> Result<Option<Vec<u8>>> {
    match zip.by_name(name) {
        Ok(mut f) => {
            let mut v = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut v)?;
            Ok(Some(v))
        }
        Err(ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Часть архива потоком: лист в сотни мегабайт не читается в память целиком.
pub fn part_reader<'a>(zip: &'a mut Zip, name: &str) -> Result<Option<BufReader<Counted<zip::read::ZipFile<'a, BufReader<File>>>>>> {
    match zip.by_name(name) {
        Ok(f) => Ok(Some(BufReader::with_capacity(1 << 16, counted(f)))),
        Err(ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Ход долгой обработки для окна: байт частей книги прочитано и ожидается всего (оценка).
/// Обработка идёт в своём потоке, окно читает счётчики из своего.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    /// Доля сделанного, 0..1 (оценка — не больше 0,99, пока обработка не закончилась).
    pub fn fraction(&self) -> f32 {
        let (d, t) = (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
        if t == 0 {
            0.0
        } else {
            (d as f64 / t as f64).min(0.99) as f32
        }
    }
}

thread_local! {
    static TRACK: RefCell<Option<Arc<Progress>>> = const { RefCell::new(None) };
}

/// Учитывать чтение частей книги в этом потоке (`None` — перестать). Предпросмотр в другом потоке не считается.
pub fn track_progress(p: Option<Arc<Progress>>) {
    TRACK.with(|t| *t.borrow_mut() = p);
}

/// Счётчик хода этого потока, если его ведут.
pub fn progress() -> Option<Arc<Progress>> {
    TRACK.with(|t| t.borrow().clone())
}

/// Чтение части книги, которое считается в ход обработки.
pub struct Counted<R> {
    inner: R,
    p: Option<Arc<Progress>>,
}

pub fn counted<R: Read>(inner: R) -> Counted<R> {
    Counted { inner, p: progress() }
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(p) = &self.p {
            p.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------- адреса ячеек

pub fn col_name(mut col: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push((b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s.iter().rev().collect()
}

pub fn parse_col(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 3 {
        return None;
    }
    let mut c: u32 = 0;
    for ch in s.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        c = c * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
    }
    Some(c - 1)
}

/// "AB12" → (колонка с 0, строка с 1).
pub fn parse_ref(s: &str) -> Option<(u32, u32)> {
    let s = s.replace('$', "");
    let i = s.find(|c: char| c.is_ascii_digit())?;
    Some((parse_col(&s[..i])?, s[i..].parse().ok()?))
}

/// Диапазон вида "C5:C120", "C5:C" (до конца), "C5", "B5:D100" (несколько колонок).
/// Возвращает (колонка от, колонка до, строка от, строка до).
pub fn parse_range(s: &str) -> Option<(u32, u32, u32, Option<u32>)> {
    let s = s.trim().replace('$', "").to_uppercase();
    let (a, b) = match s.split_once(':') {
        Some((a, b)) => (a.to_string(), Some(b.to_string())),
        None => (s.clone(), None),
    };
    let split = |x: &str| -> Option<(u32, Option<u32>)> {
        let i = x.find(|c: char| c.is_ascii_digit()).unwrap_or(x.len());
        let col = parse_col(&x[..i])?;
        let row = if i < x.len() { Some(x[i..].parse().ok()?) } else { None };
        Some((col, row))
    };
    let (c1, r1) = split(&a)?;
    let r1 = r1.unwrap_or(1);
    match b {
        None => Some((c1, c1, r1, Some(r1))),
        Some(b) => {
            let (c2, r2) = split(&b)?;
            if c2 < c1 || r2.is_some_and(|r| r < r1) {
                return None;
            }
            Some((c1, c2, r1, r2))
        }
    }
}

// ---------------------------------------------------------------- XML-утилиты

pub fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Атрибут с префиксом пространства имён по локальному имени (r:id).
fn attr_prefixed(e: &BytesStart, local: &[u8]) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.local_name().as_ref() == local && a.key.prefix().is_some())
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn resolve_target(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

pub(crate) fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

pub(crate) struct Rel {
    pub id: String,
    pub kind: String,
    pub target: String,
}

pub(crate) fn parse_rels(xml: &[u8], base_dir: &str) -> Result<Vec<Rel>> {
    let mut rd = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match rd.read_event_into(&mut buf)? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                if attr(&e, b"TargetMode").as_deref() == Some("External") {
                    continue;
                }
                out.push(Rel {
                    id: attr(&e, b"Id").unwrap_or_default(),
                    kind: attr(&e, b"Type").unwrap_or_default(),
                    target: resolve_target(base_dir, &attr(&e, b"Target").unwrap_or_default()),
                });
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

// ---------------------------------------------------------------- книга

#[derive(Clone, Debug)]
pub struct SheetInfo {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct Workbook {
    pub workbook_path: String,
    pub sheets: Vec<SheetInfo>,
    pub sst_path: Option<String>,
    pub sst: Vec<String>,
    /// Даты считаются от 01.01.1904 (книги со старых Mac), а не от 30.12.1899.
    pub date1904: bool,
    /// По номеру стиля ячейки (атрибут s): формат даты (и есть ли в нём время). Только для просмотра.
    pub date_styles: Vec<Option<bool>>,
}

impl Workbook {
    pub fn open(path: &Path) -> Result<Workbook> {
        let mut zip = open_zip(path)?;
        Workbook::from_zip(&mut zip)
    }

    pub fn from_zip(zip: &mut Zip) -> Result<Workbook> {
        let mut workbook_path = "xl/workbook.xml".to_string();
        if let Some(root) = read_part(zip, "_rels/.rels")? {
            if let Some(r) = parse_rels(&root, "")?.into_iter().find(|r| r.kind.ends_with("/officeDocument")) {
                workbook_path = r.target;
            }
        }
        let wb_xml = read_part(zip, &workbook_path)?.context("в файле нет workbook.xml — это не книга Excel")?;
        let base_dir = workbook_path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        let rels = match read_part(zip, &rels_path(&workbook_path))? {
            Some(x) => parse_rels(&x, &base_dir)?,
            None => Vec::new(),
        };
        let by_id: HashMap<&str, &Rel> = rels.iter().map(|r| (r.id.as_str(), r)).collect();

        let mut sheets = Vec::new();
        let mut date1904 = false;
        let mut rd = Reader::from_reader(&wb_xml[..]);
        let mut buf = Vec::new();
        loop {
            match rd.read_event_into(&mut buf)? {
                Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"workbookPr" => {
                    date1904 = matches!(attr(&e, b"date1904").as_deref(), Some("1") | Some("true"));
                }
                Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"sheet" => {
                    let name = attr(&e, b"name").unwrap_or_default();
                    if let Some(rel) = attr_prefixed(&e, b"id").and_then(|id| by_id.get(id.as_str()).copied()) {
                        if rel.kind.ends_with("/worksheet") {
                            sheets.push(SheetInfo { name, path: rel.target.clone() });
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }

        let sst_path = rels
            .iter()
            .find(|r| r.kind.ends_with("/sharedStrings"))
            .map(|r| r.target.clone());
        let sst = match &sst_path {
            Some(p) => match read_part(zip, p)? {
                Some(x) => parse_sst(&x)?,
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        let date_styles = match rels.iter().find(|r| r.kind.ends_with("/styles")) {
            Some(r) => read_part(zip, &r.target)?.map(|x| parse_date_styles(&x)).unwrap_or_default(),
            None => Vec::new(),
        };
        Ok(Workbook { workbook_path, sheets, sst_path, sst, date1904, date_styles })
    }

    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets.iter().position(|s| s.name == name)
    }
}

/// Формат числа — дата? `Some(true)` — дата со временем, `Some(false)` — только дата, `None` — не дата.
/// Встроенные форматы 14–22, 45–47 и свои: есть d/m/y/h/s вне кавычек и [цвета].
pub fn date_format(id: u32, code: Option<&str>) -> Option<bool> {
    match id {
        14..=17 => return Some(false),
        18..=22 | 45..=47 => return Some(true),
        _ => {}
    }
    let code = code?;
    let (mut date, mut time) = (false, false);
    let (mut quoted, mut bracket, mut escaped) = (false, false, false);
    for ch in code.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !quoted => escaped = true,
            '"' => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            ';' if !quoted && !bracket => break,
            _ if quoted || bracket => {}
            'd' | 'D' | 'y' | 'Y' | 'm' | 'M' => date = true,
            'h' | 'H' | 's' | 'S' => time = true,
            _ => {}
        }
    }
    // «m» без дня и года — это минуты (ч:мм).
    if date || time {
        Some(time)
    } else {
        None
    }
}

/// Стили ячеек (cellXfs) из styles.xml → формат даты для каждого номера стиля.
pub fn parse_date_styles(xml: &[u8]) -> Vec<Option<bool>> {
    let mut codes: HashMap<u32, String> = HashMap::new();
    let mut out = Vec::new();
    let mut rd = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut in_xfs = false;
    loop {
        match rd.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"numFmt" => {
                    if let (Some(id), Some(code)) = (attr(&e, b"numFmtId").and_then(|v| v.parse().ok()), attr(&e, b"formatCode")) {
                        codes.insert(id, code);
                    }
                }
                b"cellXfs" => in_xfs = true,
                b"xf" if in_xfs => {
                    let id: u32 = attr(&e, b"numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0);
                    out.push(date_format(id, codes.get(&id).map(String::as_str)));
                }
                _ => {}
            },
            Ok(Event::End(e)) if e.local_name().as_ref() == b"cellXfs" => in_xfs = false,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Число-дата Excel как в русском Excel: 01.01.2019 или 01.01.2019 13:05.
pub fn date_text(serial: f64, d1904: bool, with_time: bool) -> Option<String> {
    let min = if d1904 { 0.0 } else { 1.0 };
    if !(min..2_958_466.0).contains(&serial) {
        return None;
    }
    let base = if d1904 { crate::fake::days_from_civil(1904, 1, 1) } else { crate::fake::days_from_civil(1899, 12, 30) };
    let total = (serial * 86400.0).round() as i64;
    let (y, m, d) = crate::fake::civil_from_days(base + total.div_euclid(86400));
    let secs = total.rem_euclid(86400);
    let mut s = format!("{d:02}.{m:02}.{y:04}");
    if with_time {
        s.push_str(&format!(" {:02}:{:02}", secs / 3600, secs / 60 % 60));
    }
    Some(s)
}

/// Умная таблица: лист, колонки и строки данных (без шапки и строки итогов), имена колонок.
#[derive(Clone, Debug)]
pub struct TableInfo {
    pub name: String,
    pub sheet: usize,
    pub c1: u32,
    pub c2: u32,
    pub r1: u32,
    pub r2: u32,
    pub data_r1: u32,
    pub data_r2: u32,
    pub columns: Vec<String>,
}

pub fn sheet_tables(zip: &mut Zip, wb: &Workbook) -> Result<Vec<TableInfo>> {
    let mut out = Vec::new();
    for (si, sh) in wb.sheets.iter().enumerate() {
        let Some(rels) = read_part(zip, &rels_path(&sh.path))? else { continue };
        let dir = sh.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        for rel in parse_rels(&rels, dir)?.into_iter().filter(|r| r.kind.ends_with("/table")) {
            let Some(xml) = read_part(zip, &rel.target)? else { continue };
            let mut rd = Reader::from_reader(&xml[..]);
            let mut buf = Vec::new();
            let mut t: Option<TableInfo> = None;
            loop {
                match rd.read_event_into(&mut buf)? {
                    Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"table" => {
                        let name = attr(&e, b"displayName").or_else(|| attr(&e, b"name")).unwrap_or_default();
                        if let Some((c1, c2, r1, Some(r2))) = attr(&e, b"ref").and_then(|r| parse_range(&r)) {
                            let head: u32 = attr(&e, b"headerRowCount").and_then(|v| v.parse().ok()).unwrap_or(1);
                            let tot: u32 = attr(&e, b"totalsRowCount").and_then(|v| v.parse().ok()).unwrap_or(0);
                            t = Some(TableInfo { name, sheet: si, c1, c2, r1, r2, data_r1: r1 + head, data_r2: r2.saturating_sub(tot), columns: Vec::new() });
                        }
                    }
                    Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"tableColumn" => {
                        if let Some(t) = t.as_mut() {
                            t.columns.push(attr(&e, b"name").unwrap_or_default());
                        }
                    }
                    Event::Eof => break,
                    _ => {}
                }
                buf.clear();
            }
            out.extend(t);
        }
    }
    Ok(out)
}

/// Кэш сводной таблицы: определение, записи и (если удалось понять) источник данных.
#[derive(Clone, Debug)]
pub struct PivotCache {
    pub def_path: String,
    pub records_path: Option<String>,
    /// Лист-источник, первая колонка (с 0) и строка заголовков (с 1).
    pub source: Option<(String, u32, u32)>,
}

pub fn pivot_caches(zip: &mut Zip) -> Result<Vec<PivotCache>> {
    let defs: Vec<String> = zip
        .file_names()
        .filter(|n| n.starts_with("xl/pivotCache/pivotCacheDefinition") && n.ends_with(".xml"))
        .map(str::to_string)
        .collect();
    let mut out = Vec::new();
    for def in defs {
        let xml = read_part(zip, &def)?.unwrap_or_default();
        let mut source = None;
        let mut rd = Reader::from_reader(&xml[..]);
        let mut buf = Vec::new();
        loop {
            match rd.read_event_into(&mut buf)? {
                Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"worksheetSource" => {
                    // name= (таблица/имя) и r:id (внешний файл) не разбираем — источник неизвестен.
                    let external = attr_prefixed(&e, b"id").is_some();
                    if let (Some(r), Some(sheet), false) = (attr(&e, b"ref"), attr(&e, b"sheet"), external) {
                        if let Some((c0, _, r0, _)) = parse_range(&r) {
                            source = Some((sheet, c0, r0));
                        }
                    }
                    break;
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        let dir = def.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let records_path = match read_part(zip, &rels_path(&def))? {
            Some(x) => parse_rels(&x, dir)?.into_iter().find(|r| r.kind.ends_with("/pivotCacheRecords")).map(|r| r.target),
            None => None,
        };
        out.push(PivotCache { def_path: def, records_path, source });
    }
    Ok(out)
}

/// Текст общих строк (rich text склеивается, фонетические подсказки rPh пропускаются).
pub fn parse_sst(xml: &[u8]) -> Result<Vec<String>> {
    let mut rd = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_si, mut in_t, mut rph) = (false, false, 0u32);
    loop {
        match rd.read_event_into(&mut buf)? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = true;
                    cur.clear();
                }
                b"rPh" => rph += 1,
                b"t" if in_si && rph == 0 => in_t = true,
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref() == b"si" => out.push(String::new()),
            Event::End(e) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = false;
                    out.push(std::mem::take(&mut cur));
                }
                b"rPh" => rph = rph.saturating_sub(1),
                b"t" => in_t = false,
                _ => {}
            },
            Event::Text(e) if in_t => cur.push_str(&e.unescape()?),
            Event::CData(e) if in_t => cur.push_str(&String::from_utf8_lossy(&e)),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

// ---------------------------------------------------------------- ячейки

#[derive(Clone, Debug)]
pub struct CellVal {
    pub text: String,
    /// Число (тип ячейки n): заменяется на число, если фейк — число.
    pub numeric: bool,
    pub formula: bool,
    /// Ошибка (#Н/Д) или логическое значение — не данные человека.
    pub special: bool,
    /// Номер стиля ячейки (атрибут s).
    pub style: u32,
}

/// Значение ячейки по её типу и содержимому <v>/<is>.
pub fn resolve_value(t: &str, v: &str, is_text: &str, sst: &[String]) -> Option<(String, bool)> {
    let r = match t {
        "s" => (sst.get(v.trim().parse::<usize>().ok()?)?.clone(), false),
        "inlineStr" => (is_text.to_string(), false),
        "b" => ((if v.trim() == "1" { "ИСТИНА" } else { "ЛОЖЬ" }).to_string(), false),
        "str" | "e" | "d" => (v.to_string(), false),
        _ => (v.to_string(), true),
    };
    if r.0.is_empty() {
        None
    } else {
        Some(r)
    }
}

/// Сырой атрибут без копирования и раскодирования (r, t, s ячейки — только латиница и цифры).
pub fn raw_attr<'a>(e: &'a BytesStart, key: &[u8]) -> Option<std::borrow::Cow<'a, [u8]>> {
    e.attributes().with_checks(false).flatten().find(|a| a.key.as_ref() == key).map(|a| a.value)
}

/// Число из байтов ASCII.
pub fn parse_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() || b.len() > 9 {
        return None;
    }
    b.iter().try_fold(0u32, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u32))
}

/// «AB12» (и «$AB$12») из байтов: (колонка с 0, строка с 1) — без выделения памяти.
pub fn parse_ref_bytes(b: &[u8]) -> Option<(u32, u32)> {
    let mut col: u32 = 0;
    let mut letters = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'$' => {}
            c if c.is_ascii_alphabetic() => {
                letters += 1;
                col = col * 26 + (c.to_ascii_uppercase() - b'A') as u32 + 1;
            }
            _ => break,
        }
        i += 1;
    }
    if letters == 0 || letters > 3 {
        return None;
    }
    Some((col - 1, parse_u32(&b[i..])?))
}

/// Обходит непустые ячейки листа. Колбэк возвращает false, чтобы остановиться.
pub fn scan_cells(xml: impl BufRead, sst: &[String], f: impl FnMut(u32, u32, CellVal) -> bool) -> Result<()> {
    scan_cells_where(xml, sst, |_, _| true, f)
}

/// То же, но значения читаются только у нужных ячеек (`want(строка, колонка)`): на больших листах
/// под правилами — малая часть колонок.
pub fn scan_cells_where(
    xml: impl BufRead,
    sst: &[String],
    mut want: impl FnMut(u32, u32) -> bool,
    mut f: impl FnMut(u32, u32, CellVal) -> bool,
) -> Result<()> {
    let mut rd = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut row: u32 = 0;
    let mut col: i64 = -1;
    let (mut c_row, mut c_col) = (0u32, 0u32);
    let mut t = String::new();
    let mut style = 0u32;
    let mut v = String::new();
    let mut is_text = String::new();
    let (mut in_c, mut in_v, mut in_is, mut in_t, mut has_f, mut rph) = (false, false, false, false, false, 0u32);
    loop {
        let ev = rd.read_event_into(&mut buf)?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"row" => {
                    row = raw_attr(&e, b"r").and_then(|s| parse_u32(&s)).unwrap_or(row + 1);
                    col = -1;
                }
                b"c" => {
                    let (cc, rr) = raw_attr(&e, b"r").and_then(|s| parse_ref_bytes(&s)).unwrap_or(((col + 1) as u32, row));
                    col = cc as i64;
                    if !empty && want(rr, cc) {
                        in_c = true;
                        (c_row, c_col) = (rr, cc);
                        t.clear();
                        if let Some(x) = raw_attr(&e, b"t") {
                            t.push_str(&String::from_utf8_lossy(&x));
                        }
                        style = raw_attr(&e, b"s").and_then(|x| parse_u32(&x)).unwrap_or(0);
                        v.clear();
                        is_text.clear();
                        has_f = false;
                    }
                }
                b"f" if in_c => has_f = true,
                b"v" if in_c && !empty => in_v = true,
                b"is" if in_c && !empty => in_is = true,
                b"rPh" if !empty => rph += 1,
                b"t" if in_is && rph == 0 && !empty => in_t = true,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"v" => in_v = false,
                b"is" => in_is = false,
                b"t" => in_t = false,
                b"rPh" => rph = rph.saturating_sub(1),
                b"c" if in_c => {
                    in_c = false;
                    if let Some((text, numeric)) = resolve_value(&t, &v, &is_text, sst) {
                        if !f(c_row, c_col, CellVal { text, numeric, formula: has_f, special: t == "b" || t == "e", style }) {
                            return Ok(());
                        }
                    }
                }
                _ => {}
            },
            Event::Text(e) => {
                if in_v {
                    v.push_str(&e.unescape()?);
                } else if in_t {
                    is_text.push_str(&e.unescape()?);
                }
            }
            Event::CData(e) => {
                if in_v {
                    v.push_str(&String::from_utf8_lossy(&e));
                } else if in_t {
                    is_text.push_str(&String::from_utf8_lossy(&e));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

// ---------------------------------------------------------------- сетка для просмотра

#[derive(Clone, Debug)]
pub struct GridCell {
    pub text: String,
    pub numeric: bool,
    pub formula: bool,
    /// Число в формате даты — для показа: «01.01.2019».
    pub date: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub cells: HashMap<(u32, u32), GridCell>,
    /// Номер последней строки (с 1) и колонки (с 0), где есть данные.
    pub max_row: u32,
    pub max_col: u32,
    /// Лист длиннее лимита просмотра.
    pub truncated: bool,
    /// Последняя строка листа по его размеру (<dimension>) — когда показана только часть строк.
    pub dim_rows: Option<u32>,
}

impl Grid {
    pub fn get(&self, row: u32, col: u32) -> Option<&GridCell> {
        self.cells.get(&(row, col))
    }

    fn filled(&self, row: u32, col: u32) -> bool {
        self.get(row, col).is_some_and(|x| !x.text.trim().is_empty())
    }

    /// Строка шапки таблицы: среди первых 50 строк — та, где больше всего текстовых ячеек (не меньше двух).
    pub fn header_row(&self) -> Option<u32> {
        let mut best: Option<(u32, usize)> = None;
        for r in 1..=self.max_row.min(50) {
            let n = (0..=self.max_col)
                .filter(|&c| self.get(r, c).is_some_and(|x| !x.numeric && x.date.is_none() && !x.text.trim().is_empty()))
                .count();
            if n >= 2 && best.is_none_or(|(_, b)| n > b) {
                best = Some((r, n));
            }
        }
        best.map(|(r, _)| r)
    }

    /// Начало колонки целиком: строка под шапкой; `from` ниже шапки — она сама.
    pub fn column_start(&self, from: Option<u32>) -> u32 {
        let data = self.header_row().map_or(1, |h| (h + 1).min(self.max_row.max(1)));
        from.map_or(data, |r| r.max(data))
    }

    /// Ctrl+стрелка как в Excel: к краю заполненного блока, из пустоты — к следующей заполненной ячейке
    /// (или к краю листа). Позиция — (колонка, строка), шаг — (по колонкам, по строкам).
    pub fn jump(&self, (c, r): (u32, u32), (dc, dr): (i64, i64), max_c: u32, max_r: u32) -> (u32, u32) {
        let step = |(c, r): (u32, u32)| -> Option<(u32, u32)> {
            let (nc, nr) = (c as i64 + dc, r as i64 + dr);
            (nc >= 0 && nc <= max_c as i64 && nr >= 1 && nr <= max_r as i64).then_some((nc as u32, nr as u32))
        };
        let Some(mut n) = step((c, r)) else { return (c, r) };
        if self.filled(r, c) && self.filled(n.1, n.0) {
            while let Some(m) = step(n).filter(|m| self.filled(m.1, m.0)) {
                n = m;
            }
        } else {
            while !self.filled(n.1, n.0) {
                match step(n) {
                    Some(m) => n = m,
                    None => break,
                }
            }
        }
        n
    }
}

pub fn load_grid(path: &Path, wb: &Workbook, sheet: usize, max_rows: u32) -> Result<Grid> {
    let mut zip = open_zip(path)?;
    let info = wb.sheets.get(sheet).context("нет такого листа")?;
    let mut g = Grid::default();
    // Размер листа записан в его начале: <dimension ref="A1:CV500001"/>.
    if let Some(mut rd) = part_reader(&mut zip, &info.path)? {
        let mut head = vec![0u8; 4096];
        let n = rd.read(&mut head)?;
        let head = String::from_utf8_lossy(&head[..n]);
        if let Some(i) = head.find("<dimension ref=\"") {
            let r = &head[i + 16..];
            g.dim_rows = r.split('"').next().and_then(parse_range).map(|(_, _, a, b)| b.unwrap_or(a));
        }
    }
    let xml = part_reader(&mut zip, &info.path)?.with_context(|| format!("нет части {}", info.path))?;
    scan_cells(xml, &wb.sst, |r, c, v| {
        if r > max_rows {
            g.truncated = true;
            return false;
        }
        g.max_row = g.max_row.max(r);
        g.max_col = g.max_col.max(c);
        let date = match wb.date_styles.get(v.style as usize) {
            Some(Some(time)) if v.numeric => v.text.trim().parse().ok().and_then(|x| date_text(x, wb.date1904, *time)),
            _ => None,
        };
        g.cells.insert((r, c), GridCell { text: v.text, numeric: v.numeric, formula: v.formula, date });
        true
    })?;
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(27), "AB");
        assert_eq!(parse_ref("AB12"), Some((27, 12)));
        assert_eq!(parse_range("c5:c"), Some((2, 2, 5, None)));
        assert_eq!(parse_range("B5:D100"), Some((1, 3, 5, Some(100))));
        assert_eq!(parse_range("C:C"), Some((2, 2, 1, None)));
        assert_eq!(resolve_target("xl", "../customXml/item1.xml"), "customXml/item1.xml");
        assert_eq!(resolve_target("xl", "worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
    }

    #[test]
    fn refs_from_bytes() {
        for r in ["A1", "AB12", "$C$5", "XFD1048576", "z9"] {
            assert_eq!(parse_ref_bytes(r.as_bytes()), parse_ref(r), "{r}");
        }
        for r in ["", "1", "A", "ABCD1", "A1x", "A-1"] {
            assert_eq!(parse_ref_bytes(r.as_bytes()), None, "{r}");
        }
        assert_eq!(parse_u32(b"123"), Some(123));
        assert_eq!(parse_u32(b"12a"), None);
    }

    fn grid(rows: &[&[&str]]) -> Grid {
        let mut g = Grid::default();
        for (r, row) in rows.iter().enumerate() {
            for (c, t) in row.iter().enumerate().filter(|(_, t)| !t.is_empty()) {
                let numeric = t.parse::<f64>().is_ok();
                g.cells.insert((r as u32 + 1, c as u32), GridCell { text: t.to_string(), numeric, formula: false, date: None });
                g.max_row = g.max_row.max(r as u32 + 1);
                g.max_col = g.max_col.max(c as u32);
            }
        }
        g
    }

    #[test]
    fn whole_column_starts_under_header() {
        // Название отчёта сверху, шапка в 3-й строке — колонка начинается с 4-й.
        let g = grid(&[&["Отчёт за март", "", ""], &["", "", ""], &["ФИО", "Сумма", "Дата"], &["Иванов", "5", "1"], &["Петров", "7", "2"]]);
        assert_eq!(g.header_row(), Some(3));
        assert_eq!(g.column_start(None), 4);
        assert_eq!(g.column_start(Some(1)), 4, "двойной щелчок по названию или шапке — от строки под шапкой");
        assert_eq!(g.column_start(Some(5)), 5, "по ячейке данных — от неё");
        let title = grid(&[&["Отчёт за март", "", ""], &["1", "2", "3"]]);
        assert_eq!(title.header_row(), None, "одиночное название над числами — не шапка");
        let flat = grid(&[&["1", "2"], &["3", "4"]]);
        assert_eq!((flat.header_row(), flat.column_start(None)), (None, 1), "без шапки — с первой строки");
    }

    #[test]
    fn ctrl_arrows_like_excel() {
        let g = grid(&[&["a", "", "c"], &["a", "", ""], &["a", "", ""], &["", "", ""], &["", "", ""], &["a", "", "c"]]);
        let (mc, mr) = (2, 6);
        assert_eq!(g.jump((0, 1), (0, 1), mc, mr), (0, 3), "к концу заполненного блока");
        assert_eq!(g.jump((0, 3), (0, 1), mc, mr), (0, 6), "из конца блока — к следующей заполненной");
        assert_eq!(g.jump((0, 6), (0, 1), mc, mr), (0, 6), "у края листа — на месте");
        assert_eq!(g.jump((0, 6), (0, -1), mc, mr), (0, 3), "вверх");
        assert_eq!(g.jump((0, 1), (1, 0), mc, mr), (2, 1), "вправо через пустую — к заполненной");
        assert_eq!(g.jump((1, 2), (0, 1), mc, mr), (1, 6), "пустая колонка — до края");
    }

    #[test]
    fn date_formats() {
        assert_eq!(date_format(14, None), Some(false));
        assert_eq!(date_format(17, None), Some(false), "ммм.гг");
        assert_eq!(date_format(22, None), Some(true));
        assert_eq!(date_format(0, None), None);
        assert_eq!(date_format(4, None), None);
        assert_eq!(date_format(164, Some("dd/mm/yyyy;@")), Some(false));
        assert_eq!(date_format(165, Some("[$-F800]dddd\\,\\ mmmm\\ dd\\,\\ yyyy")), Some(false));
        assert_eq!(date_format(166, Some("dd.mm.yy h:mm")), Some(true));
        assert_eq!(date_format(167, Some("#,##0.00\" дней\"")), None);
        assert_eq!(date_format(168, Some("[Red]#,##0;[Blue]-#,##0")), None);
        assert_eq!(date_format(169, Some("0.00 \\d")), None);
        assert_eq!(date_text(43466.0, false, false).as_deref(), Some("01.01.2019"));
        assert_eq!(date_text(43466.5, false, true).as_deref(), Some("01.01.2019 12:00"));
        assert_eq!(date_text(0.0, true, false).as_deref(), Some("01.01.1904"));
        let xml = br#"<styleSheet><numFmts count="1"><numFmt numFmtId="164" formatCode="DD.MM.YYYY"/></numFmts>
            <cellStyleXfs><xf numFmtId="14"/></cellStyleXfs>
            <cellXfs count="4"><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="4"/><xf numFmtId="22"/></cellXfs></styleSheet>"#;
        assert_eq!(parse_date_styles(xml), vec![None, Some(false), None, Some(true)]);
    }
}
