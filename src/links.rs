//! Связи книги с внешним миром: подключения к базам данных (xl/connections.xml) и пути к связанным файлам
//! (другие книги в формулах, связанные объекты). В них — имена серверов, логины, пароли, SQL-запросы,
//! сетевые папки и имена файлов вида «\\fileserver\HR\Иванов_ИИ.xlsx». Правила их не видят: это не ячейки.

use anyhow::Result;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

/// Перезапись значений атрибутов: `f(элемент, локальное имя атрибута, значение)` → новое значение
/// (None — оставить). Остальной текст части не меняется. None — ничего не изменилось.
fn rewrite_attrs(xml: &[u8], mut f: impl FnMut(&BytesStart, &[u8], &str) -> Option<String>) -> Result<Option<Vec<u8>>> {
    let mut rd = Reader::from_reader(xml);
    let mut w = Writer::new(Vec::with_capacity(xml.len()));
    let mut changed = false;
    loop {
        let ev = rd.read_event()?;
        match &ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let mut new: Vec<(Vec<u8>, String)> = Vec::new();
                for a in e.attributes().with_checks(false).flatten() {
                    let Ok(v) = a.unescape_value() else { continue };
                    if let Some(n) = f(e, a.key.local_name().as_ref(), &v).filter(|n| *n != v) {
                        new.push((a.key.as_ref().to_vec(), n));
                    }
                }
                if !new.is_empty() {
                    let mut c = BytesStart::new(String::from_utf8_lossy(e.name().as_ref()).into_owned());
                    for a in e.attributes().with_checks(false).flatten() {
                        match new.iter().find(|(k, _)| k.as_slice() == a.key.as_ref()) {
                            Some((k, v)) => c.push_attribute((String::from_utf8_lossy(k).as_ref(), v.as_str())),
                            None => c.push_attribute(a),
                        }
                    }
                    changed = true;
                    w.write_event(if matches!(ev, Event::Start(_)) { Event::Start(c) } else { Event::Empty(c) })?;
                    continue;
                }
            }
            _ => {}
        }
        w.write_event(ev)?;
    }
    Ok(changed.then(|| w.into_inner()))
}

fn attr_local(e: &BytesStart, local: &[u8]) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.local_name().as_ref() == local)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Имя-заглушка с расширением исходного файла: «\\srv\HR\Петров.xlsx» → «external.xlsx».
/// Без расширения — `stem.default_ext`.
pub fn placeholder_name(path: &str, stem: &str, default_ext: &str) -> String {
    let last = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let last = last.split(['?', '#']).next().unwrap_or(last);
    let ext = last
        .rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or(default_ext);
    if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{}", ext.to_ascii_lowercase())
    }
}

/// Строка подключения: значения, по которым узнаётся сервер, база, пользователь, пароль, — заглушки;
/// служебные (поставщик, драйвер, «встроенная безопасность», числа и да/нет) остаются, прочие — пустые.
/// «Data Source=srv-1c;User ID=ivanov;Password=123;Initial Catalog=Зарплата» →
/// «Data Source=server;User ID=user;Password=;Initial Catalog=database».
pub fn clean_connection_string(s: &str) -> String {
    const SERVER: &[&str] = &["data source", "server", "address", "addr", "network address", "host", "hostname", "srvr"];
    const DATABASE: &[&str] =
        &["initial catalog", "database", "dbq", "dsn", "filedsn", "location", "attachdbfilename", "initial file name", "file name", "dbname"];
    const USER: &[&str] = &["user id", "uid", "user", "username", "user name", "login", "usr"];
    const PASSWORD: &[&str] = &["password", "pwd", "pass"];
    const KEEP: &[&str] = &["provider", "driver", "integrated security", "persist security info", "trusted_connection", "mode"];
    // Приставки типа подключения в начале строки («ODBC;DSN=…»).
    const PREFIX: &[&str] = &["odbc", "oledb", "text", "url", "finder"];
    let mut out: Vec<String> = Vec::new();
    for pair in split_pairs(s) {
        let Some((k, v)) = pair.split_once('=') else {
            let p = pair.trim();
            if PREFIX.contains(&p.to_ascii_lowercase().as_str()) {
                out.push(p.to_string());
            }
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim().trim_matches(['"', '\'', '{', '}']);
        let simple = val.is_empty()
            || val.chars().all(|c| c.is_ascii_digit())
            || ["true", "false", "yes", "no", "sspi", "$workbook$"].contains(&val.to_ascii_lowercase().as_str());
        let new = if PASSWORD.contains(&key.as_str()) {
            String::new()
        } else if USER.contains(&key.as_str()) {
            "user".into()
        } else if SERVER.contains(&key.as_str()) && !simple {
            "server".into()
        } else if DATABASE.contains(&key.as_str()) {
            "database".into()
        } else if KEEP.contains(&key.as_str()) || simple {
            v.trim().to_string()
        } else {
            String::new()
        };
        out.push(format!("{}={new}", k.trim()));
    }
    let mut r = out.join(";");
    if s.trim_end().ends_with(';') && !r.is_empty() {
        r.push(';');
    }
    r
}

/// Пары «ключ=значение» через «;»; значения в кавычках и фигурных скобках могут содержать «;».
fn split_pairs(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quote) = (0, None::<char>);
    let mut after_eq = false;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                ';' => {
                    out.push(&s[start..i]);
                    start = i + 1;
                    after_eq = false;
                }
                '=' => after_eq = true,
                '"' | '\'' if after_eq && s[start..i].split_once('=').is_some_and(|(_, v)| v.trim().is_empty()) => quote = Some(c),
                '{' if after_eq => quote = Some('}'),
                _ => {}
            },
        }
    }
    out.push(&s[start..]);
    out.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// xl/connections.xml: строка подключения — без сервера, логина и пароля; запрос (SQL, имя таблицы) — пуст;
/// файл подключения (.odc), исходный файл импорта, адрес веб-запроса — заглушки; обновление при открытии
/// выключено (иначе Excel при открытии полез бы по пустому подключению). None — менять нечего.
pub fn clean_connections(xml: &[u8]) -> Result<Option<Vec<u8>>> {
    rewrite_attrs(xml, |e, key, v| {
        let el = e.local_name();
        let el = el.as_ref();
        match key {
            b"connection" => Some(clean_connection_string(v)),
            b"command" | b"serverCommand" | b"singleSignOnId" => Some(String::new()),
            b"text" if el == b"dbCommand" => Some(String::new()),
            b"odcFile" => Some("connection.odc".into()),
            b"sourceFile" => Some(placeholder_name(v, "source", "")),
            b"url" | b"editPage" if el == b"webPr" => Some("http://localhost/".into()),
            b"post" if el == b"webPr" => Some(String::new()),
            b"refreshOnLoad" if el == b"connection" && v != "0" && v != "false" => Some("0".into()),
            _ => None,
        }
    })
}

/// Пути к связанным файлам: в связях (.rels) — внешние цели, кроме гиперссылок (их меняют правила и маска
/// папки профиля), в xl/externalLinks/externalLinkN.xml — путь DDE. Путь и имя → «external.<расширение>».
/// Формулы со ссылкой на другую книгу ([1]Лист!A1) остаются и показывают сохранённые значения.
pub fn mask_linked_files(xml: &[u8]) -> Result<Option<Vec<u8>>> {
    rewrite_attrs(xml, |e, key, v| {
        let el = e.local_name();
        match (el.as_ref(), key) {
            (b"Relationship", b"Target")
                if attr_local(e, b"TargetMode").as_deref() == Some("External")
                    && !attr_local(e, b"Type").is_some_and(|t| t.ends_with("/hyperlink")) =>
            {
                Some(placeholder_name(v, "external", "xlsx"))
            }
            (b"ddeLink", b"ddeTopic") if v.contains(['\\', '/']) => Some(placeholder_name(v, "external", "xlsx")),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_strings() {
        assert_eq!(
            clean_connection_string("Provider=SQLOLEDB.1;Integrated Security=SSPI;Persist Security Info=False;Data Source=srv-1c; User ID=ivanov; Password=Qwerty1;Initial Catalog=Зарплата;Workstation ID=IVANOV-PC"),
            "Provider=SQLOLEDB.1;Integrated Security=SSPI;Persist Security Info=False;Data Source=server;User ID=user;Password=;Initial Catalog=database;Workstation ID="
        );
        assert_eq!(clean_connection_string("ODBC;DSN=1C;UID=petrov;PWD={a;b};"), "ODBC;DSN=database;UID=user;PWD=;");
        assert_eq!(
            clean_connection_string(r#"Provider=Microsoft.ACE.OLEDB.12.0;Data Source=\\fs\HR\Иванов.xlsx;Extended Properties="Excel 12.0;HDR=YES""#),
            "Provider=Microsoft.ACE.OLEDB.12.0;Data Source=server;Extended Properties="
        );
        assert_eq!(
            clean_connection_string("Provider=Microsoft.Mashup.OleDb.1;Data Source=$Workbook$;Location=Сотрудники Петров"),
            "Provider=Microsoft.Mashup.OleDb.1;Data Source=$Workbook$;Location=database"
        );
    }

    #[test]
    fn placeholders() {
        assert_eq!(placeholder_name(r"file:///\\fileserver\HR\Иванов_ИИ.xlsx", "external", "xlsx"), "external.xlsx");
        assert_eq!(placeholder_name("https://x.sharepoint.com/sites/HR/Отчёт.XLSM?web=1", "external", "xlsx"), "external.xlsm");
        assert_eq!(placeholder_name(r"\\srv\HR\Петров", "external", "xlsx"), "external.xlsx");
        assert_eq!(placeholder_name(r"C:\data\выгрузка.csv", "source", ""), "source.csv");
    }
}
