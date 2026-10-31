//! Чтение структуры xlsx: листы, общие строки, значения ячеек.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

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
        Ok(Workbook { workbook_path, sheets, sst_path, sst, date1904 })
    }

    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets.iter().position(|s| s.name == name)
    }
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

/// Обходит непустые ячейки листа. Колбэк возвращает false, чтобы остановиться.
pub fn scan_cells(xml: &[u8], sst: &[String], mut f: impl FnMut(u32, u32, CellVal) -> bool) -> Result<()> {
    let mut rd = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut row: u32 = 0;
    let mut col: i64 = -1;
    let (mut c_row, mut c_col) = (0u32, 0u32);
    let mut t = String::new();
    let mut v = String::new();
    let mut is_text = String::new();
    let (mut in_c, mut in_v, mut in_is, mut in_t, mut has_f, mut rph) = (false, false, false, false, false, 0u32);
    loop {
        let ev = rd.read_event_into(&mut buf)?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"row" => {
                    row = attr(&e, b"r").and_then(|s| s.parse().ok()).unwrap_or(row + 1);
                    col = -1;
                }
                b"c" => {
                    let (cc, rr) = attr(&e, b"r").and_then(|s| parse_ref(&s)).unwrap_or(((col + 1) as u32, row));
                    col = cc as i64;
                    if !empty {
                        in_c = true;
                        (c_row, c_col) = (rr, cc);
                        t = attr(&e, b"t").unwrap_or_default();
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
                        if !f(c_row, c_col, CellVal { text, numeric, formula: has_f, special: t == "b" || t == "e" }) {
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
}

#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub cells: HashMap<(u32, u32), GridCell>,
    /// Номер последней строки (с 1) и колонки (с 0), где есть данные.
    pub max_row: u32,
    pub max_col: u32,
    /// Лист длиннее лимита просмотра.
    pub truncated: bool,
}

impl Grid {
    pub fn get(&self, row: u32, col: u32) -> Option<&GridCell> {
        self.cells.get(&(row, col))
    }
}

pub fn load_grid(path: &Path, wb: &Workbook, sheet: usize, max_rows: u32) -> Result<Grid> {
    let mut zip = open_zip(path)?;
    let info = wb.sheets.get(sheet).context("нет такого листа")?;
    let xml = read_part(&mut zip, &info.path)?.with_context(|| format!("нет части {}", info.path))?;
    let mut g = Grid::default();
    scan_cells(&xml, &wb.sst, |r, c, v| {
        if r > max_rows {
            g.truncated = true;
            return false;
        }
        g.max_row = g.max_row.max(r);
        g.max_col = g.max_col.max(c);
        g.cells.insert((r, c), GridCell { text: v.text, numeric: v.numeric, formula: v.formula });
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
}
