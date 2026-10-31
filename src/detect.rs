//! Автоподбор правил по заголовкам колонок.

use crate::profile::{Kind, Rule};
use crate::xlsx::Grid;

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace('ё', "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Тип по тексту заголовка, если похоже на персональные данные.
pub fn kind_for_header(text: &str) -> Option<Kind> {
    // Данные, а не заголовок: адрес почты («ivanov@mail.ru» — не «mail»), телефон, номер.
    let digits = text.chars().filter(|c| c.is_ascii_digit()).count();
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    // «Сотрудники на 01.10.2024» — заголовок с датой; «+7 (916) 111-22-33» — данные.
    if text.chars().count() > 60 || text.trim().parse::<f64>().is_ok() || text.contains('@') || (digits >= 5 && letters < 3) {
        return None;
    }
    let lower = text.to_lowercase().replace('ё', "е");
    let w = words(text);
    let has = |list: &[&str]| w.iter().any(|x| list.contains(&x.as_str()));
    let starts = |list: &[&str]| w.iter().any(|x| list.iter().any(|p| x.starts_with(p)));

    if lower.contains("e-mail") || has(&["email", "mail", "почта", "емейл", "имейл", "емэйл"]) {
        return Some(Kind::Email);
    }
    if starts(&["телефон", "phone", "мобильн", "сотов", "whatsapp"]) || has(&["тел", "моб"]) {
        return Some(Kind::Phone);
    }
    if has(&["инн", "inn"]) {
        return Some(Kind::Inn);
    }
    if has(&["снилс"]) {
        return Some(Kind::Snils);
    }
    if starts(&["паспорт"]) {
        return Some(Kind::Passport);
    }
    if starts(&["рожден", "birth"]) || has(&["др"]) {
        return Some(Kind::BirthDate);
    }
    if starts(&["адрес", "address"]) {
        return Some(Kind::Address);
    }
    if has(&["счет", "счета", "карта", "карты", "договор", "договора", "лс", "iban", "рс"]) {
        return Some(Kind::Account);
    }
    if lower.contains("фио") || lower.contains("ф.и.о") || (lower.contains("фамилия") && lower.contains("имя")) {
        return Some(Kind::FullName);
    }
    if starts(&["фамили"]) {
        return Some(Kind::LastName);
    }
    if has(&["имя"]) {
        return Some(Kind::FirstName);
    }
    if starts(&["отчеств"]) {
        return Some(Kind::MiddleName);
    }
    if starts(&[
        "организац", "контрагент", "компани", "поставщик", "подрядчик", "юрлиц", "плательщик", "покупател",
        "заказчик", "арендатор", "арендодател", "дебитор", "кредитор", "продав", "грузополучател", "грузоотправител",
    ]) {
        return Some(Kind::Company);
    }
    if starts(&[
        "сотрудник", "клиент", "менеджер", "ответствен", "исполнител", "руководител", "работник",
        "заемщик", "получател", "физлиц", "пациент", "студент", "водител", "курьер", "куратор",
        "владел", "подписант", "специалист",
    ]) {
        return Some(Kind::FullName);
    }
    None
}

/// Строка итогов: «Итого», «Всего», «Total».
pub fn is_total(text: &str) -> bool {
    let l = text.trim().to_lowercase();
    l.starts_with("итог") || l.starts_with("всего") || l == "total" || l.starts_with("total ")
}

/// Подпись, а не данные: заголовок колонки («Исполнитель», «ФИО», «Телефон») или итог.
/// Такие значения никогда не заменяются, даже если попали в диапазон правила.
pub fn is_label(text: &str) -> bool {
    const LABELS: &[&str] = &[
        "фио", "ф.и.о.", "ф.и.о", "фамилия", "имя", "отчество", "фамилия имя отчество", "сотрудник",
        "сотрудники", "исполнитель", "исполнители", "менеджер", "клиент", "ответственный", "руководитель",
        "работник", "телефон", "тел.", "email", "e-mail", "почта", "инн", "снилс", "паспорт", "адрес",
        "дата рождения", "организация", "контрагент", "компания", "поставщик",
    ];
    let l = text.trim().trim_end_matches(':').to_lowercase().replace('ё', "е");
    is_total(&l) || LABELS.contains(&l.as_str())
}

fn text_at<'a>(grid: &'a Grid, r: u32, c: u32) -> Option<&'a str> {
    grid.get(r, c).filter(|x| !x.numeric).map(|x| x.text.as_str()).filter(|t| !t.trim().is_empty())
}

/// Заголовок колонки, а не название раздела («Итоги по исполнителям»):
/// рядом в той же строке есть другие заголовки, либо текст из одного-двух слов.
fn looks_like_header(grid: &Grid, r: u32, c: u32, text: &str) -> bool {
    if text.chars().count() > 40 || is_total(text) {
        return false;
    }
    let neighbors = (c.saturating_sub(3)..=c + 3).filter(|&x| x != c).any(|x| text_at(grid, r, x).is_some());
    neighbors || text.split_whitespace().count() <= 2
}

/// Колонки таблицы: непрерывная группа непустых ячеек строки заголовков вокруг `c`.
/// Учитывается и строка над ней: в двухстрочной шапке «ФИО» стоит в строке 1 (объединена
/// на две строки), а «Телефон», «Email» — в строке 2 под общим «Контакты».
fn header_span(grid: &Grid, r: u32, c: u32) -> (u32, u32) {
    let filled = |x: u32| grid.get(r, x).is_some() || (r > 1 && grid.get(r - 1, x).is_some());
    let (mut a, mut b) = (c, c);
    while a > 0 && filled(a - 1) {
        a -= 1;
    }
    while b < grid.max_col && filled(b + 1) {
        b += 1;
    }
    (a, b)
}

/// Строка подзаголовков: в колонках таблицы есть ещё заголовки («Телефон», «Email» под «Контакты»).
fn subheader_row(grid: &Grid, row: u32, c1: u32, c2: u32) -> bool {
    (c1..=c2).any(|x| text_at(grid, row, x).is_some_and(|t| is_label(t) || kind_for_header(t).is_some()))
}

/// Предлагает правила для листа. Для каждого заголовка с персональными данными берётся
/// таблица под ним: строки, пока в колонках таблицы есть данные, до пустой строки,
/// строки «Итого» или следующего заголовка. На одном листе может быть несколько таблиц.
pub fn suggest(sheet: &str, grid: &Grid) -> Vec<Rule> {
    let mut out: Vec<Rule> = Vec::new();
    for r in 1..=grid.max_row {
        for c in 0..=grid.max_col {
            // Значение внутри уже найденной таблицы — данные, а не новый заголовок.
            if out.iter().any(|x| x.covers(r, c)) {
                continue;
            }
            let Some(text) = text_at(grid, r, c) else { continue };
            if grid.get(r, c).is_some_and(|x| x.formula) {
                continue;
            }
            let Some(kind) = kind_for_header(text) else { continue };
            if !looks_like_header(grid, r, c, text) {
                continue;
            }
            // Общий заголовок над подзаголовками («Клиент» над «ФИО клиента») — настоящий ниже.
            // Ячейка ниже — подзаголовок, только если это точная подпись или рядом с ней тоже
            // заголовки; иначе это первое значение («Компания Вектор» под «Организация»).
            let header_like = |t: &str| is_label(t) || kind_for_header(t).is_some();
            if let Some(below) = text_at(grid, r + 1, c) {
                let neighbors = (c.saturating_sub(3)..=c + 3).filter(|&x| x != c).any(|x| text_at(grid, r + 1, x).is_some_and(header_like));
                if is_label(below) || (kind_for_header(below).is_some() && neighbors) {
                    continue;
                }
            }
            let (c1, c2) = header_span(grid, r, c);
            // Данные начинаются после строк подзаголовков (не больше двух).
            let mut first = r + 1;
            while first <= r + 2 && first <= grid.max_row && grid.get(first, c).is_none() && subheader_row(grid, first, c1, c2) {
                first += 1;
            }
            let mut last = None;
            let mut row = first;
            while row <= grid.max_row {
                let has_data = (c1..=c2).any(|x| grid.get(row, x).is_some());
                let total = (c1..=c2).any(|x| text_at(grid, row, x).is_some_and(is_total));
                let new_header = text_at(grid, row, c).is_some_and(is_label);
                if !has_data || total || new_header {
                    break;
                }
                if grid.get(row, c).is_some() {
                    last = Some(row);
                }
                row += 1;
            }
            let Some(last) = last else { continue };
            // Таблица доходит до конца листа — правило «до конца», чтобы новые строки тоже попадали.
            let to_end = row > grid.max_row;
            out.push(Rule::new(sheet, c, first, if to_end { None } else { Some(last) }, kind));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        assert_eq!(kind_for_header("ФИО сотрудника"), Some(Kind::FullName));
        assert_eq!(kind_for_header("Телефон клиента"), Some(Kind::Phone));
        assert_eq!(kind_for_header("ИНН контрагента"), Some(Kind::Inn));
        assert_eq!(kind_for_header("Длинное название"), None);
        assert_eq!(kind_for_header("Дата рождения"), Some(Kind::BirthDate));
        assert_eq!(kind_for_header("E-mail"), Some(Kind::Email));
        assert_eq!(kind_for_header("Выручка"), None);
    }

    fn grid(rows: &[(u32, u32, &str)]) -> Grid {
        let mut g = Grid::default();
        for &(r, c, t) in rows {
            let numeric = t.parse::<f64>().is_ok();
            g.cells.insert((r, c), crate::xlsx::GridCell { text: t.to_string(), numeric, formula: false });
            g.max_row = g.max_row.max(r);
            g.max_col = g.max_col.max(c);
        }
        g
    }

    /// Отчёт: шапка, таблица задач (исполнитель не везде), пустая строка,
    /// «Итоги по исполнителям», вторая таблица и строка ИТОГО.
    #[test]
    fn report_with_two_tables() {
        let mut cells = vec![
            (1, 0, "ООО «Ромашка» | ИНН/КПП: 5029290277"),
            (5, 0, "Отчет по проекту"),
            (8, 0, "Выполненные задачи"),
            (9, 0, "№"), (9, 1, "Наименование задачи"), (9, 2, "Исполнитель"), (9, 3, "Время"),
        ];
        let names = ["Иванов Иван", "Петров Пётр"];
        let mut owned: Vec<(u32, u32, String)> = Vec::new();
        for r in 10..=53u32 {
            owned.push((r, 0, (r - 9).to_string()));
            owned.push((r, 1, format!("Задача {r}")));
            owned.push((r, 3, "01:00".into()));
            if r <= 48 {
                owned.push((r, 2, names[(r % 2) as usize].into()));
            }
        }
        cells.extend([
            (55, 0, "Итоги по исполнителям"),
            (56, 0, "Исполнитель"), (56, 1, "Должность"), (56, 2, "Ставка за час"), (56, 3, "Всего часов"),
            (57, 0, "Иванов Иван"), (57, 1, "Разработчик"), (57, 2, "3960"), (57, 3, "141.5"),
            (58, 0, "Петров Пётр"), (58, 1, "Тимлид"), (58, 2, "4920"), (58, 3, "35"),
            (59, 0, "ИТОГО"), (59, 3, "176.5"),
        ]);
        let mut all: Vec<(u32, u32, &str)> = cells;
        all.extend(owned.iter().map(|(r, c, t)| (*r, *c, t.as_str())));
        let rules = suggest("Отчет", &grid(&all));
        let got: Vec<String> = rules.iter().map(|r| format!("{} {:?}", r.range_text(), r.kind)).collect();
        assert_eq!(got, vec!["C10:C48 FullName", "A57:A58 FullName"]);
    }

    /// После пустой строки сразу следующая таблица (без «Итогов» между ними):
    /// диапазон кончается на пустой строке, а не уходит в чужие значения той же колонки.
    #[test]
    fn stops_at_blank_row_before_next_table() {
        let g = grid(&[
            (1, 0, "ФИО"), (1, 1, "Сумма"),
            (2, 0, "Иванов Иван"), (2, 1, "100"),
            (3, 0, "Петров Пётр"), (3, 1, "200"),
            (5, 0, "Должность"), (5, 1, "Ставка"),
            (6, 0, "Разработчик"), (6, 1, "3960"),
        ]);
        let got: Vec<String> = suggest("Л", &g).iter().map(|r| r.range_text()).collect();
        assert_eq!(got, vec!["A2:A3"]);
    }

    /// «Итого» в соседней колонке сразу под данными, дальше без пустой строки идёт другой список.
    #[test]
    fn stops_at_total_in_other_column() {
        let g = grid(&[
            (1, 0, "№"), (1, 1, "Исполнитель"), (1, 2, "Часы"),
            (2, 0, "1"), (2, 1, "Иванов Иван"), (2, 2, "5"),
            (3, 0, "2"), (3, 1, "Петров Пётр"), (3, 2, "7"),
            (4, 0, "Итого"), (4, 2, "12"),
            (5, 0, "Согласовано"), (5, 1, "Сидоров Олег"),
        ]);
        let got: Vec<String> = suggest("Л", &g).iter().map(|r| r.range_text()).collect();
        assert_eq!(got, vec!["B2:B3"]);
    }

    /// Название раздела прямо над списком, без строки заголовков, — не заголовок колонки.
    #[test]
    fn section_title_is_not_a_header() {
        let g = grid(&[
            (1, 0, "Список ответственных сотрудников"),
            (2, 0, "Иванов Иван"),
            (3, 0, "Петров Пётр"),
        ]);
        assert!(suggest("Л", &g).is_empty());
    }

    /// Адрес почты в колонке без заголовка ПДн («Комментарий») — не заголовок «mail».
    #[test]
    fn email_value_is_not_a_header() {
        let g = grid(&[
            (1, 0, "Комментарий"), (1, 1, "Сумма"),
            (2, 0, "ivanov@mail.ru"), (2, 1, "100"),
            (3, 0, "Перезвонить"), (3, 1, "200"),
        ]);
        assert!(suggest("Л", &g).is_empty());
    }

    #[test]
    fn labels() {
        assert!(is_label("Исполнитель"));
        assert!(is_label("ИТОГО"));
        assert!(is_label("Итого по отделу"));
        assert!(!is_label("Исполнителев Иван"));
    }
}
