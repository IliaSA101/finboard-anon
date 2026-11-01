//! Доработки 0.4.0 по третьему отзыву: предпросмотр больших листов, названия колонок в отчёте.
//! Каждый тест падал до исправления.

mod common;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::*;
use finboard_anon::anon::{preview, run, Job, Report, PREVIEW_ROWS};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};
use finboard_anon::xlsx::{track_progress, Progress};

fn profile(rules: Vec<Rule>) -> Profile {
    Profile { rules, settings: Settings { salt: "fb".into(), ..Settings::default() } }
}

fn go(input: &Path, output: &Path, rules: Vec<Rule>) -> Report {
    go_key(input, output, None, None, rules)
}

fn go_key(input: &Path, output: &Path, key_in: Option<&Path>, key_out: Option<&Path>, rules: Vec<Rule>) -> Report {
    let job = Job { input: input.into(), output: output.into(), key_in: key_in.map(Into::into), key_out: key_out.map(Into::into), profile: profile(rules) };
    run(&job, &mut |_| {}).unwrap()
}

/// Предупреждение о разных типах (если есть).
fn conflict(rep: &Report) -> Option<&String> {
    rep.warnings.iter().find(|w| w.contains("разные фейки"))
}

/// Строка отчёта о правиле.
fn rule_line(rep: &Report, rules: Vec<Rule>) -> String {
    rep.summary(&profile(rules)).into_iter().find(|l| l.contains("правило «")).unwrap()
}

/// Предпросмотр справа читал лист целиком: на 447 тыс. строк — секунды на каждую правку правил, хотя ничего
/// не проверял. Теперь — первые PREVIEW_ROWS строк; обработка по-прежнему меняет весь лист.
#[test]
fn w01_preview_reads_only_first_rows() {
    let d = tmp("w01");
    let input = d.join("in.xlsx");
    let rows = PREVIEW_ROWS + 10;
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Покупатель")));
    for r in 2..=rows {
        sh.push((r, 0, s(&format!("Покупатель {r}"))));
    }
    write_xlsx(&input, &b);
    let rule = || vec![Rule::new("Л", 0, 2, None, Kind::Text)];
    let p = preview(&input, &profile(rule()), None).unwrap();
    assert_eq!(p[0].cells, (PREVIEW_ROWS - 1) as usize, "предпросмотр — только первые строки листа");
    let rep = go(&input, &d.join("out.xlsx"), rule());
    assert_eq!(rep.rule_cells[0], (rows - 1) as usize, "обработка — весь лист");
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
}

/// В отчёте было «qAll_Sales!A2:A… [Организация]: заменено…» — непонятно, что «Организация» — тип правила,
/// и какая это колонка. Теперь — название колонки из шапки и «правило «…»».
#[test]
fn w02_report_names_columns_and_rule_types() {
    let d = tmp("w02");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Ответственный")),
        (1, 1, s("Покупатель")),
        (2, 0, s("Иванов Иван")),
        (2, 1, s("ООО «Ромашка»")),
        (3, 0, s("Петров Пётр")),
        (3, 1, s("ООО «Лютик»")),
    ]);
    write_xlsx(&input, &b);
    let rep = go(&input, &d.join("out.xlsx"), vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::Company)]);
    let lines = rep.summary(&profile(vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::Company)]));
    let text = lines.join("\n");
    assert!(text.contains("колонка «Ответственный», правило «ФИО»"), "{text}");
    assert!(text.contains("колонка «Покупатель», правило «Организация»"), "{text}");
}

/// Таблица без шапки: «шапкой» оказывается строка данных. Реальное значение в журнал (его пересылают) не попадает.
#[test]
fn w03_report_never_shows_real_value_as_column_name() {
    let d = tmp("w03");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Иванов Иван")),
        (1, 1, s("Петров Пётр")),
        (2, 0, s("Сидоров Сидор")),
        (2, 1, s("Петров Пётр")),
        (3, 0, s("Иванов Иван")),
        (3, 1, s("Кузнецов Кузьма")),
    ]);
    write_xlsx(&input, &b);
    let rules = || vec![Rule::new("Л", 0, 2, None, Kind::FullName), Rule::new("Л", 1, 2, None, Kind::FullName)];
    let rep = go(&input, &d.join("out.xlsx"), rules());
    let text = rep.summary(&profile(rules())).join("\n");
    for real in ["Иванов", "Петров", "Сидоров", "Кузнецов"] {
        assert!(!text.contains(real), "в журнале реальное значение «{real}»:\n{text}");
    }
}

/// «(М) ООО "Восход"» → «(М) ООО "Прогресс"»: пометка перед формой осталась в файле. Инвариант по всем
/// написаниям: в результате нет ни одного слова исходника, кроме формы.
#[test]
fn w04_company_keeps_only_legal_form() {
    let d = tmp("w04");
    let input = d.join("in.xlsx");
    let names = ["(М) ООО \"Восход\"", "Филиал №5 ООО «Зенит» (Тверь)", "Склад \"Сфера\"", "АО «Статус» г. Казань"];
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Контрагент")));
    for (i, n) in names.iter().enumerate() {
        sh.push((i as u32 + 2, 0, s(n)));
    }
    write_xlsx(&input, &b);
    let rep = go(&input, &d.join("out.xlsx"), vec![Rule::new("Л", 0, 2, None, Kind::Company)]);
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    let g = read_book(&d.join("out.xlsx"));
    for (i, n) in names.iter().enumerate() {
        let fake = text(&g, "Л", &format!("A{}", i + 2));
        for w in ["(М)", "Филиал", "№5", "Тверь", "Склад", "Казань", "г.", "Восход", "Зенит", "Сфера", "Статус"] {
            assert!(!fake.contains(w), "{n} → {fake}: осталось «{w}»");
        }
    }
}

/// Полоса хода: объём оценивается заранее по частям книги, и к концу обработки прочитано почти столько,
/// сколько ожидалось. Иначе полоса стоит на нуле, упирается в 99 % в начале или обрывается на середине.
/// Лист без правил и общие строки тоже читаются (поиск формул, запись, проверка) — они в оценке.
/// Замен мало: результат того же размера, что исходник, и прочитано должно быть почти ровно ожидаемое.
#[test]
fn w05_progress_bar_tracks_whole_job() {
    let d = tmp("w05");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.extend([(1, 0, s("Менеджер")), (1, 1, s("Сумма")), (1, 2, s("Количество")), (1, 3, s("Цена"))]);
    for r in 2..=4000 {
        if r <= 11 {
            sh.push((r, 0, s(&people(10)[r as usize - 2])));
        }
        sh.extend([(r, 1, Cell::N(r as f64 * 1.5)), (r, 2, Cell::N(r as f64)), (r, 3, Cell::N(r as f64 * 0.25))]);
    }
    let sh = b.sheet("Справочник");
    for r in 1..=3000 {
        sh.push((r, 0, s(&format!("Товар номер {r}: полное наименование для накладной и счёта-фактуры"))));
    }
    write_xlsx(&input, &b);
    let p = Arc::new(Progress::default());
    track_progress(Some(p.clone()));
    let rep = go(&input, &d.join("out.xlsx"), vec![Rule::new("Л", 0, 2, Some(11), Kind::FullName)]);
    track_progress(None);
    assert!(rep.leaks.is_empty(), "{:?}", rep.leaks);
    let (done, total) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
    let ratio = done as f64 / total as f64;
    eprintln!("прочитано {done} из ожидаемых {total}: {ratio:.3}");
    assert!(total > 0, "объём не оценён");
    assert!((0.9..=1.1).contains(&ratio), "прочитано {done} из ожидаемых {total} ({ratio:.2})");
}

/// Доля на полосе: до оценки объёма — пусто, по ходу — прочитанное к ожидаемому, но не 100 %,
/// пока обработка не закончилась (оценка неточная, прочитано может быть больше ожидаемого).
#[test]
fn w06_progress_fraction_never_full_before_end() {
    let p = Progress::default();
    assert_eq!(p.fraction(), 0.0, "объём ещё не оценён");
    p.done.store(500, Ordering::Relaxed);
    assert_eq!(p.fraction(), 0.0, "прочитано, но объём ещё не оценён");
    p.total.store(1000, Ordering::Relaxed);
    assert_eq!(p.fraction(), 0.5);
    p.done.store(1500, Ordering::Relaxed);
    assert_eq!(p.fraction(), 0.99, "прочитано больше ожидаемого — ещё не 100 %");
}

/// Отчёт 1С: название, период и отбор в первых строках, шапка таблицы — в 12-й; в данных числа без подписей.
/// Название колонки в отчёте — из шапки, а не из строки данных (в ней больше заполненных ячеек, но это числа).
#[test]
fn w07_report_finds_header_below_title_and_numbers() {
    let d = tmp("w07");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.extend([(1, 0, s("Продажи по менеджерам")), (2, 0, s("Период: 2024 г.")), (4, 0, s("Отбор: все склады"))]);
    sh.extend([(12, 0, s("Менеджер")), (12, 1, s("Отдел"))]);
    for (i, who) in ["Иванов Иван", "Петров Пётр", "Сидоров Сидор"].iter().enumerate() {
        let r = 13 + i as u32;
        sh.extend([(r, 0, s(who)), (r, 1, s("Опт")), (r, 2, Cell::N(100.0)), (r, 3, Cell::N(200.0)), (r, 4, Cell::N(300.0))]);
    }
    write_xlsx(&input, &b);
    let rules = || vec![Rule::new("Л", 0, 13, None, Kind::FullName)];
    let rep = go(&input, &d.join("out.xlsx"), rules());
    let line = rule_line(&rep, rules());
    assert!(line.contains("колонка «Менеджер», правило «ФИО»"), "{line}");
}

/// Лист в одну колонку, над ней название отчёта. Его не выдают за название колонки: шапка — строка,
/// где подписаны хотя бы две колонки (как при выборе «колонки целиком» в окне).
#[test]
fn w08_report_title_is_not_column_name() {
    let d = tmp("w08");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("Список сотрудников")), (3, 0, s("Иванов Иван")), (4, 0, s("Петров Пётр"))]);
    write_xlsx(&input, &b);
    let rules = || vec![Rule::new("Л", 0, 3, None, Kind::FullName)];
    let rep = go(&input, &d.join("out.xlsx"), rules());
    let line = rule_line(&rep, rules());
    assert!(!line.contains("колонка «"), "{line}");
}

/// Разные ФИО для таблиц с общими значениями.
fn people(n: usize) -> Vec<String> {
    let (last, first) = (["Сидоров", "Кузнецов", "Смирнов", "Попов", "Волков"], ["Пётр", "Иван", "Олег", "Глеб", "Лев"]);
    (0..n).map(|i| format!("{} {}", last[i % 5], first[i / 5])).collect()
}

/// Таблица без шапки: в колонках A и B одни и те же значения; A — «ФИО», B — с правилом `b_rule`.
fn shared_values(d: &Path, values: &[String], b_rule: Rule) -> Report {
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    let sh = b.sheet("Л");
    for (i, v) in values.iter().enumerate() {
        sh.extend([(i as u32 + 1, 0, s(v)), (i as u32 + 1, 1, s(v))]);
    }
    write_xlsx(&input, &b);
    go(&input, &d.join("out.xlsx"), vec![Rule::new("Л", 0, 1, None, Kind::FullName), b_rule])
}

/// Число общих значений в предупреждении — по-русски: 1 значение, 2–4 значения, 5–20 значений, 21 значение…
/// Без шапки колонка называется буквой.
#[test]
fn w09_kind_conflict_counts_in_russian() {
    let d = tmp("w09");
    for (n, start) in [
        (1, "1 одинаковое значение есть"),
        (2, "2 одинаковых значения есть"),
        (4, "4 одинаковых значения есть"),
        (5, "5 одинаковых значений есть"),
        (11, "11 одинаковых значений есть"),
        (12, "12 одинаковых значений есть"),
        (21, "21 одинаковое значение есть"),
        (22, "22 одинаковых значения есть"),
    ] {
        let rep = shared_values(&d, &people(n), Rule::new("Л", 1, 1, None, Kind::Company));
        let w = conflict(&rep).unwrap_or_else(|| panic!("{n}: нет предупреждения: {:?}", rep.warnings));
        assert!(w.starts_with(start), "{n}: {w}");
        assert!(w.contains("колонка A (in.xlsx, лист Л) — правило «ФИО»"), "{n}: {w}");
        assert!(w.contains("колонка B (in.xlsx, лист Л) — правило «Организация»"), "{n}: {w}");
    }
}

/// «Шаблон» и «ФИО» с общими людьми: у них разные фейки, связь теряется — предупреждение есть
/// (молчат только о двух «Шаблонах»). Короткие пометки вроде «НД» в обеих колонках — не люди, их не считают.
#[test]
fn w10_kind_conflict_template_vs_person_and_short_marks() {
    let d = tmp("w10");
    let mut t = Rule::new("Л", 1, 1, None, Kind::Text);
    t.label = "Сотрудник".into();
    let rep = shared_values(&d, &people(2), t);
    let w = conflict(&rep).unwrap_or_else(|| panic!("нет предупреждения: {:?}", rep.warnings));
    assert!(w.contains("колонка B (in.xlsx, лист Л) — правило «Шаблон: подпись + номер»"), "{w}");

    let rep = shared_values(&d, &["НД".to_string(), "Иванов Иван".to_string()], Rule::new("Л", 1, 1, None, Kind::Company));
    let w = conflict(&rep).unwrap_or_else(|| panic!("нет предупреждения: {:?}", rep.warnings));
    assert!(w.starts_with("1 одинаковое значение есть"), "«НД» посчитано: {w}");
}

/// Значение, размеченное в прошлом запуске другим типом (оно в ключе), — предупреждение называет ключ.
/// Если оба типа — в файлах этого запуска, ключ в предупреждении не упоминается.
#[test]
fn w11_kind_conflict_names_key_only_when_it_is_the_source() {
    let d = tmp("w11");
    let key = d.join("ключ.csv");
    let (in1, in2) = (d.join("Сотрудники.xlsx"), d.join("Покупатели.xlsx"));
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("Ответственный")), (1, 1, s("Отдел")), (2, 0, s("Сидоров Пётр Ильич")), (2, 1, s("Опт"))]);
    write_xlsx(&in1, &b);
    go_key(&in1, &d.join("o1.xlsx"), None, Some(&key), vec![Rule::new("Л", 0, 2, None, Kind::FullName)]);
    let mut b = Book::default();
    b.sheet("Л").extend([(1, 0, s("Покупатель")), (1, 1, s("Город")), (2, 0, s("Сидоров Пётр Ильич")), (2, 1, s("Тверь"))]);
    write_xlsx(&in2, &b);
    let rep = go_key(&in2, &d.join("o2.xlsx"), Some(&key), None, vec![Rule::new("Л", 0, 2, None, Kind::Company)]);
    let w = conflict(&rep).unwrap_or_else(|| panic!("нет предупреждения: {:?}", rep.warnings));
    assert!(w.contains("колонка «Покупатель» (Покупатели.xlsx, лист Л, A) — правило «Организация»"), "{w}");
    assert!(w.contains("ключ прошлых запусков — правило «ФИО»"), "{w}");
    assert!(!w.contains("ключ прошлых запусков — правило «Организация»"), "{w}");

    let rep = shared_values(&d, &people(1), Rule::new("Л", 1, 1, None, Kind::Company));
    let w = conflict(&rep).unwrap_or_else(|| panic!("нет предупреждения: {:?}", rep.warnings));
    assert!(!w.contains("ключ"), "ключа не было: {w}");
}
