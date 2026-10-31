//! Общие помощники сквозных тестов: сборка небольших xlsx и чтение результата.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use finboard_anon::xlsx::{col_name, load_grid, Grid, Workbook};

pub enum Cell {
    S(String),
    N(f64),
    /// Формула с закэшированным числовым результатом.
    F(String, f64),
    /// Формула со строковым результатом (t="str").
    FS(String, String),
    /// Ячейка с явным типом: ("e", "#N/A"), ("b", "1").
    T(&'static str, String),
    /// Готовая разметка ячейки целиком (атрибут r подставляется): `t="str"><f t="array" ref="A2:A4">…</f><v>…</v>`.
    Raw(String),
}

pub fn s(t: &str) -> Cell {
    Cell::S(t.to_string())
}

#[derive(Default)]
pub struct Book {
    pub sheets: Vec<(String, Vec<(u32, u32, Cell)>)>,
    pub author: String,
    /// Дополнительные части: (путь в zip, содержимое) — примечания, таблицы, сводные, внешние ссылки.
    pub extra: Vec<(String, String)>,
    /// Разметка после <sheetData> по номеру листа (с 0): колонтитулы, ссылки на таблицы.
    pub sheet_tail: HashMap<usize, String>,
    /// Книга с датами от 1904 года.
    pub date1904: bool,
    /// Разметка в начале workbook.xml (до workbookPr): fileVersion, fileSharing, absPath.
    pub workbook_head: String,
    /// Стили (xl/styles.xml целиком) — для форматов дат; пусто — без стилей.
    pub styles: String,
}

impl Book {
    pub fn sheet(&mut self, name: &str) -> &mut Vec<(u32, u32, Cell)> {
        self.sheets.push((name.to_string(), Vec::new()));
        &mut self.sheets.last_mut().unwrap().1
    }
}

pub fn esc(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Минимальная корректная книга: общие строки, формулы с кэшем, автор в свойствах.
pub fn write_xlsx(path: &Path, book: &Book) {
    let mut sst: Vec<String> = Vec::new();
    let mut sst_idx: HashMap<String, usize> = HashMap::new();
    let mut sheet_xml = Vec::new();
    for (si, (_, cells)) in book.sheets.iter().enumerate() {
        let mut rows: std::collections::BTreeMap<u32, Vec<(u32, &Cell)>> = Default::default();
        for (r, c, v) in cells {
            rows.entry(*r).or_default().push((*c, v));
        }
        let mut x = String::from(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#);
        for (r, mut cs) in rows {
            cs.sort_by_key(|(c, _)| *c);
            x.push_str(&format!(r#"<row r="{r}">"#));
            for (c, v) in cs {
                let a = format!("{}{r}", col_name(c));
                match v {
                    Cell::S(t) => {
                        let i = *sst_idx.entry(t.clone()).or_insert_with(|| {
                            sst.push(t.clone());
                            sst.len() - 1
                        });
                        x.push_str(&format!(r#"<c r="{a}" t="s"><v>{i}</v></c>"#));
                    }
                    Cell::N(n) => x.push_str(&format!(r#"<c r="{a}"><v>{n}</v></c>"#)),
                    Cell::F(f, n) => x.push_str(&format!(r#"<c r="{a}"><f>{}</f><v>{n}</v></c>"#, esc(f))),
                    Cell::Raw(rest) => x.push_str(&format!(r#"<c r="{a}" {rest}</c>"#)),
                    Cell::T(t, v) => x.push_str(&format!(r#"<c r="{a}" t="{t}"><v>{}</v></c>"#, esc(v))),
                    Cell::FS(f, v) => x.push_str(&format!(r#"<c r="{a}" t="str"><f>{}</f><v>{}</v></c>"#, esc(f), esc(v))),
                }
            }
            x.push_str("</row>");
        }
        x.push_str("</sheetData>");
        x.push_str(book.sheet_tail.get(&si).map(String::as_str).unwrap_or(""));
        x.push_str("</worksheet>");
        sheet_xml.push(x);
    }
    let n = book.sheets.len();
    let mut ct = String::from(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#);
    for i in 1..=n {
        ct.push_str(&format!(r#"<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#));
    }
    ct.push_str(r#"<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#);
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
    let mut wb = String::from(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#);
    wb.push_str(&book.workbook_head);
    if book.date1904 {
        wb.push_str(r#"<workbookPr date1904="1"/>"#);
    }
    wb.push_str("<sheets>");
    let mut wbr = String::from(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#);
    for (i, (name, _)) in book.sheets.iter().enumerate() {
        let k = i + 1;
        wb.push_str(&format!(r#"<sheet name="{}" sheetId="{k}" r:id="rId{k}"/>"#, esc(name)));
        wbr.push_str(&format!(r#"<Relationship Id="rId{k}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{k}.xml"/>"#));
    }
    wb.push_str(r#"</sheets><calcPr calcId="191029"/></workbook>"#);
    wbr.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>"#, n + 1));
    if !book.styles.is_empty() {
        wbr.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#, n + 2));
    }
    wbr.push_str("</Relationships>");
    let mut sx = format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{0}" uniqueCount="{0}">"#, sst.len());
    for t in &sst {
        sx.push_str(&format!(r#"<si><t xml:space="preserve">{}</t></si>"#, esc(t)));
    }
    sx.push_str("</sst>");
    let core = format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>{0}</dc:creator><cp:lastModifiedBy>{0}</cp:lastModifiedBy></cp:coreProperties>"#, esc(&book.author));

    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut put = |name: &str, data: &str| {
        z.start_file(name, o).unwrap();
        z.write_all(data.as_bytes()).unwrap();
    };
    put("[Content_Types].xml", &ct);
    put("_rels/.rels", rels);
    put("docProps/core.xml", &core);
    put("xl/workbook.xml", &wb);
    put("xl/_rels/workbook.xml.rels", &wbr);
    for (i, x) in sheet_xml.iter().enumerate() {
        put(&format!("xl/worksheets/sheet{}.xml", i + 1), x);
    }
    put("xl/sharedStrings.xml", &sx);
    if !book.styles.is_empty() {
        put("xl/styles.xml", &book.styles);
    }
    for (name, data) in &book.extra {
        put(name, data);
    }
    z.finish().unwrap();
}

/// Все листы книги в виде сеток (значения так, как их видит программа).
pub fn read_book(path: &Path) -> HashMap<String, Grid> {
    let wb = Workbook::open(path).unwrap();
    (0..wb.sheets.len()).map(|i| (wb.sheets[i].name.clone(), load_grid(path, &wb, i, 1_000_000).unwrap())).collect()
}

pub fn text(g: &HashMap<String, Grid>, sheet: &str, a1: &str) -> String {
    let (c, r) = finboard_anon::xlsx::parse_ref(a1).unwrap();
    g[sheet].get(r, c).map(|x| x.text.clone()).unwrap_or_default()
}

/// Отдельная временная папка на тест.
pub fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("finboard-tests-{}", std::process::id())).join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub const NAMES: [&str; 4] = ["Иванов Иван", "Петрова Мария", "Сидоров Пётр", "Кузнецова Анна"];

/// Отчёт по форме реального: шапка с ИНН, таблица задач (исполнитель не везде),
/// «Итоги по исполнителям», вторая таблица, ИТОГО с формулами, «Ответственный: …».
/// `dr`, `dc` — сдвиг всей раскладки (второй лист в реальном файле начинается с G6).
pub fn report_sheet(cells: &mut Vec<(u32, u32, Cell)>, dr: u32, dc: u32) {
    let mut put = |r: u32, c: u32, v: Cell| cells.push((r + dr, c + dc, v));
    put(1, 0, s("ООО «Ромашка» | ИНН/КПП: 5029290277 / 77190100"));
    put(2, 0, s("+7 (495) 960-74-38 | studio@romashka.example"));
    put(5, 0, s("Отчет по проекту"));
    put(8, 0, s("Выполненные задачи"));
    for (c, h) in ["№", "Наименование задачи", "Исполнитель", "Время"].iter().enumerate() {
        put(9, c as u32, s(h));
    }
    for n in 1..=23u32 {
        let r = 9 + n;
        put(r, 0, Cell::N(n as f64));
        put(r, 1, s(&format!("Задача {n}")));
        if n <= 20 {
            put(r, 2, s(NAMES[(n % 4) as usize]));
        }
        put(r, 3, s("01:00"));
    }
    put(34, 0, s("Итоги по исполнителям"));
    for (c, h) in ["Исполнитель", "Должность", "Ставка за час", "Всего часов", "Сумма"].iter().enumerate() {
        put(35, c as u32, s(h));
    }
    for (i, n) in NAMES.iter().enumerate() {
        let r = 36 + i as u32;
        put(r, 0, s(n));
        put(r, 1, s("Разработчик"));
        put(r, 2, Cell::N(3960.0));
        put(r, 3, Cell::N(10.0 + i as f64));
        put(r, 4, Cell::F(format!("C{0}*D{0}", r + dr), 3960.0 * (10.0 + i as f64)));
    }
    put(40, 0, s("ИТОГО"));
    put(40, 3, Cell::F("SUM(D36:D39)".into(), 46.0));
    put(42, 0, s("Ответственный: Иванов Иван"));
}

pub fn report_book() -> Book {
    let mut b = Book { author: "Иванов Иван".into(), ..Default::default() };
    report_sheet(b.sheet("Отчет"), 0, 0);
    report_sheet(b.sheet("Лист1"), 5, 6);
    let sv = b.sheet("Свод");
    sv.push((1, 0, s("Задач у Петровой")));
    sv.push((1, 1, Cell::F("COUNTIF(Отчет!C10:C29,\"Петрова Мария\")".into(), 5.0)));
    b
}
