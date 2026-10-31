//! Служебные поля книги, где Excel хранит имя пользователя и пути его папок:
//! fileSharing, absPath, журнал общей книги, пути профиля во внешних ссылках, подключениях, гиперссылках.
//! Каждый тест падал до исправления (кроме m04 — настройка выключена).

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use common::*;
use finboard_anon::anon::{run, Job};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

const RELS_NS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#;
const MAIN_NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main""#;

fn prof(clean_metadata: bool) -> Profile {
    Profile {
        rules: vec![Rule::new("Л", 0, 2, None, Kind::FullName)],
        settings: Settings { salt: "meta".into(), clean_metadata, ..Settings::default() },
    }
}

fn go(input: &Path, output: &Path, p: &Profile) {
    let job = Job { input: input.into(), output: output.into(), key_in: None, key_out: None, profile: p.clone() };
    run(&job, &mut |_| {}).unwrap();
}

fn parts(path: &Path) -> HashMap<String, String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = HashMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut s = String::new();
        if f.read_to_string(&mut s).is_ok() {
            out.insert(f.name().to_string(), s.replace("&quot;", "\"").replace("&amp;", "&"));
        }
    }
    out
}

fn people_book() -> Book {
    let mut b = Book { author: "Автор".into(), ..Default::default() };
    b.sheet("Л").extend([(1, 0, s("ФИО")), (2, 0, s("Иванов Иван Иванович"))]);
    b
}

/// Книга с именем пользователя в fileSharing и папкой файла в absPath — так их пишет Excel.
fn workbook_meta_book() -> Book {
    let mut b = people_book();
    b.workbook_head = r#"<fileVersion appName="xl" lastEdited="7" lowestEdited="7" rupBuild="27425"/><fileSharing userName="Петров Пётр"/><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><mc:Choice Requires="x15"><x15ac:absPath url="D:\Клиенты\Ромашка\" xmlns:x15ac="http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac"/></mc:Choice></mc:AlternateContent>"#.into();
    b
}

#[test]
fn m01_workbook_user_name_and_folder() {
    let d = tmp("m01");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    write_xlsx(&input, &workbook_meta_book());
    go(&input, &output, &prof(true));
    let wb = parts(&output)["xl/workbook.xml"].clone();
    for real in ["Петров", "Клиенты", "Ромашка"] {
        assert!(!wb.contains(real), "«{real}» осталось в workbook.xml: {wb}");
    }
    // Разметка остаётся на месте: Excel ждёт оба элемента целыми.
    assert!(wb.contains("<fileSharing userName=\"") && wb.contains("<x15ac:absPath url=\""), "{wb}");
}

#[test]
fn m02_user_profile_paths_in_every_service_part() {
    // Все формы пути профиля, которые пишут Windows, Excel и macOS.
    let forms = [
        r"C:\Users\petrov.p\",
        r"c:\users\petrov.p\",
        "C:/Users/petrov.p/",
        "file:///C:/Users/petrov.p/",
        r"D:\Documents and Settings\petrov.p\",
        "/Users/petrov.p/",
    ];
    for (n, form) in forms.iter().enumerate() {
        let d = tmp(&format!("m02_{n}"));
        let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
        let path = format!("{form}Documents{}Отчет.xlsx", if form.contains('\\') { '\\' } else { '/' });
        let mut b = people_book();
        // Путь в ячейке — данные пользователя, их меняют только правила.
        b.sheet("Пути").push((1, 0, s(&path)));
        b.sheet_tail.insert(0, r#"<hyperlinks><hyperlink ref="A2" r:id="rId1"/></hyperlinks>"#.into());
        b.extra.push(("xl/worksheets/_rels/sheet1.xml.rels".into(), format!(r#"{RELS_NS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{path}" TargetMode="External"/></Relationships>"#)));
        b.extra.push(("xl/externalLinks/externalLink1.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><externalLink {MAIN_NS} xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><externalBook r:id="rId1"/></externalLink>"#)));
        b.extra.push(("xl/externalLinks/_rels/externalLink1.xml.rels".into(), format!(r#"{RELS_NS}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="{path}" TargetMode="External"/></Relationships>"#)));
        b.extra.push(("xl/connections.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><connections {MAIN_NS}><connection id="1" name="Данные" type="5" odcFile="{path}"><textPr sourceFile="{path}"/></connection></connections>"#)));
        b.extra.push(("docProps/app.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><HyperlinkBase>{path}</HyperlinkBase></Properties>"#)));
        write_xlsx(&input, &b);
        go(&input, &output, &prof(true));
        let p = parts(&output);
        for (name, xml) in &p {
            if name != "xl/sharedStrings.xml" {
                assert!(!xml.contains("petrov.p"), "форма «{form}»: имя пользователя осталось в {name}: {xml}");
            }
        }
        for name in ["xl/worksheets/_rels/sheet1.xml.rels", "xl/externalLinks/_rels/externalLink1.xml.rels", "xl/connections.xml", "docProps/app.xml"] {
            assert!(p[name].contains("Отчет.xlsx"), "форма «{form}»: путь сломан в {name}: {}", p[name]);
        }
        assert!(p["xl/sharedStrings.xml"].contains(&path), "форма «{form}»: ячейка без правила изменена");
    }
}

#[test]
fn m03_shared_workbook_revision_user_names() {
    let d = tmp("m03");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = people_book();
    b.extra.push(("xl/revisions/revisionHeaders.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><headers {MAIN_NS} guid="{{00000000-0000-0000-0000-000000000001}}"><header guid="{{00000000-0000-0000-0000-000000000002}}" dateTime="2026-10-01T10:00:00" maxSheetId="2" userName="Петров Пётр" r:id="rId1" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/></headers>"#)));
    b.extra.push(("xl/revisions/userNames.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><users {MAIN_NS} count="1"><userInfo guid="{{00000000-0000-0000-0000-000000000003}}" name="Петров Пётр" id="1" dateTime="2026-10-01T10:00:00"/></users>"#)));
    write_xlsx(&input, &b);
    go(&input, &output, &prof(true));
    let p = parts(&output);
    for name in ["xl/revisions/revisionHeaders.xml", "xl/revisions/userNames.xml"] {
        assert!(!p[name].contains("Петров"), "имя пользователя осталось в {name}: {}", p[name]);
    }
}

#[test]
fn m04_setting_off_keeps_service_fields() {
    let d = tmp("m04");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = workbook_meta_book();
    b.extra.push(("xl/connections.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><connections {MAIN_NS}><connection id="1" name="Данные" type="5" odcFile="C:\Users\petrov.p\x.odc"/></connections>"#)));
    write_xlsx(&input, &b);
    go(&input, &output, &prof(false));
    let p = parts(&output);
    assert!(p["xl/workbook.xml"].contains("Петров Пётр") && p["xl/workbook.xml"].contains(r"D:\Клиенты\Ромашка\"), "{}", p["xl/workbook.xml"]);
    assert!(p["xl/connections.xml"].contains("petrov.p"), "{}", p["xl/connections.xml"]);
}
